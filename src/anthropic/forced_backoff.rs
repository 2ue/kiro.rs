//! 强制模型收敛的上下文退避。
//!
//! 客户端按外显模型的窗口（多为 1M）发送历史，目标模型（4.5 系）只有 200K。请求超出目标窗口时，
//! 在转换前按固定顺序裁剪，直到估算值落进预算：
//!
//! 0. 工具定义超过预算的一定比例时分级压缩（只看工具本身，每轮结果相同）；
//! 1. 去掉最后一条 assistant 之前所有 assistant 的 thinking 块；
//! 2. 清理较早的工具结果：保留最近若干条原文，更早的换成和官方清理一致的占位文字；
//! 3. 从最早的轮次开始成对删除历史，保持 tool_use / tool_result 配对，首条用户文字保留；
//! 4. 仍超出时，对最大的单个文本、工具结果或系统提示块做中间截断。
//!
//! 每一步都只依赖请求内容和预算，同一请求得到同一结果；清理边界按步长推进，历史增长不到一个
//! 步长时前缀保持不变，缓存照常命中。
//!
//! 本地估算和上游计数有偏差（中文、JSON schema 偏差最大），上游仍判定过长时按更小的预算重来一次，
//! 并按会话记住缩小后的比例，同一会话后续请求直接按它裁剪。

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;

use crate::anthropic::types::{Message, MessagesRequest, Tool};
use crate::model::forced_model_mapping::ForcedContextBackoffConfig;

/// 官方清理旧工具结果后的占位文字。
pub(crate) const CLEARED_TOOL_RESULT: &str = "[Old tool result content cleared]";
const TRUNCATED_MARKER: &str = "\n\n[... content truncated ...]\n\n";

/// 发送前的退避结果，挂在 `ForcedTarget` 上：上游仍判定输入过长时，服务端按它缩小预算重试。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForcedFit {
    /// 按目标窗口算出的完整预算。
    pub base_budget_tokens: i32,
    /// 本次实际使用的预算（会话之前被拒过时小于完整预算）。
    pub budget_tokens: i32,
    /// 退避后的本地估算。
    pub final_tokens: i32,
    /// 记住缩小比例用的会话标识；只为大请求计算。
    pub conversation_key: Option<String>,
}

impl ForcedFit {
    /// Requests well below the budget are not kept for a re-fit retry: the upstream does not
    /// reject them as too long, and keeping a copy of every request would cost memory.
    pub fn near_budget(&self) -> bool {
        self.final_tokens.saturating_mul(2) >= self.budget_tokens
    }
}

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
    pub compressed_tools: usize,
}

impl BackoffReport {
    pub fn changed(&self) -> bool {
        self.compressed_tools > 0
            || self.removed_thinking_blocks > 0
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
        .clamp(1, config.max_reserved_output_tokens.max(1))
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

    // Tool definitions do not shrink as the conversation is trimmed. Past their share of the
    // budget they are compressed first, judged by the tools alone, so every turn of a
    // conversation gets the same tool definitions.
    let tool_budget = (budget_tokens as f64 * config.tool_budget_ratio).max(1.0) as i32;
    report.compressed_tools = compress_tools_to_budget(request, tool_budget);
    if report.compressed_tools > 0 {
        report.final_tokens = estimate(request);
        if report.final_tokens <= budget_tokens {
            return report;
        }
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

    // The estimate is a sum over messages, so each dropped turn lowers it by that turn's own
    // count; re-estimating the whole request after every turn made long histories quadratic.
    let mut remaining_tokens = report.final_tokens as i64;
    while remaining_tokens > budget_tokens as i64 {
        let removed = drop_oldest_turn(&mut request.messages);
        if removed.is_empty() {
            break;
        }
        report.removed_messages += removed.len();
        remaining_tokens -= removed
            .iter()
            .map(|message| crate::token::count_message_tokens_local(message) as i64)
            .sum::<i64>();
    }
    if report.removed_messages > 0 {
        // Without a trace the model would not know earlier turns existed and could redo finished
        // work; the note is fixed text, so it does not change between requests.
        mark_omitted_turns(&mut request.messages);
        report.final_tokens = estimate(request);
    }

    while report.final_tokens > budget_tokens {
        let excess = report.final_tokens - budget_tokens;
        if !truncate_largest_block(request, excess) {
            break;
        }
        report.truncated_blocks += 1;
        report.final_tokens = estimate(request);
    }
    report
}

/// Budget for the server-side retry after the upstream still rejected the input as too long.
pub(crate) fn retry_budget(budget_tokens: i32, config: &ForcedContextBackoffConfig) -> i32 {
    ((budget_tokens as f64) * config.retry_budget_ratio).max(1.0) as i32
}

// ============ 按会话记住的预算比例 ============

/// 记住的比例的有效期：会话停下来超过这个时间就按完整预算重新开始。
const BUDGET_SCALE_TTL: Duration = Duration::from_secs(6 * 3600);
const BUDGET_SCALE_CAPACITY: usize = 10_000;
/// 连续被拒时比例逐次相乘，不低于这个值。
pub(crate) const MIN_BUDGET_SCALE: f64 = 0.25;

static BUDGET_SCALES: LazyLock<Mutex<HashMap<String, (f64, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The budget scale remembered for `key` after an earlier too-long rejection.
pub(crate) fn remembered_budget_scale(key: &str) -> Option<f64> {
    let mut scales = BUDGET_SCALES.lock();
    match scales.get(key) {
        Some((scale, at)) if at.elapsed() < BUDGET_SCALE_TTL => Some(*scale),
        Some(_) => {
            scales.remove(key);
            None
        }
        None => None,
    }
}

/// Remembers `scale` for `key`; later requests of the conversation are fitted to it first.
pub(crate) fn remember_budget_scale(key: String, scale: f64) {
    let scale = scale.clamp(MIN_BUDGET_SCALE, 1.0);
    let mut scales = BUDGET_SCALES.lock();
    if scales.len() >= BUDGET_SCALE_CAPACITY && !scales.contains_key(&key) {
        scales.retain(|_, (_, at)| at.elapsed() < BUDGET_SCALE_TTL);
        if scales.len() >= BUDGET_SCALE_CAPACITY
            && let Some(oldest) = scales
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(key, _)| key.clone())
        {
            scales.remove(&oldest);
        }
    }
    scales.insert(key, (scale, Instant::now()));
}

#[cfg(test)]
pub(crate) fn forget_budget_scale(key: &str) {
    BUDGET_SCALES.lock().remove(key);
}

// ============ 工具定义压缩 ============

/// One compression level for tool definitions; levels are tried in order until the tools fit.
struct ToolCompression {
    description_chars: usize,
    /// Schema `description` / `title` / `$comment` cap; 0 removes them.
    annotation_chars: usize,
    /// Enum value lists are cut to this many entries.
    enum_max: Option<usize>,
    /// Schema nodes at this depth or deeper keep only their `type` (the top-level properties are
    /// depth 1); `$ref` nodes become `{}` and `$defs` are dropped.
    flatten_depth: Option<usize>,
}

const TOOL_COMPRESSION_LEVELS: [ToolCompression; 4] = [
    ToolCompression {
        description_chars: 2_000,
        annotation_chars: 300,
        enum_max: None,
        flatten_depth: None,
    },
    ToolCompression {
        description_chars: 800,
        annotation_chars: 100,
        enum_max: None,
        flatten_depth: None,
    },
    ToolCompression {
        description_chars: 300,
        annotation_chars: 0,
        enum_max: Some(64),
        flatten_depth: None,
    },
    ToolCompression {
        description_chars: 120,
        annotation_chars: 0,
        enum_max: Some(32),
        flatten_depth: Some(2),
    },
];

fn tools_tokens(tools: &[Tool]) -> i32 {
    crate::token::count_all_tokens_local(None, &[], Some(tools)).min(i32::MAX as u64) as i32
}

/// Compresses the client's tool definitions until they fit `budget_tokens`, trying the levels
/// from the mildest; the strongest level is kept when none fits. Names and parameter names never
/// change, so the model's tool calls still match the client's tools. Server tools are left alone.
/// Returns the number of tools changed.
fn compress_tools_to_budget(request: &mut MessagesRequest, budget_tokens: i32) -> usize {
    let Some(tools) = request.tools.as_mut() else {
        return 0;
    };
    if tools.is_empty() || tools_tokens(tools) <= budget_tokens {
        return 0;
    }
    let original = tools.clone();
    let mut changed = 0;
    for level in &TOOL_COMPRESSION_LEVELS {
        let mut candidate = original.clone();
        changed = candidate
            .iter_mut()
            .map(|tool| usize::from(compress_tool(tool, level)))
            .sum();
        *tools = candidate;
        if tools_tokens(tools) <= budget_tokens {
            break;
        }
    }
    changed
}

fn compress_tool(tool: &mut Tool, level: &ToolCompression) -> bool {
    if tool
        .tool_type
        .as_deref()
        .is_some_and(|kind| kind != "custom")
    {
        return false;
    }
    let mut changed = shorten_text(&mut tool.description, level.description_chars);
    if tool.input_schema.is_empty() {
        return changed;
    }
    let mut schema = Value::Object(std::mem::take(&mut tool.input_schema).into_iter().collect());
    if level.flatten_depth.is_some()
        && let Value::Object(map) = &mut schema
    {
        changed |= map.remove("$defs").is_some();
        changed |= map.remove("definitions").is_some();
    }
    changed |= shrink_schema(&mut schema, level, 0);
    if let Value::Object(map) = schema {
        tool.input_schema = map.into_iter().collect();
    }
    changed
}

const SCHEMA_ANNOTATION_KEYS: [&str; 3] = ["description", "title", "$comment"];
const SCHEMA_CHILD_MAPS: [&str; 5] = [
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];
const SCHEMA_CHILDREN: [&str; 11] = [
    "items",
    "additionalProperties",
    "additionalItems",
    "not",
    "if",
    "then",
    "else",
    "contains",
    "propertyNames",
    "unevaluatedProperties",
    "unevaluatedItems",
];
const SCHEMA_ALTERNATIVES: [&str; 4] = ["anyOf", "oneOf", "allOf", "prefixItems"];

/// Shrinks one JSON Schema node. Keys inside `properties` are parameter names, never annotations,
/// so a parameter called `description` is kept.
fn shrink_schema(node: &mut Value, level: &ToolCompression, depth: usize) -> bool {
    let Value::Object(map) = node else {
        return false;
    };
    if let Some(flatten_depth) = level.flatten_depth {
        if map.contains_key("$ref") {
            map.clear();
            return true;
        }
        if depth >= flatten_depth {
            let kind = map.remove("type");
            let changed = !map.is_empty();
            map.clear();
            if let Some(kind) = kind {
                map.insert("type".to_string(), kind);
            }
            return changed;
        }
    }
    let mut changed = map.remove("examples").is_some();
    for key in SCHEMA_ANNOTATION_KEYS {
        let Some(Value::String(text)) = map.get_mut(key) else {
            continue;
        };
        if level.annotation_chars == 0 {
            map.remove(key);
            changed = true;
        } else {
            changed |= shorten_text(text, level.annotation_chars);
        }
    }
    if let Some(max) = level.enum_max
        && let Some(Value::Array(values)) = map.get_mut("enum")
        && values.len() > max
    {
        values.truncate(max);
        changed = true;
    }
    for key in SCHEMA_CHILD_MAPS {
        if let Some(Value::Object(children)) = map.get_mut(key) {
            for child in children.values_mut() {
                changed |= shrink_schema(child, level, depth + 1);
            }
        }
    }
    for key in SCHEMA_CHILDREN {
        match map.get_mut(key) {
            Some(child @ Value::Object(_)) => changed |= shrink_schema(child, level, depth + 1),
            Some(Value::Array(children)) => {
                for child in children {
                    changed |= shrink_schema(child, level, depth + 1);
                }
            }
            _ => {}
        }
    }
    for key in SCHEMA_ALTERNATIVES {
        if let Some(Value::Array(children)) = map.get_mut(key) {
            for child in children {
                changed |= shrink_schema(child, level, depth);
            }
        }
    }
    changed
}

/// Cuts `text` to at most `max_chars` characters, at the last sentence or line end in the second
/// half when there is one, and marks the cut with an ellipsis.
fn shorten_text(text: &mut String, max_chars: usize) -> bool {
    if text.chars().count() <= max_chars {
        return false;
    }
    let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    let boundary = cut
        .char_indices()
        .filter(|(_, ch)| matches!(ch, '.' | '\n' | '!' | '?' | '。' | '！' | '？'))
        .map(|(index, ch)| index + ch.len_utf8())
        .rfind(|end| cut[..*end].chars().count() * 2 >= max_chars);
    let mut shortened = boundary
        .map_or(cut.as_str(), |end| &cut[..end])
        .trim_end()
        .to_string();
    shortened.push('…');
    *text = shortened;
    true
}

/// 客户端的 `context_management.edits`，按官方参数在本地执行（`clear_thinking_*` 先于
/// `clear_tool_uses_*`，与官方要求的顺序一致）。返回 (清理的工具结果数, 删除的 thinking 块数)。
///
/// - `clear_tool_uses_*`：输入达到 `trigger`（默认 100K input tokens，或按 tool_uses 计数）时才
///   清理；保留最近 `keep` 个工具调用（默认 3）；`exclude_tools` 里的工具不清；`clear_tool_inputs`
///   为 true（或列出工具名）时同时清空对应 tool_use 的入参；`clear_at_least` 不足时整条跳过。
/// - `clear_thinking_*`：`keep: "all"` 不清；`{type: "thinking_turns", value: N}` 保留最近 N 个
///   带 thinking 的 assistant 轮；不写 keep 时保留 1 轮。
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
    let (thinking_edits, other_edits): (Vec<&Value>, Vec<&Value>) =
        edits.iter().partition(|edit| {
            edit.get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with("clear_thinking_"))
        });
    for edit in thinking_edits {
        let keep_turns = match edit.get("keep") {
            Some(Value::String(all)) if all == "all" => continue,
            Some(keep) => keep.get("value").and_then(Value::as_u64).unwrap_or(1) as usize,
            None => 1,
        };
        removed_thinking += keep_recent_thinking_turns(&mut request.messages, keep_turns.max(1));
    }
    for edit in other_edits {
        let kind = edit.get("type").and_then(Value::as_str).unwrap_or_default();
        if !kind.starts_with("clear_tool_uses_") {
            continue;
        }
        let positions = tool_result_positions(&request.messages);
        let trigger = edit.get("trigger");
        let trigger_kind = trigger
            .and_then(|trigger| trigger.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("input_tokens");
        let trigger_value = trigger
            .and_then(|trigger| trigger.get("value"))
            .and_then(Value::as_u64);
        let reached = match trigger_kind {
            "tool_uses" => positions.len() as u64 >= trigger_value.unwrap_or(0),
            _ => estimate(request) as u64 >= trigger_value.unwrap_or(100_000),
        };
        if !reached {
            continue;
        }
        let keep = edit
            .get("keep")
            .and_then(|keep| keep.get("value"))
            .and_then(Value::as_u64)
            .unwrap_or(3) as usize;
        let excluded: Vec<String> = edit
            .get("exclude_tools")
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let clear_inputs = match edit.get("clear_tool_inputs") {
            Some(Value::Bool(all)) => ClearInputs::All(*all),
            Some(Value::Array(names)) => ClearInputs::Named(
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            ),
            _ => ClearInputs::All(false),
        };
        let clear_at_least = edit
            .get("clear_at_least")
            .and_then(|value| value.get("value"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let before = estimate(request);
        let mut trial = request.messages.clone();
        let count = clear_tool_uses(&mut trial, keep, &excluded, &clear_inputs);
        if count == 0 {
            continue;
        }
        let saved = before.saturating_sub(estimate_messages(request, &trial)) as u64;
        if saved < clear_at_least {
            continue;
        }
        request.messages = trial;
        cleared += count;
    }
    (cleared, removed_thinking)
}

enum ClearInputs {
    All(bool),
    Named(Vec<String>),
}

impl ClearInputs {
    fn applies_to(&self, name: &str) -> bool {
        match self {
            Self::All(all) => *all,
            Self::Named(names) => names.iter().any(|candidate| candidate.as_str() == name),
        }
    }
}

fn estimate_messages(request: &MessagesRequest, messages: &[Message]) -> i32 {
    crate::token::count_all_tokens_local(
        request.system.as_deref(),
        messages,
        request.tools.as_deref(),
    )
    .min(i32::MAX as u64) as i32
}

/// 工具名（按 tool_use id 查）。
fn tool_names_by_id(messages: &[Message]) -> std::collections::HashMap<String, String> {
    messages
        .iter()
        .filter(|message| message.role == "assistant")
        .filter_map(|message| message.content.as_array())
        .flatten()
        .filter(|block| block_type(block) == Some("tool_use"))
        .filter_map(|block| {
            Some((
                block.get("id")?.as_str()?.to_string(),
                block.get("name")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// 清理较早的工具结果（官方 `clear_tool_uses`）：最近 `keep` 个保留，`excluded` 里的工具不清；
/// 需要时把对应 tool_use 的入参清空。返回清理的条数。
fn clear_tool_uses(
    messages: &mut [Message],
    keep: usize,
    excluded: &[String],
    clear_inputs: &ClearInputs,
) -> usize {
    let names = tool_names_by_id(messages);
    let positions = tool_result_positions(messages);
    let clearable = positions.len().saturating_sub(keep);
    let mut cleared_ids = Vec::new();
    for &(mi, bi) in positions.iter().take(clearable) {
        let Some(block) = blocks_mut(&mut messages[mi]).and_then(|blocks| blocks.get_mut(bi))
        else {
            continue;
        };
        let id = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let name = names.get(&id).cloned().unwrap_or_default();
        if excluded.contains(&name) || is_cleared(block) {
            continue;
        }
        block["content"] = Value::String(CLEARED_TOOL_RESULT.to_string());
        cleared_ids.push((id, name));
    }
    for (id, name) in &cleared_ids {
        if !clear_inputs.applies_to(name) {
            continue;
        }
        for message in messages.iter_mut().filter(|m| m.role == "assistant") {
            let Some(blocks) = blocks_mut(message) else {
                continue;
            };
            for block in blocks.iter_mut() {
                if block_type(block) == Some("tool_use")
                    && block.get("id").and_then(Value::as_str) == Some(id.as_str())
                {
                    block["input"] = serde_json::json!({});
                }
            }
        }
    }
    cleared_ids.len()
}

/// 只保留最近 `keep_turns` 个带 thinking 的 assistant 轮的 thinking 块。返回删除的块数。
fn keep_recent_thinking_turns(messages: &mut [Message], keep_turns: usize) -> usize {
    let with_thinking: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "assistant")
        .filter(|(_, m)| {
            m.content.as_array().is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    matches!(
                        block_type(block),
                        Some("thinking") | Some("redacted_thinking")
                    )
                })
            })
        })
        .map(|(index, _)| index)
        .collect();
    let drop = with_thinking.len().saturating_sub(keep_turns);
    let mut removed = 0;
    for &index in with_thinking.iter().take(drop) {
        let Some(blocks) = blocks_mut(&mut messages[index]) else {
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

/// Text added to the first user message when earlier turns were dropped to fit the window.
pub(crate) const OMITTED_TURNS_NOTE: &str = "<system-reminder>\nSome earlier turns of this conversation are not shown here. If something you need from them is missing, ask the user or check the files again instead of assuming.\n</system-reminder>";

/// Adds [`OMITTED_TURNS_NOTE`] to the first user message once.
fn mark_omitted_turns(messages: &mut [Message]) {
    let Some(first) = messages
        .first_mut()
        .filter(|message| message.role == "user")
    else {
        return;
    };
    if first
        .content
        .to_string()
        .contains("Some earlier turns of this conversation are not shown")
    {
        return;
    }
    let note = serde_json::json!({"type": "text", "text": OMITTED_TURNS_NOTE});
    first.content = match std::mem::take(&mut first.content) {
        Value::Array(mut blocks) => {
            blocks.push(note);
            Value::Array(blocks)
        }
        Value::String(text) if !text.is_empty() => Value::Array(vec![
            serde_json::json!({"type": "text", "text": text}),
            note,
        ]),
        _ => Value::Array(vec![note]),
    };
}

/// 删除最早的一轮（首条用户消息之后的第一组 assistant + 回应它的 user），保持配对；
/// 最后一条消息（当前轮）不动。返回被删除的消息，没有可删的轮次时为空。
fn drop_oldest_turn(messages: &mut Vec<Message>) -> Vec<Message> {
    // Keep the first user message (the original request) and the current turn.
    let keep_tail = 2;
    if messages.len() <= 1 + keep_tail {
        return Vec::new();
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
        return Vec::new();
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
        return Vec::new();
    }
    // A dangling leading assistant (no user before it) is not allowed; the first user stays.
    messages.drain(first..end).collect()
}

fn text_len_tokens(text: &str) -> i32 {
    crate::token::count_tokens(text).min(i32::MAX as u64) as i32
}

/// 最大单块（文本、工具结果或系统提示块）做中间截断，至少去掉 `excess` 个 token。找不到可截断的块时
/// 返回 false。
fn truncate_largest_block(request: &mut MessagesRequest, excess: i32) -> bool {
    let largest_system = request.system.as_ref().and_then(|blocks| {
        blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (index, text_len_tokens(&block.text)))
            .max_by_key(|(_, tokens)| *tokens)
    });
    let largest_message = largest_message_block(&request.messages);
    match largest_system {
        Some((index, tokens))
            if tokens >= 64 && largest_message.is_none_or(|(_, _, _, t)| tokens > t) =>
        {
            let Some(block) = request
                .system
                .as_mut()
                .and_then(|blocks| blocks.get_mut(index))
            else {
                return false;
            };
            let keep_tokens = (tokens - excess - 32).max(tokens / 8).max(16);
            let truncated = truncate_middle(&block.text, keep_tokens, tokens);
            if truncated.len() >= block.text.len() {
                return false;
            }
            block.text = truncated;
            true
        }
        _ => truncate_message_block(&mut request.messages, largest_message, excess),
    }
}

/// 消息里最大的单块：(消息下标, 块下标或 `usize::MAX` 表示整条字符串, 工具结果内的条目下标, tokens)。
fn largest_message_block(messages: &[Message]) -> Option<(usize, usize, Option<usize>, i32)> {
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
    best
}

fn truncate_message_block(
    messages: &mut [Message],
    best: Option<(usize, usize, Option<usize>, i32)>,
    excess: i32,
) -> bool {
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

    /// `fit_to_budget` as it was before dropping turns stopped re-estimating the whole request
    /// after each turn: same steps, a full estimate after every dropped turn.
    fn fit_to_budget_reestimating(
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
            if removed.is_empty() {
                break;
            }
            report.removed_messages += removed.len();
            report.final_tokens = estimate(request);
        }
        if report.removed_messages > 0 {
            mark_omitted_turns(&mut request.messages);
            report.final_tokens = estimate(request);
        }
        while report.final_tokens > budget_tokens {
            let excess = report.final_tokens - budget_tokens;
            if !truncate_largest_block(request, excess) {
                break;
            }
            report.truncated_blocks += 1;
            report.final_tokens = estimate(request);
        }
        report
    }

    fn chatty_conversation(turns: usize, words: usize) -> MessagesRequest {
        let mut messages = vec![json!({"role": "user", "content": "please fix the build"})];
        for i in 0..turns {
            messages.push(json!({"role": "assistant", "content": [
                {"type": "text", "text": big(words + i % 7)},
                {"type": "tool_use", "id": format!("toolu_{i}"), "name": "Bash", "input": {"command": big(3)}}
            ]}));
            messages.push(json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": format!("toolu_{i}"), "content": big(words / 2)},
                {"type": "text", "text": big(words / 3)}
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

    #[test]
    fn dropping_turns_without_reestimating_matches_reestimating_every_turn() {
        for (turns, words, budget) in [
            (40, 200, 30_000),
            (120, 120, 20_000),
            (300, 60, 50_000),
            (60, 400, 1_000),
            (5, 50, 100),
        ] {
            for request in [
                chatty_conversation(turns, words),
                conversation(turns, words),
            ] {
                let mut fast = request.clone();
                let mut reference = request;
                let fast_report = fit_to_budget(&mut fast, budget, &config());
                let reference_report =
                    fit_to_budget_reestimating(&mut reference, budget, &config());
                assert_eq!(
                    fast_report, reference_report,
                    "turns={turns} words={words} budget={budget}"
                );
                assert_eq!(
                    serde_json::to_value(&fast.messages).unwrap(),
                    serde_json::to_value(&reference.messages).unwrap(),
                    "turns={turns} words={words} budget={budget}"
                );
            }
        }
    }

    #[test]
    fn context_edits_clear_tool_results_and_thinking() {
        let mut request = conversation(6, 50);
        request.context_management = Some(json!({"edits": [
        {"type": "clear_tool_uses_20250919", "trigger": {"type": "tool_uses", "value": 1}, "keep": {"type": "tool_uses", "value": 2}},
        {"type": "clear_thinking_20251015"},
        {"type": "compact_20260112"}
           ]}));
        let (cleared, thinking) = apply_context_edits(&mut request);
        assert_eq!(cleared, 4);
        // Thinking is kept on the latest assistant turn with thinking only.
        assert_eq!(thinking, 5);
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
    fn context_edits_follow_the_official_parameters() {
        // Claude Code sends `keep: "all"` on every request: nothing is cleared.
        let mut request = conversation(6, 50);
        request.context_management = Some(json!({"edits": [
        {"type": "clear_thinking_20251015", "keep": "all"}
           ]}));
        assert_eq!(apply_context_edits(&mut request), (0, 0));

        // The default trigger is 100K input tokens: a small conversation is left alone.
        let mut request = conversation(6, 50);
        request.context_management = Some(json!({"edits": [
        {"type": "clear_tool_uses_20250919"}
           ]}));
        assert_eq!(apply_context_edits(&mut request), (0, 0));

        // Excluded tools keep their results; cleared tool inputs are emptied when asked.
        let mut request = conversation(6, 50);
        request.context_management = Some(json!({"edits": [
                   {"type": "clear_tool_uses_20250919", "trigger": {"type": "input_tokens", "value": 1},
             "keep": {"type": "tool_uses", "value": 1}, "exclude_tools": ["Read"],
             "clear_tool_inputs": true}
        ]}));
        let (cleared, _) = apply_context_edits(&mut request);
        assert_eq!(cleared, 5);
        let first_use = request.messages[1]
            .content
            .as_array()
            .unwrap()
            .iter()
            .find(|b| block_type(b) == Some("tool_use"))
            .unwrap();
        assert_eq!(first_use["input"], json!({}));
        assert!(paired(&request));

        // clear_at_least: skipped when clearing would not save enough.
        let mut request = conversation(6, 5);
        request.context_management = Some(json!({"edits": [
        {"type": "clear_tool_uses_20250919", "trigger": {"type": "input_tokens", "value": 1},
         "clear_at_least": {"type": "input_tokens", "value": 1000000}}
           ]}));
        assert_eq!(apply_context_edits(&mut request), (0, 0));
    }

    #[test]
    fn budget_reserves_output_and_margin() {
        let budget = input_budget(200_000, 128_000, &config());
        // 32K reserved (configured cap), 5% margin.
        assert_eq!(budget, 159_600);
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
            assert!(
                request.messages[0]
                    .content
                    .to_string()
                    .contains("please fix the build"),
                "{turns}x{words}: first user message kept"
            );
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
        assert_eq!(
            request.messages[0].content[0]["text"],
            json!("refactor the module")
        );
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

#[cfg(test)]
mod phase2_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dropped_turns_leave_a_fixed_note_on_the_first_message() {
        let mut messages: Vec<Message> = serde_json::from_value(json!([
        {"role": "user", "content": "task"},
        {"role": "assistant", "content": "a"}
           ]))
        .unwrap();
        mark_omitted_turns(&mut messages);
        mark_omitted_turns(&mut messages);
        let text = messages[0].content.to_string();
        assert_eq!(text.matches("Some earlier turns").count(), 1, "{text}");
        assert!(text.contains("task"));
    }
}

#[cfg(test)]
mod too_long_tests {
    use super::*;
    use serde_json::json;

    fn config() -> ForcedContextBackoffConfig {
        ForcedContextBackoffConfig::default()
    }

    fn long_text(words: usize) -> String {
        "Runs the selected operation against the workspace. ".repeat(words)
    }

    /// Many client tools with long descriptions and nested, annotated schemas, as sent by
    /// clients with many MCP servers.
    fn tool_heavy_request(tools: usize) -> MessagesRequest {
        let tools: Vec<Value> = (0..tools)
            .map(|i| {
                json!({
                    "name": format!("mcp__server__tool_{i}"),
                    "description": long_text(120),
                    "input_schema": {
                        "type": "object",
                        "$defs": {"Mode": {"type": "string", "description": long_text(40)}},
                        "properties": {
                            "description": {"type": "string", "description": long_text(40)},
                            "options": {
                                "type": "object",
                                "description": long_text(40),
                                "examples": [{"a": 1}, {"b": 2}],
                                "properties": {
                                    "mode": {"$ref": "#/$defs/Mode"},
                                    "level": {"type": "string", "enum": (0..200).map(|n| format!("v{n}")).collect::<Vec<_>>()}
                                }
                            }
                        },
                        "required": ["description"]
                    }
                })
            })
            .collect();
        serde_json::from_value(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 8192,
            "tools": tools,
            "messages": [
                {"role": "user", "content": "first task"},
                {"role": "assistant", "content": "working on it"},
                {"role": "user", "content": "continue"}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn oversized_tool_definitions_are_compressed_before_history_is_touched() {
        let mut request = tool_heavy_request(120);
        let tool_names: Vec<String> = request
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        let before = tools_tokens(request.tools.as_ref().unwrap());
        let budget = before / 2;
        let report = fit_to_budget(&mut request, budget, &config());
        assert!(report.compressed_tools > 0, "{report:?}");
        assert_eq!(report.removed_messages, 0, "{report:?}");
        assert!(report.final_tokens <= budget, "{report:?}");
        let tools = request.tools.as_ref().unwrap();
        let after = tools_tokens(tools);
        assert!(
            after <= (budget as f64 * config().tool_budget_ratio) as i32 || after < before / 3,
            "tools {before} -> {after}"
        );
        // Names and parameter names never change, so the model's calls still match the client.
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
            tool_names
        );
        let properties = tools[0].input_schema["properties"].as_object().unwrap();
        assert!(properties.contains_key("description"), "{properties:?}");
        assert!(properties.contains_key("options"), "{properties:?}");
        assert!(tools[0].description.ends_with('…'));
        assert_eq!(request.messages.len(), 3);
    }

    #[test]
    fn tool_compression_is_deterministic_and_skipped_within_budget() {
        let mut first = tool_heavy_request(60);
        let mut second = tool_heavy_request(60);
        let budget = tools_tokens(first.tools.as_ref().unwrap()) / 2;
        fit_to_budget(&mut first, budget, &config());
        fit_to_budget(&mut second, budget, &config());
        assert_eq!(
            serde_json::to_value(&first.tools).unwrap(),
            serde_json::to_value(&second.tools).unwrap()
        );

        let mut within = tool_heavy_request(5);
        let original = serde_json::to_value(&within.tools).unwrap();
        let report = fit_to_budget(&mut within, i32::MAX, &config());
        assert_eq!(report.compressed_tools, 0);
        assert_eq!(serde_json::to_value(&within.tools).unwrap(), original);
    }

    #[test]
    fn strongest_level_flattens_nested_schemas_but_keeps_top_level_parameters() {
        let mut tool = tool_heavy_request(1).tools.unwrap().remove(0);
        assert!(compress_tool(&mut tool, &TOOL_COMPRESSION_LEVELS[3]));
        let schema = Value::Object(tool.input_schema.clone().into_iter().collect());
        assert!(schema.get("$defs").is_none(), "{schema}");
        // A parameter called `description` is a parameter, not an annotation.
        assert_eq!(
            schema["properties"]["description"],
            json!({"type": "string"})
        );
        let options = &schema["properties"]["options"];
        assert_eq!(options["type"], "object", "{schema}");
        assert!(options.get("description").is_none(), "{schema}");
        assert!(options.get("examples").is_none(), "{schema}");
        // Deeper nodes keep only their type; references become an empty schema.
        assert_eq!(options["properties"]["level"], json!({"type": "string"}));
        assert_eq!(options["properties"]["mode"], json!({}));
        assert_eq!(schema["required"], json!(["description"]));
        assert!(tool.description.chars().count() <= 120);
    }

    #[test]
    fn server_tools_are_not_compressed() {
        let mut tool: Tool = serde_json::from_value(json!({
            "type": "web_search_20250305",
            "name": "web_search",
            "max_uses": 5
        }))
        .unwrap();
        assert!(!compress_tool(&mut tool, &TOOL_COMPRESSION_LEVELS[3]));
    }

    #[test]
    fn a_huge_system_prompt_is_truncated_when_it_is_the_largest_block() {
        let mut request: MessagesRequest = serde_json::from_value(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 1000,
            "system": [
                {"type": "text", "text": "short rules"},
                {"type": "text", "text": format!("HEAD{}TAIL", long_text(20_000))}
            ],
            "messages": [
                {"role": "user", "content": "summarize the document"}
            ]
        }))
        .unwrap();
        let report = fit_to_budget(&mut request, 50_000, &config());
        assert!(report.final_tokens <= 50_000, "{report:?}");
        assert!(report.truncated_blocks > 0, "{report:?}");
        let system = request.system.as_ref().unwrap();
        assert_eq!(system[0].text, "short rules");
        assert!(system[1].text.starts_with("HEAD"));
        assert!(system[1].text.ends_with("TAIL"));
        assert!(system[1].text.contains("content truncated"));
        assert_eq!(
            request.messages[0].content.as_str(),
            Some("summarize the document")
        );
    }

    #[test]
    fn shorten_text_prefers_a_sentence_end_and_counts_characters() {
        let mut text = "第一句话说明用途。第二句话补充细节。第三句话很长很长很长".to_string();
        assert!(shorten_text(&mut text, 20));
        assert_eq!(text, "第一句话说明用途。第二句话补充细节。…");
        let mut short = "fits".to_string();
        assert!(!shorten_text(&mut short, 10));
        assert_eq!(short, "fits");
    }

    #[test]
    fn retry_budget_scales_the_budget() {
        assert_eq!(retry_budget(100_000, &config()), 65_000);
    }

    #[test]
    fn budget_scales_are_remembered_per_conversation_and_clamped() {
        let key = "too-long-test-conversation".to_string();
        forget_budget_scale(&key);
        assert_eq!(remembered_budget_scale(&key), None);
        remember_budget_scale(key.clone(), 0.65);
        assert_eq!(remembered_budget_scale(&key), Some(0.65));
        remember_budget_scale(key.clone(), 0.01);
        assert_eq!(remembered_budget_scale(&key), Some(MIN_BUDGET_SCALE));
        forget_budget_scale(&key);
        assert_eq!(remembered_budget_scale(&key), None);
    }
}
