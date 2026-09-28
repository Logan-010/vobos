use base64::{Engine, engine::general_purpose::STANDARD};
use rig::{
    message::{ImageMediaType, ToolResultContent},
    tool::{Tool, ToolContext},
};
use serde::Deserialize;
use std::{error::Error, fmt, io::Cursor};
use xcap::{
    Monitor,
    image::{ImageFormat, RgbaImage},
};

pub struct Screencap;

#[derive(Debug, Deserialize)]
pub struct ScreencapArgs {
    pub monitor: Option<usize>,
}

#[derive(Debug)]
pub struct ScreencapError(String);

impl fmt::Display for ScreencapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ScreencapError {}

impl Tool for Screencap {
    const NAME: &'static str = "screencap";
    type Args = ScreencapArgs;
    type Output = Vec<ToolResultContent>;
    type Error = ScreencapError;

    fn description(&self) -> String {
        "Capture a monitor and return its screenshot as visual input so you can inspect the screen. Defaults to the primary monitor, or the first monitor if no primary is reported. Requires a vision-capable model.".to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "monitor": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional zero-based monitor index; omit to capture the primary monitor"
                }
            },
            "required": []
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        // xcap and PNG encoding are synchronous. Keep display connections and
        // capture work on the blocking worker, away from the agent/UI executor.
        tokio::task::spawn_blocking(move || capture(args.monitor))
            .await
            .map_err(|error| ScreencapError(format!("Screen capture task failed: {error}")))?
    }
}

fn capture(requested: Option<usize>) -> Result<Vec<ToolResultContent>, ScreencapError> {
    let monitors = Monitor::all()
        .map_err(|error| ScreencapError(format!("Could not enumerate monitors: {error}")))?;
    let primary = monitors
        .iter()
        .position(|monitor| monitor.is_primary().unwrap_or(false));
    let index = select_monitor(requested, primary, monitors.len())?;
    let monitor = &monitors[index];
    let name = monitor.name().unwrap_or_else(|_| "Unknown monitor".into());
    let image = monitor.capture_image().map_err(|error| {
        ScreencapError(format!(
            "Could not capture monitor {index} ({name}): {error}"
        ))
    })?;
    encode_capture(image, index, &name, monitors.len())
}

fn select_monitor(
    requested: Option<usize>,
    primary: Option<usize>,
    count: usize,
) -> Result<usize, ScreencapError> {
    if count == 0 {
        return Err(ScreencapError(
            "No monitors are available for screen capture".into(),
        ));
    }
    let index = requested.or(primary).unwrap_or(0);
    if index >= count {
        return Err(ScreencapError(format!(
            "Monitor index {index} is out of range; available indices are 0 through {}",
            count - 1
        )));
    }
    Ok(index)
}

fn encode_capture(
    image: RgbaImage,
    index: usize,
    name: &str,
    count: usize,
) -> Result<Vec<ToolResultContent>, ScreencapError> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err(ScreencapError(
            "Screen capture returned an empty image".into(),
        ));
    }
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|error| ScreencapError(format!("Could not encode screenshot: {error}")))?;
    Ok(vec![
        ToolResultContent::text(format!(
            "Screenshot of monitor {index} ({name}), {width}x{height} pixels. {count} monitor(s) available; indices 0 through {}.",
            count - 1
        )),
        ToolResultContent::image_base64(
            STANDARD.encode(png.into_inner()),
            Some(ImageMediaType::PNG),
            None,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::message::DocumentSourceKind;
    use xcap::image::Rgba;

    #[test]
    fn selects_primary_or_first_and_validates_explicit_index() {
        assert_eq!(select_monitor(None, Some(1), 3).unwrap(), 1);
        assert_eq!(select_monitor(None, None, 3).unwrap(), 0);
        assert_eq!(select_monitor(Some(2), Some(1), 3).unwrap(), 2);
        assert!(select_monitor(Some(3), Some(1), 3).is_err());
        assert!(select_monitor(None, None, 0).is_err());
        let args: ScreencapArgs = serde_json::from_str("{}").unwrap();
        assert_eq!(args.monitor, None);
        assert!(serde_json::from_str::<ScreencapArgs>(r#"{"monitor":-1}"#).is_err());
    }

    #[test]
    fn screenshot_is_lossless_png_visual_content() {
        let image = RgbaImage::from_pixel(3, 2, Rgba([12, 34, 56, 255]));
        let output = encode_capture(image.clone(), 1, "Test display", 2).unwrap();
        let ToolResultContent::Text(text) = &output[0] else {
            panic!()
        };
        assert!(text.text.contains("monitor 1 (Test display), 3x2"));
        let ToolResultContent::Image(result) = &output[1] else {
            panic!()
        };
        assert_eq!(result.media_type, Some(ImageMediaType::PNG));
        let DocumentSourceKind::Base64(data) = &result.data else {
            panic!()
        };
        let bytes = STANDARD.decode(data).unwrap();
        let decoded = xcap::image::load_from_memory_with_format(&bytes, ImageFormat::Png).unwrap();
        assert_eq!(decoded.to_rgba8(), image);
        assert!(encode_capture(RgbaImage::new(0, 0), 0, "Empty", 1).is_err());
    }
}
