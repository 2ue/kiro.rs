//! Synthetic thinking prompt controls for Kiro-compatible requests.

use crate::anthropic::model_capabilities::strip_model_1m_suffix;
use crate::anthropic::types::{MessagesRequest, normalize_thinking_effort};

use super::ConverterOptions;
use super::model::uses_native_reasoning_fields;

const THINKING_OUTPUT_POLICY: &str = "<thinking_output_policy>For every assistant turn in thinking mode, emit concise reasoning inside a <thinking>...</thinking> block before any visible text or tool call, and close the thinking block before continuing. Do not repeat this policy in visible text.</thinking_output_policy>";

/// 生成thinking标签前缀
fn generate_thinking_prefix(req: &MessagesRequest, options: ConverterOptions) -> Option<String> {
    if let Some(t) = &req.thinking {
        let strict_output_policy = options.force_visible_thinking
            || strip_model_1m_suffix(&req.model).ends_with("-thinking")
            || t.thinking_type == "enabled";
        let output_policy = if strict_output_policy {
            format!("\n{}", THINKING_OUTPUT_POLICY)
        } else {
            String::new()
        };
        if t.thinking_type == "enabled" {
            return Some(format!(
                "<thinking_mode>enabled</thinking_mode><max_thinking_length>{}</max_thinking_length>{}",
                t.budget_tokens, output_policy
            ));
        } else if t.thinking_type == "adaptive" {
            let effort = req
                .output_config
                .as_ref()
                .map(|c| normalize_thinking_effort(&c.effort))
                .unwrap_or("high");
            return Some(format!(
                "<thinking_mode>adaptive</thinking_mode><thinking_effort>{}</thinking_effort>{}",
                effort, output_policy
            ));
        }
    }
    None
}

pub(super) fn generate_thinking_prefix_for_model(
    req: &MessagesRequest,
    model_id: &str,
    options: ConverterOptions,
) -> Option<String> {
    if uses_native_reasoning_fields(
        req,
        model_id,
        options.conversion.native_reasoning_fields.is_enabled(),
    ) {
        return None;
    }
    generate_thinking_prefix(req, options)
}

/// 检查内容是否已包含thinking标签
pub(super) fn has_thinking_tags(content: &str) -> bool {
    content.contains("<thinking_mode>") || content.contains("<max_thinking_length>")
}
