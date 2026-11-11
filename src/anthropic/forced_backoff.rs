//! 强制模型收敛的上下文退避。
//!
//! 客户端按外显模型的窗口（多为 1M）发送历史，目标模型（4.5 系）只有 200K。请求超出目标窗口时，
//! 在转换前按固定顺序裁剪，直到估算值落进预算：
//!
//! 1. 去掉最后一条 assistant 之前所有 assistant 的 thinking 块；
//! 2. 清理较早的工具结果：保留最近若干条原文，更早的换成和官方清理一致的占位文字；
//! 3. 从最早的轮次开始成对删除历史，保持 tool_use / tool_result 配对，首条用户文字保留；
//! 4. 仍超出时，对最大的单个文本或工具结果做中间截断。
//!
//! 每一步都只依赖请求内容和预算，同一请求得到同一结果；清理边界按步长推进，历史增长不到一个
//! 步长时前缀保持不变，缓存照常命中。

use serde_json::Value;

use crate::anthropic::types::{Message, MessagesRequest};
use crate::model::forced_model_mapping::ForcedContextBackoffConfig;

/// 官方清理旧工具结果后的占位文字。
pub(crate) const CLEARED_TOOL_RESULT: &str = "[Old tool result content cleared]";
const TRUNCATED_MARKER: &str = "\n\n[... content truncated ...]\n\n";

/// 一次退避的结果，用于日志和诊断。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct BackoffReport {
    pub budget_tokens: i32,
    pub original_tokens: i32,
    pub final_tokens: i32,
    pub removed_thinking_blocks: usize,
    pub cleared_tool_results: usize,
    pub removed_messages: usize,
    pub truncated_blocks: usize,
}

impl BackoffReport {
    pub fn changed(&self) -> bool {
        self.removed_thinking_blocks > 0
            || self.cleared_tool_results > 0
            || self.removed_messages > 0
            || self.truncated_blocks > 0
    }
}

/// 发给目标模型的输入预算：窗口减去预留输出，再留出安全余量。
pub(crate) fn input_budget(
    window_tokens: i32,
    max_tokens: i32,
    config: &ForcedContextBackoffConfig,
) -> i32 {
    let reserved_output = max_tokens
        .clamp(
            1,
            crate::anthropic::forced_model::FORCED_TARGET_MAX_OUTPUT_TOKENS,
        )
        .max(config.min_reserved_output_tokens);
    let available = (window_tokens - reserved_output).max(1);
    let margin = (available as f64 * config.safety_margin_ratio).ceil() as i32;
    (available - margin).max(1)
}

/// Local estimate only: a remote counter would make the trimming non-deterministic and block.
fn estimate(request: &MessagesRequest) -> i32 {
    crate::token::count_all_tokens_local(
        request.system.as_deref(),
        &request.messages,
        request.tools.as_deref(),
    )
    .min(i32::MAX as u64) as i32
}

/// 超出预算时按顺序裁剪，返回本次退避的报告；未超出时请求不变。
pub(crate) fn fit_to_budget(
    request: &mut MessagesRequest,
    budget_tokens: i32,
    config: &ForcedContextBackoffConfig,
) -> BackoffReport {
    let original = estimate(request);
    let mut report = BackoffReport {
        budget_tokens,
        original_tokens: original,
        final_tokens: original,
        ..BackoffReport::default()
    };
    if original <= budget_tokens {
        return report;
    }

    report.removed_thinking_blocks = drop_old_thinking(&mut request.messages);
    report.final_tokens = estimate(request);
    if report.final_tokens <= budget_tokens {
        return report;
    }

    // Clear old tool results in step-sized rounds so consecutive requests share one prefix
    // until the history grows by another step.
    let overflow = report.final_tokens - budget_tokens;
    let rounds = (overflow / config.step_tokens.max(1)) as usize + 1;
    report.cleared_tool_results = clear_old_tool_results(
        &mut request.messages,
        config.keep_recent_tool_results,
        rounds,
    );
    report.final_tokens = estimate(request);
    if report.final_tokens <= budget_tokens {
        return report;
    }
    // Still over: clear every tool result except the most recent ones.
    report.cleared_tool_results += clear_old_tool_results(
        &mut request.messages,
        config.keep_recent_tool_results,
        usize::MAX,
    );
    report.final_tokens = estimate(request);
    if report.final_tokens <= budget_tokens {
        return report;
    }

    while report.final_tokens > budget_tokens {
        let removed = drop_oldest_turn(&mut request.messages);
        if removed == 0 {
            break;
        }
        report.removed_messages += removed;
        report.final_tokens = estimate(request);
    }

    while report.final_tokens > budget_tokens {
        let excess = report.final_tokens - budget_tokens;
        if !truncate_largest_block(&mut request.messages, excess) {
            break;
        }
        report.truncated_blocks += 1;
        report.final_tokens = estimate(request);
    }
    report
}

/// 客户端的 `context_management.edits`：`clear_tool_uses_*` 与 `clear_thinking_*` 在本地执行，
/// 其它类型忽略。返回 (清理的工具结果数, 删除的 thinking 块数)。
pub(crate) fn apply_context_edits(request: &mut MessagesRequest) -> (usize, usize) {
    let Some(edits) = request
        .context_management
        .as_ref()
        .and_then(|value| value.get("edits"))
        .and_then(Value::as_array)
        .cloned()
    else {
        return (0, 0);
    };
    let mut cleared = 0;
    let mut removed_thinking = 0;
    for edit in edits {
        let kind = edit.get("type").and_then(Value::as_str).unwrap_or_default();
        if kind.starts_with("clear_tool_uses_") {
            // `keep: {type: "tool_uses", value: N}`; the API keeps the 3 most recent by default.
            let keep = edit
                .get("keep")
                .and_then(|keep| keep.get("value"))
                .and_then(Value::as_u64)
                .unwrap_or(3) as usize;
            cleared += clear_old_tool_results(&mut request.messages, keep, usize::MAX);
        } else if kind.starts_with("clear_thinking_") {
            removed_thinking += drop_old_thinking(&mut request.messages);
        }
    }
    (cleared, removed_thinking)
}

fn blocks_mut(message: &mut Message) -> Option<&mut Vec<Value>> {
    message.content.as_array_mut()
}

fn block_type(block: &Value) -> Option<&str> {
    block.get("type").and_then(Value::as_str)
}

/// 去掉最后一条 assistant 之前的 thinking / redacted_thinking 块；去空的 assistant 补一个空文本。
fn drop_old_thinking(messages: &mut [Message]) -> usize {
    let Some(last_assistant) = messages.iter().rposition(|m| m.role == "assistant") else {
        return 0;
    };
    let mut removed = 0;
    for message in messages[..last_assistant]
        .iter_mut()
        .filter(|m| m.role == "assistant")
    {
        let Some(blocks) = blocks_mut(message) else {
            continue;
        };
        let before = blocks.len();
        blocks.retain(|block| {
            !matches!(
                block_type(block),
                Some("thinking") | Some("redacted_thinking")
            )
        });
        removed += before - blocks.len();
        if blocks.is_empty() {
            blocks.push(serde_json::json!({"type": "text", "text": " "}));
        }
    }
    removed
}

/// 工具结果的位置（消息下标，块下标），按出现顺序。
fn tool_result_positions(messages: &[Message]) -> Vec<(usize, usize)> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .flat_map(|(mi, m)| {
            m.content
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .enumerate()
                        .filter(|(_, b)| block_type(b) == Some("tool_result"))
                        .map(|(bi, _)| (mi, bi))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect()
}

fn is_cleared(block: &Value) -> bool {
    block.get("content").and_then(Value::as_str) == Some(CLEARED_TOOL_RESULT)
}

/// 清理较早的工具结果：最近 `keep_recent` 条保留，更早的按每轮 `keep_recent` 条从最早开始清理，
/// 共 `rounds` 轮。返回本次清理的条数。
fn clear_old_tool_results(messages: &mut [Message], keep_recent: usize, rounds: usize) -> usize {
    let positions = tool_result_positions(messages);
    let clearable = positions.len().saturating_sub(keep_recent.max(1));
    let limit = rounds.saturating_mul(keep_recent.max(1)).min(clearable);
    let mut cleared = 0;
    for &(mi, bi) in positions.iter().take(limit) {
        let Some(block) = blocks_mut(&mut messages[mi]).and_then(|blocks| blocks.get_mut(bi))
        else {
            continue;
        };
        if is_cleared(block) {
            continue;
        }
        block["content"] = Value::String(CLEARED_TOOL_RESULT.to_string());
        cleared += 1;
    }
    cleared
}

fn message_tool_use_ids(message: &Message) -> Vec<String> {
    message
        .content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| matches!(block_type(b), Some("tool_use") | Some("server_tool_use")))
                .filter_map(|b| b.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn has_user_text(message: &Message) -> bool {
    match &message.content {
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(blocks) => blocks.iter().any(|b| {
            block_type(b) == Some("text")
                && b.get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|t| !t.trim().is_empty())
        }),
        _ => false,
    }
}

/// 删除最早的一轮（首条用户消息之后的第一组 assistant + 回应它的 user），保持配对；
/// 最后一条消息（当前轮）不动。返回删除的消息数。
fn drop_oldest_turn(messages: &mut Vec<Message>) -> usize {
    // Keep the first user message (the original request) and the current turn.
    let keep_tail = 2;
    if messages.len() <= 1 + keep_tail {
        return 0;
    }
    let start = if messages.first().is_some_and(|m| m.role == "user") {
        1
    } else {
        0
    };
    // Find the first assistant at or after `start`.
    let Some(offset) = messages[start..messages.len() - keep_tail]
        .iter()
        .position(|m| m.role == "assistant")
    else {
        return 0;
    };
    let first = start + offset;
    let mut end = first + 1;
    // Remove the following user turn too when it answers this assistant's tool calls, so no
    // tool_result is left without its tool_use.
    let ids = message_tool_use_ids(&messages[first]);
    if end < messages.len() - keep_tail && messages[end].role == "user" {
        let answers_tools = !ids.is_empty()
            || messages[end]
                .content
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|b| block_type(b) == Some("tool_result")));
        if answers_tools || !has_user_text(&messages[end]) {
            end += 1;
        }
    }
    if end > messages.len() - keep_tail {
        return 0;
    }
    messages.drain(first..end);
    // A dangling leading assistant (no user before it) is not allowed; the first user stays.
    end - first
}

fn text_len_tokens(text: &str) -> i32 {
    crate::token::count_tokens(text).min(i32::MAX as u64) as i32
}

/// 最大单块（文本或工具结果）做中间截断，至少去掉 `excess` 个 token。找不到可截断的块时返回 false。
fn truncate_largest_block(messages: &mut [Message], excess: i32) -> bool {
    let mut best: Option<(usize, usize, Option<usize>, i32)> = None;
    for (mi, message) in messages.iter().enumerate() {
        let Some(blocks) = message.content.as_array() else {
            if let Value::String(text) = &message.content {
                let tokens = text_len_tokens(text);
                if best.is_none_or(|(_, _, _, t)| tokens > t) {
                    best = Some((mi, usize::MAX, None, tokens));
                }
            }
            continue;
        };
        for (bi, block) in blocks.iter().enumerate() {
            match block_type(block) {
                Some("text") => {
                    let tokens = block
                        .get("text")
                        .and_then(Value::as_str)
                        .map_or(0, text_len_tokens);
                    if best.is_none_or(|(_, _, _, t)| tokens > t) {
                        best = Some((mi, bi, None, tokens));
                    }
                }
                Some("tool_result") => match block.get("content") {
                    Some(Value::String(text)) => {
                        let tokens = text_len_tokens(text);
                        if best.is_none_or(|(_, _, _, t)| tokens > t) {
                            best = Some((mi, bi, None, tokens));
                        }
                    }
                    Some(Value::Array(items)) => {
                        for (ii, item) in items.iter().enumerate() {
                            if block_type(item) != Some("text") {
                                continue;
                            }
                            let tokens = item
                                .get("text")
                                .and_then(Value::as_str)
                                .map_or(0, text_len_tokens);
                            if best.is_none_or(|(_, _, _, t)| tokens > t) {
                                best = Some((mi, bi, Some(ii), tokens));
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
    let Some((mi, bi, item, tokens)) = best else {
        return false;
    };
    if tokens < 64 {
        return false;
    }
    let keep_tokens = (tokens - excess - 32).max(tokens / 8).max(16);
    let message = &mut messages[mi];
    let slot: &mut Value = if bi == usize::MAX {
        &mut message.content
    } else {
        let block = &mut message.content.as_array_mut().expect("array")[bi];
        match item {
            Some(ii) => &mut block["content"][ii]["text"],
            None if block_type(block) == Some("tool_result") => &mut block["content"],
            None => &mut block["text"],
        }
    };
    let Some(text) = slot.as_str() else {
        return false;
    };
    let truncated = truncate_middle(text, keep_tokens, tokens);
    if truncated.len() >= text.len() {
        return false;
    }
    *slot = Value::String(truncated);
    true
}

/// 保留首尾，截掉中间，使剩余长度约为 `keep_tokens / tokens` 的比例。
fn truncate_middle(text: &str, keep_tokens: i32, tokens: i32) -> String {
    let chars: Vec<char> = text.chars().collect();
    let ratio = (keep_tokens.max(1) as f64 / tokens.max(1) as f64).clamp(0.0, 1.0);
    let keep_chars = ((chars.len() as f64) * ratio) as usize;
    if keep_chars >= chars.len() {
        return text.to_string();
    }
    let head = keep_chars * 2 / 3;
    let tail = keep_chars - head;
    let mut out: String = chars[..head].iter().collect();
    out.push_str(TRUNCATED_MARKER);
    out.extend(chars[chars.len() - tail..].iter());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> ForcedContextBackoffConfig {
        ForcedContextBackoffConfig::default()
    }

    fn big(n: usize) -> String {
        "lorem ipsum dolor sit amet ".repeat(n)
    }

    fn conversation(turns: usize, tool_result_words: usize) -> MessagesRequest {
        let mut messages = vec![json!({"role": "user", "content": "please fix the build"})];
        for i in 0..turns {
            messages.push(json!({"role": "assistant", "content": [
         {"type": "thinking", "thinking": big(50), "signature": "sig"},
         {"type": "tool_use", "id": format!("toolu_{i}"), "name": "Bash", "input": {"command": "ls"}}
     ]}));
            messages.push(json!({"role": "user", "content": [
         {"type": "tool_result", "tool_use_id": format!("toolu_{i}"), "content": big(tool_result_words)}
     ]}));
        }
        messages.push(json!({"role": "assistant", "content": [{"type": "text", "text": "done"}]}));
        messages.push(json!({"role": "user", "content": "now run the tests"}));
        serde_json::from_value(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 32000,
            "messages": messages
        }))
        .unwrap()
    }

    fn paired(request: &MessagesRequest) -> bool {
        let mut open: Vec<String> = Vec::new();
        for message in &request.messages {
            let Some(blocks) = message.content.as_array() else {
                continue;
            };
            for block in blocks {
                match block_type(block) {
                    Some("tool_use") => open.push(block["id"].as_str().unwrap().to_string()),
                    Some("tool_result") => {
                        let id = block["tool_use_id"].as_str().unwrap();
                        let Some(pos) = open.iter().position(|o| o == id) else {
                            return false;
                        };
                        open.remove(pos);
                    }
                    _ => {}
                }
            }
        }
        true
    }

    #[test]
    fn context_edits_clear_tool_results_and_thinking() {
        let mut request = conversation(6, 50);
        request.context_management = Some(json!({"edits": [
            {"type": "clear_tool_uses_20250919", "keep": {"type": "tool_uses", "value": 2}},
            {"type": "clear_thinking_20251015"},
            {"type": "compact_20260112"}
        ]}));
        let (cleared, thinking) = apply_context_edits(&mut request);
        assert_eq!(cleared, 4);
        assert!(thinking > 0);
        assert!(paired(&request));
        let results: Vec<&Value> = request
            .messages
            .iter()
            .filter_map(|m| m.content.as_array())
            .flatten()
            .filter(|b| block_type(b) == Some("tool_result"))
            .collect();
        assert_eq!(results.iter().filter(|b| is_cleared(b)).count(), 4);
        assert!(!is_cleared(results.last().unwrap()));

        let mut untouched = conversation(2, 10);
        assert_eq!(apply_context_edits(&mut untouched), (0, 0));
    }

    #[test]
    fn budget_reserves_output_and_margin() {
        let budget = input_budget(200_000, 128_000, &config());
        // 64K reserved (target cap), 5% margin.
        assert_eq!(budget, 129_200);
        assert_eq!(input_budget(200_000, 1_000, &config()), 182_400);
    }

    #[test]
    fn requests_within_budget_are_untouched() {
        let mut request = conversation(3, 10);
        let before = serde_json::to_value(&request.messages).unwrap();
        let report = fit_to_budget(&mut request, 1_000_000, &config());
        assert!(!report.changed());
        assert_eq!(serde_json::to_value(&request.messages).unwrap(), before);
    }

    #[test]
    fn oversized_history_fits_and_keeps_tool_pairing() {
        for (turns, words) in [(40, 2_000), (120, 1_000), (10, 30_000)] {
            let mut request = conversation(turns, words);
            let budget = 60_000;
            let report = fit_to_budget(&mut request, budget, &config());
            assert!(report.final_tokens <= budget, "{turns}x{words}: {report:?}");
            assert!(paired(&request), "{turns}x{words}: pairing broken");
            assert_eq!(request.messages[0].content, json!("please fix the build"));
            assert_eq!(
                request.messages.last().unwrap().content,
                json!("now run the tests")
            );
        }
    }

    #[test]
    fn backoff_is_deterministic() {
        let mut a = conversation(80, 1_500);
        let mut b = conversation(80, 1_500);
        fit_to_budget(&mut a, 50_000, &config());
        fit_to_budget(&mut b, 50_000, &config());
        assert_eq!(
            serde_json::to_value(&a.messages).unwrap(),
            serde_json::to_value(&b.messages).unwrap()
        );
    }

    #[test]
    fn thinking_goes_first_and_recent_tool_results_stay() {
        let mut request = conversation(30, 400);
        let report = fit_to_budget(&mut request, 40_000, &config());
        assert!(report.removed_thinking_blocks > 0);
        let last_result = request
            .messages
            .iter()
            .rev()
            .filter_map(|m| m.content.as_array())
            .flatten()
            .find(|b| block_type(b) == Some("tool_result"))
            .unwrap();
        assert_ne!(last_result["content"], json!(CLEARED_TOOL_RESULT));
    }

    #[test]
    fn one_long_agent_turn_is_trimmed_by_dropping_old_tool_pairs() {
        // One user request followed by hundreds of tool calls, still running (ends on a tool
        // result): there is no plain user turn to cut at.
        let mut messages = vec![json!({"role": "user", "content": "refactor the module"})];
        for i in 0..300 {
            messages.push(json!({"role": "assistant", "content": [
                {"type": "text", "text": big(40)},
                {"type": "tool_use", "id": format!("toolu_{i}"), "name": "Read", "input": {"file": big(5)}}
            ]}));
            messages.push(json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": format!("toolu_{i}"), "content": "ok"}
            ]}));
        }
        let mut request: MessagesRequest = serde_json::from_value(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 32000,
            "messages": messages
        }))
        .unwrap();
        let report = fit_to_budget(&mut request, 30_000, &config());
        assert!(report.final_tokens <= 30_000, "{report:?}");
        assert!(report.removed_messages > 0);
        assert!(paired(&request));
        assert_eq!(request.messages[0].content, json!("refactor the module"));
        assert_eq!(request.messages.last().unwrap().role, "user");
        let roles: Vec<&str> = request.messages.iter().map(|m| m.role.as_str()).collect();
        assert!(
            roles.windows(2).all(|pair| pair[0] != pair[1]),
            "roles must alternate"
        );
    }

    #[test]
    fn a_single_huge_message_is_truncated_in_the_middle() {
        let mut request: MessagesRequest = serde_json::from_value(json!({
            "model": "claude-sonnet-4-5",
                   "max_tokens": 1000,
                   "messages": [{"role": "user", "content": format!("BEGIN{}END", big(60_000))}]
        }))
        .unwrap();
        let report = fit_to_budget(&mut request, 50_000, &config());
        assert!(report.final_tokens <= 50_000, "{report:?}");
        let text = request.messages[0].content.as_str().unwrap();
        assert!(text.starts_with("BEGIN"));
        assert!(text.ends_with("END"));
        assert!(text.contains("content truncated"));
    }
}
