//! `stop_sequences` 的服务端实现。
//!
//! Kiro 上游没有停止序列参数，因此由代理在输出侧检测：一旦 text 块中出现任一
//! stop sequence，就截断到它之前、停止继续下发后续内容，并以
//! `stop_reason: "stop_sequence"` / `stop_sequence: <命中值>` 结束。
//! 只作用于 text 块；thinking 与 tool_use 不参与匹配。

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::stream::SseEvent;

pub(crate) const STOP_SEQUENCE_STOP_REASON: &str = "stop_sequence";

/// 规范化请求中的 stop_sequences：去掉空串与重复项，保持原顺序。
pub(crate) fn normalize_stop_sequences(sequences: Option<&[String]>) -> Vec<String> {
    let mut seen = HashSet::new();
    sequences
        .unwrap_or_default()
        .iter()
        .filter(|sequence| !sequence.is_empty())
        .filter(|sequence| seen.insert(sequence.as_str()))
        .cloned()
        .collect()
}

/// 返回 `text` 中最早出现的 stop sequence（字节位置, 命中值）。
fn earliest_match<'a>(text: &str, sequences: &'a [String]) -> Option<(usize, &'a str)> {
    sequences
        .iter()
        .filter_map(|sequence| {
            text.find(sequence.as_str())
                .map(|pos| (pos, sequence.as_str()))
        })
        .min_by(|(a_pos, a_seq), (b_pos, b_seq)| {
            a_pos.cmp(b_pos).then_with(|| b_seq.len().cmp(&a_seq.len()))
        })
}

/// `text` 末尾可能是某个 stop sequence 前缀的最长字节长度（不含完整命中）。
fn held_suffix_len(text: &str, sequences: &[String]) -> usize {
    sequences
        .iter()
        .filter_map(|sequence| {
            (1..sequence.len())
                .rev()
                .filter(|len| sequence.is_char_boundary(*len))
                .find(|len| text.ends_with(&sequence[..*len]))
        })
        .max()
        .unwrap_or(0)
}

/// 流式 stop sequence 过滤器，作用于 StreamContext 产出的 SSE 事件序列。
#[derive(Debug, Default)]
pub(crate) struct StopSequenceFilter {
    sequences: Vec<String>,
    /// 已向下游发出 content_block_start 的块。
    forwarded_blocks: HashSet<i32>,
    /// 其中的 text 块。
    text_blocks: HashSet<i32>,
    /// 可能构成 stop sequence 前缀、暂缓下发的文本。
    pending: HashMap<i32, String>,
    matched: Option<String>,
}

impl StopSequenceFilter {
    pub(crate) fn new(sequences: Vec<String>) -> Option<Self> {
        (!sequences.is_empty()).then(|| Self {
            sequences,
            ..Self::default()
        })
    }

    pub(crate) fn matched(&self) -> Option<&str> {
        self.matched.as_deref()
    }

    pub(crate) fn apply(&mut self, events: Vec<SseEvent>) -> Vec<SseEvent> {
        let mut output = Vec::with_capacity(events.len());
        for event in events {
            self.apply_one(event, &mut output);
        }
        output
    }

    fn apply_one(&mut self, mut event: SseEvent, output: &mut Vec<SseEvent>) {
        let index = event
            .data
            .get("index")
            .and_then(Value::as_i64)
            .map(|i| i as i32);
        match event.event.as_str() {
            "content_block_start" => {
                if self.matched.is_some() {
                    return;
                }
                if let Some(index) = index {
                    self.forwarded_blocks.insert(index);
                    if event.data["content_block"]["type"] == "text" {
                        self.text_blocks.insert(index);
                    }
                }
                output.push(event);
            }
            "content_block_delta" => {
                if self.matched.is_some() {
                    return;
                }
                let Some(index) = index else {
                    output.push(event);
                    return;
                };
                if !self.text_blocks.contains(&index) || event.data["delta"]["type"] != "text_delta"
                {
                    output.push(event);
                    return;
                }
                let text = event.data["delta"]["text"].as_str().unwrap_or_default();
                let mut combined = self.pending.remove(&index).unwrap_or_default();
                combined.push_str(text);
                if combined.is_empty() {
                    // 保活用的空 delta 原样透传。
                    output.push(event);
                    return;
                }
                let visible =
                    if let Some((pos, sequence)) = earliest_match(&combined, &self.sequences) {
                        tracing::debug!(
                            stop_sequence = sequence,
                            "stop sequence matched in streamed text; truncating output"
                        );
                        self.matched = Some(sequence.to_string());
                        self.pending.clear();
                        combined.truncate(pos);
                        combined
                    } else {
                        let held = held_suffix_len(&combined, &self.sequences);
                        let split = combined.len() - held;
                        if held > 0 {
                            self.pending.insert(index, combined[split..].to_string());
                        }
                        combined.truncate(split);
                        combined
                    };
                if !visible.is_empty() {
                    event.data["delta"]["text"] = json!(visible);
                    output.push(event);
                }
            }
            "content_block_stop" => {
                let Some(index) = index else {
                    output.push(event);
                    return;
                };
                if !self.forwarded_blocks.contains(&index) {
                    return;
                }
                if let Some(pending) = self.pending.remove(&index).filter(|p| !p.is_empty()) {
                    output.push(SseEvent::new(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "text_delta", "text": pending }
                        }),
                    ));
                }
                output.push(event);
            }
            "message_delta" => {
                if let Some(sequence) = &self.matched {
                    event.data["delta"]["stop_reason"] = json!(STOP_SEQUENCE_STOP_REASON);
                    event.data["delta"]["stop_sequence"] = json!(sequence);
                }
                output.push(event);
            }
            _ => output.push(event),
        }
    }
}

/// 非流式：在完整 content 中查找 stop sequence。命中时截断该 text 块，
/// 丢弃其后的全部块，返回命中值。
pub(crate) fn truncate_content_at_stop_sequence(
    content: &mut Vec<Value>,
    sequences: &[String],
) -> Option<String> {
    if sequences.is_empty() {
        return None;
    }
    let (block_index, pos, sequence) = content.iter().enumerate().find_map(|(idx, block)| {
        if block["type"] != "text" {
            return None;
        }
        let text = block["text"].as_str()?;
        earliest_match(text, sequences).map(|(pos, sequence)| (idx, pos, sequence.to_string()))
    })?;
    let text = content[block_index]["text"]
        .as_str()
        .map(|text| text[..pos].to_string())
        .unwrap_or_default();
    content.truncate(block_index + 1);
    if text.is_empty() {
        content.pop();
    } else {
        content[block_index]["text"] = json!(text);
    }
    Some(sequence)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seqs(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn start(index: i32, block_type: &str) -> SseEvent {
        SseEvent::new(
            "content_block_start",
            json!({"type": "content_block_start", "index": index, "content_block": {"type": block_type}}),
        )
    }

    fn text_delta(index: i32, text: &str) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": text}}),
        )
    }

    fn thinking_delta(index: i32, text: &str) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index, "delta": {"type": "thinking_delta", "thinking": text}}),
        )
    }

    fn stop(index: i32) -> SseEvent {
        SseEvent::new(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        )
    }

    fn message_delta() -> SseEvent {
        SseEvent::new(
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 5}}),
        )
    }

    fn visible_text(events: &[SseEvent]) -> String {
        events
            .iter()
            .filter(|event| event.data["delta"]["type"] == "text_delta")
            .filter_map(|event| event.data["delta"]["text"].as_str())
            .collect()
    }

    #[test]
    fn normalize_drops_empty_and_duplicates() {
        let input = seqs(&["", "END", "END", "STOP"]);
        assert_eq!(
            normalize_stop_sequences(Some(&input)),
            seqs(&["END", "STOP"])
        );
        assert!(normalize_stop_sequences(None).is_empty());
        assert!(StopSequenceFilter::new(Vec::new()).is_none());
    }

    #[test]
    fn truncates_single_chunk_and_rewrites_message_delta() {
        let mut filter = StopSequenceFilter::new(seqs(&["STOP"])).unwrap();
        let mut out = filter.apply(vec![start(0, "text"), text_delta(0, "hello STOP world")]);
        out.extend(filter.apply(vec![text_delta(0, "more"), stop(0), message_delta()]));

        assert_eq!(visible_text(&out), "hello ");
        let delta = out.iter().find(|e| e.event == "message_delta").unwrap();
        assert_eq!(delta.data["delta"]["stop_reason"], "stop_sequence");
        assert_eq!(delta.data["delta"]["stop_sequence"], "STOP");
        assert_eq!(delta.data["usage"]["output_tokens"], 5);
        assert_eq!(
            out.iter()
                .filter(|e| e.event == "content_block_stop")
                .count(),
            1
        );
    }

    #[test]
    fn matches_across_chunks_and_holds_prefix() {
        let mut filter = StopSequenceFilter::new(seqs(&["</answer>"])).unwrap();
        let first = filter.apply(vec![start(0, "text"), text_delta(0, "abc</ans")]);
        assert_eq!(visible_text(&first), "abc");
        let second = filter.apply(vec![text_delta(0, "wer>tail"), stop(0), message_delta()]);
        assert_eq!(visible_text(&second), "");
        let delta = second.iter().find(|e| e.event == "message_delta").unwrap();
        assert_eq!(delta.data["delta"]["stop_sequence"], "</answer>");
    }

    #[test]
    fn releases_held_prefix_when_it_does_not_complete() {
        let mut filter = StopSequenceFilter::new(seqs(&["STOP"])).unwrap();
        let mut out = filter.apply(vec![start(0, "text"), text_delta(0, "go ST")]);
        assert_eq!(visible_text(&out), "go ");
        out.extend(filter.apply(vec![text_delta(0, "ART now ST")]));
        out.extend(filter.apply(vec![stop(0), message_delta()]));
        assert_eq!(visible_text(&out), "go START now ST");
        let delta = out.iter().find(|e| e.event == "message_delta").unwrap();
        assert_eq!(delta.data["delta"]["stop_reason"], "end_turn");
        assert!(filter.matched().is_none());
    }

    #[test]
    fn ignores_thinking_and_suppresses_blocks_after_match() {
        let mut filter = StopSequenceFilter::new(seqs(&["STOP"])).unwrap();
        let out = filter.apply(vec![
            start(0, "thinking"),
            thinking_delta(0, "STOP in thinking"),
            stop(0),
            start(1, "text"),
            text_delta(1, "answer STOP"),
            start(2, "tool_use"),
            stop(2),
            stop(1),
            message_delta(),
        ]);
        let thinking = out
            .iter()
            .find(|e| e.data["delta"]["type"] == "thinking_delta")
            .unwrap();
        assert_eq!(thinking.data["delta"]["thinking"], "STOP in thinking");
        assert_eq!(visible_text(&out), "answer ");
        assert!(!out.iter().any(|e| e.data["index"] == 2));
        assert_eq!(filter.matched(), Some("STOP"));
    }

    #[test]
    fn picks_earliest_sequence_and_handles_multibyte() {
        let mut filter = StopSequenceFilter::new(seqs(&["结束", "B"])).unwrap();
        let out = filter.apply(vec![
            start(0, "text"),
            text_delta(0, "甲乙结"),
            text_delta(0, "束 B"),
        ]);
        assert_eq!(visible_text(&out), "甲乙");
        assert_eq!(filter.matched(), Some("结束"));
    }

    #[test]
    fn non_stream_truncates_text_and_drops_following_blocks() {
        let mut content = vec![
            json!({"type": "thinking", "thinking": "STOP"}),
            json!({"type": "text", "text": "keep STOP drop"}),
            json!({"type": "tool_use", "id": "t", "name": "x", "input": {}}),
        ];
        let matched = truncate_content_at_stop_sequence(&mut content, &seqs(&["STOP"]));
        assert_eq!(matched.as_deref(), Some("STOP"));
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["thinking"], "STOP");
        assert_eq!(content[1]["text"], "keep ");

        let mut empty_prefix = vec![json!({"type": "text", "text": "STOP"})];
        assert!(truncate_content_at_stop_sequence(&mut empty_prefix, &seqs(&["STOP"])).is_some());
        assert!(empty_prefix.is_empty());

        let mut no_match = vec![json!({"type": "text", "text": "fine"})];
        assert!(truncate_content_at_stop_sequence(&mut no_match, &seqs(&["STOP"])).is_none());
        assert_eq!(no_match[0]["text"], "fine");
    }
}
