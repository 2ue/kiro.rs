//! 清洗模型在正文中复述的代理注入控制块。
//!
//! prompt 兼容模式下，代理会在上游请求里注入 `<thinking_mode>` / `<thinking_effort>` /
//! `<max_thinking_length>` / `<thinking_output_policy>` 等控制提示。用户请求含 `<thinking>`
//! 的模板时，模型可能把这些控制块原样写进正文。这里只移除与注入内容逐字一致的完整块：
//! 标签名、取值和策略正文都必须与代理生成的模板相同，其余同名文字一律保留。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde_json::{Value, json};

use super::stream::SseEvent;
use super::types::{MessagesRequest, THINKING_EFFORT_VALUES};

const CONTROL_TAG_NAMES: &[&str] = &[
    "<thinking_mode>",
    "<thinking_effort>",
    "<max_thinking_length>",
    "<thinking_output_policy>",
];
const MAX_LENGTH_OPEN: &str = "<max_thinking_length>";
const MAX_LENGTH_CLOSE: &str = "</max_thinking_length>";
/// `max_thinking_length` 取值为 i32 预算，最多 10 位数字。
const MAX_LENGTH_DIGITS: usize = 10;

/// 代理可能注入的全部固定控制块（逐字）。
fn literal_blocks() -> &'static [String] {
    static BLOCKS: OnceLock<Vec<String>> = OnceLock::new();
    BLOCKS.get_or_init(|| {
        let mut blocks = vec![
            "<thinking_mode>enabled</thinking_mode>".to_string(),
            "<thinking_mode>adaptive</thinking_mode>".to_string(),
            super::converter::THINKING_OUTPUT_POLICY.to_string(),
        ];
        blocks.extend(
            THINKING_EFFORT_VALUES
                .iter()
                .map(|effort| format!("<thinking_effort>{effort}</thinking_effort>")),
        );
        blocks
    })
}

/// 客户端自己的 system / user 内容里已经出现这些标签时，说明用户在正常使用同名文字，
/// 此时不做清洗，避免误删。
pub(crate) fn request_mentions_control_tags(req: &MessagesRequest) -> bool {
    let mentions = |text: &str| CONTROL_TAG_NAMES.iter().any(|tag| text.contains(tag));
    let system = req
        .system
        .iter()
        .flatten()
        .any(|message| mentions(&message.text));
    system
        || req
            .messages
            .iter()
            .filter(|message| message.role == "user")
            .any(|message| value_mentions(&message.content, &mentions))
}

fn value_mentions(value: &Value, mentions: &impl Fn(&str) -> bool) -> bool {
    match value {
        Value::String(text) => mentions(text),
        Value::Array(items) => items.iter().any(|item| value_mentions(item, mentions)),
        Value::Object(map) => map.values().any(|item| value_mentions(item, mentions)),
        _ => false,
    }
}

/// 在 `text[from..]` 中查找 `<max_thinking_length>数字</max_thinking_length>` 完整块。
fn find_max_length_block(text: &str) -> Option<(usize, usize)> {
    let mut search = 0;
    while let Some(rel) = text[search..].find(MAX_LENGTH_OPEN) {
        let start = search + rel;
        let digits_start = start + MAX_LENGTH_OPEN.len();
        let digits = text[digits_start..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let close_start = digits_start + digits;
        if (1..=MAX_LENGTH_DIGITS).contains(&digits)
            && text[close_start..].starts_with(MAX_LENGTH_CLOSE)
        {
            return Some((start, close_start + MAX_LENGTH_CLOSE.len()));
        }
        search = digits_start;
    }
    None
}

/// 最早出现的完整控制块（起止字节）。
fn find_control_block(text: &str) -> Option<(usize, usize)> {
    literal_blocks()
        .iter()
        .filter_map(|block| {
            text.find(block.as_str())
                .map(|pos| (pos, pos + block.len()))
        })
        .chain(find_max_length_block(text))
        .min_by_key(|(start, _)| *start)
}

/// 移除全部完整控制块；块后紧跟的一个换行一并移除，避免留下空行。
pub(crate) fn strip_control_blocks(text: &str) -> Cow<'_, str> {
    strip_control_blocks_with_tail(text).0
}

/// 同 [`strip_control_blocks`]，额外返回"文本恰好以被移除的控制块结尾"，
/// 流式时据此吞掉下一个 chunk 开头的换行。
fn strip_control_blocks_with_tail(text: &str) -> (Cow<'_, str>, bool) {
    if !CONTROL_TAG_NAMES.iter().any(|tag| text.contains(tag)) {
        return (Cow::Borrowed(text), false);
    }
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    let mut removed = false;
    let mut ended_at_block = false;
    while let Some((start, end)) = find_control_block(rest) {
        output.push_str(&rest[..start]);
        rest = &rest[end..];
        ended_at_block = rest.is_empty();
        rest = rest.strip_prefix('\n').unwrap_or(rest);
        removed = true;
    }
    if !removed {
        return (Cow::Borrowed(text), false);
    }
    ended_at_block = ended_at_block && rest.is_empty();
    output.push_str(rest);
    (Cow::Owned(output), ended_at_block)
}

/// `candidate`（以 `<` 开头）是否可能是某个控制块的未完成前缀。
fn is_partial_control_block(candidate: &str) -> bool {
    if literal_blocks()
        .iter()
        .any(|block| block.len() > candidate.len() && block.starts_with(candidate))
    {
        return true;
    }
    if MAX_LENGTH_OPEN.starts_with(candidate) {
        return true;
    }
    let Some(after_open) = candidate.strip_prefix(MAX_LENGTH_OPEN) else {
        return false;
    };
    let digits = after_open.bytes().take_while(u8::is_ascii_digit).count();
    let tail = &after_open[digits..];
    digits <= MAX_LENGTH_DIGITS
        && tail.len() < MAX_LENGTH_CLOSE.len()
        && MAX_LENGTH_CLOSE.starts_with(tail)
}

/// 需要暂缓下发的尾部起点：从最早一个可能构成控制块前缀的 `<` 开始。
fn held_start(text: &str) -> Option<usize> {
    let longest = literal_blocks()
        .iter()
        .map(String::len)
        .max()
        .unwrap_or(0)
        .max(MAX_LENGTH_OPEN.len() + MAX_LENGTH_DIGITS + MAX_LENGTH_CLOSE.len());
    let window_start = text.len().saturating_sub(longest);
    text.char_indices()
        .filter(|(pos, ch)| *pos >= window_start && *ch == '<')
        .map(|(pos, _)| pos)
        .find(|pos| is_partial_control_block(&text[*pos..]))
}

/// 流式清洗器：跨 chunk 识别控制块，只作用于 text 块。
#[derive(Debug, Default)]
pub(crate) struct ControlBlockCleaner {
    text_blocks: HashSet<i32>,
    pending: HashMap<i32, String>,
    /// 上一段以被移除的控制块结尾，下一段开头的换行一并移除。
    skip_leading_newline: HashSet<i32>,
}

impl ControlBlockCleaner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn clean(&mut self, index: i32, text: &str) -> String {
        match strip_control_blocks_with_tail(text) {
            (Cow::Borrowed(text), _) => text.to_string(),
            (Cow::Owned(cleaned), ended_at_block) => {
                if ended_at_block {
                    self.skip_leading_newline.insert(index);
                }
                tracing::warn!("removed proxy-injected control prompt echoed in assistant text");
                cleaned
            }
        }
    }

    pub(crate) fn apply(&mut self, events: Vec<SseEvent>) -> Vec<SseEvent> {
        let mut output = Vec::with_capacity(events.len());
        for mut event in events {
            let index = event
                .data
                .get("index")
                .and_then(Value::as_i64)
                .map(|index| index as i32);
            match (event.event.as_str(), index) {
                ("content_block_start", Some(index)) => {
                    if event.data["content_block"]["type"] == "text" {
                        self.text_blocks.insert(index);
                    }
                    output.push(event);
                }
                ("content_block_delta", Some(index))
                    if self.text_blocks.contains(&index)
                        && event.data["delta"]["type"] == "text_delta" =>
                {
                    let text = event.data["delta"]["text"].as_str().unwrap_or_default();
                    let mut combined = self.pending.remove(&index).unwrap_or_default();
                    combined.push_str(text);
                    if combined.is_empty() {
                        output.push(event);
                        continue;
                    }
                    if self.skip_leading_newline.remove(&index) && combined.starts_with('\n') {
                        combined.remove(0);
                    }
                    let mut cleaned = self.clean(index, &combined);
                    if let Some(start) = held_start(&cleaned) {
                        self.pending.insert(index, cleaned[start..].to_string());
                        cleaned.truncate(start);
                    }
                    if !cleaned.is_empty() {
                        event.data["delta"]["text"] = json!(cleaned);
                        output.push(event);
                    }
                }
                ("content_block_stop", Some(index)) => {
                    if let Some(pending) = self.pending.remove(&index).filter(|p| !p.is_empty()) {
                        let pending = self.clean(index, &pending);
                        if !pending.is_empty() {
                            output.push(SseEvent::new(
                                "content_block_delta",
                                json!({
                                    "type": "content_block_delta",
                                    "index": index,
                                    "delta": { "type": "text_delta", "text": pending }
                                }),
                            ));
                        }
                    }
                    output.push(event);
                }
                _ => output.push(event),
            }
        }
        output
    }
}

/// 非流式：清洗 content 中 text 块。
pub(crate) fn strip_control_blocks_in_content(content: &mut Vec<Value>) {
    for block in content.iter_mut() {
        if block["type"] != "text" {
            continue;
        }
        let Some(text) = block["text"].as_str() else {
            continue;
        };
        if let Cow::Owned(cleaned) = strip_control_blocks(text) {
            tracing::warn!("removed proxy-injected control prompt echoed in assistant text");
            block["text"] = json!(cleaned);
        }
    }
    content.retain(|block| block["type"] != "text" || block["text"].as_str() != Some(""));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::converter::THINKING_OUTPUT_POLICY;

    fn injected_prefix() -> String {
        format!(
            "<thinking_mode>adaptive</thinking_mode><thinking_effort>high</thinking_effort>\n{THINKING_OUTPUT_POLICY}"
        )
    }

    #[test]
    fn removes_exact_injected_blocks() {
        let text = format!(
            "Here is the template:\n{}\n<thinking>\nplan\n</thinking>\nAnswer",
            injected_prefix()
        );
        let cleaned = strip_control_blocks(&text);
        assert_eq!(
            cleaned,
            "Here is the template:\n<thinking>\nplan\n</thinking>\nAnswer"
        );

        let enabled = "<thinking_mode>enabled</thinking_mode><max_thinking_length>20000</max_thinking_length>\nok";
        assert_eq!(strip_control_blocks(enabled), "ok");
    }

    #[test]
    fn keeps_same_named_text_that_differs_from_injection() {
        for text in [
            "The `<thinking_mode>` tag toggles reasoning.",
            "<thinking_mode>custom</thinking_mode>",
            "<thinking_effort>extreme</thinking_effort>",
            "<max_thinking_length>abc</max_thinking_length>",
            "<max_thinking_length>12345678901</max_thinking_length>",
            "<thinking_output_policy>my own policy</thinking_output_policy>",
            "<thinking>normal model thinking tag</thinking>",
        ] {
            assert!(
                matches!(strip_control_blocks(text), Cow::Borrowed(_)),
                "{text}"
            );
        }
    }

    #[test]
    fn detects_control_tags_in_client_request() {
        let mut req: MessagesRequest = serde_json::from_value(json!({
            "model": "claude-sonnet-4",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]
        }))
        .unwrap();
        assert!(!request_mentions_control_tags(&req));
        req.messages[0].content =
            json!([{"type": "text", "text": "explain <thinking_mode>enabled</thinking_mode>"}]);
        assert!(request_mentions_control_tags(&req));
        // assistant 历史里的泄漏不应关闭清洗。
        req.messages[0].role = "assistant".to_string();
        assert!(!request_mentions_control_tags(&req));
    }

    fn delta(text: &str) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        )
    }

    fn collect_text(events: &[SseEvent]) -> String {
        events
            .iter()
            .filter_map(|event| event.data["delta"]["text"].as_str())
            .collect()
    }

    #[test]
    fn streaming_removes_blocks_split_across_chunks() {
        let text = format!(
            "Template:\n{}\n<thinking>x</thinking> done",
            injected_prefix()
        );
        let mut cleaner = ControlBlockCleaner::new();
        let mut out = cleaner.apply(vec![SseEvent::new(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        )]);
        let chars: Vec<char> = text.chars().collect();
        for chunk in chars.chunks(7) {
            out.extend(cleaner.apply(vec![delta(&chunk.iter().collect::<String>())]));
        }
        out.extend(cleaner.apply(vec![SseEvent::new(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        )]));
        assert_eq!(collect_text(&out), "Template:\n<thinking>x</thinking> done");
    }

    #[test]
    fn streaming_releases_held_text_that_is_not_a_control_block() {
        let mut cleaner = ControlBlockCleaner::new();
        let mut out = cleaner.apply(vec![
            SseEvent::new(
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ),
            delta("see <thinking_mo"),
        ]);
        assert_eq!(collect_text(&out), "see ");
        out.extend(cleaner.apply(vec![delta("de>custom</thinking_mode> end")]));
        out.extend(cleaner.apply(vec![SseEvent::new(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        )]));
        assert_eq!(
            collect_text(&out),
            "see <thinking_mode>custom</thinking_mode> end"
        );
    }

    #[test]
    fn non_stream_content_drops_blocks_that_become_empty() {
        let mut content = vec![
            json!({"type": "text", "text": injected_prefix()}),
            json!({"type": "text", "text": "answer"}),
        ];
        strip_control_blocks_in_content(&mut content);
        assert_eq!(content, vec![json!({"type": "text", "text": "answer"})]);
    }
}
