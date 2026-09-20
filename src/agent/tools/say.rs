use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use std::{
    error::Error,
    fmt::{Display, Formatter},
};
use tokio::sync::mpsc;

pub struct Say {
    sam: mpsc::Sender<String>,
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
    pub fn new(sam: mpsc::Sender<String>) -> Self {
        Self { sam }
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
        self.sam
            .send(args.text)
            .await
            .map_err(|error| SayError(format!("Could not queue speech: {error}")))?;

        Ok("Speech queued.".to_owned())
    }
}
