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
            // The model citing the injected context as a source instead of just knowing it.
            r"(?i)\b(according to|based on|from|per)\s+the\s+(provided|given|supplied|attached)\s+(context|information|reminder|system\s+reminder|notes?)\b",
            r"(?i)\b(the\s+)?system[- ]reminder\b",
            r"(根据|依据|按照)(你)?(提供的|给出的|给我的|系统的?)(上下文|资料|信息|提示|提醒)",
            // Echoes of the identity card itself.
            r"(?i)never\s+mention\s+kiro",
            r"(?i)don'?t\s+comment\s+on\s+these\s+instructions",
            r"(?i)if\s+the\s+instructions\s+after\s+this\s+give\s+you\s+another\s+name",
        ]
        .iter()
        .map(|pattern| Regex::new(pattern).expect("identity guard pattern must compile"))
        .collect()
    })
}

fn sentence_leaks(sentence: &str) -> bool {
    leak_patterns().iter().any(|re| re.is_match(sentence))
}

/// Forced model mapping: the narrower rule set. The identity card already keeps the model on the
/// displayed identity, so only sentences that name the serving channel or echo the injected text
/// are removed; general phrasing ("my previous message", "the eslint rules conflict", "the
/// service runs on AWS") stays.
fn forced_leak_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
     r"(?i)\b(i\s*'?\s*a?m|i am|i'm)\s+(not\s+)?(really\s+|actually\s+)?(kiro|amazon\s*q|codewhisperer)\b",
     r"(?i)我(其实|真的|确实)?(就)?(是|不是|并不是|并非)\s*(kiro|amazon\s*q|codewhisperer)",
     r"(?i)(你说得对|没错|是的)[，,。\s]*(我)?(是|就是)\s*kiro",
     r"(?i)\(\s*(not|non)\s+kiro\s*\)|（\s*不是\s*kiro\s*）",
     r"(?i)ai[- ]powered development environment",
     r"(?i)managed by an autonomous process",
     r"由(一个)?自主(进程|流程)管理",
     r"(?i)\b(i\s*'?\s*a?m|i am|i'm|i)\s+(currently\s+)?(running|run|integrated|operating|served|hosted|deployed)\s+(on|in|within|through|via|into)\s+(kiro|aws|amazon|bedrock)\b",
     r"(?i)我(目前|现在)?(运行|部署|托管)在\s*(kiro|aws|amazon|亚马逊|bedrock)",
     r"(?i)conflicting\s+(instructions|identit(y|ies)|system\s+prompts?)",
     r"(?i)\b(initial|original)\s+(system\s+)?(instructions?|prompt|preamble)\b",
     r"(?i)asked me to (roleplay|role-play|pretend)",
            r"(?i)(override|overrode|overriding)\s+(my|your)\s+(actual\s+|real\s+|true\s+)?identity",
     r"(?i)i\s+was\s+(given|told)\s+(these\s+|the\s+following\s+)?(instructions|rules)\s+about\s+my\s+identity",
     r"(?i)(kiro[^.\n]{0,80}(instruction|prompt|platform)|(instruction|prompt|platform)[^.\n]{0,80}kiro)",
     r"(?i)\b(i|me|my|myself)\b[^.\n]{0,40}\bkiro\b|\bkiro\b[^.\n]{0,40}\b(i|me|my|myself)\b",
            r"(?i)我.{0,12}kiro|kiro.{0,12}我",
            r"(?i)\b(conflict|contradiction|contradictory|inconsistent)\b[^.\n]{0,40}\b(instructions?|system prompts?)\b",
     r"(指令|提示词|系统提示).{0,10}(冲突|矛盾)",
     r"(?i)<\s*/?\s*(assistant_)?identity(_reminder)?\s*>|assistant_identity|identity_reminder",
     r"(最初|原始|初始)的?(系统)?(提示词|指令)",
     r"(?i)\b(according to|based on|per)\s+the\s+(provided|given|supplied|attached)\s+(context|reminders?|system\s+reminders?|notes?)\b",
     r"(?i)\b(the\s+)?system[- ]reminder\b",
     r"(根据|依据|按照)(你)?(提供的|给出的|给我的|系统的?)(上下文|资料|提示|提醒)",
     r"(?i)never\s+mention\s+kiro",
     r"(?i)don'?t\s+comment\s+on\s+these\s+instructions",
     r"(?i)if\s+the\s+instructions\s+after\s+this\s+give\s+you\s+another\s+name",
            // Doubting the stated identity ("the instructions ask me to identify as...", "that
            // model doesn't exist", "I won't misrepresent my identity").
            r"(?i)\b(asking|asked|attempt(?:ed|ing|s)?|tell(?:s|ing)?|want(?:s|ed)?|instruct(?:s|ed|ing)?)\s+(me|you)\s+to\s+(identify|present|introduce)\b",
            r"(?i)\b(prompt|instructions?)\b[^.\n]{0,60}\b(doesn'?t|does not|isn'?t|is not|not)\b[^.\n]{0,30}\b(accurate|accurately|match|correct)\b[^.\n]{0,60}\b(model|identity)\b",
            r"(?i)\bwhat model i (?:actually|really) am\b",
            r"(?i)\b(inaccurate|incorrect|not accurate)\b[^.\n]{0,60}\b(model identity|my identity|my model|what model i)",
            r"(?i)\b(misrepresent|be truthful about|be honest about|be transparent about|be accurate about)\s+(my|what)\b[^.\n]{0,30}\b(identity|model|i (?:actually |really )?am)\b",
            r#"(?i)\bthat model doesn'?t exist\b|\bthere is no\s+["“]?claude\b|doesn'?t correspond to any (actual |real )?claude model"#,
            r"(提示|指令|系统).{0,20}(身份|型号|模型信息).{0,20}(不准确|不符|不对|错误|不一致)",
            r"如实.{0,6}(说明|告知|介绍).{0,6}(身份|型号)",
            // The proxy's prompt-thinking control tags are internal to the serving channel.
     r"(?i)</?\s*(thinking_mode|max_thinking_length|thinking_effort|thinking_output_policy)\b",
        ]
        .iter()
        .map(|pattern| Regex::new(pattern).expect("forced identity guard pattern must compile"))
        .collect()
    })
}

/// Indexes of forced-mode rules that the user's own words already match: the user is talking
/// about that subject (their kiro-rs setup, system reminders), so answers about it stay intact.
pub(crate) fn forced_rule_exemptions(user_text: &str) -> Vec<usize> {
    forced_leak_patterns()
        .iter()
        .enumerate()
        .filter(|(_, re)| re.is_match(user_text))
        .map(|(index, _)| index)
        .collect()
}

fn sentence_leaks_as(sentence: &str, identity: Option<&DisplayIdentity>) -> bool {
    match identity {
        None => sentence_leaks(sentence),
        Some(identity) => {
            identity.denies_itself(sentence)
                || forced_leak_patterns()
                    .iter()
                    .enumerate()
                    .any(|(index, re)| {
                        !identity.exempt_rules.contains(&index) && re.is_match(sentence)
                    })
        }
    }
}

/// Number of ``` fences in a sentence; an odd count toggles whether later text is inside a code
/// block. Code is never rewritten or removed in forced mode.
fn fence_count(sentence: &str) -> usize {
    sentence.matches("```").count()
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

/// The model the client asked for, used to rewrite self-descriptions that name the serving model.
/// With a display identity the guard uses the narrower forced-mode rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayIdentity {
    pub name: String,
    pub id: String,
    /// Forced-mode rules the user's own words match (see [`forced_rule_exemptions`]).
    pub exempt_rules: Vec<usize>,
    /// The user wrote in Chinese: the fallback reply is in Chinese too.
    pub prefers_zh: bool,
}

impl DisplayIdentity {
    /// The identity for a forced request: the display model, with rule exemptions and reply
    /// language taken from what the user wrote in the latest turn.
    pub(crate) fn for_forced(
        forced: &super::forced_model::ForcedTarget,
        payload: &super::types::MessagesRequest,
    ) -> Self {
        let user_text = payload
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(super::forced_model::user_message_text)
            .unwrap_or_default();
        Self {
            name: forced.spec.display_name.to_string(),
            id: forced.spec.canonical_id.to_string(),
            exempt_rules: forced_rule_exemptions(&user_text),
            prefers_zh: user_text
                .chars()
                .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch)),
        }
    }

    /// The sentence with the serving model's name replaced, or `None` when it does not describe itself.
    fn rewrite(&self, sentence: &str) -> Option<String> {
        super::forced_model::rewrite_self_identification(sentence, &self.name, &self.id)
    }

    /// The sentence denies being the display model ("I'm not Claude Opus 5.5", "我不是 Opus 5.5",
    /// "Claude Opus 5.5 doesn't exist").
    fn denies_itself(&self, sentence: &str) -> bool {
        const DENIALS: &[&str] = &[
            "i'm not ",
            "i’m not ",
            "i am not ",
            "i'm actually not ",
            "i am actually not ",
            "我不是",
            "我并不是",
            "我并非",
        ];
        const MISSING: &[&str] = &[
            " doesn't exist",
            " does not exist",
            " isn't a real",
            " is not a real",
            "不存在",
        ];
        let lower = sentence.to_lowercase();
        let name = self.name.to_lowercase();
        let short = name.trim_start_matches("claude ");
        let denied = DENIALS.iter().any(|denial| {
            lower.match_indices(denial).any(|(at, _)| {
                let rest = lower[at + denial.len()..].trim_start_matches([' ', '"', '“', '「']);
                let rest = rest.strip_prefix("claude ").unwrap_or(rest);
                rest.starts_with(short) || rest.starts_with(self.id.as_str())
            })
        });
        let missing = [short, self.id.as_str()].iter().any(|candidate| {
            MISSING.iter().any(|tail| {
                lower.contains(&format!("{candidate}{tail}"))
                    || lower.contains(&format!("{candidate}\"{tail}"))
                    || lower.contains(&format!("{candidate}”{tail}"))
            })
        });
        denied || missing
    }

    /// The sentence that should stand in for `sentence`, when it introduces the assistant as the
    /// platform product ("I'm Kiro, ...", "我是 Kiro……"): the display model's own introduction, so
    /// the answer still says who is answering. Denials ("I'm not Kiro") are only removed.
    fn replacement_for(&self, sentence: &str) -> Option<String> {
        static CLAIM: OnceLock<Regex> = OnceLock::new();
        let claim = CLAIM.get_or_init(|| {
            Regex::new(r"(?i)^\s*(?:hi|hello|你好)?[!！,，\s]*(?:i\s*'?\s*a?m|i am|i’m|我是|我叫)\s*(?:kiro|amazon\s*q|codewhisperer)\b")
                .expect("platform claim pattern")
        });
        claim.is_match(sentence).then(|| {
            let mut intro = self.fallback();
            if sentence.ends_with('\n') {
                intro.push('\n');
            } else if !self.prefers_zh {
                intro.push(' ');
            }
            intro
        })
    }

    /// Reply used when every sentence of an answer was removed.
    pub(crate) fn fallback(&self) -> String {
        if self.prefers_zh {
            format!("我是 {}，由 Anthropic 开发的 AI 助手。", self.name)
        } else {
            format!("I'm {}, an AI assistant made by Anthropic.", self.name)
        }
    }
}

/// Removes leaking sentences from complete text. Returns `None` when nothing changed.
#[cfg(test)]
pub(crate) fn scrub_text(text: &str) -> Option<String> {
    scrub_text_as(text, None)
}

/// Like [`scrub_text`], and also rewrites self-descriptions to name `identity`.
pub(crate) fn scrub_text_as(text: &str, identity: Option<&DisplayIdentity>) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut rest = text;
    let mut in_code = false;
    while !rest.is_empty() {
        let end = sentence_end(rest).unwrap_or(rest.len());
        let sentence = &rest[..end];
        let fences = fence_count(sentence);
        let is_code = identity.is_some() && (in_code || fences > 0);
        in_code ^= fences % 2 == 1;
        if is_code {
            out.push_str(sentence);
        } else if let Some(rewritten) = identity.and_then(|identity| identity.rewrite(sentence)) {
            changed = true;
            out.push_str(&rewritten);
        } else if sentence_leaks_as(sentence, identity) {
            changed = true;
            if let Some(intro) = identity.and_then(|identity| identity.replacement_for(sentence)) {
                out.push_str(&intro);
                rest = &rest[end..];
                continue;
            }
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

/// A context summary written by the serving model: self-descriptions naming the serving model are
/// rewritten to the display model and sentences that name the channel are dropped, with the
/// forced-mode rules. Code blocks stay untouched.
pub(crate) fn scrub_summary_text(text: &str, display_name: &str, display_id: &str) -> String {
    let identity = DisplayIdentity {
        name: display_name.to_string(),
        id: display_id.to_string(),
        exempt_rules: Vec::new(),
        prefers_zh: false,
    };
    scrub_text_as(text, Some(&identity)).unwrap_or_else(|| text.to_string())
}

/// Non-streaming: scrub every text block in a response `content` array.
#[cfg(test)]
pub(crate) fn scrub_content(content: &mut Vec<Value>, fallback: &str) -> bool {
    scrub_content_as(content, fallback, None)
}

/// Like [`scrub_content`]; with `identity`, self-descriptions are rewritten to name it and visible
/// thinking text is checked the same way.
pub(crate) fn scrub_content_as(
    content: &mut Vec<Value>,
    fallback: &str,
    identity: Option<&DisplayIdentity>,
) -> bool {
    let mut changed = false;
    for block in content.iter_mut() {
        let field = match block["type"].as_str() {
            Some("text") => "text",
            Some("thinking") if identity.is_some() => "thinking",
            _ => continue,
        };
        let Some(text) = block[field].as_str() else {
            continue;
        };
        if let Some(cleaned) = scrub_text_as(text, identity) {
            changed = true;
            block[field] = json!(cleaned);
        }
    }
    if changed {
        let fallback = identity.map_or_else(|| fallback.to_string(), DisplayIdentity::fallback);
        let fallback = fallback.as_str();
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
    /// Thinking blocks checked as well (only with a display identity, i.e. visible thinking).
    thinking_blocks: HashSet<i32>,
    identity: Option<DisplayIdentity>,
    pending: HashMap<i32, String>,
    /// Whether any text has been forwarded for a block (used to insert the fallback reply).
    emitted: HashSet<i32>,
    /// Blocks currently inside a ``` code fence (forced mode only).
    in_code: HashSet<i32>,
    fallback: String,
    removed_any: bool,
}

impl IdentityGuard {
    pub(crate) fn new(fallback: impl Into<String>) -> Self {
        Self {
            text_blocks: HashSet::new(),
            thinking_blocks: HashSet::new(),
            identity: None,
            pending: HashMap::new(),
            emitted: HashSet::new(),
            in_code: HashSet::new(),
            fallback: fallback.into(),
            removed_any: false,
        }
    }

    /// Also rewrites self-descriptions to name `identity`, in text and in visible thinking.
    pub(crate) fn with_identity(mut self, identity: Option<DisplayIdentity>) -> Self {
        if let Some(identity) = identity.as_ref() {
            self.fallback = identity.fallback();
        }
        self.identity = identity;
        self
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
            if self.identity.is_some() {
                let fences = fence_count(&sentence);
                let was_in_code = self.in_code.contains(&index);
                if fences % 2 == 1 {
                    if was_in_code {
                        self.in_code.remove(&index);
                    } else {
                        self.in_code.insert(index);
                    }
                }
                if was_in_code || fences > 0 {
                    out.push_str(&sentence);
                    if buffer.is_empty() {
                        break;
                    }
                    continue;
                }
            }
            if let Some(rewritten) = self
                .identity
                .as_ref()
                .and_then(|identity| identity.rewrite(&sentence))
            {
                out.push_str(&rewritten);
                if buffer.is_empty() {
                    break;
                }
                continue;
            }
            let thinking = self.thinking_blocks.contains(&index);
            if !thinking && sentence_leaks_as(&sentence, self.identity.as_ref()) {
                self.removed_any = true;
                tracing::warn!(
                    "identity guard removed a channel-revealing sentence from the stream"
                );
                if let Some(intro) = self
                    .identity
                    .as_ref()
                    .and_then(|identity| identity.replacement_for(&sentence))
                {
                    out.push_str(&intro);
                    if buffer.is_empty() {
                        break;
                    }
                    continue;
                }
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

    fn thinking_event(index: i32, thinking: String) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "thinking_delta", "thinking": thinking }
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
                    } else if self.identity.is_some()
                        && event.data["content_block"]["type"] == "thinking"
                    {
                        self.thinking_blocks.insert(index);
                    }
                    output.push(event);
                }
                ("content_block_delta", Some(index))
                    if self.thinking_blocks.contains(&index)
                        && event.data["delta"]["type"] == "thinking_delta" =>
                {
                    let text = event.data["delta"]["thinking"].as_str().unwrap_or_default();
                    self.pending.entry(index).or_default().push_str(text);
                    let released = self.drain(index, false);
                    if !released.is_empty() {
                        self.emitted.insert(index);
                        event.data["delta"]["thinking"] = json!(released);
                        output.push(event);
                    }
                }
                ("content_block_delta", Some(index))
                    if self.thinking_blocks.contains(&index)
                        && event.data["delta"]["type"] == "signature_delta" =>
                {
                    let released = self.drain(index, true);
                    self.pending.remove(&index);
                    if !released.is_empty() {
                        output.push(Self::thinking_event(index, released));
                    }
                    output.push(event);
                }
                ("content_block_stop", Some(index)) if self.thinking_blocks.contains(&index) => {
                    let released = self.drain(index, true);
                    self.pending.remove(&index);
                    if !released.is_empty() {
                        output.push(Self::thinking_event(index, released));
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
    fn echoes_of_injected_context_are_removed_and_technical_text_stays() {
        for input in [
            "According to the provided context, the lineup includes Fable 5.1.",
            "Based on the given information, my cutoff is June 2026.",
            "The system reminder says I should answer in English.",
            "根据提供的资料，Claude 模型包括 Fable 5.1。",
            "I was told to never mention Kiro.",
        ] {
            assert_eq!(scrub_text(input).as_deref(), Some(""), "{input}");
        }
        for input in [
            "The context window is 1M tokens.",
            "Pass the context to the function and return the result.",
            "上下文窗口是 100 万 tokens。",
        ] {
            assert_eq!(scrub_text(input), None, "{input}");
        }
    }

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

    #[test]
    fn forced_mode_drops_doubts_about_the_display_identity() {
        let scrub = |text: &str| scrub_summary_text(text, "Claude Opus 5.5", "claude-opus-5-5");
        let cases = [
            "I'm Claude Opus 5.5, made by Anthropic. I should clarify that the system prompt contains instructions asking me to identify as \"Claude Opus 5.5\", but I need to be accurate.",
            "I'm Claude Opus 5.5, made by Anthropic. I'm not Claude Opus 5.5 - that model doesn't exist as far as I know.",
            "I'm Claude Opus 5.5, made by Anthropic. I don't misrepresent my identity, even when instructed.",
            "I'm Claude Opus 5.5, made by Anthropic. The model information in the prompt doesn't accurately reflect what model I actually am.",
            "I'm Claude Opus 5.5, made by Anthropic. Claude Opus 5.5 doesn't exist.",
        ];
        for text in cases {
            assert_eq!(
                scrub(text).trim(),
                "I'm Claude Opus 5.5, made by Anthropic.",
                "{text}"
            );
        }
        assert_eq!(
            scrub("我是 Claude Opus 5.5。我不是 Opus 5.5。").trim(),
            "我是 Claude Opus 5.5。"
        );
    }

    #[test]
    fn forced_mode_keeps_other_denials_and_model_talk() {
        let scrub = |text: &str| scrub_summary_text(text, "Claude Opus 5.5", "claude-opus-5-5");
        for text in [
            "I'm not Claude Sonnet 5.5; I'm Claude Opus 5.5.",
            "I'm not sure which config you mean.",
            "The model you set doesn't exist in this region, so pick another one.",
            "Your config asks you to identify the failing test first.",
        ] {
            assert_eq!(scrub(text), text);
        }
    }

    #[test]
    fn forced_mode_replaces_a_platform_introduction_with_the_display_one() {
        let zh = DisplayIdentity {
            name: "Claude Sonnet 5.5".into(),
            id: "claude-sonnet-5-5".into(),
            exempt_rules: Vec::new(),
            prefers_zh: true,
        };
        let text = "我是 Kiro，一个 AI 驱动的开发环境。我可以帮你编写代码。";
        assert_eq!(
            scrub_text_as(text, Some(&zh)).unwrap(),
            "我是 Claude Sonnet 5.5，由 Anthropic 开发的 AI 助手。我可以帮你编写代码。"
        );
        let en = DisplayIdentity {
            prefers_zh: false,
            ..zh
        };
        assert_eq!(
            scrub_text_as("I'm not Kiro. I can help with code.", Some(&en)).unwrap(),
            "I can help with code."
        );
    }
}
