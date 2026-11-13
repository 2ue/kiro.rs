//! 强制模型收敛的摘要检查点（分析文档 12 篇）。
//!
//! 客户端以为窗口是 1M，实际服务的模型只有 200K。超出预算时，不再直接删掉早期轮次，而是把它们
//! 浓缩成一段工作记录，接在首条用户消息后面：
//!
//! ```text
//! 客户端： [m0 任务] [m1] … [mB] … [mN 当前轮]
//! 上游：   [m0 任务 + 早期记录(m1..mB-1 的摘要)] [mB] … [mN]      mB 一定是 assistant 消息
//! ```
//!
//! 检查点按消息内容的累积哈希索引：客户端事后改写旧工具结果、移动 cache_control 都不影响查找。
//! 同一个检查点一直复用到尾部再次长满，期间发给上游的前缀逐字不变，缓存照常命中。本模块只有
//! 纯逻辑和进程内的检查点表；调用模型生成摘要在 `handlers/forced_summary_call.rs`。

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use parking_lot::Mutex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::anthropic::types::{Message, MessagesRequest};

/// 检查点：消息 `cut` 之前（不含 m0）的对话由 `summary` 代替。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    /// 切点：一条 assistant 消息的下标，它前面是一条 user 消息。
    pub cut: usize,
    pub summary: String,
}

type Key = [u8; 32];

/// 每条消息的累积哈希：`h[i]` 覆盖 m0..=mi。只取角色、文字、工具调用的 id/名称/入参和工具结果
/// 对应的 id；工具结果的内容、thinking、图片、cache_control 都不参与。
pub(crate) fn prefix_hashes(messages: &[Message]) -> Vec<Key> {
    let mut hashes = Vec::with_capacity(messages.len());
    let mut previous = [0u8; 32];
    for message in messages {
        let mut hasher = Sha256::new();
        hasher.update(previous);
        hasher.update(message.role.as_bytes());
        hasher.update([0]);
        canonical_content(&message.content, &mut hasher);
        previous = hasher.finalize().into();
        hashes.push(previous);
    }
    hashes
}

fn canonical_content(content: &Value, hasher: &mut Sha256) {
    match content {
        Value::String(text) => {
            hasher.update(b"t");
            hasher.update(text.as_bytes());
        }
        Value::Array(blocks) => {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        hasher.update(b"t");
                        let text = block.get("text").and_then(Value::as_str);
                        hasher.update(text.unwrap_or_default().as_bytes());
                    }
                    Some("tool_use") | Some("server_tool_use") => {
                        hasher.update(b"u");
                        for key in ["id", "name"] {
                            let value = block.get(key).and_then(Value::as_str);
                            hasher.update(value.unwrap_or_default().as_bytes());
                            hasher.update([0]);
                        }
                        let input = block.get("input").map(Value::to_string);
                        hasher.update(input.unwrap_or_default().as_bytes());
                    }
                    Some("tool_result") => {
                        hasher.update(b"r");
                        let id = block.get("tool_use_id").and_then(Value::as_str);
                        hasher.update(id.unwrap_or_default().as_bytes());
                    }
                    _ => {}
                }
                hasher.update([1]);
            }
        }
        _ => {}
    }
}

/// `cut` 能否作为切点：m0 是 user，`cut` 是 assistant，前一条是 user。
fn valid_cut(messages: &[Message], cut: usize) -> bool {
    cut >= 2
        && cut < messages.len()
        && messages[0].role == "user"
        && messages[cut].role == "assistant"
        && messages[cut - 1].role == "user"
}

/// 摘要块的文字，追加在首条用户消息里。
pub(crate) fn summary_block_text(summary: &str) -> String {
    format!(
        "<system-reminder>\nThis conversation continues from earlier turns that are no longer shown in full. Summary of those turns:\n\n{}\n</system-reminder>",
        summary.trim()
    )
}

/// 按检查点拼出发给上游的消息：m0 加摘要块，再接 `cut` 之后的原样消息。
pub(crate) fn apply_checkpoint(messages: &[Message], checkpoint: &Checkpoint) -> Vec<Message> {
    if !valid_cut(messages, checkpoint.cut) {
        return messages.to_vec();
    }
    let mut first = messages[0].clone();
    let block =
        serde_json::json!({"type": "text", "text": summary_block_text(&checkpoint.summary)});
    first.content = match std::mem::take(&mut first.content) {
        Value::Array(mut blocks) => {
            blocks.push(block);
            Value::Array(blocks)
        }
        Value::String(text) if !text.is_empty() => Value::Array(vec![
            serde_json::json!({"type": "text", "text": text}),
            block,
        ]),
        _ => Value::Array(vec![block]),
    };
    std::iter::once(first)
        .chain(messages[checkpoint.cut..].iter().cloned())
        .collect()
}

/// 每条消息的本地 token 估算。
pub(crate) fn message_tokens(messages: &[Message]) -> Vec<i32> {
    messages
        .iter()
        .map(|message| {
            crate::token::count_all_tokens_local(None, std::slice::from_ref(message), None)
                .min(i32::MAX as u64) as i32
        })
        .collect()
}

/// 下一个切点：从末尾往前累加，尾部达到 `tail_budget` 之前的位置之后的第一个合法切点，且要比
/// `after` 靠后（检查点只向后推进）。找不到时返回 `None`。
pub(crate) fn next_cut(
    messages: &[Message],
    tokens: &[i32],
    tail_budget: i32,
    after: usize,
) -> Option<usize> {
    let mut tail = 0i64;
    let mut start = messages.len();
    for index in (1..messages.len()).rev() {
        tail += i64::from(tokens.get(index).copied().unwrap_or(0));
        if tail > i64::from(tail_budget.max(0)) {
            break;
        }
        start = index;
    }
    // The last two messages (latest answer and current turn) always stay.
    let latest = messages.len().saturating_sub(2);
    (start.max(after + 1)..=latest).find(|&cut| valid_cut(messages, cut))
}

// ===================== 文字记录与摘要请求 =====================

const REMINDER_OPEN: &str = "<system-reminder>";
const REMINDER_CLOSE: &str = "</system-reminder>";
const TRANSCRIPT_REMINDER_MAX_CHARS: usize = 600;
const TRANSCRIPT_TOOL_INPUT_MAX_CHARS: usize = 1_500;
const TRANSCRIPT_TOOL_RESULT_MAX_CHARS: usize = 2_000;
/// Findings (errors, notes, decisions) kept from a long tool result cut to its head and tail.
const TRANSCRIPT_SALIENT_LINE_MAX_CHARS: usize = 1_500;
/// 一次摘要调用里文字记录的上限（tokens），超出时分段滚动摘要。
pub(crate) const SUMMARY_CHUNK_TOKENS: i32 = 100_000;
/// 摘要正文的上限（字符），防止模型写得过长。
const SUMMARY_MAX_CHARS: usize = 40_000;
/// 预留给摘要块的 token 数。
pub(crate) const SUMMARY_RESERVE_TOKENS: i32 = 8_000;

fn truncate_middle_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let head = max_chars * 2 / 3;
    let tail = max_chars - head;
    let head_text: String = text.chars().take(head).collect();
    let tail_text: String = text.chars().skip(count - tail).collect();
    format!(
        "{head_text}\n[... {} characters omitted ...]\n{tail_text}",
        count - max_chars
    )
}

/// 用户文字里的提醒块各自截短，其余原样。
fn shorten_reminders(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(64 * 1024));
    let mut rest = text;
    while let Some(start) = rest.find(REMINDER_OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let end = after
            .find(REMINDER_CLOSE)
            .map_or(after.len(), |end| end + REMINDER_CLOSE.len());
        out.push_str(&truncate_middle_chars(
            &after[..end],
            TRANSCRIPT_REMINDER_MAX_CHARS,
        ));
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn block_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") => part
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                Some(other) => format!("[{other}]"),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// 把一段消息渲染成供摘要的文字记录：文字原样（提醒块截短），工具调用写成 名称(入参)，工具结果
/// 截取头尾，thinking 和图片只留标记。
pub(crate) fn render_transcript(messages: &[Message]) -> String {
    let mut lines = Vec::new();
    for message in messages {
        let role = message.role.as_str();
        match &message.content {
            Value::String(text) => {
                if !text.trim().is_empty() {
                    lines.push(format!("[{role}] {}", shorten_reminders(text)));
                }
            }
            Value::Array(blocks) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if !text.trim().is_empty() {
                                lines.push(format!("[{role}] {}", shorten_reminders(text)));
                            }
                        }
                        Some("tool_use") | Some("server_tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let input =
                                block.get("input").map(Value::to_string).unwrap_or_default();
                            lines.push(format!(
                                "[assistant -> tool {name}] {}",
                                truncate_middle_chars(&input, TRANSCRIPT_TOOL_INPUT_MAX_CHARS)
                            ));
                        }
                        Some("tool_result") => {
                            let error =
                                block.get("is_error").and_then(Value::as_bool) == Some(true);
                            let full = block_text(block);
                            let mut rendered =
                                truncate_middle_chars(&full, TRANSCRIPT_TOOL_RESULT_MAX_CHARS);
                            // A finding buried in the middle of a long result survives the cut.
                            if full.chars().count() > TRANSCRIPT_TOOL_RESULT_MAX_CHARS {
                                let salient = salient_lines(&full);
                                if !salient.is_empty() {
                                    rendered.push_str("\n[notable lines] ");
                                    rendered.push_str(&truncate_middle_chars(
                                        &salient,
                                        TRANSCRIPT_SALIENT_LINE_MAX_CHARS,
                                    ));
                                }
                            }
                            let label = if error { "error" } else { "result" };
                            lines.push(format!("[tool {label}] {rendered}"));
                        }
                        Some("image") => lines.push(format!("[{role}] [image]")),
                        Some("document") => lines.push(format!("[{role}] [document]")),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    lines.join("\n")
}

/// Markers of an explicit finding in tool output.
const STRONG: &[&str] = &[
    "note:",
    "root cause",
    "important:",
    "conclusion",
    "finding",
    "decided",
    "decision:",
    "caused by",
];
/// Most findings kept verbatim in one checkpoint, and the longest one.
const VERBATIM_FINDINGS_MAX: usize = 12;
const VERBATIM_FINDING_MAX_CHARS: usize = 300;
const VERBATIM_FINDINGS_HEADING: &str = "## Notes from tool output (verbatim)";

/// The record with the explicit findings of the summarized tool output appended word for word.
/// The summary model tends to drop a one-line note in a long file as incidental; a root cause or
/// decision found by a tool must survive the checkpoint exactly. Findings the record already
/// quotes are not repeated.
pub(crate) fn with_verbatim_findings(record: String, chunks: &[String]) -> String {
    let mut findings: Vec<String> = Vec::new();
    for chunk in chunks {
        for line in chunk
            .lines()
            .filter(|line| line.starts_with("[tool ") || line.starts_with("[notable lines]"))
        {
            for piece in line.split(" | ").flat_map(|piece| piece.split(['\n'])) {
                let piece = piece
                    .trim_start_matches("[notable lines]")
                    .trim_start_matches("[tool result]")
                    .trim_start_matches("[tool error]")
                    .trim();
                let lower = piece.to_ascii_lowercase();
                let Some(at) = STRONG.iter().filter_map(|marker| lower.find(marker)).min() else {
                    continue;
                };
                // Start at the marker's sentence: the text before it on the same line is
                // usually unrelated output.
                let start = piece[..at].rfind(['.', ';']).map_or(0, |dot| dot + 1);
                let finding =
                    truncate_middle_chars(piece[start..].trim(), VERBATIM_FINDING_MAX_CHARS);
                if !finding.is_empty() && !findings.contains(&finding) && !record.contains(&finding)
                {
                    findings.push(finding);
                }
            }
        }
    }
    if findings.is_empty() {
        return record;
    }
    let keep = findings.len().saturating_sub(VERBATIM_FINDINGS_MAX);
    let mut out = record.trim_end().to_string();
    if !out.contains(VERBATIM_FINDINGS_HEADING) {
        out.push_str("\n\n");
        out.push_str(VERBATIM_FINDINGS_HEADING);
    }
    for finding in &findings[keep..] {
        out.push_str("\n- ");
        out.push_str(finding);
    }
    out
}

/// Sentences of a long tool result that state a finding: errors, causes, notes, decisions. A
/// very long sentence keeps only the words around the marker. Explicit findings ("NOTE:", "root
/// cause") are picked before incidental matches, so a note at the end of a long file is not
/// crowded out by the `error` and `must` that ordinary code is full of; the picked lines keep
/// their original order.
fn salient_lines(text: &str) -> String {
    const LIMIT: usize = 12;
    const MARKERS: &[&str] = &[
        "error",
        "fail",
        "panic",
        "cause",
        "note",
        "todo",
        "fixme",
        "warning",
        "decid",
        "must",
        "important",
        "bug",
        "fix",
    ];
    const WINDOW: usize = 160;
    let excerpt = |line: &str| -> Option<String> {
        let lower = line.to_ascii_lowercase();
        let at = MARKERS
            .iter()
            .filter_map(|marker| lower.find(marker))
            .min()?;
        if line.len() <= 400 {
            return Some(line.to_string());
        }
        let mut start = at.saturating_sub(WINDOW);
        while !line.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (at + WINDOW).min(line.len());
        while !line.is_char_boundary(end) {
            end += 1;
        }
        Some(format!("...{}...", &line[start..end]))
    };
    let mut picked: Vec<(bool, usize, String)> = text
        .split(['\n', '.'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
        .filter_map(|(position, line)| {
            let lower = line.to_ascii_lowercase();
            let strong = STRONG.iter().any(|marker| lower.contains(marker));
            excerpt(line).map(|excerpt| (!strong, position, excerpt))
        })
        .collect();
    picked.sort_by_key(|(weak, position, _)| (*weak, *position));
    picked.truncate(LIMIT);
    picked.sort_by_key(|(_, position, _)| *position);
    picked
        .into_iter()
        .map(|(_, _, excerpt)| excerpt)
        .collect::<Vec<_>>()
        .join(" | ")
}

const SUMMARY_SYSTEM_PROMPT: &str = "You write the working record of a long conversation between a user and an AI assistant, so the assistant can continue the same work later without the full history. Record only what happened in the conversation.";

/// 一次摘要调用的用户消息：上一份记录（如有）加这一段文字记录，以及要求。
pub(crate) fn summary_prompt(previous: Option<&str>, transcript: &str) -> String {
    let mut prompt = String::new();
    if let Some(previous) = previous.filter(|text| !text.trim().is_empty()) {
        prompt.push_str("<previous_record>\n");
        prompt.push_str(previous.trim());
        prompt.push_str("\n</previous_record>\n\n");
    }
    prompt.push_str("<conversation>\n");
    prompt.push_str(transcript);
    prompt.push_str("\n</conversation>\n\n");
    prompt.push_str(
        "Write the updated working record so it covers the previous record and the conversation above. Include, in this order:\n\
1. The user's goal and every explicit request, preference or constraint, quoting the important ones exactly.\n\
2. Decisions made and why, including approaches that were tried and rejected.\n\
3. Files, functions, commands and key code involved, and what was changed.\n\
4. Errors encountered and how they were resolved.\n\
5. Facts found in tool output (root causes, notes, values, names), with names and identifiers exactly as written.\n\
6. Current state: what is done and what is in progress.\n\
7. Next steps and open questions.\n\
Write in the same language the user writes in, keep it under about 2500 words, and output only the record.",
    );
    prompt
}

/// 摘要调用的请求体：不带工具和思考，非流式。`instructions` 是客户端给的压缩要求（按需压缩的
/// `compaction.instructions`），替换默认的要求。
pub(crate) fn summary_request(
    model: &str,
    previous: Option<&str>,
    transcript: &str,
    instructions: Option<&str>,
) -> MessagesRequest {
    let content = match instructions.map(str::trim).filter(|text| !text.is_empty()) {
        Some(instructions) => {
            format!("<conversation>\n{transcript}\n</conversation>\n\n{instructions}")
        }
        None => summary_prompt(previous, transcript),
    };
    serde_json::from_value(serde_json::json!({
    "model": model,
           "max_tokens": 8192,
           "stream": false,
    "system": SUMMARY_SYSTEM_PROMPT,
           "messages": [{"role": "user", "content": content}],
       }))
    .expect("summary request is a valid MessagesRequest")
}

/// 请求历史里的 `compaction` 块（客户端从官方 API 带过来的服务端压缩结果）：官方会丢弃它之前的
/// 全部内容，以它的摘要接着对话。这里照做：保留 m0 之后的摘要形态（与检查点相同），从包含该块的
/// assistant 消息开始接原样消息。返回是否改写。
pub(crate) fn apply_client_compaction_block(messages: &mut Vec<Message>) -> bool {
    let found = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| {
            let blocks = message.content.as_array()?;
            blocks
                .iter()
                .position(|block| block.get("type").and_then(Value::as_str) == Some("compaction"))
                .map(|position| (index, position))
        });
    let Some((index, position)) = found else {
        return false;
    };
    let summary = messages[index].content.as_array().and_then(|blocks| {
        blocks[position]
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    if let Some(blocks) = messages[index].content.as_array_mut() {
        blocks.remove(position);
        if blocks.is_empty() {
            blocks.push(serde_json::json!({"type": "text", "text": " "}));
        }
    }
    let Some(summary) = summary.filter(|text| !text.trim().is_empty()) else {
        return true;
    };
    let mut rest: Vec<Message> = messages.drain(index..).collect();
    messages.clear();
    let first: Message = serde_json::from_value(serde_json::json!({
           "role": "user",
    "content": [{"type": "text", "text": summary_block_text(&summary)}],
       }))
    .expect("summary message");
    messages.push(first);
    if rest.first().is_some_and(|message| message.role == "user") {
        // The block sat in a user message: merge it into the summary turn.
        let merged = rest.remove(0);
        if let (Some(target), Some(source)) = (
            messages[0].content.as_array_mut(),
            merged.content.as_array(),
        ) {
            target.extend(source.iter().cloned());
        } else if let Some(target) = messages[0].content.as_array_mut() {
            target.push(serde_json::json!({"type": "text", "text": merged.content.as_str().unwrap_or_default()}));
        }
    }
    messages.extend(rest);
    true
}

/// 把待摘要的消息渲染成文字记录，并按渲染后的长度切成若干段（每段不超过
/// `SUMMARY_CHUNK_TOKENS`），用于滚动摘要。按渲染后的长度切：工具结果在记录里已经截短，按原始
/// token 切会多出不必要的分段，每多一段就多一次串行的模型调用。
pub(crate) fn transcript_chunks(messages: &[Message], _tokens: &[i32]) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0i64;
    for message in messages {
        let rendered = render_transcript(std::slice::from_ref(message));
        if rendered.trim().is_empty() {
            continue;
        }
        let tokens = crate::token::count_tokens(&rendered).min(i32::MAX as u64) as i64;
        if !current.is_empty() && current_tokens + tokens > i64::from(SUMMARY_CHUNK_TOKENS) {
            chunks.push(std::mem::take(&mut current));
            current_tokens = 0;
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(&rendered);
        current_tokens += tokens;
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

pub(crate) fn clamp_summary(summary: &str) -> Option<String> {
    let summary = summary.trim();
    (!summary.is_empty()).then(|| truncate_middle_chars(summary, SUMMARY_MAX_CHARS))
}

// ===================== 检查点表 =====================

const STORE_MAX_ENTRIES: usize = 2_048;
const STORE_MAX_BYTES: usize = 32 * 1024 * 1024;
const FAILURE_BACKOFF: Duration = Duration::from_secs(60);

pub(crate) type SummaryFuture = Shared<BoxFuture<'static, Option<String>>>;

#[derive(Default)]
struct Store {
    summaries: HashMap<Key, (String, u64)>,
    bytes: usize,
    tick: u64,
    in_flight: HashMap<Key, SummaryFuture>,
    failures: HashMap<Key, Instant>,
}

impl Store {
    fn insert(&mut self, key: Key, summary: String) {
        self.tick += 1;
        self.bytes += summary.len();
        if let Some((old, _)) = self.summaries.insert(key, (summary, self.tick)) {
            self.bytes -= old.len();
        }
        while self.summaries.len() > STORE_MAX_ENTRIES || self.bytes > STORE_MAX_BYTES {
            let Some(oldest) = self
                .summaries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some((removed, _)) = self.summaries.remove(&oldest) {
                self.bytes -= removed.len();
            }
        }
    }

    fn get(&mut self, key: &Key) -> Option<String> {
        self.tick += 1;
        let tick = self.tick;
        self.summaries.get_mut(key).map(|(summary, used)| {
            *used = tick;
            summary.clone()
        })
    }
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

/// 最近的已有检查点：从后往前找第一个有摘要的前缀。
pub(crate) fn latest_checkpoint(messages: &[Message], hashes: &[Key]) -> Option<Checkpoint> {
    let mut store = store().lock();
    (1..hashes.len()).rev().find_map(|index| {
        let cut = index + 1;
        if !valid_cut(messages, cut) {
            return None;
        }
        store
            .get(&hashes[index])
            .map(|summary| Checkpoint { cut, summary })
    })
}

/// 为 `key` 生成摘要：已经在生成就加入，最近失败过就返回 `None`，否则用 `make` 开始生成。
/// 生成在独立任务里完成并写入检查点表，请求方等不到也不影响它完成。
pub(crate) fn start_or_join(
    key: Key,
    make: impl FnOnce() -> BoxFuture<'static, Option<String>>,
) -> Option<SummaryFuture> {
    let mut guard = store().lock();
    if let Some(existing) = guard.in_flight.get(&key) {
        return Some(existing.clone());
    }
    if guard.summaries.contains_key(&key) {
        let summary = guard.get(&key);
        return Some(futures::future::ready(summary).boxed().shared());
    }
    if guard
        .failures
        .get(&key)
        .is_some_and(|failed_at| failed_at.elapsed() < FAILURE_BACKOFF)
    {
        return None;
    }
    guard
        .failures
        .retain(|_, failed_at| failed_at.elapsed() < FAILURE_BACKOFF);
    let future = make().shared();
    guard.in_flight.insert(key, future.clone());
    drop(guard);
    let driver = future.clone();
    tokio::spawn(async move {
        let result = driver.await;
        let mut guard = store().lock();
        guard.in_flight.remove(&key);
        match result {
            Some(summary) => guard.insert(key, summary),
            None => {
                guard.failures.insert(key, Instant::now());
            }
        }
    });
    Some(future)
}

#[cfg(test)]
pub(crate) fn clear_store_for_tests() {
    *store().lock() = Store::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(role: &str, content: Value) -> Message {
        serde_json::from_value(json!({"role": role, "content": content})).unwrap()
    }

    /// m0 user task, then `turns` rounds of assistant tool_use + user tool_result, then a final
    /// assistant answer and the current user turn.
    fn conversation(turns: usize, result_words: usize) -> Vec<Message> {
        let mut messages = vec![message(
            "user",
            json!("Refactor the parser and keep the public API."),
        )];
        for i in 0..turns {
            messages.push(message("assistant", json!([
         {"type": "text", "text": format!("Step {i}: reading the file.")},
         {"type": "tool_use", "id": format!("toolu_{i}"), "name": "Read", "input": {"path": format!("src/f{i}.rs")}}
     ])));
            messages.push(message("user", json!([
  {"type": "tool_result", "tool_use_id": format!("toolu_{i}"), "content": "word ".repeat(result_words)}
     ])));
        }
        messages.push(message("assistant", json!("All files read.")));
        messages.push(message("user", json!("Now update the tests.")));
        messages
    }

    #[test]
    fn prefix_hashes_ignore_rewritten_tool_results_and_cache_control() {
        let original = conversation(4, 50);
        let mut rewritten = original.clone();
        // Claude Code microcompaction replaces old tool results and moves cache_control.
        rewritten[2].content = json!([
            {"type": "tool_result", "tool_use_id": "toolu_0", "content": "[Old tool result content cleared]", "cache_control": {"type": "ephemeral"}}
        ]);
        let a = prefix_hashes(&original);
        let b = prefix_hashes(&rewritten);
        assert_eq!(a, b);
        // A different user instruction changes every later hash.
        let mut edited = original.clone();
        edited[0].content = json!("Refactor the lexer instead.");
        let c = prefix_hashes(&edited);
        assert!(a.iter().zip(&c).all(|(x, y)| x != y));
    }

    #[test]
    fn cuts_land_on_assistant_messages_that_follow_a_user_message() {
        let messages = conversation(10, 200);
        let tokens = message_tokens(&messages);
        let tail_budget = tokens[tokens.len() - 6..].iter().sum::<i32>();
        let cut = next_cut(&messages, &tokens, tail_budget, 0).expect("cut");
        assert_eq!(messages[cut].role, "assistant");
        assert_eq!(messages[cut - 1].role, "user");
        assert!(cut >= messages.len() - 6);
        assert!(cut <= messages.len() - 2);
        // Checkpoints only move forward.
        assert!(next_cut(&messages, &tokens, tail_budget, messages.len() - 2).is_none());
    }

    #[test]
    fn checkpoint_keeps_the_task_and_pairs_tool_calls() {
        let messages = conversation(6, 50);
        let checkpoint = Checkpoint {
            cut: 7,
            summary: "Read f0..f2; the parser keeps its public API.".to_string(),
        };
        let applied = apply_checkpoint(&messages, &checkpoint);
        assert_eq!(applied.len(), messages.len() - 6);
        assert_eq!(applied[0].role, "user");
        let first = applied[0].content.to_string();
        assert!(first.contains("Refactor the parser"));
        assert!(first.contains("the parser keeps its public API"));
        assert_eq!(applied[1].role, "assistant");
        // Every tool_result still follows its tool_use.
        let mut open = Vec::new();
        for message in &applied {
            for block in message.content.as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("tool_use") => open.push(block["id"].as_str().unwrap().to_string()),
                    Some("tool_result") => {
                        let id = block["tool_use_id"].as_str().unwrap();
                        assert!(open.iter().any(|o| o == id), "{id} without its tool_use");
                    }
                    _ => {}
                }
            }
        }
        // The same checkpoint gives the same upstream prefix on the next, longer request.
        let mut longer = messages.clone();
        longer.push(message("assistant", json!("Tests updated.")));
        longer.push(message("user", json!("Run them.")));
        let applied_longer = apply_checkpoint(&longer, &checkpoint);
        assert_eq!(
            serde_json::to_value(&applied_longer[..applied.len()]).unwrap(),
            serde_json::to_value(&applied[..]).unwrap()
        );
    }

    #[test]
    fn transcript_keeps_requests_and_bounds_tool_output() {
        let messages = conversation(2, 5_000);
        let transcript = render_transcript(&messages);
        assert!(transcript.contains("[user] Refactor the parser and keep the public API."));
        assert!(transcript.contains("[assistant -> tool Read]"));
        assert!(transcript.contains("characters omitted"));
        assert!(transcript.len() < 20_000);
        let prompt = summary_prompt(Some("Earlier record."), &transcript);
        assert!(prompt.contains("<previous_record>\nEarlier record."));
        let request = summary_request("claude-sonnet-4-5", None, &transcript, None);
        assert!(request.tools.is_none());
        assert!(request.thinking.is_none());
        assert!(!request.stream);
    }

    #[test]
    fn long_spans_are_summarized_in_chunks() {
        // Tool results are shortened in the transcript, so a tool-heavy span stays one chunk.
        let messages = conversation(30, 6_000);
        let tokens = message_tokens(&messages);
        assert_eq!(transcript_chunks(&messages, &tokens).len(), 1);
        // Long user text is kept as written, so it is split into several chunks.
        let long_text: Vec<Message> = (0..12)
            .flat_map(|i| {
                [
                    message(
                        "user",
                        json!(format!("part {i}: {}", "word ".repeat(30_000))),
                    ),
                    message("assistant", json!("ok")),
                ]
            })
            .collect();
        let tokens = message_tokens(&long_text);
        let chunks = transcript_chunks(&long_text, &tokens);
        assert!(chunks.len() >= 2, "{} chunk(s)", chunks.len());
    }

    #[tokio::test]
    async fn store_shares_one_summary_per_prefix_and_backs_off_after_failure() {
        clear_store_for_tests();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let key = [7u8; 32];
        let make = |calls: std::sync::Arc<std::sync::atomic::AtomicUsize>| {
            move || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Some("record".to_string()) }.boxed()
            }
        };
        let first = start_or_join(key, make(calls.clone())).unwrap();
        let second = start_or_join(key, make(calls.clone())).unwrap();
        assert_eq!(first.await.as_deref(), Some("record"));
        assert_eq!(second.await.as_deref(), Some("record"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let failing = [8u8; 32];
        let failed = start_or_join(failing, || async { None }.boxed()).unwrap();
        assert_eq!(failed.await, None);
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(start_or_join(failing, make(calls.clone())).is_none());
    }
}

/// 顶层 `compaction` 参数（按需压缩）：返回客户端给的压缩要求，没有要求时为空串；没有这个参数时
/// 返回 `None`。
pub(crate) fn on_demand_compaction_instructions(raw_body: &[u8]) -> Option<String> {
    // Cheap guard: only bodies that mention the field are parsed.
    if !raw_body
        .windows(12)
        .any(|window| window == b"\"compaction\"")
    {
        return None;
    }
    let value: Value = serde_json::from_slice(raw_body).ok()?;
    let compaction = value.get("compaction")?;
    if compaction.get("type").and_then(Value::as_str) != Some("summarize") {
        return None;
    }
    Some(
        compaction
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

#[cfg(test)]
mod compaction_tests {
    use super::*;
    use serde_json::json;

    fn message(role: &str, content: Value) -> Message {
        serde_json::from_value(json!({"role": role, "content": content})).unwrap()
    }

    #[test]
    fn client_compaction_block_replaces_everything_before_it() {
        let mut messages = vec![
            message("user", json!("Build the CLI.")),
            message("assistant", json!("Working on it.")),
            message("user", json!("Add tests.")),
            message(
                "assistant",
                json!([
                {"type": "compaction", "content": "Built the CLI; tests are next."},
                {"type": "text", "text": "Continuing with tests."}
                   ]),
            ),
            message("user", json!("Run them.")),
        ];
        assert!(apply_client_compaction_block(&mut messages));
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "user");
        assert!(
            messages[0]
                .content
                .to_string()
                .contains("Built the CLI; tests are next.")
        );
        assert!(!messages[0].content.to_string().contains("Build the CLI."));
        assert_eq!(messages[1].role, "assistant");
        assert!(!messages[1].content.to_string().contains("compaction"));
        assert_eq!(messages[2].role, "user");

        let mut plain = vec![message("user", json!("hi"))];
        assert!(!apply_client_compaction_block(&mut plain));
    }

    #[test]
    fn on_demand_compaction_is_detected_from_the_raw_body() {
        let body = br#"{"model":"m","compaction":{"type":"summarize","instructions":"keep file names"},"messages":[]}"#;
        assert_eq!(
            on_demand_compaction_instructions(body).as_deref(),
            Some("keep file names")
        );
        let body = br#"{"model":"m","messages":[{"role":"user","content":"what is compaction?"}]}"#;
        assert_eq!(on_demand_compaction_instructions(body), None);
    }
}

#[cfg(test)]
mod salient_tests {
    #[test]
    fn checkpoint_keeps_tool_findings_word_for_word() {
        let chunk = "[assistant] Reading file 4.\n[tool result] fn main() {}\n NOTE: The root cause of the crash was a race in the cache_evict worker.\n[notable lines] fn x() | NOTE: The root cause of the crash was a race in the cache_evict worker".to_string();
        let record = super::with_verbatim_findings(
            "# Working Record\nRead files.".to_string(),
            std::slice::from_ref(&chunk),
        );
        assert!(
            record.contains("## Notes from tool output (verbatim)"),
            "{record}"
        );
        assert_eq!(record.matches("cache_evict").count(), 1, "{record}");
        // Already quoted by the record: unchanged.
        let quoted =
            "Record: NOTE: The root cause of the crash was a race in the cache_evict worker."
                .to_string();
        assert!(!super::with_verbatim_findings(quoted.clone(), &[chunk]).contains("verbatim"));
        // A user turn mentioning a note is not tool output.
        let plain =
            super::with_verbatim_findings("r".to_string(), &["[user] note: be brief".to_string()]);
        assert_eq!(plain, "r");
    }

    #[test]
    fn explicit_findings_win_over_incidental_markers() {
        let mut text = String::new();
        for line in 0..40 {
            text.push_str(&format!(
                "let value_{line} = parse().map_err(|error| error)?;\n"
            ));
        }
        text.push_str(" NOTE: The root cause of the crash was a race in the cache_evict worker.");
        let picked = super::salient_lines(&text);
        assert!(picked.contains("cache_evict"), "{picked}");
        assert!(picked.ends_with("cache_evict worker"), "{picked}");
    }

    use super::*;

    #[test]
    fn findings_in_the_middle_of_long_tool_results_survive_the_transcript() {
        let mut body = "lorem ipsum dolor ".repeat(400);
        body.push_str("NOTE: The root cause of the crash was a race in the cache_evict worker. ");
        body.push_str(&"sit amet consectetur ".repeat(400));
        let messages: Vec<Message> = serde_json::from_value(serde_json::json!([
     {"role": "user", "content": "go"},
     {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Read", "input": {}}]},
     {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": body}]}
        ]))
 .unwrap();
        let transcript = render_transcript(&messages);
        assert!(transcript.contains("cache_evict"), "{transcript}");
        assert!(transcript.len() < 8_000);
    }
}
