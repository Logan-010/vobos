use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use std::{
    error::Error,
    fmt::{Display, Formatter},
};
use tokio::sync::mpsc;

pub struct Say {
    sam: mpsc::Sender<String>,
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
    pub fn new(
        sam: mpsc::Sender<String>,
        responses: Option<mpsc::UnboundedSender<String>>,
    ) -> Self {
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
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        // The GUI must see tool-delivered replies even if audio playback fails.
        if let Some(responses) = &self.responses {
            let _ = responses.send(args.text.clone());
        }
        self.sam
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
    async fn spoken_reply_is_also_delivered_to_the_gui() {
        let (sam, mut audio) = mpsc::channel(1);
        let (responses, mut gui) = mpsc::unbounded_channel();
        let say = Say::new(sam, Some(responses));
        let text = "## Result\n\nThe task is **complete**.";
        say.call(&mut ToolContext::new(), SayArgs { text: text.into() })
            .await
            .expect("speech queued");
        assert_eq!(audio.recv().await.as_deref(), Some(text));
        assert_eq!(gui.recv().await.as_deref(), Some(text));
        assert!(gui.try_recv().is_err());
    }

    #[tokio::test]
    async fn audio_failure_does_not_hide_the_text_reply() {
        let (sam, audio) = mpsc::channel(1);
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
