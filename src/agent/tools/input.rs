use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use std::{
    error::Error,
    fmt::{Display, Formatter},
    sync::{Arc, Mutex},
};

pub struct Input {
    enigo: Arc<Mutex<Enigo>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct InputArgs {
    pub keystrokes: Vec<String>,
    pub text: Option<String>,
    pub mouse_move: Option<MouseMove>,
    pub click: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MouseMove {
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub relative: bool,
}

#[derive(Debug)]
pub struct InputError(String);

impl Display for InputError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for InputError {}

impl Input {
    pub fn new() -> color_eyre::Result<Self> {
        Ok(Self {
            enigo: Arc::new(Mutex::new(Enigo::new(&Settings::default())?)),
        })
    }
}

impl Tool for Input {
    const NAME: &'static str = "input";
    type Args = InputArgs;
    type Output = String;
    type Error = InputError;

    fn description(&self) -> String {
        "Emit keyboard and mouse events to the system.".to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "keystrokes": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Keys or key combinations. Each list entry is one action; use Ctrl+C (or CtrlC) for a combination."
                },
                "text": {
                    "type": "string",
                    "description": "Text to type. Use keystrokes for keyboard shortcuts."
                },
                "mouse_move": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "integer" },
                        "y": { "type": "integer" },
                        "relative": { "type": "boolean", "description": "Move relative to the current cursor position instead of absolute screen coordinates." }
                    },
                    "required": ["x", "y"]
                },
                "click": {
                    "type": "string",
                    "enum": ["left", "middle", "right"]
                }
            },
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let enigo = Arc::clone(&self.enigo);
        tokio::task::spawn_blocking(move || send_input(&enigo, args))
            .await
            .map_err(|error| InputError(format!("Input task failed: {error}")))??;

        Ok("Input sent".to_owned())
    }
}

fn send_input(enigo: &Arc<Mutex<Enigo>>, args: InputArgs) -> Result<(), InputError> {
    let mut enigo = enigo
        .lock()
        .map_err(|_| InputError("Input device is unavailable".to_owned()))?;

    if let Some(text) = args.text {
        tracing::info!("Typing \"{}\"", text);

        enigo
            .text(&text)
            .map_err(|error| InputError(format!("Could not type text: {error}")))?;
    }

    for keystroke in args.keystrokes {
        let keys = parse_keystroke(&keystroke)?;

        tracing::info!("Executing keystroke \"{:?}\"", keys);

        let modifiers = keys.iter().copied().filter(|key| is_modifier(*key));
        let regular_keys = keys.iter().copied().filter(|key| !is_modifier(*key));

        for key in modifiers {
            enigo
                .key(key, Direction::Press)
                .map_err(|error| InputError(format!("Could not press key: {error}")))?;
        }

        // Click non-modifiers instead of leaving them held between separate
        // press and release events. This avoids key-repeat on Linux backends.
        for key in regular_keys {
            enigo
                .key(key, Direction::Click)
                .map_err(|error| InputError(format!("Could not click key: {error}")))?;
        }

        for &key in keys.iter().rev().filter(|key| is_modifier(**key)) {
            enigo
                .key(key, Direction::Release)
                .map_err(|error| InputError(format!("Could not release key: {error}")))?;
        }
    }

    if let Some(mouse_move) = args.mouse_move {
        tracing::info!("Moving mouse: {:?}", mouse_move);

        let coordinate = if mouse_move.relative {
            Coordinate::Rel
        } else {
            Coordinate::Abs
        };
        enigo
            .move_mouse(mouse_move.x, mouse_move.y, coordinate)
            .map_err(|error| InputError(format!("Could not move mouse: {error}")))?;
    }

    if let Some(click) = args.click {
        tracing::info!("Clicking {}", click);

        let button = match click.to_ascii_lowercase().as_str() {
            "left" => Button::Left,
            "middle" => Button::Middle,
            "right" => Button::Right,
            _ => return Err(InputError(format!("Unknown mouse button: {click}"))),
        };
        enigo
            .button(button, Direction::Click)
            .map_err(|error| InputError(format!("Could not click mouse: {error}")))?;
    }

    Ok(())
}

fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::Control | Key::Shift | Key::Alt | Key::Meta | Key::RControl | Key::RShift
    )
}

fn parse_keystroke(value: &str) -> Result<Vec<Key>, InputError> {
    let parts: Vec<&str> = if value.contains('+') {
        value.split('+').filter(|part| !part.is_empty()).collect()
    } else if let Some(parts) = split_camel_case_key_combo(value) {
        parts
    } else {
        vec![value]
    };

    if parts.is_empty() {
        return Err(InputError("Keystroke cannot be empty".to_owned()));
    }

    parts.iter().map(|part| parse_key(part)).collect()
}

fn split_camel_case_key_combo(value: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        let Some(modifier) = ["Control", "Ctrl", "Shift", "Alt", "Meta", "Super"]
            .iter()
            .find(|modifier| rest.starts_with(**modifier))
        else {
            break;
        };
        parts.push(&rest[..modifier.len()]);
        rest = &rest[modifier.len()..];
    }

    if parts.is_empty() || rest.is_empty() {
        None
    } else {
        parts.push(rest);
        Some(parts)
    }
}

fn parse_key(value: &str) -> Result<Key, InputError> {
    let normalized = value.to_ascii_lowercase();
    let key = match normalized.as_str() {
        "ctrl" | "control" => Key::Control,
        "shift" => Key::Shift,
        "alt" | "option" => Key::Alt,
        "meta" | "super" | "command" | "cmd" | "win" | "windows" => Key::Meta,
        "enter" | "return" => Key::Return,
        "esc" | "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "up" | "arrowup" => Key::UpArrow,
        "down" | "arrowdown" => Key::DownArrow,
        "left" | "arrowleft" => Key::LeftArrow,
        "right" | "arrowright" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        _ => {
            let mut chars = value.chars();
            let Some(character) = chars.next() else {
                return Err(InputError("Keystroke cannot be empty".to_owned()));
            };
            if chars.next().is_some() {
                return Err(InputError(format!("Unknown key: {value}")));
            }
            Key::Unicode(character)
        }
    };
    Ok(key)
}
