use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::anthropic::types::{Message, MessagesRequest, SystemMessage, Tool};
use crate::token;

const DEFAULT_PROMPT_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const HOUR_PROMPT_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const DEFAULT_MIN_CACHEABLE_TOKENS: i32 = 1024;
const OPUS_MIN_CACHEABLE_TOKENS: i32 = 4096;

#[derive(Debug, Clone, Eq)]
pub struct PromptCacheScope {
    pub credential_id: u64,
    pub conversation_id: String,
    pub model: String,
}

impl PartialEq for PromptCacheScope {
    fn eq(&self, other: &Self) -> bool {
        self.credential_id == other.credential_id
            && self.conversation_id == other.conversation_id
            && self.model == other.model
    }
}

impl Hash for PromptCacheScope {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.credential_id.hash(state);
        self.conversation_id.hash(state);
        self.model.hash(state);
    }
}

#[derive(Debug, Clone, Copy)]
struct PromptCacheEntry {
    expires_at: DateTime<Utc>,
    ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct PromptCacheBreakpoint {
    fingerprint: [u8; 32],
    cumulative_tokens: i32,
    ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct PromptCacheProfile {
    breakpoints: Vec<PromptCacheBreakpoint>,
    total_input_tokens: i32,
    model: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromptCacheUsage {
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
}

#[derive(Debug, Default)]
pub struct PromptCacheTracker {
    entries: Mutex<HashMap<PromptCacheScope, HashMap<[u8; 32], PromptCacheEntry>>>,
}

impl PromptCacheTracker {
    pub fn build_profile(
        &self,
        req: &MessagesRequest,
        total_input_tokens: i32,
    ) -> Option<PromptCacheProfile> {
        let blocks = flatten_cache_blocks(req);
        if blocks.is_empty() {
            return None;
        }

        let mut hasher = Sha256::new();
        let mut breakpoints = Vec::new();
        let mut cumulative_tokens = 0;
        let mut active_ttl: Option<Duration> = None;

        for block in blocks {
            let canonical = canonicalize_cache_value(&block.value);
            write_hash_chunk(&mut hasher, &canonical);
            cumulative_tokens += block.tokens;

            let breakpoint_ttl = if let Some(ttl) = block.ttl {
                active_ttl = Some(ttl);
                Some(ttl)
            } else if block.is_message_end {
                active_ttl
            } else {
                None
            };

            let Some(ttl) = breakpoint_ttl else {
                continue;
            };

            let digest = hasher.clone().finalize();
            let mut fingerprint = [0_u8; 32];
            fingerprint.copy_from_slice(&digest[..]);
            breakpoints.push(PromptCacheBreakpoint {
                fingerprint,
                cumulative_tokens,
                ttl,
            });
        }

        if breakpoints.is_empty() {
            return None;
        }

        Some(PromptCacheProfile {
            breakpoints,
            total_input_tokens: total_input_tokens.max(cumulative_tokens),
            model: req.model.clone(),
        })
    }

    pub fn compute(
        &self,
        scope: Option<PromptCacheScope>,
        profile: Option<&PromptCacheProfile>,
    ) -> PromptCacheUsage {
        let Some(scope) = scope else {
            return PromptCacheUsage::default();
        };
        let Some(profile) = profile else {
            return PromptCacheUsage::default();
        };
        if profile.breakpoints.is_empty() {
            return PromptCacheUsage::default();
        }

        let min_tokens = min_cacheable_tokens_for_model(&profile.model);
        let last = profile.breakpoints.last().unwrap();
        let mut last_tokens = last.cumulative_tokens.min(profile.total_input_tokens);
        let now = Utc::now();

        let mut entries_by_scope = self.entries.lock();
        prune_expired_locked(&mut entries_by_scope, now);

        let Some(entries) = entries_by_scope.get_mut(&scope) else {
            let effective_creation = if last_tokens >= min_tokens {
                last_tokens
            } else {
                0
            };
            let (cache5m, cache1h) = compute_ttl_breakdown(profile, 0);
            return PromptCacheUsage {
                cache_creation_input_tokens: effective_creation,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: cache5m,
                cache_creation_1h_input_tokens: cache1h,
            };
        };

        let max_cacheable = ((profile.total_input_tokens as f64) * 0.85).round() as i32;
        if last_tokens > max_cacheable {
            last_tokens = max_cacheable;
        }

        let mut matched_tokens = 0;
        for breakpoint in profile.breakpoints.iter().rev() {
            if breakpoint.cumulative_tokens < min_tokens {
                continue;
            }
            let Some(entry) = entries.get_mut(&breakpoint.fingerprint) else {
                continue;
            };
            if entry.expires_at <= now {
                continue;
            }
            entry.expires_at = now + chrono::Duration::from_std(entry.ttl).unwrap_or_default();
            matched_tokens = breakpoint
                .cumulative_tokens
                .min(profile.total_input_tokens)
                .min(last_tokens);
            break;
        }

        let creation = (last_tokens - matched_tokens).max(0);
        let (cache5m, cache1h) = compute_ttl_breakdown(profile, matched_tokens);
        PromptCacheUsage {
            cache_creation_input_tokens: creation,
            cache_read_input_tokens: matched_tokens,
            cache_creation_5m_input_tokens: cache5m,
            cache_creation_1h_input_tokens: cache1h,
        }
    }

    pub fn update(&self, scope: Option<PromptCacheScope>, profile: Option<&PromptCacheProfile>) {
        let Some(scope) = scope else {
            return;
        };
        let Some(profile) = profile else {
            return;
        };
        if profile.breakpoints.is_empty() {
            return;
        }

        let min_tokens = min_cacheable_tokens_for_model(&profile.model);
        let now = Utc::now();
        let mut entries_by_scope = self.entries.lock();
        prune_expired_locked(&mut entries_by_scope, now);
        let entries = entries_by_scope.entry(scope).or_default();

        for breakpoint in &profile.breakpoints {
            if breakpoint.cumulative_tokens < min_tokens {
                continue;
            }
            entries.insert(
                breakpoint.fingerprint,
                PromptCacheEntry {
                    expires_at: now
                        + chrono::Duration::from_std(breakpoint.ttl).unwrap_or_default(),
                    ttl: breakpoint.ttl,
                },
            );
        }
    }

    pub fn clear_credential(&self, credential_id: u64) {
        self.entries
            .lock()
            .retain(|scope, _| scope.credential_id != credential_id);
    }
}

#[derive(Debug)]
struct CacheBlock {
    value: Value,
    tokens: i32,
    ttl: Option<Duration>,
    is_message_end: bool,
}

fn flatten_cache_blocks(req: &MessagesRequest) -> Vec<CacheBlock> {
    let mut blocks = Vec::new();
    let prelude = serde_json::json!({
        "kind": "request_prelude",
        "model": req.model,
        "tool_choice": req.tool_choice,
    });
    append_cache_block(&mut blocks, prelude, false);

    if let Some(tools) = &req.tools {
        for tool in tools {
            append_tool_block(&mut blocks, tool);
        }
    }

    if let Some(system) = &req.system {
        for block in system {
            append_system_block(&mut blocks, block);
        }
    }

    for msg in &req.messages {
        append_message_blocks(&mut blocks, msg);
    }

    blocks
}

fn append_tool_block(blocks: &mut Vec<CacheBlock>, tool: &Tool) {
    let value = serde_json::json!({
        "kind": "tool",
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
    });
    append_cache_block(blocks, value, false);
}

fn append_system_block(blocks: &mut Vec<CacheBlock>, system: &SystemMessage) {
    let mut block = serde_json::json!({
        "type": "text",
        "text": system.text,
    });
    if let Some(cache_control) = &system.cache_control {
        block["cache_control"] = cache_control.clone();
    }

    let value = serde_json::json!({
        "kind": "system",
        "block": block
    });
    append_cache_block(blocks, value, false);
}

fn append_message_blocks(blocks: &mut Vec<CacheBlock>, msg: &Message) {
    match &msg.content {
        Value::String(text) => {
            let value = serde_json::json!({
                "kind": "message",
                "role": msg.role,
                "block": {
                    "type": "text",
                    "text": text,
                }
            });
            append_cache_block(blocks, value, true);
        }
        Value::Array(items) => {
            let last_idx = items.len().saturating_sub(1);
            for (idx, item) in items.iter().enumerate() {
                let value = serde_json::json!({
                    "kind": "message",
                    "role": msg.role,
                    "block": item,
                });
                append_cache_block(blocks, value, idx == last_idx);
            }
        }
        other if !other.is_null() => {
            let value = serde_json::json!({
                "kind": "message",
                "role": msg.role,
                "block": other,
            });
            append_cache_block(blocks, value, true);
        }
        _ => {}
    }
}

fn append_cache_block(blocks: &mut Vec<CacheBlock>, value: Value, is_message_end: bool) {
    let block_value = value.get("block").unwrap_or(&value);
    if is_anthropic_billing_header_block(block_value) {
        return;
    }

    let ttl = extract_prompt_cache_ttl(block_value);
    let canonical = canonicalize_cache_value(&value);
    let tokens = token::count_tokens(&canonical) as i32;
    blocks.push(CacheBlock {
        value,
        tokens,
        ttl,
        is_message_end,
    });
}

fn extract_prompt_cache_ttl(value: &Value) -> Option<Duration> {
    let cache = value.get("cache_control")?;
    if cache.is_null() {
        return None;
    }
    let cache_type = cache.get("type").and_then(|v| v.as_str())?;
    if !cache_type.eq_ignore_ascii_case("ephemeral") {
        return None;
    }
    let ttl = cache
        .get("ttl")
        .and_then(parse_ttl)
        .unwrap_or(DEFAULT_PROMPT_CACHE_TTL);
    Some(normalize_ttl(ttl))
}

fn parse_ttl(value: &Value) -> Option<Duration> {
    match value {
        Value::Number(n) => n.as_u64().map(Duration::from_secs),
        Value::String(s) => {
            let trimmed = s.trim().to_lowercase();
            if let Some(raw) = trimmed.strip_suffix('h') {
                raw.parse::<u64>()
                    .ok()
                    .map(|hours| Duration::from_secs(hours * 60 * 60))
            } else if let Some(raw) = trimmed.strip_suffix('m') {
                raw.parse::<u64>()
                    .ok()
                    .map(|mins| Duration::from_secs(mins * 60))
            } else if let Some(raw) = trimmed.strip_suffix('s') {
                raw.parse::<u64>().ok().map(Duration::from_secs)
            } else {
                trimmed.parse::<u64>().ok().map(Duration::from_secs)
            }
        }
        _ => None,
    }
}

fn normalize_ttl(ttl: Duration) -> Duration {
    if ttl > DEFAULT_PROMPT_CACHE_TTL {
        HOUR_PROMPT_CACHE_TTL
    } else {
        DEFAULT_PROMPT_CACHE_TTL
    }
}

fn compute_ttl_breakdown(profile: &PromptCacheProfile, matched_tokens: i32) -> (i32, i32) {
    let mut cache5m = 0;
    let mut cache1h = 0;
    let mut previous = matched_tokens;
    for breakpoint in &profile.breakpoints {
        let current = breakpoint.cumulative_tokens.min(profile.total_input_tokens);
        if current <= previous {
            continue;
        }
        let delta = current - previous;
        if breakpoint.ttl >= HOUR_PROMPT_CACHE_TTL {
            cache1h += delta;
        } else {
            cache5m += delta;
        }
        previous = current;
    }
    (cache5m, cache1h)
}

fn prune_expired_locked(
    entries_by_scope: &mut HashMap<PromptCacheScope, HashMap<[u8; 32], PromptCacheEntry>>,
    now: DateTime<Utc>,
) {
    entries_by_scope.retain(|_, entries| {
        entries.retain(|_, entry| entry.expires_at > now);
        !entries.is_empty()
    });
}

fn min_cacheable_tokens_for_model(model: &str) -> i32 {
    if model.to_lowercase().contains("opus") {
        OPUS_MIN_CACHEABLE_TOKENS
    } else {
        DEFAULT_MIN_CACHEABLE_TOKENS
    }
}

fn is_anthropic_billing_header_block(value: &Value) -> bool {
    if let Some(block) = value.get("block") {
        return is_anthropic_billing_header_block(block);
    }

    let Some(obj) = value.as_object() else {
        return false;
    };
    if let Some(kind) = obj.get("type").and_then(|v| v.as_str()) {
        if kind != "text" {
            return false;
        }
    }
    obj.get("text")
        .and_then(|v| v.as_str())
        .map(|text| {
            text.trim_start()
                .to_lowercase()
                .starts_with("x-anthropic-billing-header:")
        })
        .unwrap_or(false)
}

fn canonicalize_cache_value(value: &Value) -> String {
    let mut out = String::new();
    write_canonical_json(value, &mut out);
    out
}

fn write_canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
        Value::Number(v) => out.push_str(&v.to_string()),
        Value::String(v) => out.push_str(&serde_json::to_string(v).unwrap_or_default()),
        Value::Array(items) => {
            out.push('[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_canonical_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<_> = map
                .keys()
                .filter(|key| key.as_str() != "cache_control")
                .collect();
            keys.sort();
            for (idx, key) in keys.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key.as_str()).unwrap_or_default());
                out.push(':');
                if is_cache_position_key(key) {
                    out.push_str("null");
                } else if let Some(value) = map.get(*key) {
                    write_canonical_json(value, out);
                }
            }
            out.push('}');
        }
    }
}

fn is_cache_position_key(key: &str) -> bool {
    matches!(
        key,
        "tool_index" | "system_index" | "message_index" | "block_index"
    )
}

fn write_hash_chunk(hasher: &mut Sha256, chunk: &str) {
    hasher.update(chunk.len().to_string().as_bytes());
    hasher.update([0]);
    hasher.update(chunk.as_bytes());
    hasher.update([0]);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::anthropic::types::Metadata;

    fn long_text() -> String {
        "You are a helpful coding assistant with deep knowledge of Go, Rust, Python, and TypeScript. "
            .repeat(80)
    }

    fn request(system_text: String) -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 1024,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("hello"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: system_text,
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: Some(Metadata {
                user_id: Some("session".to_string()),
            }),
        }
    }

    #[test]
    fn first_request_creates_second_reads() {
        let tracker = PromptCacheTracker::default();
        let req = request(long_text());
        let profile = tracker.build_profile(&req, 4096);
        assert!(
            profile.is_none(),
            "system text wrapper has no cache_control"
        );

        let mut req = request(long_text());
        req.messages[0].content = json!([
            {
                "type": "text",
                "text": long_text(),
                "cache_control": {"type": "ephemeral"}
            }
        ]);
        let profile = tracker.build_profile(&req, 4096).unwrap();
        let scope = PromptCacheScope {
            credential_id: 1,
            conversation_id: "session-a".to_string(),
            model: req.model.clone(),
        };

        let first = tracker.compute(Some(scope.clone()), Some(&profile));
        assert!(first.cache_creation_input_tokens > 0);
        assert_eq!(first.cache_read_input_tokens, 0);

        tracker.update(Some(scope.clone()), Some(&profile));
        let second = tracker.compute(Some(scope), Some(&profile));
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn system_cache_control_creates_profile() {
        let tracker = PromptCacheTracker::default();
        let mut req = request(long_text());
        req.system = Some(vec![SystemMessage {
            text: long_text(),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]);

        let profile = tracker.build_profile(&req, 4096);
        assert!(profile.is_some());
    }

    #[test]
    fn credential_and_conversation_are_isolated() {
        let tracker = PromptCacheTracker::default();
        let mut req = request(long_text());
        req.messages[0].content = json!([
            {
                "type": "text",
                "text": long_text(),
                "cache_control": {"type": "ephemeral"}
            }
        ]);
        let profile = tracker.build_profile(&req, 4096).unwrap();
        let scope_a = PromptCacheScope {
            credential_id: 1,
            conversation_id: "a".to_string(),
            model: req.model.clone(),
        };
        let scope_b = PromptCacheScope {
            credential_id: 2,
            conversation_id: "a".to_string(),
            model: req.model.clone(),
        };
        tracker.update(Some(scope_a), Some(&profile));
        let usage = tracker.compute(Some(scope_b), Some(&profile));
        assert_eq!(usage.cache_read_input_tokens, 0);
    }

    #[test]
    fn billing_header_block_is_not_cacheable() {
        let tracker = PromptCacheTracker::default();
        let mut req = request(long_text());
        req.messages[0].content = json!([
            {
                "type": "text",
                "text": "x-anthropic-billing-header: cache-read=999999",
                "cache_control": {"type": "ephemeral"}
            }
        ]);

        assert!(
            tracker.build_profile(&req, 4096).is_none(),
            "synthetic billing header text must not create local cache entries"
        );
    }

    #[test]
    fn hour_ttl_breakdown_is_reported_separately() {
        let tracker = PromptCacheTracker::default();
        let mut req = request(long_text());
        req.messages[0].content = json!([
            {
                "type": "text",
                "text": long_text(),
                "cache_control": {"type": "ephemeral", "ttl": "1h"}
            }
        ]);
        let profile = tracker.build_profile(&req, 4096).unwrap();
        let scope = PromptCacheScope {
            credential_id: 1,
            conversation_id: "ttl-session".to_string(),
            model: req.model.clone(),
        };

        let usage = tracker.compute(Some(scope), Some(&profile));
        assert!(usage.cache_creation_input_tokens > 0);
        assert_eq!(usage.cache_creation_5m_input_tokens, 0);
        assert!(usage.cache_creation_1h_input_tokens > 0);
    }
}
