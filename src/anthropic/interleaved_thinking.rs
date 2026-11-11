//! 工具调用之前的文字改写为空 thinking 块。
//!
//! 外显模型在 thinking display 为 omitted/updates 时，官方 API 会把"工具调用之间的文字"
//! 作为内容为空的 thinking 块返回，而上游目标模型输出的是普通 text 块。为了让客户端看到
//! 与真模型一致的结构：本轮出现 tool_use 时，把紧邻 tool_use 之前的 text 块改写为空
//! thinking 块；后面没有 tool_use 的 text（最终回答）原样下发。
//!
//! 流式场景下 text 块是否位于 tool_use 之前，要等到下一个内容块开始才能确定，所以 text 块
//! 会被整体扣住，直到出现决定性事件再决定改写或原样放出。扣住的文字超过
//! [`MAX_HELD_TEXT_BYTES`] 时放弃改写并恢复透传，长文本大概率是最终回答，不能失去流式效果。

use serde_json::{Value, json};

use super::signature::synthetic_thinking_signature;
use super::stream::SseEvent;

/// 单个 text 块最多扣住的文字字节数，超过后放弃改写并透传。
const MAX_HELD_TEXT_BYTES: usize = 2048;

/// 正在被扣住的 text 块。
struct HeldText {
    /// 原始 index，改写时沿用，不重新编号。
    index: Value,
    /// 该块已收到的全部事件（start、delta、stop），按到达顺序保存。
    events: Vec<SseEvent>,
    /// 已扣住的文字字节数。
    bytes: usize,
    /// 是否已收到该块的 content_block_stop。
    stopped: bool,
}

/// 流式过滤器：把紧邻 tool_use 之前的 text 块改写为空 thinking 块。
pub(crate) struct InterleavedTextAsThinking {
    held: Option<HeldText>,
    /// 在自身 stop 到达之前就已被改写的块 index。改写时已经补发了 stop，
    /// 之后到达的同 index 事件直接丢弃，避免客户端看到已关闭块的增量。
    closed_early: Vec<Value>,
}

impl InterleavedTextAsThinking {
    pub(crate) fn new() -> Self {
        Self {
            held: None,
            closed_early: Vec::new(),
        }
    }

    /// 处理一批 SSE 事件，返回应下发的事件（顺序保持）。
    pub(crate) fn apply(&mut self, events: Vec<SseEvent>) -> Vec<SseEvent> {
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            self.push(event, &mut out);
        }
        out
    }

    /// 是否正在扣住某个 text 块（扣住期间不应对该块发送保活 delta）。
    pub(crate) fn holding(&self) -> bool {
        self.held.is_some()
    }

    /// 流结束时调用：原样放出仍被扣住的事件。
    ///
    /// 正常结束的流一定以 message_delta / message_stop 收尾，届时扣住的块已经放出；
    /// 这里只兜底上游中途断开等异常结束，避免已生成的文字被静默吞掉。
    #[cfg(test)]
    pub(crate) fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        self.release(&mut out);
        out
    }

    fn push(&mut self, event: SseEvent, out: &mut Vec<SseEvent>) {
        let index = event.data.get("index").cloned();

        if matches!(
            event.event.as_str(),
            "content_block_delta" | "content_block_stop"
        ) && let Some(index) = index.as_ref()
            && let Some(position) = self.closed_early.iter().position(|item| item == index)
        {
            if event.event == "content_block_stop" {
                self.closed_early.remove(position);
            }
            return;
        }

        match event.event.as_str() {
            "content_block_start" => {
                let block_type = event
                    .data
                    .pointer("/content_block/type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if is_tool_use_type(block_type) {
                    if let Some(held) = self.held.take() {
                        self.rewrite(held, out);
                    }
                } else {
                    self.release(out);
                }

                if block_type == "text"
                    && let Some(index) = index
                {
                    let bytes = event
                        .data
                        .pointer("/content_block/text")
                        .and_then(Value::as_str)
                        .map_or(0, str::len);
                    self.held = Some(HeldText {
                        index,
                        events: vec![event],
                        bytes,
                        stopped: false,
                    });
                    self.release_if_over_limit(out);
                    return;
                }
                out.push(event);
            }
            "content_block_delta" | "content_block_stop" => {
                if let Some(held) = self.held.as_mut()
                    && index.as_ref() == Some(&held.index)
                {
                    if event.event == "content_block_stop" {
                        held.stopped = true;
                    } else if event.data.pointer("/delta/type").and_then(Value::as_str)
                        == Some("text_delta")
                    {
                        held.bytes += event
                            .data
                            .pointer("/delta/text")
                            .and_then(Value::as_str)
                            .map_or(0, str::len);
                    }
                    held.events.push(event);
                    self.release_if_over_limit(out);
                    return;
                }
                out.push(event);
            }
            "message_start" | "message_delta" | "message_stop" | "error" => {
                self.release(out);
                out.push(event);
            }
            // ping 等无关事件不影响扣住，直接放行。
            _ => out.push(event),
        }
    }

    /// 原样放出被扣住的事件。
    fn release(&mut self, out: &mut Vec<SseEvent>) {
        if let Some(held) = self.held.take() {
            out.extend(held.events);
        }
    }

    fn release_if_over_limit(&mut self, out: &mut Vec<SseEvent>) {
        if self
            .held
            .as_ref()
            .is_some_and(|held| held.bytes > MAX_HELD_TEXT_BYTES)
        {
            self.release(out);
        }
    }

    /// 把被扣住的 text 块改写为同 index 的空 thinking 块。
    fn rewrite(&mut self, held: HeldText, out: &mut Vec<SseEvent>) {
        let index = held.index;
        out.push(SseEvent::new(
            "content_block_start",
            json!({
                   "type": "content_block_start",
            "index": index,
            "content_block": {"type": "thinking", "thinking": "", "signature": ""}
               }),
        ));
        out.push(SseEvent::new(
            "content_block_delta",
            json!({
            "type": "content_block_delta",
                   "index": index,
                   "delta": {"type": "thinking_delta", "thinking": ""}
               }),
        ));
        out.push(SseEvent::new(
            "content_block_delta",
            json!({
            "type": "content_block_delta",
                   "index": index,
                   "delta": {
                "type": "signature_delta",
                "signature": synthetic_thinking_signature("")
                   }
               }),
        ));
        out.push(SseEvent::new(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        ));
        if !held.stopped {
            self.closed_early.push(index);
        }
    }
}

/// 非流式：把紧邻 tool_use / server_tool_use 之前的 text 块（含连续多个）原地替换为空
/// thinking 块，最后一个工具调用之后的 text 保留。返回是否有改动。
pub(crate) fn convert_content(content: &mut [Value]) -> bool {
    let mut changed = false;
    for tool_position in 0..content.len() {
        if !content[tool_position]
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(is_tool_use_type)
        {
            continue;
        }
        let mut position = tool_position;
        while position > 0
            && content[position - 1].get("type").and_then(Value::as_str) == Some("text")
        {
            position -= 1;
            content[position] = json!({
            "type": "thinking",
            "thinking": "",
                   "signature": synthetic_thinking_signature("")
               });
            changed = true;
        }
    }
    changed
}

fn is_tool_use_type(block_type: &str) -> bool {
    matches!(block_type, "tool_use" | "server_tool_use")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::signature::synthetic_thinking_signature_matches;

    fn message_start() -> SseEvent {
        SseEvent::new(
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_1", "content": []}}),
        )
    }

    fn block_start(index: u64, content_block: Value) -> SseEvent {
        SseEvent::new(
            "content_block_start",
            json!({"type": "content_block_start", "index": index, "content_block": content_block}),
        )
    }

    fn text_start(index: u64) -> SseEvent {
        block_start(index, json!({"type": "text", "text": ""}))
    }

    fn thinking_start(index: u64) -> SseEvent {
        block_start(
            index,
            json!({"type": "thinking", "thinking": "", "signature": ""}),
        )
    }

    fn tool_start(index: u64, block_type: &str) -> SseEvent {
        block_start(
            index,
            json!({"type": block_type, "id": "toolu_1", "name": "Read", "input": {}}),
        )
    }

    fn delta(index: u64, delta: Value) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index, "delta": delta}),
        )
    }

    fn text_delta(index: u64, text: &str) -> SseEvent {
        delta(index, json!({"type": "text_delta", "text": text}))
    }

    fn block_stop(index: u64) -> SseEvent {
        SseEvent::new(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        )
    }

    fn ping() -> SseEvent {
        SseEvent::new("ping", json!({"type": "ping"}))
    }

    fn message_delta(stop_reason: &str) -> SseEvent {
        SseEvent::new(
            "message_delta",
            json!({
                   "type": "message_delta",
            "delta": {"stop_reason": stop_reason},
                   "usage": {"output_tokens": 10}
               }),
        )
    }

    fn message_stop() -> SseEvent {
        SseEvent::new("message_stop", json!({"type": "message_stop"}))
    }

    /// 把事件压缩为 "事件名:index:块或增量类型" 便于断言顺序。
    fn shape(events: &[SseEvent]) -> Vec<String> {
        events
            .iter()
            .map(|event| {
                let index = event
                    .data
                    .get("index")
                    .map(|index| index.to_string())
                    .unwrap_or_default();
                let kind = event
                    .data
                    .pointer("/content_block/type")
                    .or_else(|| event.data.pointer("/delta/type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                format!("{name}:{index}:{kind}", name = event.event)
            })
            .collect()
    }

    fn data(events: &[SseEvent]) -> Vec<Value> {
        events.iter().map(|event| event.data.clone()).collect()
    }

    fn assert_empty_thinking_rewrite(events: &[SseEvent], index: u64) {
        assert_eq!(
            shape(events),
            vec![
                format!("content_block_start:{index}:thinking"),
                format!("content_block_delta:{index}:thinking_delta"),
                format!("content_block_delta:{index}:signature_delta"),
                format!("content_block_stop:{index}:"),
            ]
        );
        assert_eq!(
            events[0].data["content_block"],
            json!({"type": "thinking", "thinking": "", "signature": ""})
        );
        assert_eq!(events[1].data["delta"]["thinking"], json!(""));
        let signature = events[2].data["delta"]["signature"]
            .as_str()
            .expect("signature_delta 必须带 signature");
        assert!(!signature.is_empty());
        assert!(synthetic_thinking_signature_matches("", signature));
    }

    #[test]
    fn text_before_tool_use_becomes_empty_thinking() {
        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(vec![
            message_start(),
            text_start(0),
            text_delta(0, "我先读取一下文件。"),
            block_stop(0),
            tool_start(1, "tool_use"),
            delta(
                1,
                json!({"type": "input_json_delta", "partial_json": "{\"path\":\"a\"}"}),
            ),
            block_stop(1),
            message_delta("tool_use"),
            message_stop(),
        ]);

        assert_eq!(out[0].event, "message_start");
        assert_empty_thinking_rewrite(&out[1..5], 0);
        assert_eq!(
            shape(&out[5..]),
            vec![
                "content_block_start:1:tool_use",
                "content_block_delta:1:input_json_delta",
                "content_block_stop:1:",
                "message_delta::",
                "message_stop::",
            ]
        );
        assert!(
            !out.iter()
                .any(|event| event.data.pointer("/delta/type") == Some(&json!("text_delta"))),
            "改写后不应残留原文字"
        );
        assert!(!filter.holding());
    }

    #[test]
    fn server_tool_use_also_triggers_rewrite() {
        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(vec![
            text_start(0),
            text_delta(0, "搜索一下。"),
            block_stop(0),
            tool_start(1, "server_tool_use"),
        ]);
        assert_empty_thinking_rewrite(&out[..4], 0);
        assert_eq!(
            shape(&out[4..]),
            vec!["content_block_start:1:server_tool_use"]
        );
    }

    #[test]
    fn final_text_without_tool_use_passes_through_unchanged() {
        let input = vec![
            message_start(),
            text_start(0),
            text_delta(0, "这是最终回答，"),
            text_delta(0, "没有工具调用。"),
            block_stop(0),
            message_delta("end_turn"),
            message_stop(),
        ];
        let expected = data(&input);

        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(input);

        assert_eq!(data(&out), expected);
        assert!(!filter.holding());
    }

    #[test]
    fn only_text_directly_before_tool_use_is_rewritten() {
        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(vec![
            message_start(),
            thinking_start(0),
            delta(0, json!({"type": "thinking_delta", "thinking": "思考"})),
            delta(0, json!({"type": "signature_delta", "signature": "sig"})),
            block_stop(0),
            text_start(1),
            text_delta(1, "先看看目录。"),
            block_stop(1),
            tool_start(2, "tool_use"),
            delta(2, json!({"type": "input_json_delta", "partial_json": "{}"})),
            block_stop(2),
            text_start(3),
            text_delta(3, "目录里有三个文件。"),
            block_stop(3),
            message_delta("end_turn"),
            message_stop(),
        ]);

        assert_eq!(
            shape(&out[..5]),
            vec![
                "message_start::",
                "content_block_start:0:thinking",
                "content_block_delta:0:thinking_delta",
                "content_block_delta:0:signature_delta",
                "content_block_stop:0:",
            ]
        );
        assert_eq!(out[2].data["delta"]["thinking"], json!("思考"));
        assert_empty_thinking_rewrite(&out[5..9], 1);
        assert_eq!(
            shape(&out[9..]),
            vec![
                "content_block_start:2:tool_use",
                "content_block_delta:2:input_json_delta",
                "content_block_stop:2:",
                "content_block_start:3:text",
                "content_block_delta:3:text_delta",
                "content_block_stop:3:",
                "message_delta::",
                "message_stop::",
            ]
        );
        assert_eq!(out[13].data["delta"]["text"], json!("目录里有三个文件。"));
    }

    #[test]
    fn ping_while_holding_is_released_immediately() {
        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(vec![text_start(0), text_delta(0, "稍等"), ping()]);
        assert_eq!(shape(&out), vec!["ping::"]);
        assert!(filter.holding());

        let out = filter.apply(vec![block_stop(0), ping()]);
        assert_eq!(shape(&out), vec!["ping::"]);
        assert!(filter.holding(), "ping 不应结束扣住");

        let out = filter.apply(vec![tool_start(1, "tool_use")]);
        assert_empty_thinking_rewrite(&out[..4], 0);
        assert_eq!(shape(&out[4..]), vec!["content_block_start:1:tool_use"]);
    }

    #[test]
    fn long_text_over_limit_streams_through_without_rewrite() {
        let mut filter = InterleavedTextAsThinking::new();
        let chunk = "a".repeat(1000);

        let out = filter.apply(vec![
            text_start(0),
            text_delta(0, &chunk),
            text_delta(0, &chunk),
        ]);
        assert!(out.is_empty(), "2000 字节仍在上限内，应继续扣住");
        assert!(filter.holding());

        let out = filter.apply(vec![text_delta(0, &chunk)]);
        assert_eq!(
            shape(&out),
            vec![
                "content_block_start:0:text",
                "content_block_delta:0:text_delta",
                "content_block_delta:0:text_delta",
                "content_block_delta:0:text_delta",
            ],
            "超过上限时立即原样放出已扣事件"
        );
        assert!(!filter.holding());

        let out = filter.apply(vec![text_delta(0, "tail")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data["delta"]["text"], json!("tail"));

        let out = filter.apply(vec![block_stop(0), tool_start(1, "tool_use")]);
        assert_eq!(
            shape(&out),
            vec!["content_block_stop:0:", "content_block_start:1:tool_use"],
            "放弃改写后即使后面是 tool_use 也不再改写"
        );
    }

    #[test]
    fn exactly_limit_bytes_is_still_rewritten() {
        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(vec![
            text_start(0),
            text_delta(0, &"b".repeat(MAX_HELD_TEXT_BYTES)),
            block_stop(0),
            tool_start(1, "tool_use"),
        ]);
        assert_empty_thinking_rewrite(&out[..4], 0);
    }

    #[test]
    fn holding_spans_multiple_apply_calls() {
        let mut filter = InterleavedTextAsThinking::new();
        assert!(!filter.holding());
        assert_eq!(
            shape(&filter.apply(vec![message_start()])),
            vec!["message_start::"]
        );

        assert!(filter.apply(vec![text_start(0)]).is_empty());
        assert!(filter.holding());
        assert!(filter.apply(vec![text_delta(0, "第一段")]).is_empty());
        assert!(filter.apply(vec![text_delta(0, "第二段")]).is_empty());
        assert!(filter.apply(vec![block_stop(0)]).is_empty());
        assert!(filter.holding(), "stop 之后仍需等待决定性事件");
        assert!(filter.apply(Vec::new()).is_empty());

        let out = filter.apply(vec![tool_start(1, "tool_use")]);
        assert_empty_thinking_rewrite(&out[..4], 0);
        assert_eq!(shape(&out[4..]), vec!["content_block_start:1:tool_use"]);
        assert!(!filter.holding());

        assert_eq!(
            shape(&filter.apply(vec![block_stop(1)])),
            vec!["content_block_stop:1:"]
        );
        assert!(filter.apply(vec![text_start(2)]).is_empty());
        assert!(filter.apply(vec![text_delta(2, "完成。")]).is_empty());
        assert!(filter.apply(vec![block_stop(2)]).is_empty());

        let out = filter.apply(vec![message_delta("end_turn")]);
        assert_eq!(
            shape(&out),
            vec![
                "content_block_start:2:text",
                "content_block_delta:2:text_delta",
                "content_block_stop:2:",
                "message_delta::",
            ]
        );
        assert_eq!(out[1].data["delta"]["text"], json!("完成。"));
        assert!(!filter.holding());
    }

    #[test]
    fn consecutive_text_blocks_release_previous_and_hold_next() {
        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(vec![
            text_start(0),
            text_delta(0, "第一块"),
            block_stop(0),
            text_start(1),
            text_delta(1, "第二块"),
            block_stop(1),
        ]);
        assert_eq!(
            shape(&out),
            vec![
                "content_block_start:0:text",
                "content_block_delta:0:text_delta",
                "content_block_stop:0:",
            ]
        );
        assert!(filter.holding());

        let out = filter.apply(vec![tool_start(2, "tool_use")]);
        assert_empty_thinking_rewrite(&out[..4], 1);
        assert_eq!(shape(&out[4..]), vec!["content_block_start:2:tool_use"]);
    }

    #[test]
    fn thinking_start_or_error_releases_held_text_unchanged() {
        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(vec![
            text_start(0),
            text_delta(0, "前言"),
            block_stop(0),
            thinking_start(1),
        ]);
        assert_eq!(
            shape(&out),
            vec![
                "content_block_start:0:text",
                "content_block_delta:0:text_delta",
                "content_block_stop:0:",
                "content_block_start:1:thinking",
            ]
        );

        let mut filter = InterleavedTextAsThinking::new();
        let error = SseEvent::new(
            "error",
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "x"}}),
        );
        let out = filter.apply(vec![text_start(0), text_delta(0, "半句"), error]);
        assert_eq!(
            shape(&out),
            vec![
                "content_block_start:0:text",
                "content_block_delta:0:text_delta",
                "error::",
            ]
        );
        assert!(!filter.holding());
    }

    #[test]
    fn whitespace_only_text_follows_same_rules() {
        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(vec![
            text_start(0),
            text_delta(0, "\n\n  "),
            block_stop(0),
            tool_start(1, "tool_use"),
        ]);
        assert_empty_thinking_rewrite(&out[..4], 0);

        let mut filter = InterleavedTextAsThinking::new();
        let input = vec![
            text_start(0),
            text_delta(0, "\n"),
            block_stop(0),
            message_delta("end_turn"),
        ];
        let expected = data(&input);
        assert_eq!(data(&filter.apply(input)), expected);
    }

    #[test]
    fn tool_use_before_text_stop_drops_late_events_of_rewritten_block() {
        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(vec![
            text_start(0),
            text_delta(0, "调用工具"),
            tool_start(1, "tool_use"),
        ]);
        assert_empty_thinking_rewrite(&out[..4], 0);
        assert_eq!(shape(&out[4..]), vec!["content_block_start:1:tool_use"]);

        let out = filter.apply(vec![
            text_delta(0, "迟到的文字"),
            block_stop(0),
            block_stop(1),
        ]);
        assert_eq!(shape(&out), vec!["content_block_stop:1:"]);
    }

    #[test]
    fn non_text_blocks_pass_through_immediately() {
        let mut filter = InterleavedTextAsThinking::new();
        let out = filter.apply(vec![
            thinking_start(0),
            delta(0, json!({"type": "thinking_delta", "thinking": "嗯"})),
        ]);
        assert_eq!(out.len(), 2);
        assert!(!filter.holding());
    }

    #[test]
    fn finish_releases_held_events() {
        let mut filter = InterleavedTextAsThinking::new();
        assert!(
            filter
                .apply(vec![text_start(0), text_delta(0, "断流")])
                .is_empty()
        );
        let out = filter.finish();
        assert_eq!(
            shape(&out),
            vec![
                "content_block_start:0:text",
                "content_block_delta:0:text_delta"
            ]
        );
        assert!(!filter.holding());
        assert!(filter.finish().is_empty());
    }

    fn empty_thinking() -> Value {
        json!({
            "type": "thinking",
            "thinking": "",
            "signature": synthetic_thinking_signature("")
        })
    }

    #[test]
    fn convert_content_rewrites_text_before_tool_use() {
        let mut content = vec![
            json!({"type": "text", "text": "先读文件"}),
            json!({"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {}}),
        ];
        assert!(convert_content(&mut content));
        assert_eq!(content[0], empty_thinking());
        assert_eq!(content[1]["type"], json!("tool_use"));
        assert!(!content[0]["signature"].as_str().unwrap().is_empty());
    }

    #[test]
    fn convert_content_keeps_text_only_answer() {
        let original = vec![json!({"type": "text", "text": "最终回答"})];
        let mut content = original.clone();
        assert!(!convert_content(&mut content));
        assert_eq!(content, original);
    }

    #[test]
    fn convert_content_keeps_text_after_last_tool_use() {
        let mut content = vec![
            json!({"type": "thinking", "thinking": "想", "signature": "s"}),
            json!({"type": "text", "text": "第一句"}),
            json!({"type": "text", "text": "第二句"}),
            json!({"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {}}),
            json!({"type": "text", "text": "搜索"}),
            json!({"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}}),
            json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": []}),
            json!({"type": "text", "text": "最终回答"}),
        ];
        assert!(convert_content(&mut content));
        assert_eq!(content[0]["thinking"], json!("想"));
        assert_eq!(content[1], empty_thinking());
        assert_eq!(content[2], empty_thinking());
        assert_eq!(content[3]["type"], json!("tool_use"));
        assert_eq!(content[4], empty_thinking());
        assert_eq!(content[5]["type"], json!("server_tool_use"));
        assert_eq!(content[6]["type"], json!("web_search_tool_result"));
        assert_eq!(content[7], json!({"type": "text", "text": "最终回答"}));
    }
}
