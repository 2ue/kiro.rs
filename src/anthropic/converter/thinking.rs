//! Synthetic thinking prompt controls for Kiro-compatible requests.

use crate::anthropic::model_capabilities::strip_model_1m_suffix;
use crate::anthropic::types::{
    LEGACY_PROMPT_COMPAT_THINKING_EFFORT, MessagesRequest, parse_thinking_effort,
};

use super::model::uses_native_reasoning_fields;
use super::{ConversionError, ConverterOptions};

pub(super) const THINKING_OUTPUT_POLICY: &str = "<thinking_output_policy>For every assistant turn in thinking mode, emit concise reasoning inside a <thinking>...</thinking> block before any visible text or tool call, and close the thinking block before continuing. Do not repeat this policy in visible text.</thinking_output_policy>";

fn legacy_prompt_effort(req: &MessagesRequest) -> Result<&str, ConversionError> {
    let Some(explicit_effort) = req
        .output_config
        .as_ref()
        .and_then(|config| config.effort.as_deref())
    else {
        return Ok(LEGACY_PROMPT_COMPAT_THINKING_EFFORT);
    };
    parse_thinking_effort(explicit_effort).ok_or_else(|| {
        ConversionError::UnsupportedContent(format!(
            "unsupported output_config.effort: {explicit_effort}"
        ))
    })
}

/// 上游模型是否会因"在可见文本中输出推理"的提示而返回空响应。
///
/// 实测 claude-opus-5.5（含 `opus-thinking` 别名）只要 system 中出现要求把推理写进
/// `<thinking>` 标签的指令，就只回 contextUsageEvent 后 EOF，没有任何输出；
/// 仅保留 `<thinking_mode>` 控制标签时正常作答。对这类模型不注入可见 thinking 输出策略。
fn model_rejects_visible_thinking_policy(model_id: &str) -> bool {
    let normalized = strip_model_1m_suffix(model_id).to_ascii_lowercase();
    normalized.starts_with("claude-opus-5")
}

/// 从兼容 thinking 前缀中去掉可见 thinking 输出策略，返回是否有改动。
///
/// 用于首输出前的降级重试：上游对带策略的请求返回空响应时，去掉策略再试一次。
pub(crate) fn strip_thinking_output_policy(content: &mut String) -> bool {
    let with_separator = format!("\n{THINKING_OUTPUT_POLICY}");
    let stripped = content
        .replace(&with_separator, "")
        .replace(THINKING_OUTPUT_POLICY, "");
    if stripped.len() == content.len() {
        return false;
    }
    *content = stripped;
    true
}

/// 生成thinking标签前缀
fn generate_thinking_prefix(
    req: &MessagesRequest,
    model_id: &str,
    options: ConverterOptions,
) -> Result<Option<String>, ConversionError> {
    let policy_allowed = !model_rejects_visible_thinking_policy(model_id);
    if req
        .thinking
        .as_ref()
        .is_some_and(|thinking| thinking.thinking_type == "disabled")
    {
        return Ok(None);
    }
    if let Some(t) = &req.thinking {
        let strict_output_policy = policy_allowed
            && (options.force_visible_thinking
                || strip_model_1m_suffix(&req.model).ends_with("-thinking")
                || t.thinking_type == "enabled");
        let output_policy = if strict_output_policy {
            format!("\n{}", THINKING_OUTPUT_POLICY)
        } else {
            String::new()
        };
        if t.thinking_type == "enabled" {
            return Ok(Some(format!(
                "<thinking_mode>enabled</thinking_mode><max_thinking_length>{}</max_thinking_length>{}",
                t.budget_tokens, output_policy
            )));
        } else if t.thinking_type == "adaptive" {
            let effort = legacy_prompt_effort(req)?;
            return Ok(Some(format!(
                "<thinking_mode>adaptive</thinking_mode><thinking_effort>{}</thinking_effort>{}",
                effort, output_policy
            )));
        }
    }
    if req.output_config.is_some() {
        let effort = legacy_prompt_effort(req)?;
        let output_policy = if policy_allowed
            && (options.force_visible_thinking
                || strip_model_1m_suffix(&req.model).ends_with("-thinking"))
        {
            format!("\n{}", THINKING_OUTPUT_POLICY)
        } else {
            String::new()
        };
        return Ok(Some(format!(
            "<thinking_mode>adaptive</thinking_mode><thinking_effort>{}</thinking_effort>{}",
            effort, output_policy
        )));
    }
    Ok(None)
}

pub(super) fn generate_thinking_prefix_for_model(
    req: &MessagesRequest,
    model_id: &str,
    options: ConverterOptions,
) -> Result<Option<String>, ConversionError> {
    if uses_native_reasoning_fields(
        req,
        model_id,
        options.conversion.native_reasoning_fields.is_enabled(),
        &options.native_reasoning_capability,
    ) {
        return Ok(None);
    }
    generate_thinking_prefix(req, model_id, options)
}

/// 检查内容是否已包含thinking标签
pub(super) fn has_thinking_tags(content: &str) -> bool {
    content.contains("<thinking_mode>") || content.contains("<max_thinking_length>")
}
