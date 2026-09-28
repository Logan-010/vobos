use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use std::{
    error::Error,
    fmt::{Display, Formatter},
};
use tokio::sync::mpsc;

pub struct Say {
    sam: crate::sam::Speech,
    responses: Option<mpsc::UnboundedSender<String>>,
}

#[derive(Debug, Deserialize)]
pub struct SayArgs {
    pub text: String,
}

#[derive(Debug)]
pub struct SayError(String);

impl Display for SayError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for SayError {}

impl Say {
    pub fn new(sam: crate::sam::Speech, responses: Option<mpsc::UnboundedSender<String>>) -> Self {
        Self { sam, responses }
    }
}

impl Tool for Say {
    const NAME: &'static str = "say";
    type Args = SayArgs;
    type Output = String;
    type Error = SayError;

    fn description(&self) -> String {
        "Speak the provided text aloud to the user.".to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to speak aloud"
                }
            },
            "required": ["text"]
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let speech = context
            .get::<crate::sam::Speech>()
            .cloned()
            .unwrap_or_else(|| self.sam.current());
        if speech.is_cancelled() {
            return Err(SayError("Speech cancelled".into()));
        }
        // Each GUI run supplies its own response channel, so late tool results
        // cannot appear in a subsequent prompt after cancellation.
        if let Some(responses) = context
            .get::<mpsc::UnboundedSender<String>>()
            .or(self.responses.as_ref())
        {
            let _ = responses.send(args.text.clone());
        }
        speech
            .send(args.text)
            .await
            .map_err(|error| SayError(format!("Could not queue speech: {error}")))?;

        Ok("Speech queued.".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_turn_cannot_speak_or_publish_a_late_reply() {
        let (sam, mut audio) = crate::sam::speech_channel();
        let (responses, mut gui) = mpsc::unbounded_channel::<String>();
        let say = Say::new(sam.clone(), None);
        let mut context = ToolContext::new();
        context.insert(sam.current());
        context.insert(responses);
        sam.cancel();
        assert!(
            say.call(
                &mut context,
                SayArgs {
                    text: "Late reply".into()
                }
            )
            .await
            .is_err()
        );
        assert!(audio.try_recv().is_err());
        assert!(gui.try_recv().is_err());
    }

    #[tokio::test]
    async fn spoken_reply_is_also_delivered_to_the_gui() {
        let (sam, mut audio) = crate::sam::speech_channel();
        let (responses, mut gui) = mpsc::unbounded_channel();
        let say = Say::new(sam, Some(responses));
        let text = "## Result\n\nThe task is **complete**.";
        say.call(&mut ToolContext::new(), SayArgs { text: text.into() })
            .await
            .expect("speech queued");
        assert_eq!(audio.recv().await.unwrap().text, text);
        assert_eq!(gui.recv().await.as_deref(), Some(text));
        assert!(gui.try_recv().is_err());
    }

    #[tokio::test]
    async fn audio_failure_does_not_hide_the_text_reply() {
        let (sam, audio) = crate::sam::speech_channel();
        drop(audio);
        let (responses, mut gui) = mpsc::unbounded_channel();
        let say = Say::new(sam, Some(responses));
        assert!(
            say.call(
                &mut ToolContext::new(),
                SayArgs {
                    text: "Hello".into()
                }
            )
            .await
            .is_err()
        );
        assert_eq!(gui.recv().await.as_deref(), Some("Hello"));
    }
}
