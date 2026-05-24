//! Additional Kiro event payloads.
//!
//! These events are emitted by newer Kiro runtimes but were not part of the
//! original minimal parser. Keeping them typed lets the Anthropic adapter use
//! authoritative token usage, native thinking, and upstream invalid-state
//! failures instead of treating them as opaque unknown frames.

use serde::{Deserialize, Serialize};

use crate::kiro::parser::error::ParseResult;
use crate::kiro::parser::frame::Frame;

use super::base::EventPayload;

/// Native reasoning/thinking event.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningContentEvent {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_content: Option<String>,
}

impl EventPayload for ReasoningContentEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

/// Token usage details reported by `metadataEvent`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetadataTokenUsage {
    #[serde(default)]
    pub uncached_input_tokens: i32,
    #[serde(default)]
    pub output_tokens: i32,
    #[serde(default)]
    pub total_tokens: i32,
    #[serde(default)]
    pub cache_read_input_tokens: i32,
    #[serde(default)]
    pub cache_write_input_tokens: i32,
}

impl MetadataTokenUsage {
    pub fn input_tokens(&self) -> i32 {
        self.uncached_input_tokens
    }

    pub fn total_input_tokens(&self) -> i32 {
        self.uncached_input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_write_input_tokens)
    }
}

/// Metadata event containing authoritative token usage.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetadataEvent {
    #[serde(default)]
    pub token_usage: Option<MetadataTokenUsage>,
}

impl EventPayload for MetadataEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

/// Conversation metadata event.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MessageMetadataEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utterance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<MetadataTokenUsage>,
}

impl EventPayload for MessageMetadataEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

/// Metering event emitted by Kiro with credit usage information.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MeteringEvent {
    #[serde(default)]
    pub usage: f64,
}

impl EventPayload for MeteringEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

/// Code content event emitted by Amazon Q CLI style streams.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodeEvent {
    #[serde(default)]
    pub content: String,
}

impl EventPayload for CodeEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

/// Invalid state event returned inside an otherwise successful event stream.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InvalidStateEvent {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub message: String,
}

impl InvalidStateEvent {
    pub fn error_text(&self) -> String {
        if self.message.is_empty() {
            self.reason.clone()
        } else {
            self.message.clone()
        }
    }
}

impl EventPayload for InvalidStateEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_usage_deserializes_camel_case_and_computes_input_tokens() {
        let event: MetadataEvent = serde_json::from_str(
            r#"{
                "tokenUsage": {
                    "uncachedInputTokens": 120,
                    "cacheReadInputTokens": 30,
                    "cacheWriteInputTokens": 7,
                    "outputTokens": 11,
                    "totalTokens": 168
                }
            }"#,
        )
        .unwrap();

        let usage = event.token_usage.unwrap();
        assert_eq!(usage.input_tokens(), 120);
        assert_eq!(usage.total_input_tokens(), 157);
        assert_eq!(usage.output_tokens, 11);
        assert_eq!(usage.cache_write_input_tokens, 7);
    }

    #[test]
    fn high_cache_metadata_usage_keeps_large_cache_token_counts() {
        let event: MetadataEvent = serde_json::from_str(
            r#"{
                "tokenUsage": {
                    "uncachedInputTokens": 1200,
                    "cacheReadInputTokens": 180000,
                    "cacheWriteInputTokens": 24000,
                    "outputTokens": 900,
                    "totalTokens": 206100
                }
            }"#,
        )
        .unwrap();

        let usage = event.token_usage.unwrap();
        assert_eq!(usage.input_tokens(), 1200);
        assert_eq!(usage.total_input_tokens(), 205200);
        assert_eq!(usage.cache_read_input_tokens, 180000);
        assert_eq!(usage.cache_write_input_tokens, 24000);
        assert_eq!(usage.output_tokens, 900);
    }

    #[test]
    fn message_metadata_usage_deserializes_token_usage() {
        let event: MessageMetadataEvent = serde_json::from_str(
            r#"{
                "conversationId": "conv-1",
                "utteranceId": "utt-1",
                "tokenUsage": {
                    "uncachedInputTokens": 12,
                    "cacheReadInputTokens": 345,
                    "cacheWriteInputTokens": 67,
                    "outputTokens": 8,
                    "totalTokens": 432
                }
            }"#,
        )
        .unwrap();

        assert_eq!(event.conversation_id.as_deref(), Some("conv-1"));
        assert_eq!(event.utterance_id.as_deref(), Some("utt-1"));
        let usage = event.token_usage.unwrap();
        assert_eq!(usage.input_tokens(), 12);
        assert_eq!(usage.cache_read_input_tokens, 345);
        assert_eq!(usage.cache_write_input_tokens, 67);
        assert_eq!(usage.output_tokens, 8);
    }

    #[test]
    fn metering_and_code_events_deserialize_without_extra_requirements() {
        let metering: MeteringEvent = serde_json::from_str(r#"{"usage":1.25}"#).unwrap();
        assert_eq!(metering.usage, 1.25);

        let code: CodeEvent = serde_json::from_str(r#"{"content":"println!(\"hi\");"}"#).unwrap();
        assert_eq!(code.content, "println!(\"hi\");");
    }

    #[test]
    fn invalid_state_prefers_message_but_falls_back_to_reason() {
        let with_message: InvalidStateEvent =
            serde_json::from_str(r#"{"reason":"Expired","message":"session expired"}"#).unwrap();
        assert_eq!(with_message.error_text(), "session expired");

        let without_message: InvalidStateEvent =
            serde_json::from_str(r#"{"reason":"Expired"}"#).unwrap();
        assert_eq!(without_message.error_text(), "Expired");
    }

    #[test]
    fn reasoning_content_supports_signature_and_redacted_content() {
        let event: ReasoningContentEvent =
            serde_json::from_str(r#"{"text":"thinking","signature":"sig"}"#).unwrap();
        assert_eq!(event.text, "thinking");
        assert_eq!(event.signature.as_deref(), Some("sig"));

        let redacted: ReasoningContentEvent =
            serde_json::from_str(r#"{"redactedContent":"opaque"}"#).unwrap();
        assert_eq!(redacted.redacted_content.as_deref(), Some("opaque"));
        assert!(redacted.text.is_empty());
    }
}
