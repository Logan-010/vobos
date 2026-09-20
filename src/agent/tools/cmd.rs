use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use std::{
    error::Error,
    fmt::{Display, Formatter},
    process::Command,
};

pub struct Cmd;

#[derive(Debug, Deserialize)]
pub struct CmdArgs {
    pub command: String,
}

#[derive(Debug)]
pub struct CmdError(String);

impl Display for CmdError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CmdError {}

impl Tool for Cmd {
    const NAME: &'static str = "cmd";
    type Args = CmdArgs;
    type Output = String;
    type Error = CmdError;

    fn description(&self) -> String {
        "Run a shell command and return its output.".to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to run"
                }
            },
            "required": ["command"]
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let command = args.command;
        tokio::task::spawn_blocking(move || run_command(&command))
            .await
            .map_err(|error| CmdError(format!("Command task failed: {error}")))?
    }
}

fn run_command(command: &str) -> Result<String, CmdError> {
    tracing::info!("vOBOS is running the command \"{}\"", command);

    #[cfg(target_os = "windows")]
    let output = Command::new("cmd").args(["/C", command]).output();

    #[cfg(not(target_os = "windows"))]
    let output = Command::new("sh").args(["-c", command]).output();

    let output = output.map_err(|error| CmdError(format!("Could not run command: {error}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(&stderr);
    }

    if !output.status.success() {
        return Err(CmdError(format!(
            "Command exited with {}: {}",
            output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            result.trim_end()
        )));
    }

    Ok(result)
}
