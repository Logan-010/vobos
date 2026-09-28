//! Iced frontend. All agent, model, and audio work runs in the background.

mod backend;
mod shortcut;

use crate::cli::Cli;
use backend::{Command, Event};
use iced::widget::{
    button, column, container, markdown, row, scrollable, space, svg, text, text_input,
};
use iced::{Alignment, Element, Fill, Font, Size, Subscription, Task, Theme, window};
use std::time::Duration;
use tokio::sync::mpsc;

const THEME: Theme = Theme::KanagawaDragon;

pub fn run(cli: Cli) -> iced::Result {
    let mut settings = window::Settings {
        size: Size::new(900.0, 700.0),
        position: window::Position::Centered,
        min_size: Some(Size::new(400.0, 350.0)),
        ..window::Settings::default()
    };
    #[cfg(target_os = "linux")]
    {
        settings.platform_specific.application_id = "com.seedse.vobos".into();
    }

    iced::application(move || Home::new(cli.clone()), Home::update, Home::view)
        .title("vOBOS")
        .window(settings)
        .theme(THEME)
        .subscription(Home::subscription)
        .run()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Initializing,
    Ready,
    LoadingSpeech,
    StoppingSpeech,
    Processing,
    Unavailable,
}

impl Phase {
    fn busy(self) -> bool {
        matches!(
            self,
            Self::Initializing | Self::LoadingSpeech | Self::StoppingSpeech
        )
    }
}

#[derive(Default)]
struct Reply {
    source: String,
    formatted: markdown::Content,
}

impl Reply {
    fn update(&mut self, source: String) {
        if let Some(delta) = source.strip_prefix(&self.source) {
            self.formatted.push_str(delta);
        } else {
            // A model retry or authoritative final response can replace text.
            self.formatted = markdown::Content::parse(&source);
        }
        self.source = source;
    }
}

struct Turn {
    prompt: String,
    replies: Vec<Reply>,
}

struct Home {
    commands: mpsc::Sender<Command>,
    phase: Phase,
    listening: bool,
    stopping_listening: bool,
    cancelling: bool,
    window_opened: bool,
    activity: String,
    input: String,
    turns: Vec<Turn>,
    error: Option<String>,
    notice: Option<String>,
    tick: usize,
}

#[derive(Debug, Clone)]
enum Message {
    Opened(window::Id),
    MonitorSize(window::Id, Option<Size>),
    Backend(Event),
    Input(String),
    Submit,
    ToggleListening,
    Cancel,
    Retry,
    Tick,
    CopyReply(usize, usize),
    LinkClicked(markdown::Uri),
}

impl Home {
    fn new(cli: Cli) -> (Self, Task<Message>) {
        let (commands, receiver) = mpsc::channel(8);
        let task = Task::run(
            iced::stream::channel(8, async move |output| {
                backend::run(cli, receiver, output).await;
            }),
            Message::Backend,
        );
        (
            Self {
                commands,
                phase: Phase::Initializing,
                listening: false,
                stopping_listening: false,
                cancelling: false,
                window_opened: false,
                activity: "Thinking…".into(),
                input: String::new(),
                turns: Vec::new(),
                error: None,
                notice: None,
                tick: 0,
            },
            task,
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        let animation = if self.phase.busy() {
            iced::time::every(Duration::from_millis(350)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        };
        // An event subscription receives *all* window input before filtering.
        // Keeping open_events alive needlessly fills Iced's 100-event queue
        // during scroll bursts. Drop it as soon as the one-time sizing starts.
        let opening = if self.window_opened {
            Subscription::none()
        } else {
            window::open_events().map(Message::Opened)
        };
        Subscription::batch([opening, animation])
    }

    fn send(&mut self, command: Command) -> bool {
        match self.commands.try_send(command) {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(format!("Could not reach the agent: {error}"));
                false
            }
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Opened(id) => {
                self.window_opened = true;
                return window::monitor_size(id).map(move |size| Message::MonitorSize(id, size));
            }
            Message::MonitorSize(id, Some(monitor)) => {
                return window::resize(
                    id,
                    Size::new(
                        (monitor.width * 0.4).max(360.0),
                        (monitor.height * 0.3).max(240.0),
                    ),
                );
            }
            Message::MonitorSize(_, None) => {}
            Message::Input(input) => self.input = input,
            Message::Submit
                if self.phase == Phase::Ready
                    && !self.cancelling
                    && !self.input.trim().is_empty() =>
            {
                if self.send(Command::Prompt(self.input.trim().to_owned())) {
                    self.input.clear();
                    self.error = None;
                    self.notice = None;
                    self.phase = Phase::Processing;
                }
            }
            Message::Cancel if self.can_cancel() => {
                if self.send(Command::Cancel) {
                    self.cancelling = true;
                    self.notice = Some("Stopping prompt and speech…".into());
                }
            }
            Message::ToggleListening
                if !self.cancelling
                    && !self.stopping_listening
                    && (self.phase == Phase::LoadingSpeech
                        || (matches!(self.phase, Phase::Ready | Phase::Processing)
                            && self.listening)) =>
            {
                if self.send(Command::StopListening) {
                    // Wait for acknowledgement before allowing another start or prompt.
                    self.stopping_listening = true;
                    if self.phase != Phase::Processing {
                        self.phase = Phase::StoppingSpeech;
                    }
                    self.notice = Some("Stopping microphone…".into());
                }
            }
            Message::ToggleListening
                if self.phase == Phase::Ready && !self.stopping_listening && !self.cancelling =>
            {
                if self.send(Command::Listen) {
                    self.phase = Phase::LoadingSpeech;
                    self.error = None;
                    self.notice = None;
                }
            }
            Message::Retry if self.phase == Phase::Unavailable => {
                if self.send(Command::Retry) {
                    self.phase = Phase::Initializing;
                    self.error = None;
                }
            }
            Message::Backend(event) => self.handle_event(event),
            Message::Tick => self.tick = (self.tick + 1) % 4,
            Message::CopyReply(turn, reply) => {
                if let Some(reply) = self
                    .turns
                    .get(turn)
                    .and_then(|turn| turn.replies.get(reply))
                {
                    self.notice = Some("Reply copied.".into());
                    return iced::clipboard::write(reply.source.clone());
                }
            }
            Message::LinkClicked(url) => {
                self.notice = Some("Link copied to clipboard.".into());
                return iced::clipboard::write(url);
            }
            _ => {}
        }
        Task::none()
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Initializing => self.phase = Phase::Initializing,
            Event::Ready | Event::Finished => {
                if !self.cancelling {
                    self.phase = Phase::Ready;
                }
            }
            Event::Cancelled => {
                self.cancelling = false;
                self.phase = Phase::Ready;
                self.activity = "Thinking…".into();
                self.notice = Some("Prompt and speech stopped.".into());
            }
            Event::InitializationFailed(error) => {
                self.phase = Phase::Unavailable;
                self.error = Some(error);
            }
            Event::LoadingSpeech if self.phase != Phase::StoppingSpeech => {
                self.phase = Phase::LoadingSpeech;
            }
            Event::LoadingSpeech => {}
            Event::Listening(listening) => {
                self.listening = listening;
                self.stopping_listening = false;
                if self.phase != Phase::Processing {
                    self.phase = Phase::Ready;
                }
                self.notice = None;
                self.activity = "Thinking…".into();
            }
            Event::Started(prompt) => {
                self.phase = Phase::Processing;
                self.error = None;
                if !self.cancelling {
                    self.notice = None;
                }
                self.turns.push(Turn {
                    prompt,
                    replies: Vec::new(),
                });
            }
            Event::ReplyUpdated { id, text } => {
                if let Some(turn) = self.turns.last_mut() {
                    if turn.replies.len() <= id {
                        turn.replies.resize_with(id + 1, Reply::default);
                    }
                    turn.replies[id].update(text);
                    self.activity = "Responding…".into();
                }
            }
            Event::Activity(activity) => self.activity = activity,
            Event::Error(error) => self.error = Some(error),
        }
    }

    fn can_cancel(&self) -> bool {
        !self.cancelling && matches!(self.phase, Phase::Processing | Phase::Ready)
    }

    fn view(&self) -> Element<'_, Message> {
        let header = row![
            svg(svg::Handle::from_memory(include_bytes!(
                "../assets/com.seedse.vobos.svg"
            )))
            .width(30)
            .height(30),
            text("vOBOS").size(20).font(Font {
                weight: iced::font::Weight::Bold,
                ..Font::DEFAULT
            }),
            space::horizontal(),
            text(if self.listening { "MIC ON" } else { "HOME" }).size(11),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let content: Element<'_, Message> = if self.phase.busy() {
            let (title, detail) = match self.phase {
                Phase::Initializing => (
                    "Starting your agent",
                    "Loading configuration, SAM, and tools.",
                ),
                Phase::LoadingSpeech => (
                    "Preparing voice input",
                    "Downloading / loading Whisper and voice detection models. The first start may take a while.",
                ),
                Phase::StoppingSpeech => ("Stopping voice input", "Releasing the microphone."),
                _ => (
                    "Working on your command",
                    "The agent is thinking and using tools. SAM will speak its response.",
                ),
            };
            container(
                column![
                    text(format!("{title}{}", ".".repeat(self.tick))).size(21),
                    text(detail).size(13),
                ]
                .spacing(12),
            )
            .padding(16)
            .center_x(Fill)
            .center_y(Fill)
            .into()
        } else {
            let mut history = column![].spacing(14).width(Fill);
            if self.turns.is_empty() {
                history = history.push(
                    container(
                        column![
                            text("What can I help you with?").size(22),
                            text("Type a command below, or begin listening and speak naturally.")
                                .size(14),
                        ]
                        .spacing(10),
                    )
                    .padding(16)
                    .width(Fill)
                    .style(container::bordered_box),
                );
            }
            for (turn_index, turn) in self.turns.iter().enumerate() {
                let mut conversation =
                    column![text("YOU").size(11), text(&turn.prompt).size(15),].spacing(10);
                for (reply_index, reply) in turn.replies.iter().enumerate() {
                    if reply.source.is_empty() {
                        continue;
                    }
                    conversation = conversation
                        .push(
                            row![
                                text("vOBOS").size(11),
                                space::horizontal(),
                                button(text("Copy").size(11))
                                    .style(button::text)
                                    .on_press(Message::CopyReply(turn_index, reply_index)),
                            ]
                            .align_y(Alignment::Center),
                        )
                        .push(
                            markdown::view(reply.formatted.items(), THEME)
                                .map(Message::LinkClicked),
                        );
                }
                history = history.push(
                    container(conversation)
                        .padding(14)
                        .width(Fill)
                        .style(container::bordered_box),
                );
            }
            if self.phase == Phase::Processing {
                history = history.push(text(&self.activity).size(12));
            }
            scrollable(history).anchor_bottom().height(Fill).into()
        };

        let can_send =
            self.phase == Phase::Ready && !self.cancelling && !self.input.trim().is_empty();
        let input = text_input("Type a command…", &self.input)
            .on_input(Message::Input)
            .on_submit_maybe(can_send.then_some(Message::Submit))
            .padding(10);
        let send = button("Send")
            .padding(10)
            .on_press_maybe(can_send.then_some(Message::Submit));
        let listen_label = if self.phase == Phase::LoadingSpeech {
            "Cancel listening"
        } else if self.listening {
            "Stop listening"
        } else {
            "Begin listening"
        };
        let can_listen = !self.cancelling
            && !self.stopping_listening
            && (matches!(self.phase, Phase::Ready | Phase::LoadingSpeech)
                || (self.phase == Phase::Processing && self.listening));
        let microphone_icon = svg(svg::Handle::from_memory(include_bytes!(
            "../assets/icons/microphone.svg"
        )))
        .width(16)
        .height(16)
        .style(|theme: &Theme, _| svg::Style {
            color: Some(theme.palette().text),
        });
        let listen = button(
            row![microphone_icon, text(listen_label).size(13)]
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .style(button::secondary)
        .on_press_maybe(can_listen.then_some(Message::ToggleListening));
        let status = self.notice.as_deref().unwrap_or(match self.phase {
            Phase::Initializing => "Starting…",
            Phase::LoadingSpeech => "Loading speech models…",
            Phase::StoppingSpeech => "Stopping microphone…",
            Phase::Processing => &self.activity,
            Phase::Unavailable => "Agent unavailable",
            Phase::Ready if self.listening => "Listening · pauses during replies",
            Phase::Ready => "Ready · Space: listen",
        });

        let mut layout = column![header, content].spacing(12);
        if let Some(error) = &self.error {
            layout = layout.push(
                scrollable(container(text(error).size(13).style(text::danger)).padding(8))
                    .height(60),
            );
        }
        if self.phase == Phase::Unavailable {
            layout = layout.push(button("Retry initialization").on_press(Message::Retry));
        }
        let stop = button(if self.cancelling {
            "Stopping…"
        } else {
            "Stop (Esc)"
        })
        .style(button::danger)
        .padding(10)
        .on_press_maybe(self.can_cancel().then_some(Message::Cancel));
        layout = layout.push(row![input, send, stop].spacing(8)).push(
            row![listen, text(status).size(11).width(Fill)]
                .spacing(12)
                .align_y(Alignment::Center),
        );
        shortcut::space(
            container(layout).padding(16).width(Fill).height(Fill),
            Message::ToggleListening,
            self.can_cancel().then_some(Message::Cancel),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cancellation_waits_for_ack_preserves_draft_and_partial_reply_and_allows_retry() {
        let (mut home, _task) = Home::new(Cli::parse_from(["vobos"]));
        // Keep the command receiver alive without running a real agent.
        let (sender, mut commands) = mpsc::channel(8);
        home.commands = sender;
        home.handle_event(Event::Started("Hanging prompt".into()));
        home.handle_event(Event::ReplyUpdated {
            id: 0,
            text: "Partial reply".into(),
        });
        home.input = "Next prompt".into();
        let _ = home.update(Message::Cancel);
        assert!(matches!(commands.try_recv(), Ok(Command::Cancel)));
        assert!(home.cancelling);
        assert!(!home.can_cancel());
        home.handle_event(Event::Finished); // A completion raced with Stop.
        let _ = home.update(Message::Submit);
        assert!(commands.try_recv().is_err());
        assert_eq!(home.input, "Next prompt");
        home.handle_event(Event::Cancelled);
        assert_eq!(home.phase, Phase::Ready);
        assert_eq!(home.turns[0].replies[0].source, "Partial reply");
        assert!(
            home.can_cancel(),
            "Stop must also work during post-response speech"
        );
        let _ = home.update(Message::Submit);
        assert!(matches!(commands.try_recv(), Ok(Command::Prompt(text)) if text == "Next prompt"));
    }

    #[test]
    fn prompts_are_serialized_and_replies_preserved_after_failure() {
        let (mut home, _task) = Home::new(Cli::parse_from(["vobos"]));
        home.handle_event(Event::Ready);
        home.handle_event(Event::Started("Help me".into()));
        home.input = "Another prompt".into();
        let _ = home.update(Message::Submit);
        assert_eq!(home.input, "Another prompt");
        assert_eq!(home.turns.len(), 1);
        home.handle_event(Event::ReplyUpdated {
            id: 0,
            text: "## Result\n\n**Done**".into(),
        });
        home.handle_event(Event::Error("Tool failed".into()));
        home.handle_event(Event::Finished);
        assert_eq!(home.phase, Phase::Ready);
        assert_eq!(home.error.as_deref(), Some("Tool failed"));
        assert_eq!(home.turns[0].replies[0].formatted.items().len(), 2);
    }

    #[test]
    fn stopping_voice_restores_manual_input() {
        let (mut home, _task) = Home::new(Cli::parse_from(["vobos"]));
        home.handle_event(Event::LoadingSpeech);
        home.handle_event(Event::Listening(false));
        assert_eq!(home.phase, Phase::Ready);
        assert!(!home.listening);
    }

    #[test]
    fn processing_has_no_window_event_or_animation_subscriptions() {
        let (mut home, _task) = Home::new(Cli::parse_from(["vobos"]));
        let _ = home.update(Message::Opened(window::Id::unique()));
        for phase in [Phase::Ready, Phase::Processing] {
            home.phase = phase;
            assert!(iced::advanced::subscription::into_recipes(home.subscription()).is_empty());
        }
    }

    #[test]
    fn streaming_updates_one_reply_and_stop_does_not_finish_the_prompt() {
        let (mut home, _task) = Home::new(Cli::parse_from(["vobos"]));
        home.handle_event(Event::Listening(true));
        home.handle_event(Event::Started("Hello".into()));
        home.handle_event(Event::ReplyUpdated {
            id: 0,
            text: "# Hello\n\n**Wor".into(),
        });
        home.handle_event(Event::ReplyUpdated {
            id: 0,
            text: "# Hello\n\n**World**".into(),
        });
        home.handle_event(Event::Listening(false));
        assert_eq!(home.phase, Phase::Processing);
        assert!(!home.phase.busy(), "streamed history must stay visible");
        assert!(!home.listening);
        assert_eq!(home.turns[0].replies.len(), 1);
        assert_eq!(home.turns[0].replies[0].source, "# Hello\n\n**World**");
        home.handle_event(Event::ReplyUpdated {
            id: 0,
            text: "Replaced".into(),
        });
        assert_eq!(home.turns[0].replies[0].formatted.items().len(), 1);
    }
}
