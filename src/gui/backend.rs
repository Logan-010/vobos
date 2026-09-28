//! A single background owner of the agent, conversation, and microphone.
//! Commands are serialized so typed and spoken prompts cannot overlap.

use crate::{agent, cli::Cli, config::Config, mcp, sam, whisper::Whisper};
use iced::futures::{FutureExt, SinkExt, StreamExt, channel::mpsc::Sender};
use rig::{
    agent::MultiTurnStreamItem,
    streaming::{StreamedAssistantContent, StreamingPrompt},
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum Command {
    Prompt(String),
    Listen,
    StopListening,
    Cancel,
    Retry,
}

#[derive(Debug, Clone)]
pub enum Event {
    Initializing,
    Ready,
    InitializationFailed(String),
    LoadingSpeech,
    Listening(bool),
    Started(String),
    ReplyUpdated { id: usize, text: String },
    Activity(String),
    Finished,
    Cancelled,
    Error(String),
}

struct Session {
    config: Config,
    agent: rig::Agent,
    // MCP services must live for as long as the agent uses their tools.
    _connections: mcp::Connections,
    session_id: Arc<Mutex<String>>,
    sam: sam::Speech,
    playback_active: Arc<AtomicBool>,
    agent_running: Arc<AtomicBool>,
}

impl Session {
    async fn new(cli: &Cli) -> color_eyre::Result<Self> {
        let (config, mcp_config) = crate::load_config(cli).await?;
        let (sam, playback_active) = tokio::task::spawn_blocking(sam::init_task).await??;
        let session_id = Arc::new(Mutex::new("default".to_owned()));
        let (agent, connections) = agent::build(
            &config,
            sam.clone(),
            Arc::clone(&session_id),
            mcp_config,
            true,
        )
        .await?;
        Ok(Self {
            config,
            agent,
            _connections: connections,
            session_id,
            sam,
            playback_active,
            agent_running: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn prompt(
        &mut self,
        prompt: String,
        output: &mut Sender<Event>,
        commands: &mut mpsc::Receiver<Command>,
        microphone: &mut Option<Microphone>,
        pending: &mut VecDeque<Command>,
    ) {
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() || prompt == "[BLANK_AUDIO]" {
            return;
        }
        self.agent_running.store(true, Ordering::Release);
        let _ = output.send(Event::Started(prompt.clone())).await;
        let mut control_output = output.clone();
        let mut replies = StreamReplies::default();
        let (responses, mut spoken) = mpsc::unbounded_channel();
        let result = run_until_cancelled(
            self.complete(prompt, output, &mut replies, responses, &mut spoken),
            commands,
            microphone,
            pending,
            &mut control_output,
        )
        .await;
        // The completion future (and Rig stream) is dropped before acknowledging
        // cancellation, including when it is stuck awaiting a model or tool.
        if result.is_none() {
            self.sam.cancel();
            pending.retain(|command| !matches!(command, Command::Prompt(_)));
            discard_pending(microphone);
        }
        self.agent_running.store(false, Ordering::Release);
        while let Ok(reply) = spoken.try_recv() {
            replies.spoken(reply);
        }
        replies.flush(output).await;
        match result {
            Some(result) => {
                if let Err(error) = result {
                    let _ = output.send(Event::Error(format!("{error:#}"))).await;
                }
                let _ = output.send(Event::Finished).await;
            }
            None => {
                let _ = output.send(Event::Cancelled).await;
            }
        }
    }

    async fn complete(
        &mut self,
        prompt: String,
        output: &mut Sender<Event>,
        replies: &mut StreamReplies,
        responses: mpsc::UnboundedSender<String>,
        spoken: &mut mpsc::UnboundedReceiver<String>,
    ) -> color_eyre::Result<()> {
        let conversation = self
            .session_id
            .lock()
            .map_err(|_| color_eyre::eyre::eyre!("Session state is unavailable"))?
            .clone();
        let speech = self.sam.current();
        let mut context = rig::tool::ToolContext::new();
        context.insert(speech.clone());
        context.insert(responses);
        let mut stream = self
            .agent
            .stream_prompt(prompt)
            .conversation(conversation)
            .tool_context(context)
            .await;
        // Bound UI updates to 20 Hz regardless of provider token speed. This
        // interval is local to the worker, not a window-event subscription.
        let mut frame = tokio::time::interval(Duration::from_millis(50));
        frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let result: color_eyre::Result<()> = loop {
            tokio::select! {
                item = stream.next() => match item {
                    Some(Ok(item)) => {
                        if let Some(activity) = replies.push(item) {
                            let _ = output.send(Event::Activity(activity)).await;
                        }
                    }
                    Some(Err(error)) => break Err(error.into()),
                    None => break Ok(()),
                },
                Some(reply) = spoken.recv() => {
                    replies.spoken(reply);
                },
                _ = frame.tick() => replies.flush(output).await,
            }
        };
        // Preserve partial output on failure, including the last tool reply.
        while let Ok(reply) = spoken.try_recv() {
            replies.spoken(reply);
        }
        replies.flush(output).await;
        result?;
        let final_text = replies.final_text.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!("The agent stream ended before completing its response")
        })?;
        if !replies.used_say && !final_text.trim().is_empty() {
            speech.send(final_text.clone()).await?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct StreamReplies {
    replies: Vec<(String, bool)>,
    current: Option<usize>,
    final_text: Option<String>,
    used_say: bool,
}

impl StreamReplies {
    fn push(&mut self, item: MultiTurnStreamItem) -> Option<String> {
        match item {
            MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(delta)) => {
                if !delta.text.is_empty() {
                    let id = *self.current.get_or_insert_with(|| {
                        self.replies.push((String::new(), false));
                        self.replies.len() - 1
                    });
                    self.replies[id].0.push_str(&delta.text);
                    self.replies[id].1 = true;
                }
            }
            MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
                tool_call,
                ..
            }) => {
                self.current = None;
                return Some(format!("Running {}…", tool_call.function.name));
            }
            MultiTurnStreamItem::ModelTurnRetried { .. } => {
                if let Some(id) = self.current.take() {
                    self.replies[id] = (String::new(), true);
                }
                return Some("Retrying response…".into());
            }
            MultiTurnStreamItem::FinalResponse(response) => {
                self.complete_text(&response.output);
                self.final_text = Some(response.output);
            }
            _ => {}
        }
        None
    }

    fn complete_text(&mut self, text: &str) {
        if text.trim().is_empty()
            || self
                .replies
                .iter()
                .any(|(reply, _)| reply.trim() == text.trim())
        {
            return;
        }
        if let Some(id) = self.current {
            self.replies[id] = (text.to_owned(), true);
        } else {
            self.replies.push((text.to_owned(), true));
        }
    }

    fn spoken(&mut self, text: String) {
        self.used_say = true;
        // A say call may echo text already streamed. Never append it twice.
        if !text.trim().is_empty()
            && !self
                .replies
                .iter()
                .any(|(reply, _)| reply.trim() == text.trim())
        {
            self.replies.push((text, true));
        }
    }

    async fn flush(&mut self, output: &mut Sender<Event>) {
        for (id, (text, dirty)) in self.replies.iter_mut().enumerate() {
            if std::mem::take(dirty) {
                let _ = output
                    .send(Event::ReplyUpdated {
                        id,
                        text: text.clone(),
                    })
                    .await;
            }
        }
    }
}

type Microphone = mpsc::UnboundedReceiver<crate::whisper::SpeechSegment>;

async fn run_until_cancelled(
    completion: impl std::future::Future<Output = color_eyre::Result<()>>,
    commands: &mut mpsc::Receiver<Command>,
    microphone: &mut Option<Microphone>,
    pending: &mut VecDeque<Command>,
    output: &mut Sender<Event>,
) -> Option<color_eyre::Result<()>> {
    tokio::pin!(completion);
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(Command::Cancel) | None => return None,
                Some(Command::StopListening) => {
                    *microphone = None;
                    let _ = output.send(Event::Listening(false)).await;
                }
                // A typed prompt may race with the first voice event.
                Some(command) => pending.push_back(command),
            },
            result = &mut completion => return Some(result),
        }
    }
}

pub async fn run(cli: Cli, mut commands: mpsc::Receiver<Command>, mut output: Sender<Event>) {
    let mut session = loop {
        let _ = output.send(Event::Initializing).await;
        match Session::new(&cli).await {
            Ok(session) => break session,
            Err(error) => {
                let _ = output
                    .send(Event::InitializationFailed(format!("{error:#}")))
                    .await;
                loop {
                    match commands.recv().await {
                        Some(Command::Retry) => break,
                        None => return,
                        _ => {}
                    }
                }
            }
        }
    };
    let _ = output.send(Event::Ready).await;
    let mut microphone: Option<Microphone> = None;
    let mut pending = VecDeque::new();

    loop {
        tokio::select! {
            // Stop requests take precedence over buffered microphone segments.
            biased;
            command = async {
                match pending.pop_front() {
                    Some(command) => Some(command),
                    None => commands.recv().await,
                }
            } => match command {
                None => { session.sam.cancel(); return; },
                Some(Command::Retry) => {},
                Some(Command::Cancel) => {
                    session.sam.cancel();
                    pending.retain(|command| !matches!(command, Command::Prompt(_)));
                    discard_pending(&mut microphone);
                    let _ = output.send(Event::Cancelled).await;
                }
                Some(Command::StopListening) => {
                    // Dropping the receiver stops the transcription worker and releases CPAL.
                    microphone = None;
                    let _ = output.send(Event::Listening(false)).await;
                }
                Some(Command::Listen) if microphone.is_none() => {
                    let _ = output.send(Event::LoadingSpeech).await;
                    let loading = Whisper::new(
                        session.config.repo.clone(),
                        session.config.model.clone(),
                        session.config.cache.clone(),
                    ).boxed();
                    let mut cancelled = false;
                    let result = tokio::select! {
                        biased;
                        command = commands.recv() => match command {
                            None => { session.sam.cancel(); return; },
                            Some(Command::Cancel) => {
                                session.sam.cancel();
                                cancelled = true;
                                None
                            }
                            // While loading, the UI exposes only Stop listening.
                            _ => None,
                        },
                        result = loading => Some(result),
                    };
                    let Some(result) = result else {
                        let _ = output.send(Event::Listening(false)).await;
                        if cancelled {
                            let _ = output.send(Event::Cancelled).await;
                        }
                        continue;
                    };
                    match result.and_then(|whisper| whisper.transcribe_mic(
                        Arc::clone(&session.playback_active),
                        Arc::clone(&session.agent_running),
                    )) {
                        Ok(receiver) => {
                            microphone = Some(receiver);
                            let _ = output.send(Event::Listening(true)).await;
                        }
                        Err(error) => {
                            let _ = output.send(Event::Listening(false)).await;
                            let _ = output.send(Event::Error(format!("Could not start listening: {error:#}"))).await;
                        }
                    }
                }
                Some(Command::Listen) => {},
                Some(Command::Prompt(prompt)) => {
                    session.prompt(prompt, &mut output, &mut commands, &mut microphone, &mut pending).await;
                    discard_pending(&mut microphone);
                }
            },
            segment = async {
                match &mut microphone {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            } => match segment {
                Some(segment) => {
                    session.prompt(segment.text, &mut output, &mut commands, &mut microphone, &mut pending).await;
                    discard_pending(&mut microphone);
                }
                None => {
                    microphone = None;
                    let _ = output.send(Event::Listening(false)).await;
                    let _ = output.send(Event::Error(
                        "Microphone stopped. Check your input device and logs, then try listening again.".into()
                    )).await;
                }
            }
        }
    }
}

fn discard_pending(microphone: &mut Option<Microphone>) {
    if let Some(receiver) = microphone {
        while receiver.try_recv().is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancels_a_hanging_completion_and_preserves_unflushed_text() {
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Dropped(dropped.clone());
        let (commands, mut receiver) = mpsc::channel(8);
        let (mut output, mut events) = iced::futures::channel::mpsc::channel(8);
        let mut replies = StreamReplies::default();
        let (started, running) = tokio::sync::oneshot::channel();
        let completion = async {
            let _guard = guard;
            replies.push(delta("Partial"));
            let _ = started.send(());
            std::future::pending::<color_eyre::Result<()>>().await
        };
        let cancel = async {
            running.await.unwrap();
            commands.send(Command::Cancel).await.unwrap();
        };
        let mut microphone = None;
        let mut pending = VecDeque::new();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                run_until_cancelled(
                    completion,
                    &mut receiver,
                    &mut microphone,
                    &mut pending,
                    &mut output
                ),
                cancel
            )
        })
        .await
        .expect("cancellation must not wait for completion");
        assert!(result.is_none());
        assert!(dropped.load(Ordering::Acquire));
        replies.flush(&mut output).await;
        assert!(
            matches!(events.next().await, Some(Event::ReplyUpdated { text, .. }) if text == "Partial")
        );
        assert!(
            run_until_cancelled(
                async { Ok(()) },
                &mut receiver,
                &mut microphone,
                &mut pending,
                &mut output
            )
            .await
            .unwrap()
            .is_ok()
        );
    }

    fn delta(text: &str) -> MultiTurnStreamItem {
        MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::text(text))
    }

    #[tokio::test]
    async fn tokens_are_batched_and_visible_before_completion_without_duplicates() {
        let mut replies = StreamReplies::default();
        let (mut output, mut events) = iced::futures::channel::mpsc::channel(8);
        for token in ["## Result", "\n\n", "**Done**"] {
            replies.push(delta(token));
        }
        replies.flush(&mut output).await;
        assert!(
            matches!(events.next().await, Some(Event::ReplyUpdated { id: 0, text })
            if text == "## Result\n\n**Done**")
        );
        assert!(replies.final_text.is_none());
        replies.spoken("## Result\n\n**Done**".into());
        replies.push(MultiTurnStreamItem::FinalResponse(
            rig::agent::PromptResponse::new("## Result\n\n**Done**", Default::default()),
        ));
        assert_eq!(replies.replies.len(), 1);
        assert!(!replies.replies[0].1);
        assert!(replies.used_say);
    }

    #[test]
    fn retry_replaces_provisional_text_and_final_only_responses_are_kept() {
        let mut replies = StreamReplies::default();
        replies.push(delta("Rejected"));
        replies.push(MultiTurnStreamItem::ModelTurnRetried { turn: 1 });
        replies.push(delta("Accepted"));
        assert!(replies.replies[0].0.is_empty());
        assert_eq!(replies.replies[1].0, "Accepted");
        let mut replies = StreamReplies::default();
        replies.complete_text("Final-only output");
        assert_eq!(replies.replies[0].0, "Final-only output");
    }
}
