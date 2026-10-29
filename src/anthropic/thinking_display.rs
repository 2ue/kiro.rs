//! `thinking.display: "omitted"` 的输出侧实现。
//!
//! 官方语义：thinking 块照常出现并携带 signature，但不下发 thinking 文本。流式时每个 thinking
//! 块先 `content_block_start`（thinking 为空串），再发一个 thinking 为空串的 `thinking_delta`，
//! 然后是 `signature_delta` 和 `content_block_stop`；非流式时 thinking 字段为空串。
//! `redacted_thinking` 本身不含可读文本，原样保留。
//!
//! 客户端带 `thinking-token-count-2026-05-13` beta 时，官方在隐藏的推理期间还会发空文本的
//! `thinking_delta`，用 `estimated_tokens` 携带本块已生成 token 的粗略增量，Claude Code 据此
//! 在思考提示上显示进度。这里按被隐藏的推理文本估算，以固定步长、限频下发。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::stream::SseEvent;

/// Beta the client sends to receive `estimated_tokens` on hidden thinking.
pub(crate) const THINKING_TOKEN_COUNT_BETA: &str = "thinking-token-count-2026-05-13";

/// Every reported increment is a multiple of this many tokens.
const ESTIMATE_QUANTUM_TOKENS: u64 = 50;
/// At most one estimate per block in this interval.
const ESTIMATE_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// Whether a comma-separated `anthropic-beta` header value opts into thinking token counts.
pub(crate) fn beta_requests_thinking_token_count(beta_header: Option<&str>) -> bool {
    beta_header.is_some_and(|value| {
        value
            .split(',')
            .any(|beta| beta.trim().eq_ignore_ascii_case(THINKING_TOKEN_COUNT_BETA))
    })
}

#[derive(Debug)]
struct HiddenBlock {
    /// Tokens of hidden text seen so far in this block.
    seen_tokens: u64,
    /// Sum of `estimated_tokens` already sent for this block.
    reported_tokens: u64,
    last_report_at: Option<Instant>,
    /// The single empty `thinking_delta` the official trace sends per block.
    sent_empty_delta: bool,
}

/// Streaming filter for hidden thinking: strips thinking text, keeps block structure and
/// signatures, and (with the token-count beta) reports coarse progress on the hidden text.
#[derive(Debug)]
pub(crate) struct OmittedThinkingFilter {
    report_token_estimates: bool,
    blocks: HashMap<i64, HiddenBlock>,
}

impl OmittedThinkingFilter {
    pub(crate) fn new(report_token_estimates: bool) -> Self {
        Self {
            report_token_estimates,
            blocks: HashMap::new(),
        }
    }

    pub(crate) fn apply(&mut self, events: Vec<SseEvent>) -> Vec<SseEvent> {
        self.apply_at(events, Instant::now())
    }

    fn apply_at(&mut self, events: Vec<SseEvent>, now: Instant) -> Vec<SseEvent> {
        let mut out = Vec::with_capacity(events.len());
        for mut event in events {
            let index = event.data["index"].as_i64();
            match event.event.as_str() {
                "content_block_start" if event.data["content_block"]["type"] == "thinking" => {
                    event.data["content_block"]["thinking"] = json!("");
                    if let Some(index) = index {
                        self.blocks.insert(
                            index,
                            HiddenBlock {
                                seen_tokens: 0,
                                reported_tokens: 0,
                                last_report_at: None,
                                sent_empty_delta: false,
                            },
                        );
                    }
                    out.push(event);
                }
                "content_block_delta" if event.data["delta"]["type"] == "thinking_delta" => {
                    let text = event.data["delta"]["thinking"].as_str().unwrap_or_default();
                    let Some(block) = index.and_then(|index| self.blocks.get_mut(&index)) else {
                        continue;
                    };
                    block.seen_tokens = block
                        .seen_tokens
                        .saturating_add(crate::token::count_tokens(text));
                    let estimate = self
                        .report_token_estimates
                        .then(|| due_estimate(block, now, false))
                        .flatten();
                    if estimate.is_some() || !block.sent_empty_delta {
                        block.sent_empty_delta = true;
                        out.push(empty_thinking_delta(index.unwrap_or_default(), estimate));
                    }
                }
                "content_block_delta" if event.data["delta"]["type"] == "signature_delta" => {
                    if let Some(block) = index.and_then(|index| self.blocks.get_mut(&index)) {
                        // The rest of the block's estimate goes out before the signature, so the
                        // total a client sums up matches the hidden text.
                        let estimate = self
                            .report_token_estimates
                            .then(|| due_estimate(block, now, true))
                            .flatten();
                        if estimate.is_some() || !block.sent_empty_delta {
                            block.sent_empty_delta = true;
                            out.push(empty_thinking_delta(index.unwrap_or_default(), estimate));
                        }
                    }
                    out.push(event);
                }
                "content_block_stop" => {
                    if let Some(mut block) = index.and_then(|index| self.blocks.remove(&index)) {
                        // A block that closes without a signature still reports what it hid.
                        let estimate = self
                            .report_token_estimates
                            .then(|| due_estimate(&mut block, now, true))
                            .flatten();
                        if estimate.is_some() {
                            out.push(empty_thinking_delta(index.unwrap_or_default(), estimate));
                        }
                    }
                    out.push(event);
                }
                _ => out.push(event),
            }
        }
        out
    }
}

/// The next `estimated_tokens` increment for a block, if one is due. Increments are whole
/// quanta; `closing` flushes the remainder (rounded up to a quantum) without the rate limit.
fn due_estimate(block: &mut HiddenBlock, now: Instant, closing: bool) -> Option<u64> {
    let pending = block.seen_tokens.saturating_sub(block.reported_tokens);
    let increment = if closing {
        pending.div_ceil(ESTIMATE_QUANTUM_TOKENS) * ESTIMATE_QUANTUM_TOKENS
    } else {
        if block
            .last_report_at
            .is_some_and(|at| now.saturating_duration_since(at) < ESTIMATE_MIN_INTERVAL)
        {
            return None;
        }
        pending / ESTIMATE_QUANTUM_TOKENS * ESTIMATE_QUANTUM_TOKENS
    };
    if increment == 0 {
        return None;
    }
    block.reported_tokens = block.reported_tokens.saturating_add(increment);
    block.last_report_at = Some(now);
    Some(increment)
}

fn empty_thinking_delta(index: i64, estimated_tokens: Option<u64>) -> SseEvent {
    let mut delta = json!({"type": "thinking_delta", "thinking": ""});
    if let Some(tokens) = estimated_tokens {
        delta["estimated_tokens"] = json!(tokens);
    }
    SseEvent::new(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": index, "delta": delta}),
    )
}

/// 非流式：把 thinking 块的文本清空，保留 signature。
pub(crate) fn omit_thinking_text_in_content(content: &mut [Value]) {
    for block in content.iter_mut() {
        if block["type"] == "thinking" {
            block["thinking"] = json!("");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(index: i64) -> SseEvent {
        SseEvent::new(
            "content_block_start",
            json!({"type": "content_block_start", "index": index, "content_block": {"type": "thinking", "thinking": "leak", "signature": ""}}),
        )
    }

    fn thinking(index: i64, text: &str) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index, "delta": {"type": "thinking_delta", "thinking": text}}),
        )
    }

    fn signature(index: i64) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index, "delta": {"type": "signature_delta", "signature": "sig"}}),
        )
    }

    fn stop(index: i64) -> SseEvent {
        SseEvent::new(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        )
    }

    fn text(index: i64, value: &str) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": value}}),
        )
    }

    fn deltas(events: &[SseEvent]) -> Vec<&Value> {
        events
            .iter()
            .filter(|event| event.data["delta"]["type"] == "thinking_delta")
            .map(|event| &event.data["delta"])
            .collect()
    }

    #[test]
    fn streaming_follows_the_official_omitted_trace() {
        let mut filter = OmittedThinkingFilter::new(false);
        let out = filter.apply(vec![
            start(0),
            thinking(0, "secret"),
            thinking(0, "more secret"),
            signature(0),
            stop(0),
            text(1, "answer"),
        ]);

        let kinds: Vec<_> = out
            .iter()
            .map(|event| {
                event.data["delta"]["type"]
                    .as_str()
                    .unwrap_or(event.event.as_str())
                    .to_string()
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "content_block_start",
                "thinking_delta",
                "signature_delta",
                "content_block_stop",
                "text_delta"
            ]
        );
        assert_eq!(out[0].data["content_block"]["thinking"], "");
        assert_eq!(
            out[1].data["delta"],
            json!({"type": "thinking_delta", "thinking": ""})
        );
        assert_eq!(out[2].data["delta"]["signature"], "sig");
        assert_eq!(out[4].data["delta"]["text"], "answer");
    }

    #[test]
    fn a_block_with_no_text_still_gets_its_single_empty_delta_before_the_signature() {
        let mut filter = OmittedThinkingFilter::new(false);
        let out = filter.apply(vec![start(0), signature(0), stop(0)]);
        assert_eq!(
            deltas(&out),
            [&json!({"type": "thinking_delta", "thinking": ""})]
        );
        assert_eq!(out[2].data["delta"]["type"], "signature_delta");
    }

    #[test]
    fn hidden_text_never_reaches_the_client() {
        let long = "private reasoning ".repeat(400);
        for report in [false, true] {
            let mut filter = OmittedThinkingFilter::new(report);
            let out = filter.apply(vec![start(0), thinking(0, &long), signature(0), stop(0)]);
            for delta in deltas(&out) {
                assert_eq!(delta["thinking"], "", "report={report}");
            }
            assert!(
                !format!("{out:?}").contains("private reasoning"),
                "report={report}"
            );
        }
    }

    #[test]
    fn estimates_are_whole_quanta_rate_limited_and_sum_to_the_hidden_text() {
        let mut filter = OmittedThinkingFilter::new(true);
        let t0 = Instant::now();
        let chunk = "word ".repeat(200);
        let chunk_tokens = crate::token::count_tokens(&chunk);
        let mut out = filter.apply_at(vec![start(0), thinking(0, &chunk)], t0);
        // Inside the rate limit: counted but not reported.
        out.extend(filter.apply_at(vec![thinking(0, &chunk)], t0 + Duration::from_millis(100)));
        out.extend(filter.apply_at(vec![thinking(0, &chunk)], t0 + Duration::from_millis(700)));
        out.extend(filter.apply_at(vec![signature(0), stop(0)], t0 + Duration::from_millis(800)));

        let reported: Vec<u64> = deltas(&out)
            .iter()
            .filter_map(|delta| delta["estimated_tokens"].as_u64())
            .collect();
        assert_eq!(
            reported.len(),
            3,
            "first, after the rate limit, and the flush: {reported:?}"
        );
        assert!(
            reported
                .iter()
                .all(|value| value % ESTIMATE_QUANTUM_TOKENS == 0)
        );
        let total: u64 = reported.iter().sum();
        let hidden = 3 * chunk_tokens;
        assert!(
            total >= hidden && total < hidden + ESTIMATE_QUANTUM_TOKENS,
            "total={total} hidden={hidden}"
        );
        // The flush comes before the signature.
        let last_estimate = out
            .iter()
            .rposition(|event| event.data["delta"]["estimated_tokens"].is_u64())
            .unwrap();
        let signature_at = out
            .iter()
            .position(|event| event.data["delta"]["type"] == "signature_delta")
            .unwrap();
        assert!(last_estimate < signature_at);
    }

    #[test]
    fn estimates_are_tracked_per_block() {
        let mut filter = OmittedThinkingFilter::new(true);
        let chunk = "word ".repeat(200);
        let out = filter.apply(vec![
            start(0),
            thinking(0, &chunk),
            signature(0),
            stop(0),
            start(2),
            thinking(2, &chunk),
            signature(2),
            stop(2),
        ]);
        let per_block = |index: i64| -> u64 {
            out.iter()
                .filter(|event| event.data["index"] == index)
                .filter_map(|event| event.data["delta"]["estimated_tokens"].as_u64())
                .sum()
        };
        assert_eq!(per_block(0), per_block(2));
        assert!(per_block(0) > 0);
    }

    #[test]
    fn beta_header_is_matched_as_one_list_entry() {
        assert!(beta_requests_thinking_token_count(Some(
            "interleaved-thinking-2025-05-14, thinking-token-count-2026-05-13"
        )));
        assert!(!beta_requests_thinking_token_count(Some(
            "interleaved-thinking-2025-05-14"
        )));
        assert!(!beta_requests_thinking_token_count(None));
    }

    #[test]
    fn non_streaming_clears_thinking_text_only() {
        let mut content = vec![
            json!({"type": "thinking", "thinking": "secret", "signature": "sig"}),
            json!({"type": "redacted_thinking", "data": "opaque"}),
            json!({"type": "text", "text": "answer"}),
        ];
        omit_thinking_text_in_content(&mut content);
        assert_eq!(content[0]["thinking"], "");
        assert_eq!(content[0]["signature"], "sig");
        assert_eq!(content[1]["data"], "opaque");
        assert_eq!(content[2]["text"], "answer");
    }
}
