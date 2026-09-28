use rig::{
    completion::{
        CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
        ProviderCapabilities,
    },
    message::{Message, ToolResultContent, UserContent},
    streaming::StreamingCompletionResponse,
};

/// Ollama accepts user images, but Rig's Ollama adapter rejects image tool
/// results. Adapt every request, including subsequent turns and saved history,
/// while leaving the canonical conversation and tool-call IDs intact.
#[derive(Clone)]
pub struct VisionModel<M>(pub M);

impl<M: CompletionModel> CompletionModel for VisionModel<M> {
    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        self.0.completion(with_user_images(request)).await
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        self.0.stream(with_user_images(request)).await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.0.capabilities()
    }
}

fn with_user_images(mut request: CompletionRequest) -> CompletionRequest {
    request.chat_history = image_history(request.chat_history);
    request
}

fn image_history(messages: Vec<Message>) -> Vec<Message> {
    let mut history = Vec::with_capacity(messages.len());
    for mut message in messages {
        let mut images = Vec::new();
        if let Message::User { content } = &mut message {
            for item in content {
                if let UserContent::ToolResult(result) = item {
                    for block in &mut result.content {
                        if let ToolResultContent::Image(image) = block {
                            images.push(UserContent::text(format!(
                                "Image returned by {} (tool call {}):",
                                result.name, result.call
                            )));
                            images.push(UserContent::Image(image.clone()));
                            *block = ToolResultContent::text(
                                "Image attached in the following user message.",
                            );
                        }
                    }
                }
            }
        }
        history.push(message);
        if !images.is_empty() {
            history.push(Message::User { content: images });
        }
    }
    history
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::message::{ImageMediaType, ToolCallId, ToolResult};

    #[test]
    fn image_results_become_ollama_user_images_and_keep_tool_identity() {
        let image = ToolResultContent::image_base64("aW1hZ2U=", Some(ImageMediaType::PNG), None);
        let result = ToolResult {
            call: ToolCallId::new_or_mint("image-call"),
            provider: None,
            name: "image".into(),
            content: vec![ToolResultContent::text("Image from test.png"), image],
        };
        let original = vec![Message::User {
            content: vec![UserContent::ToolResult(result.clone())],
        }];
        let history = image_history(original.clone());
        assert_eq!(history.len(), 2);
        let Message::User { content } = &history[0] else {
            panic!()
        };
        let UserContent::ToolResult(adapted) = &content[0] else {
            panic!()
        };
        assert_eq!(adapted.call, result.call);
        assert_eq!(adapted.name, result.name);
        assert_eq!(adapted.content[0], result.content[0]);
        assert!(
            adapted
                .content
                .iter()
                .all(|block| !matches!(block, ToolResultContent::Image(_)))
        );
        let wire: Vec<rig::providers::ollama::Message> = history[1].clone().try_into().unwrap();
        let wire = serde_json::to_value(wire).unwrap();
        assert_eq!(wire[0]["role"], "user");
        assert_eq!(wire[0]["images"][0], "aW1hZ2U=");
        let _: Vec<rig::providers::ollama::Message> = history[0].clone().try_into().unwrap();
        // Replaying saved history is deterministic, and already adapted input
        // does not gain duplicate attachments.
        assert_eq!(image_history(original), history);
        assert_eq!(image_history(history.clone()), history);
    }

    #[test]
    fn text_and_direct_image_messages_are_unchanged() {
        let messages = vec![
            Message::user("Describe this"),
            Message::User {
                content: vec![UserContent::image_base64(
                    "aW1hZ2U=",
                    Some(ImageMediaType::PNG),
                    None,
                )],
            },
        ];
        assert_eq!(image_history(messages.clone()), messages);
    }
}
