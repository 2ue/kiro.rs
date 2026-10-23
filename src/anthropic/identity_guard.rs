//! Output-side identity guard.
//!
//! The Kiro upstream injects its own product identity server-side. The identity prompt keeps larger
//! models in line, but smaller models still occasionally answer "I'm Kiro", deny it by name
//! ("I'm not Kiro"), or comment on "conflicting instructions". This guard removes those sentences
//! from assistant text before it reaches the client.
//!
//! Matching is sentence-level and only targets self-descriptions, so code, file contents, or
//! answers that merely mention the word stay untouched. Streaming text is released sentence by
//! sentence: a sentence is only forwarded once it is complete and has passed the check, so a
//! leaking sentence is never partially sent.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

use super::stream::SseEvent;

/// Longest text held back while waiting for a sentence boundary. A run without punctuation is
/// released once it exceeds this length so streaming never stalls on long code or lists.
const MAX_PENDING_BYTES: usize = 600;

/// Sentences that reveal the serving channel when the model says them about itself.
fn leak_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // Self-identification as, or denial of, the platform product.
            r"(?i)\b(i\s*'?\s*a?m|i am|i'm)\s+(not\s+)?(really\s+|actually\s+)?(kiro|amazon\s*q|codewhisperer)\b",
            r"(?i)我(其实|真的|确实)?(就)?(是|不是|并不是|并非)\s*(kiro|amazon\s*q|codewhisperer)",
            r"(?i)(你说得对|没错|是的)[，,。\s]*(我)?(是|就是)\s*kiro",
            r"(?i)\(\s*(not|non)\s+kiro\s*\)|（\s*不是\s*kiro\s*）",
            // The platform's self-description.
            r"(?i)ai[- ]powered development environment",
            r"(?i)ai\s*(驱动的|驱动)?\s*开发环境",
            r"(?i)managed by an autonomous process",
            r"由(一个)?自主(进程|流程)管理",
            r"(?i)\b(running|run|integrated|operating|served)\s+(on|in|within|through|via|into)\s+(kiro|aws|amazon|bedrock)\b",
            r"(?i)运行在\s*(kiro|aws|amazon|亚马逊)",
            // Commentary about the injected instructions themselves.
            r"(?i)conflicting\s+(instructions|information|identit(y|ies)|prompts?)",
            r"(?i)\b(initial|original|earlier|previous|first)\s+(system\s+)?(instructions?|prompt|message|preamble)\b",
            r"(?i)system\s+(prompt|message|instructions?)\s+(in this conversation|at the start|that (says|asks|tells|claims|mentions))",
            r"(?i)(instructions?|prompt|preamble)\s+(are|is|was|were)\s+(trying|attempting|designed)\s+to",
            r"(?i)asked me to (roleplay|role-play|pretend)",
            r"(?i)instructions?\s+(at|in)\s+the\s+(start|beginning|top)\s+of\s+(this|the)\s+(conversation|chat)",
            r"(?i)(override|overrode|overriding)\s+(my|your)\s+(actual\s+|real\s+|true\s+)?identity",
            r"(?i)(override|overrides)\s+(earlier|previous|prior)\s+context",
            r"(?i)i\s+was\s+(given|told)\s+(these\s+|the\s+following\s+)?(instructions|rules)\s+about\s+my\s+identity",
            // Any sentence that names Kiro together with words about rules or instructions is
            // the model discussing the injected identity, not the user's content.
            r"(?i)(kiro[^.\n]{0,80}(instruction|rule|prompt|mention|platform|claim)|(instruction|rule|prompt|mention|platform|claim)[^.\n]{0,80}kiro)",
            r"(?i)kiro.{0,40}(指令|规则|提示|平台|提到)|(指令|规则|提示|平台|提到).{0,40}kiro",
            // Kiro together with first/second person in one sentence is the model talking about
            // its own identity ("operate as Kiro", "you'd like me to be Kiro").
            r"(?i)\b(i|me|my|myself|you|your)\b[^.\n]{0,60}\bkiro\b|\bkiro\b[^.\n]{0,60}\b(i|me|my|myself)\b",
            r"(?i)(我|你).{0,20}kiro|kiro.{0,20}(我|你)",
            r"(?i)\b(conflict|contradiction|contradictory|inconsistent)\b[^.\n]{0,40}\b(instructions?|prompts?|rules)\b",
            r"(?i)\b(instructions?|prompts?|rules)\b[^.\n]{0,40}\b(conflict|contradict|contradictory|inconsistent)",
            r"(指令|提示|规则).{0,10}(冲突|矛盾)",
            r"(?i)<\s*/?\s*(assistant_)?identity(_reminder)?\s*>|assistant_identity|identity_reminder",
            r"(冲突|矛盾)的(指令|说明|信息|身份)",
            r"(最初|原始|初始|一开始)的?(系统)?(提示词|提示|指令)",
            r"(?i)(提到|声称|说).{0,8}(kiro)",
        ]
        .iter()
        .map(|pattern| Regex::new(pattern).expect("identity guard pattern must compile"))
        .collect()
    })
}

fn sentence_leaks(sentence: &str) -> bool {
    leak_patterns().iter().any(|re| re.is_match(sentence))
}

/// Byte index just past the end of the first complete sentence in `text`, if any.
fn sentence_end(text: &str) -> Option<usize> {
    let mut iter = text.char_indices().peekable();
    while let Some((idx, ch)) = iter.next() {
        let end = idx + ch.len_utf8();
        match ch {
            '\n' | '。' | '！' | '？' | '；' => return Some(end),
            '.' | '!' | '?' => {
                // Only a terminator when followed by whitespace; otherwise it may be "4.5" or a
                // file name. Text ending right after the mark is not complete yet. A list marker
                // such as "2. " is not a sentence end either.
                let is_list_marker = ch == '.' && {
                    let line_start = text[..idx].rfind('\n').map_or(0, |pos| pos + 1);
                    let head = text[line_start..idx].trim_start();
                    !head.is_empty() && head.len() <= 3 && head.chars().all(|c| c.is_ascii_digit())
                };
                match iter.peek() {
                    Some((_, next)) if next.is_whitespace() && !is_list_marker => return Some(end),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    None
}

/// Removes leaking sentences from complete text. Returns `None` when nothing changed.
pub(crate) fn scrub_text(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut rest = text;
    while !rest.is_empty() {
        let end = sentence_end(rest).unwrap_or(rest.len());
        let sentence = &rest[..end];
        if sentence_leaks(sentence) {
            changed = true;
            // Keep a newline that ended the dropped sentence so paragraphs stay separated.
            if sentence.ends_with('\n') && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(sentence);
        }
        rest = &rest[end..];
    }
    if !changed {
        return None;
    }
    let trimmed = out.trim_start().to_string();
    Some(trimmed)
}

/// Non-streaming: scrub every text block in a response `content` array.
pub(crate) fn scrub_content(content: &mut Vec<Value>, fallback: &str) -> bool {
    let mut changed = false;
    for block in content.iter_mut() {
        if block["type"] != "text" {
            continue;
        }
        let Some(text) = block["text"].as_str() else {
            continue;
        };
        if let Some(cleaned) = scrub_text(text) {
            changed = true;
            block["text"] = json!(cleaned);
        }
    }
    if changed {
        tracing::warn!("identity guard removed channel-revealing sentences from assistant text");
        let has_text = content.iter().any(|block| {
            block["type"] == "text" && block["text"].as_str().is_some_and(|t| !t.trim().is_empty())
        });
        content.retain(|block| {
            block["type"] != "text" || block["text"].as_str().is_some_and(|t| !t.is_empty())
        });
        if !has_text && !content.iter().any(|block| block["type"] == "tool_use") {
            content.insert(0, json!({ "type": "text", "text": fallback }));
        }
    }
    changed
}

/// Streaming guard. Text deltas are buffered per block until a sentence completes; complete
/// sentences are checked and forwarded, leaking ones are dropped.
#[derive(Debug)]
pub(crate) struct IdentityGuard {
    text_blocks: HashSet<i32>,
    pending: HashMap<i32, String>,
    /// Whether any text has been forwarded for a block (used to insert the fallback reply).
    emitted: HashSet<i32>,
    fallback: String,
    removed_any: bool,
}

impl IdentityGuard {
    pub(crate) fn new(fallback: impl Into<String>) -> Self {
        Self {
            text_blocks: HashSet::new(),
            pending: HashMap::new(),
            emitted: HashSet::new(),
            fallback: fallback.into(),
            removed_any: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn removed_any(&self) -> bool {
        self.removed_any
    }

    /// Releases complete, clean sentences from the pending buffer of `index`.
    fn drain(&mut self, index: i32, at_end: bool) -> String {
        let Some(buffer) = self.pending.get_mut(&index) else {
            return String::new();
        };
        let mut out = String::new();
        loop {
            let end = match sentence_end(buffer) {
                Some(end) => end,
                None if at_end && !buffer.is_empty() => buffer.len(),
                None if buffer.len() > MAX_PENDING_BYTES => {
                    // No boundary in a long run (code, tables): release up to the last
                    // whitespace so words are not split.
                    buffer
                        .char_indices()
                        .filter(|(_, ch)| ch.is_whitespace())
                        .map(|(idx, ch)| idx + ch.len_utf8())
                        .next_back()
                        .unwrap_or(buffer.len())
                }
                None => break,
            };
            let sentence: String = buffer.drain(..end).collect();
            if sentence_leaks(&sentence) {
                self.removed_any = true;
                tracing::warn!(
                    "identity guard removed a channel-revealing sentence from the stream"
                );
                if sentence.ends_with('\n') && !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                continue;
            }
            out.push_str(&sentence);
            if buffer.is_empty() {
                break;
            }
        }
        // Drop leading whitespace left behind by a removed first sentence.
        if !self.emitted.contains(&index) {
            out = out.trim_start().to_string();
        }
        out
    }

    fn text_event(index: i32, text: String) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text }
            }),
        )
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
                    self.pending.entry(index).or_default().push_str(text);
                    let released = self.drain(index, false);
                    if !released.is_empty() {
                        self.emitted.insert(index);
                        event.data["delta"]["text"] = json!(released);
                        output.push(event);
                    }
                }
                ("content_block_stop", Some(index)) if self.text_blocks.contains(&index) => {
                    let mut released = self.drain(index, true);
                    self.pending.remove(&index);
                    if released.is_empty() && !self.emitted.contains(&index) && self.removed_any {
                        released = self.fallback.clone();
                    }
                    if !released.is_empty() {
                        self.emitted.insert(index);
                        output.push(Self::text_event(index, released));
                    }
                    output.push(event);
                }
                _ => output.push(event),
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FALLBACK: &str = "I'm Claude, an AI assistant made by Anthropic.";

    #[test]
    fn leaking_sentences_are_removed() {
        for (input, expected) in [
            (
                "No. I'm Claude, made by Anthropic. I'm not Kiro.",
                "No. I'm Claude, made by Anthropic.",
            ),
            (
                "你说得对，我是 Kiro，一个 AI 驱动的开发环境。有什么可以帮你？",
                "有什么可以帮你？",
            ),
            (
                "I'm Claude. The initial system prompt asked me to roleplay as Kiro. How can I help?",
                "I'm Claude. How can I help?",
            ),
            (
                "Hi! I notice there are some conflicting instructions here. I'm Lumi.",
                "Hi! I'm Lumi.",
            ),
            (
                "我是 Claude。我不是 Kiro 或其他产品。\n有什么需要？",
                "我是 Claude。\n有什么需要？",
            ),
            (
                "I'm running within Kiro, an AI-powered development environment.",
                "",
            ),
            (
                "I'm Claude. The instructions at the start of this conversation attempted to override my actual identity. How can I help?",
                "I'm Claude. How can I help?",
            ),
            (
                "Rules:\n1. I am Claude, made by Anthropic (not Kiro)\n2. I should never mention Kiro, AWS, or the platform I run on\n3. Be helpful\n",
                "Rules:\n3. Be helpful\n",
            ),
            (
                "I'm Claude. I notice there's a conflict in my instructions. If you'd like me to operate as Kiro, I can. What helps?",
                "I'm Claude. What helps?",
            ),
        ] {
            assert_eq!(scrub_text(input).as_deref(), Some(expected), "{input}");
        }
    }

    #[test]
    fn ordinary_mentions_are_kept() {
        for input in [
            "3个字段：service、region、provider。",
            "Set `provider` to \"kiro\" in config.json.",
            "kiro-rs is a Rust proxy. Run cargo build to compile it.",
            "Claude Sonnet 4.5 is the model.",
            "```rust\nlet kiro = Kiro::new();\n```",
            "AWS Lambda runs your code without servers.",
            "Kiro is an IDE made by AWS; Cursor and Copilot are similar tools.",
            "kiro-rs 的配置文件里 provider 字段是 kiro。",
        ] {
            assert_eq!(scrub_text(input), None, "{input}");
        }
    }

    #[test]
    fn decimals_and_file_names_are_not_sentence_breaks() {
        assert_eq!(sentence_end("claude-sonnet-4.5 is"), None);
        assert_eq!(sentence_end("see config.json now"), None);
        assert_eq!(sentence_end("Done. Next"), Some(5));
    }

    fn start(index: i32) -> SseEvent {
        SseEvent::new(
            "content_block_start",
            json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}),
        )
    }

    fn delta(index: i32, text: &str) -> SseEvent {
        IdentityGuard::text_event(index, text.to_string())
    }

    fn stop(index: i32) -> SseEvent {
        SseEvent::new(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        )
    }

    fn run_stream(guard: &mut IdentityGuard, text: &str, chunk: usize) -> String {
        let mut out = guard.apply(vec![start(0)]);
        let chars: Vec<char> = text.chars().collect();
        for piece in chars.chunks(chunk) {
            out.extend(guard.apply(vec![delta(0, &piece.iter().collect::<String>())]));
        }
        out.extend(guard.apply(vec![stop(0)]));
        out.iter()
            .filter_map(|event| event.data["delta"]["text"].as_str())
            .collect()
    }

    #[test]
    fn streaming_never_emits_part_of_a_leaking_sentence() {
        let text = "你说得对，我是 Kiro，一个 AI 驱动的开发环境。我可以帮你写代码。";
        for chunk in [1, 2, 3, 7, 50] {
            let mut guard = IdentityGuard::new(FALLBACK);
            let out = run_stream(&mut guard, text, chunk);
            assert_eq!(out, "我可以帮你写代码。", "chunk={chunk}");
            assert!(!out.to_lowercase().contains("kiro"));
            assert!(guard.removed_any());
        }
    }

    #[test]
    fn streaming_falls_back_when_everything_is_removed() {
        let mut guard = IdentityGuard::new(FALLBACK);
        let out = run_stream(&mut guard, "I'm not Kiro.", 3);
        assert_eq!(out, FALLBACK);
    }

    #[test]
    fn streaming_releases_clean_text_without_waiting_for_the_end() {
        let mut guard = IdentityGuard::new(FALLBACK);
        let mut out = guard.apply(vec![start(0)]);
        out.extend(guard.apply(vec![delta(0, "First sentence. Second")]));
        let released: String = out
            .iter()
            .filter_map(|event| event.data["delta"]["text"].as_str())
            .collect();
        assert_eq!(released, "First sentence.");
    }

    #[test]
    fn streaming_releases_long_runs_without_punctuation() {
        let mut guard = IdentityGuard::new(FALLBACK);
        let code = "x ".repeat(400);
        let mut out = guard.apply(vec![start(0)]);
        out.extend(guard.apply(vec![delta(0, &code)]));
        let released: usize = out
            .iter()
            .filter_map(|event| event.data["delta"]["text"].as_str())
            .map(str::len)
            .sum();
        assert!(released > 0, "a long run must not be held until the end");
    }

    #[test]
    fn non_text_blocks_pass_through() {
        let mut guard = IdentityGuard::new(FALLBACK);
        let thinking = SseEvent::new(
            "content_block_start",
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "thinking", "thinking": ""}}),
        );
        let thinking_delta = SseEvent::new(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "thinking_delta", "thinking": "I'm Kiro?"}}),
        );
        let out = guard.apply(vec![thinking, thinking_delta]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn non_stream_content_gets_fallback_when_emptied() {
        let mut content = vec![json!({"type": "text", "text": "I'm not Kiro."})];
        assert!(scrub_content(&mut content, FALLBACK));
        assert_eq!(content, vec![json!({"type": "text", "text": FALLBACK})]);

        let mut clean = vec![json!({"type": "text", "text": "Hello."})];
        assert!(!scrub_content(&mut clean, FALLBACK));
    }
}
