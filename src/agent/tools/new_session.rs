use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    error::Error,
    fmt::{Display, Formatter},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct NewSession {
    session_id: Arc<Mutex<String>>,
}

#[derive(Debug, Deserialize)]
pub struct NewSessionArgs {}

#[derive(Debug)]
pub struct NewSessionError(String);

impl Display for NewSessionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for NewSessionError {}

impl NewSession {
    pub fn new(session_id: Arc<Mutex<String>>) -> Self {
        Self { session_id }
    }
}

impl Tool for NewSession {
    const NAME: &'static str = "new_session";
    type Args = NewSessionArgs;
    type Output = String;
    type Error = NewSessionError;

    fn description(&self) -> String {
        "Start a new conversation and forget the current conversation history.".to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| NewSessionError(format!("Could not create session ID: {error}")))?
            .as_nanos();
        let counter = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("session-{timestamp}-{counter}");

        *self
            .session_id
            .lock()
            .map_err(|_| NewSessionError("Session state is unavailable".to_owned()))? = id.clone();

        Ok(format!("Started a new session: {id}"))
    }
}
