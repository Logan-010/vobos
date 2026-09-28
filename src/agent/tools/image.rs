use base64::{Engine, engine::general_purpose::STANDARD};
use rig::{
    message::{ImageMediaType, ToolResultContent},
    tool::{Tool, ToolContext},
};
use serde::Deserialize;
use std::{error::Error, fmt, path::PathBuf};
use tokio::io::AsyncReadExt;

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

pub struct Image;

#[derive(Debug, Deserialize)]
pub struct ImageArgs {
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct ImageError(String);

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ImageError {}

impl Tool for Image {
    const NAME: &'static str = "image";
    type Args = ImageArgs;
    type Output = Vec<ToolResultContent>;
    type Error = ImageError;

    fn description(&self) -> String {
        "Open a local image so you can inspect its visual contents. Supports PNG, JPEG, GIF, and WebP files up to 20 MiB. Requires a vision-capable model. Use cmd to download remote images first, or screencap to inspect the current screen.".to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the image file (absolute or relative to the working directory)"
                }
            },
            "required": ["path"]
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let file = tokio::fs::File::open(&args.path).await.map_err(|error| {
            ImageError(format!("Could not open {}: {error}", args.path.display()))
        })?;
        let metadata = file
            .metadata()
            .await
            .map_err(|error| ImageError(error.to_string()))?;
        if !metadata.is_file() {
            return Err(ImageError("Image path must be a regular file".into()));
        }
        if metadata.len() > MAX_IMAGE_BYTES {
            return Err(ImageError("Image exceeds the 20 MiB size limit".into()));
        }
        // Bound the read as well: the file may grow after the metadata check.
        let mut bytes = Vec::new();
        file.take(MAX_IMAGE_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| ImageError(format!("Could not read image: {error}")))?;
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err(ImageError("Image exceeds the 20 MiB size limit".into()));
        }
        let media_type = media_type(&bytes)?;
        Ok(vec![
            ToolResultContent::text(format!("Image from {}", args.path.display())),
            ToolResultContent::image_base64(STANDARD.encode(bytes), Some(media_type), None),
        ])
    }
}

fn media_type(bytes: &[u8]) -> Result<ImageMediaType, ImageError> {
    // Inspect the contents rather than trusting a file extension.
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok(ImageMediaType::PNG)
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Ok(ImageMediaType::JPEG)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Ok(ImageMediaType::GIF)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Ok(ImageMediaType::WEBP)
    } else {
        Err(ImageError(
            "Unsupported or empty image; expected PNG, JPEG, GIF, or WebP".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::message::DocumentSourceKind;

    #[tokio::test]
    async fn reads_image_contents_without_relying_on_extension() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("picture.data");
        // A one-pixel PNG.
        let encoded = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aD1sAAAAASUVORK5CYII=";
        let bytes = STANDARD.decode(encoded).unwrap();
        tokio::fs::write(&path, &bytes).await.unwrap();
        let output = Image
            .call(&mut ToolContext::new(), ImageArgs { path })
            .await
            .unwrap();
        let ToolResultContent::Image(image) = &output[1] else {
            panic!()
        };
        assert_eq!(image.media_type, Some(ImageMediaType::PNG));
        let DocumentSourceKind::Base64(data) = &image.data else {
            panic!()
        };
        assert_eq!(STANDARD.decode(data).unwrap(), bytes);
    }

    #[tokio::test]
    async fn rejects_missing_unsupported_and_oversized_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        assert!(
            Image
                .call(&mut ToolContext::new(), ImageArgs { path: path.clone() })
                .await
                .is_err()
        );
        tokio::fs::write(&path, b"not an image").await.unwrap();
        let error = Image
            .call(&mut ToolContext::new(), ImageArgs { path: path.clone() })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Unsupported"));
        let file = tokio::fs::File::create(&path).await.unwrap();
        file.set_len(MAX_IMAGE_BYTES + 1).await.unwrap();
        let error = Image
            .call(&mut ToolContext::new(), ImageArgs { path })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("size limit"));
    }

    #[test]
    fn detects_supported_formats_and_rejects_short_or_unrelated_data() {
        assert_eq!(
            media_type(b"\xff\xd8\xff\xe0").unwrap(),
            ImageMediaType::JPEG
        );
        assert_eq!(media_type(b"GIF87a").unwrap(), ImageMediaType::GIF);
        assert_eq!(media_type(b"GIF89a").unwrap(), ImageMediaType::GIF);
        assert_eq!(
            media_type(b"RIFF\x00\x00\x00\x00WEBP").unwrap(),
            ImageMediaType::WEBP
        );
        for bytes in [
            b"".as_slice(),
            b"RIFF",
            b"RIFF\x00\x00\x00\x00WAVE",
            b"<svg/>",
        ] {
            assert!(media_type(bytes).is_err());
        }
    }
}
