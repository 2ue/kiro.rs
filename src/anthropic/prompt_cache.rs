use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::anthropic::types::{Message, MessagesRequest, SystemMessage, Tool};
use crate::model::config::KiroRsToolCachePolicy;
use crate::token;

const DEFAULT_PROMPT_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const HOUR_PROMPT_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const DEFAULT_MIN_CACHEABLE_TOKENS: i32 = 1024;
const EXTENDED_MIN_CACHEABLE_TOKENS: i32 = 4096;
const HAIKU_3_MIN_CACHEABLE_TOKENS: i32 = 2048;
const TARGET_READ_RATIO_SPREAD: f64 = 0.03;
const DEFAULT_MAX_ENTRIES_PER_ACCOUNT: usize = 200;
const DEFAULT_MAX_ENTRIES_GLOBAL: usize = 20_000;
const DEFAULT_MAX_ENTRY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_ESTIMATED_BYTES_LIMIT: u64 = 256 * 1024 * 1024;
const ESTIMATED_ENTRY_BYTES: u64 = 256;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct PromptCacheScope {
    pub conversation_id: String,
    pub route_namespace: Option<String>,
}

impl PromptCacheScope {
    pub fn new(conversation_id: String, route_namespace: Option<String>) -> Self {
        Self {
            conversation_id,
            route_namespace,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PromptCacheEntry {
    expires_at: DateTime<Utc>,
    ttl: Duration,
    last_used_at: DateTime<Utc>,
    cached_tokens: i32,
}

#[derive(Debug, Clone)]
pub struct PromptCacheBreakpoint {
    ttl: Duration,
}

#[derive(Debug, Clone)]
struct PromptCacheLookupPoint {
    fingerprint: [u8; 32],
    cumulative_tokens: i32,
}

#[derive(Debug, Clone)]
pub struct PromptCacheProfile {
    breakpoints: Vec<PromptCacheBreakpoint>,
    lookup_points: Vec<PromptCacheLookupPoint>,
    total_input_tokens: i32,
    model: String,
}

impl PromptCacheProfile {
    pub(crate) fn cache_jitter_seed(&self) -> u64 {
        let Some(point) = self.lookup_points.last() else {
            return 0;
        };

        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&point.fingerprint[..8]);
        u64::from_be_bytes(bytes)
    }
}

#[derive(Debug, Clone)]
pub struct KiroRsToolPromptCachePlan {
    profile: Option<PromptCacheProfile>,
    usage: PromptCacheUsage,
    committed_cache_tokens: i32,
}

impl KiroRsToolPromptCachePlan {
    pub fn usage(&self) -> PromptCacheUsage {
        self.usage
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PromptCacheUsage {
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
    pub effective_cache_ratio: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct PromptCacheBounds {
    pub max_entries_per_account: usize,
    pub max_entries_global: usize,
    pub entry_ttl: Duration,
    pub estimated_bytes_limit: u64,
}

impl Default for PromptCacheBounds {
    fn default() -> Self {
        Self {
            max_entries_per_account: DEFAULT_MAX_ENTRIES_PER_ACCOUNT,
            max_entries_global: DEFAULT_MAX_ENTRIES_GLOBAL,
            entry_ttl: DEFAULT_MAX_ENTRY_TTL,
            estimated_bytes_limit: DEFAULT_ESTIMATED_BYTES_LIMIT,
        }
    }
}

impl PromptCacheBounds {
    pub fn from_config(
        max_entries_per_account: usize,
        max_entries_global: usize,
        entry_ttl_secs: u64,
        estimated_bytes_limit: u64,
    ) -> Self {
        Self {
            max_entries_per_account,
            max_entries_global,
            entry_ttl: Duration::from_secs(entry_ttl_secs.max(1)),
            estimated_bytes_limit,
        }
    }

    fn effective_ttl(self, ttl: Duration) -> Duration {
        ttl.min(self.entry_ttl)
    }

    fn max_entries_by_bytes(self) -> usize {
        if self.estimated_bytes_limit == 0 {
            return usize::MAX;
        }
        (self.estimated_bytes_limit / ESTIMATED_ENTRY_BYTES)
            .try_into()
            .unwrap_or(usize::MAX)
    }
}

#[derive(Debug, Default)]
pub struct PromptCacheTracker {
    entries: Mutex<HashMap<PromptCacheScope, HashMap<[u8; 32], PromptCacheEntry>>>,
}

impl PromptCacheTracker {
    #[allow(dead_code)]
    pub fn build_profile(
        &self,
        req: &MessagesRequest,
        total_input_tokens: i32,
    ) -> Option<PromptCacheProfile> {
        self.build_profile_with_policy(req, total_input_tokens, false, &req.model)
    }

    #[cfg(test)]
    pub fn build_high_cache_profile(
        &self,
        req: &MessagesRequest,
        total_input_tokens: i32,
    ) -> Option<PromptCacheProfile> {
        self.build_high_cache_profile_for_model(req, total_input_tokens, &req.model)
    }

    pub fn build_high_cache_profile_for_model(
        &self,
        req: &MessagesRequest,
        total_input_tokens: i32,
        cache_model: &str,
    ) -> Option<PromptCacheProfile> {
        self.build_profile_with_policy(req, total_input_tokens, true, cache_model)
    }

    pub fn compute_kiro_rs_tool_with_bounds(
        &self,
        scope: Option<PromptCacheScope>,
        req: &MessagesRequest,
        total_input_tokens: i32,
        cache_model: &str,
        bounds: PromptCacheBounds,
        policy: KiroRsToolCachePolicy,
    ) -> KiroRsToolPromptCachePlan {
        let policy = policy.normalized();
        let profile =
            self.build_kiro_rs_tool_profile_for_model(req, total_input_tokens, cache_model, policy);
        let usage =
            self.compute_kiro_rs_tool_profile_with_bounds(scope, profile.as_ref(), bounds, policy);
        KiroRsToolPromptCachePlan {
            profile,
            usage,
            committed_cache_tokens: usage
                .cache_read_input_tokens
                .saturating_add(usage.cache_creation_input_tokens)
                .max(0),
        }
    }

    pub fn commit_kiro_rs_tool_success_with_bounds(
        &self,
        scope: Option<PromptCacheScope>,
        plan: &KiroRsToolPromptCachePlan,
        bounds: PromptCacheBounds,
    ) {
        self.update_kiro_rs_tool_profile_with_bounds(
            scope,
            plan.profile.as_ref(),
            bounds,
            plan.committed_cache_tokens,
        );
    }

    fn build_kiro_rs_tool_profile_for_model(
        &self,
        req: &MessagesRequest,
        total_input_tokens: i32,
        cache_model: &str,
        policy: KiroRsToolCachePolicy,
    ) -> Option<PromptCacheProfile> {
        self.build_profile_with_blocks(
            kiro_rs_tool_cache_blocks(req, policy),
            total_input_tokens,
            cache_model,
            false,
            false,
        )
    }

    fn build_profile_with_policy(
        &self,
        req: &MessagesRequest,
        total_input_tokens: i32,
        synthesize_stable_prefix: bool,
        cache_model: &str,
    ) -> Option<PromptCacheProfile> {
        self.build_profile_with_blocks(
            flatten_cache_blocks(req),
            total_input_tokens,
            cache_model,
            synthesize_stable_prefix,
            true,
        )
    }

    fn build_profile_with_blocks(
        &self,
        blocks: Vec<CacheBlock>,
        total_input_tokens: i32,
        cache_model: &str,
        synthesize_stable_prefix: bool,
        lookup_every_block: bool,
    ) -> Option<PromptCacheProfile> {
        if blocks.is_empty() {
            return None;
        }

        let mut hasher = Sha256::new();
        let mut breakpoints = Vec::new();
        let mut lookup_points = Vec::new();
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

            let digest = hasher.clone().finalize();
            let mut fingerprint = [0_u8; 32];
            fingerprint.copy_from_slice(&digest[..]);
            if lookup_every_block || breakpoint_ttl.is_some() {
                lookup_points.push(PromptCacheLookupPoint {
                    fingerprint,
                    cumulative_tokens,
                });
            }

            let Some(ttl) = breakpoint_ttl else {
                continue;
            };

            breakpoints.push(PromptCacheBreakpoint { ttl });
        }

        if breakpoints.is_empty() && synthesize_stable_prefix {
            breakpoints.push(PromptCacheBreakpoint {
                ttl: DEFAULT_PROMPT_CACHE_TTL,
            });
        }

        if breakpoints.is_empty() {
            return None;
        }

        Some(PromptCacheProfile {
            breakpoints,
            lookup_points,
            total_input_tokens: total_input_tokens.max(cumulative_tokens),
            model: cache_model.to_string(),
        })
    }

    #[cfg(test)]
    pub fn compute(
        &self,
        scope: Option<PromptCacheScope>,
        profile: Option<&PromptCacheProfile>,
        target_read_ratio: f64,
    ) -> PromptCacheUsage {
        self.compute_with_bounds(
            scope,
            profile,
            target_read_ratio,
            PromptCacheBounds::default(),
        )
    }

    pub fn compute_with_bounds(
        &self,
        scope: Option<PromptCacheScope>,
        profile: Option<&PromptCacheProfile>,
        target_read_ratio: f64,
        bounds: PromptCacheBounds,
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
        let effective_ratio = effective_cache_read_ratio(profile, target_read_ratio);
        let target_tokens =
            target_cache_tokens(profile.total_input_tokens, effective_ratio, min_tokens);
        if target_tokens <= 0 {
            return PromptCacheUsage::default();
        }
        let now = Utc::now();

        let mut entries_by_scope = self.entries.lock();
        prune_expired_locked(&mut entries_by_scope, now);

        let Some(entries) = entries_by_scope.get_mut(&scope) else {
            let (cache5m, cache1h) = target_ttl_breakdown(profile, target_tokens);
            return PromptCacheUsage {
                cache_creation_input_tokens: target_tokens,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: cache5m,
                cache_creation_1h_input_tokens: cache1h,
                effective_cache_ratio: Some(effective_ratio),
            };
        };

        let mut matched_tokens = 0;
        for point in profile.lookup_points.iter().rev() {
            if point.cumulative_tokens < min_tokens {
                continue;
            }
            let Some(entry) = entries.get_mut(&point.fingerprint) else {
                continue;
            };
            if entry.expires_at <= now {
                continue;
            }
            entry.last_used_at = now;
            entry.expires_at = now
                + chrono::Duration::from_std(bounds.effective_ttl(entry.ttl)).unwrap_or_default();
            matched_tokens = entry.cached_tokens.min(target_tokens).max(0);
            break;
        }

        let creation = (target_tokens - matched_tokens).max(0);
        let (cache5m, cache1h) = target_ttl_breakdown(profile, creation);
        PromptCacheUsage {
            cache_creation_input_tokens: creation,
            cache_read_input_tokens: matched_tokens,
            cache_creation_5m_input_tokens: cache5m,
            cache_creation_1h_input_tokens: cache1h,
            effective_cache_ratio: Some(effective_ratio),
        }
    }

    #[cfg(test)]
    pub fn update(
        &self,
        scope: Option<PromptCacheScope>,
        profile: Option<&PromptCacheProfile>,
        target_read_ratio: f64,
    ) {
        self.update_with_bounds(
            scope,
            profile,
            target_read_ratio,
            PromptCacheBounds::default(),
        )
    }

    pub fn update_with_bounds(
        &self,
        scope: Option<PromptCacheScope>,
        profile: Option<&PromptCacheProfile>,
        target_read_ratio: f64,
        bounds: PromptCacheBounds,
    ) {
        let Some(scope) = scope else {
            return;
        };
        let Some(profile) = profile else {
            return;
        };
        if profile.breakpoints.is_empty() || profile.lookup_points.is_empty() {
            return;
        }

        let min_tokens = min_cacheable_tokens_for_model(&profile.model);
        let effective_ratio = effective_cache_read_ratio(profile, target_read_ratio);
        let target_tokens =
            target_cache_tokens(profile.total_input_tokens, effective_ratio, min_tokens);
        if target_tokens <= 0 {
            return;
        }

        let Some(flat_total_tokens) = profile
            .lookup_points
            .last()
            .map(|point| point.cumulative_tokens)
        else {
            return;
        };
        if flat_total_tokens <= 0 {
            return;
        }

        let ttl = bounds.effective_ttl(
            profile
                .breakpoints
                .last()
                .map(|breakpoint| breakpoint.ttl)
                .unwrap_or(DEFAULT_PROMPT_CACHE_TTL),
        );
        let now = Utc::now();
        let mut entries_by_scope = self.entries.lock();
        prune_expired_locked(&mut entries_by_scope, now);
        let entries = entries_by_scope.entry(scope).or_default();

        for point in &profile.lookup_points {
            let scaled_tokens = ((point.cumulative_tokens as f64 / flat_total_tokens as f64)
                * target_tokens as f64)
                .round() as i32;
            let cached_tokens = scaled_tokens.min(target_tokens).max(0);
            if cached_tokens < min_tokens {
                continue;
            }
            entries.insert(
                point.fingerprint,
                PromptCacheEntry {
                    expires_at: now + chrono::Duration::from_std(ttl).unwrap_or_default(),
                    ttl,
                    last_used_at: now,
                    cached_tokens,
                },
            );
        }
        enforce_cache_bounds_locked(&mut entries_by_scope, bounds);
    }

    fn compute_kiro_rs_tool_profile_with_bounds(
        &self,
        scope: Option<PromptCacheScope>,
        profile: Option<&PromptCacheProfile>,
        bounds: PromptCacheBounds,
        policy: KiroRsToolCachePolicy,
    ) -> PromptCacheUsage {
        let policy = policy.normalized();
        let Some(scope) = scope else {
            return PromptCacheUsage::default();
        };
        let Some(profile) = profile else {
            return PromptCacheUsage::default();
        };
        let Some(covered_tokens) = profile
            .lookup_points
            .last()
            .map(|point| point.cumulative_tokens.max(0))
        else {
            return PromptCacheUsage::default();
        };
        if covered_tokens <= 0 {
            return PromptCacheUsage::default();
        }
        let covered_tokens = apply_kiro_rs_tool_coverage_policy(covered_tokens, policy);
        if covered_tokens <= 0 {
            return PromptCacheUsage::default();
        }

        let now = Utc::now();
        let mut entries_by_scope = self.entries.lock();
        prune_expired_locked(&mut entries_by_scope, now);

        let mut read_tokens = 0;
        if let Some(entries) = entries_by_scope.get_mut(&scope) {
            for point in profile.lookup_points.iter().rev() {
                let Some(entry) = entries.get_mut(&point.fingerprint) else {
                    continue;
                };
                if entry.expires_at <= now {
                    continue;
                }
                entry.last_used_at = now;
                entry.expires_at = now
                    + chrono::Duration::from_std(bounds.effective_ttl(entry.ttl))
                        .unwrap_or_default();
                read_tokens = point
                    .cumulative_tokens
                    .min(entry.cached_tokens)
                    .min(covered_tokens)
                    .max(0);
                break;
            }
        }

        let mut creation = covered_tokens.saturating_sub(read_tokens).max(0);
        if !policy.incremental_create_enabled && read_tokens > 0 {
            creation = 0;
        }
        if policy.max_new_creation_tokens_per_request > 0 {
            creation = creation.min(policy.max_new_creation_tokens_per_request);
        }
        let (cache5m, cache1h) = target_ttl_breakdown(profile, creation);
        PromptCacheUsage {
            cache_creation_input_tokens: creation,
            cache_read_input_tokens: read_tokens,
            cache_creation_5m_input_tokens: cache5m,
            cache_creation_1h_input_tokens: cache1h,
            effective_cache_ratio: (profile.total_input_tokens > 0)
                .then(|| covered_tokens as f64 / profile.total_input_tokens as f64),
        }
    }

    fn update_kiro_rs_tool_profile_with_bounds(
        &self,
        scope: Option<PromptCacheScope>,
        profile: Option<&PromptCacheProfile>,
        bounds: PromptCacheBounds,
        committed_cache_tokens: i32,
    ) {
        let Some(scope) = scope else {
            return;
        };
        let Some(profile) = profile else {
            return;
        };
        let committed_cache_tokens = committed_cache_tokens.max(0);
        if profile.lookup_points.is_empty() || committed_cache_tokens <= 0 {
            return;
        }

        let ttl = bounds.effective_ttl(
            profile
                .breakpoints
                .last()
                .map(|breakpoint| breakpoint.ttl)
                .unwrap_or(DEFAULT_PROMPT_CACHE_TTL),
        );
        let now = Utc::now();
        let mut entries_by_scope = self.entries.lock();
        prune_expired_locked(&mut entries_by_scope, now);
        let entries = entries_by_scope.entry(scope).or_default();

        for point in &profile.lookup_points {
            let cached_tokens = point.cumulative_tokens.min(committed_cache_tokens).max(0);
            if cached_tokens <= 0 {
                continue;
            }
            entries.insert(
                point.fingerprint,
                PromptCacheEntry {
                    expires_at: now + chrono::Duration::from_std(ttl).unwrap_or_default(),
                    ttl,
                    last_used_at: now,
                    cached_tokens,
                },
            );
        }
        enforce_cache_bounds_locked(&mut entries_by_scope, bounds);
    }

    pub fn clear_credential(&self, credential_id: u64) {
        let _ = credential_id;
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.entries.lock().values().map(HashMap::len).sum()
    }
}

fn enforce_cache_bounds_locked(
    entries_by_scope: &mut HashMap<PromptCacheScope, HashMap<[u8; 32], PromptCacheEntry>>,
    bounds: PromptCacheBounds,
) {
    if entries_by_scope.is_empty() {
        return;
    }

    if bounds.max_entries_per_account > 0 {
        let scope_counts: Vec<_> = entries_by_scope
            .iter()
            .map(|(scope, entries)| (scope.clone(), entries.len()))
            .collect();
        for (target_scope, count) in scope_counts {
            let overflow = count.saturating_sub(bounds.max_entries_per_account);
            if overflow > 0 {
                remove_oldest_entries_locked(entries_by_scope, overflow, |scope| {
                    scope == &target_scope
                });
            }
        }
    }

    let global_entry_limit = if bounds.max_entries_global == 0 {
        usize::MAX
    } else {
        bounds.max_entries_global
    }
    .min(bounds.max_entries_by_bytes());
    let total = total_cache_entries(entries_by_scope);
    if total > global_entry_limit {
        remove_oldest_entries_locked(entries_by_scope, total - global_entry_limit, |_| true);
    }
}

fn total_cache_entries(
    entries_by_scope: &HashMap<PromptCacheScope, HashMap<[u8; 32], PromptCacheEntry>>,
) -> usize {
    entries_by_scope.values().map(HashMap::len).sum()
}

fn remove_oldest_entries_locked(
    entries_by_scope: &mut HashMap<PromptCacheScope, HashMap<[u8; 32], PromptCacheEntry>>,
    remove_count: usize,
    include_scope: impl Fn(&PromptCacheScope) -> bool,
) {
    if remove_count == 0 {
        return;
    }

    let mut candidates = Vec::new();
    for (scope, entries) in entries_by_scope.iter() {
        if !include_scope(scope) {
            continue;
        }
        for (fingerprint, entry) in entries {
            candidates.push((
                entry.last_used_at,
                entry.expires_at,
                scope.clone(),
                *fingerprint,
            ));
        }
    }
    candidates.sort_by_key(|(last_used_at, expires_at, scope, fingerprint)| {
        (
            *last_used_at,
            *expires_at,
            scope.conversation_id.clone(),
            scope.route_namespace.clone(),
            *fingerprint,
        )
    });

    for (_, _, scope, fingerprint) in candidates.into_iter().take(remove_count) {
        if let Some(entries) = entries_by_scope.get_mut(&scope) {
            entries.remove(&fingerprint);
        }
    }
    entries_by_scope.retain(|_, entries| !entries.is_empty());
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

fn kiro_rs_tool_cache_blocks(
    req: &MessagesRequest,
    policy: KiroRsToolCachePolicy,
) -> Vec<CacheBlock> {
    let mut blocks = Vec::new();
    let policy = policy.normalized();

    if let Some(tools) = &req.tools {
        for tool in tools {
            append_tool_block(&mut blocks, tool);
        }
    }

    if let Some(system) = &req.system {
        let start = system
            .iter()
            .position(|block| block.cache_control.is_some())
            .unwrap_or(0);
        for block in system.iter().skip(start) {
            append_system_block(&mut blocks, block);
        }
    }

    if let Some(block) = blocks.last_mut() {
        block.ttl = Some(block.ttl.unwrap_or(DEFAULT_PROMPT_CACHE_TTL));
    }

    let cache_current_user_prefix =
        policy.cache_current_user_stable_prefix && policy.current_user_stable_prefix_max_tokens > 0;
    let last_message_idx = req.messages.len().saturating_sub(1);
    for (idx, msg) in req.messages.iter().enumerate() {
        if cache_current_user_prefix
            && idx == last_message_idx
            && msg.role == "user"
            && !message_has_explicit_cache_control(msg)
        {
            append_kiro_rs_tool_current_user_stable_prefix_block(&mut blocks, msg, policy);
            continue;
        }
        append_kiro_rs_tool_message_blocks(&mut blocks, msg, idx != last_message_idx);
    }

    blocks
}

fn append_tool_block(blocks: &mut Vec<CacheBlock>, tool: &Tool) {
    let mut value = serde_json::json!({
        "kind": "tool",
        "type": tool.tool_type,
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
        "max_uses": tool.max_uses,
    });
    if let Some(cache_control) = &tool.cache_control {
        value["cache_control"] = cache_control.clone();
    }
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

fn append_kiro_rs_tool_message_blocks(
    blocks: &mut Vec<CacheBlock>,
    msg: &Message,
    auto_breakpoint_at_message_end: bool,
) {
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
            append_cache_block_with_forced_ttl(
                blocks,
                value,
                Some(DEFAULT_PROMPT_CACHE_TTL).filter(|_| auto_breakpoint_at_message_end),
            );
        }
        Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                let value = serde_json::json!({
                    "kind": "message",
                    "role": msg.role,
                    "block": item,
                });
                let explicit_ttl = item
                    .get("cache_control")
                    .and_then(extract_prompt_cache_ttl)
                    .or_else(|| {
                        (auto_breakpoint_at_message_end && idx + 1 == items.len())
                            .then_some(DEFAULT_PROMPT_CACHE_TTL)
                    });
                append_cache_block_with_forced_ttl(blocks, value, explicit_ttl);
            }
        }
        other if !other.is_null() => {
            let value = serde_json::json!({
                "kind": "message",
                "role": msg.role,
                "block": other,
            });
            let explicit_ttl = other
                .get("cache_control")
                .and_then(extract_prompt_cache_ttl)
                .or_else(|| auto_breakpoint_at_message_end.then_some(DEFAULT_PROMPT_CACHE_TTL));
            append_cache_block_with_forced_ttl(blocks, value, explicit_ttl);
        }
        _ => {}
    }
}

fn message_has_explicit_cache_control(msg: &Message) -> bool {
    match &msg.content {
        Value::Array(items) => items.iter().any(|item| item.get("cache_control").is_some()),
        other => other.get("cache_control").is_some(),
    }
}

fn append_kiro_rs_tool_current_user_stable_prefix_block(
    blocks: &mut Vec<CacheBlock>,
    msg: &Message,
    policy: KiroRsToolCachePolicy,
) {
    let max_tokens = policy.current_user_stable_prefix_max_tokens.max(0);
    if max_tokens <= 0 {
        return;
    }

    let Some(prefix_text) = current_user_stable_prefix_text(&msg.content, max_tokens) else {
        return;
    };
    if prefix_text.trim().is_empty() {
        return;
    }

    let value = serde_json::json!({
        "kind": "current_user_stable_prefix",
        "role": msg.role,
        "block": {
            "type": "text",
            "text": prefix_text,
        }
    });
    let Some(value) = shrink_current_user_prefix_value_to_token_cap(value, max_tokens) else {
        return;
    };
    append_cache_block_with_forced_ttl(blocks, value, Some(DEFAULT_PROMPT_CACHE_TTL));
}

fn shrink_current_user_prefix_value_to_token_cap(
    mut value: Value,
    max_tokens: i32,
) -> Option<Value> {
    if max_tokens <= 0 {
        return None;
    }
    loop {
        let canonical = canonicalize_cache_value(&value);
        if token::count_tokens(&canonical) as i32 <= max_tokens {
            return Some(value);
        }

        let text = value
            .get_mut("block")
            .and_then(|block| block.get_mut("text"))
            .and_then(|text| text.as_str())?
            .to_string();
        let next_len = text.len().saturating_mul(9) / 10;
        if next_len == 0 {
            return None;
        }
        let mut end = next_len;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        let next = text[..end].trim_end();
        if next.is_empty() {
            return None;
        }
        value["block"]["text"] = Value::String(next.to_string());
    }
}

fn current_user_stable_prefix_text(content: &Value, max_tokens: i32) -> Option<String> {
    match content {
        Value::String(text) => stable_prefix_text_with_token_cap(text, max_tokens),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(|item| {
                    (item.get("type").and_then(|value| value.as_str()) == Some("text"))
                        .then(|| item.get("text").and_then(|value| value.as_str()))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n");
            stable_prefix_text_with_token_cap(&text, max_tokens)
        }
        _ => None,
    }
}

fn stable_prefix_text_with_token_cap(text: &str, max_tokens: i32) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || max_tokens <= 0 {
        return None;
    }
    let full_tokens = token::count_tokens(trimmed) as i32;
    if full_tokens <= max_tokens {
        return Some(trimmed.to_string());
    }

    let max_chars = (((trimmed.len() as f64) * (max_tokens as f64 / full_tokens as f64)).floor()
        as usize)
        .max(1)
        .min(trimmed.len());
    let mut end = max_chars;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let mut prefix = trimmed[..end].trim_end().to_string();
    while !prefix.is_empty() && token::count_tokens(&prefix) as i32 > max_tokens {
        let next_len = prefix.len().saturating_mul(9) / 10;
        if next_len == 0 {
            return None;
        }
        let mut next_end = next_len.min(prefix.len());
        while next_end > 0 && !prefix.is_char_boundary(next_end) {
            next_end -= 1;
        }
        prefix = prefix[..next_end].trim_end().to_string();
    }
    (!prefix.is_empty()).then_some(prefix)
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

fn append_cache_block_with_forced_ttl(
    blocks: &mut Vec<CacheBlock>,
    value: Value,
    forced_ttl: Option<Duration>,
) {
    let block_value = value.get("block").unwrap_or(&value);
    if is_anthropic_billing_header_block(block_value) {
        return;
    }

    let ttl = forced_ttl.or_else(|| extract_prompt_cache_ttl(block_value));
    let canonical = canonicalize_cache_value(&value);
    let tokens = token::count_tokens(&canonical) as i32;
    blocks.push(CacheBlock {
        value,
        tokens,
        ttl,
        is_message_end: false,
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

fn target_cache_tokens(total_input_tokens: i32, target_read_ratio: f64, min_tokens: i32) -> i32 {
    if total_input_tokens <= 1 {
        0
    } else {
        let target =
            ((total_input_tokens as f64) * target_read_ratio.clamp(0.0, 0.99)).round() as i32;
        let target = target.clamp(0, total_input_tokens.saturating_sub(1));
        if target >= min_tokens { target } else { 0 }
    }
}

fn apply_kiro_rs_tool_coverage_policy(covered_tokens: i32, policy: KiroRsToolCachePolicy) -> i32 {
    let policy = policy.normalized();
    let mut covered_tokens = covered_tokens.max(0);
    if policy.coverage_ratio < 1.0 {
        covered_tokens = ((covered_tokens as f64) * policy.coverage_ratio).floor() as i32;
    }
    if policy.max_coverage_tokens > 0 {
        covered_tokens = covered_tokens.min(policy.max_coverage_tokens);
    }
    covered_tokens.max(0)
}

fn effective_cache_read_ratio(profile: &PromptCacheProfile, target_read_ratio: f64) -> f64 {
    let target = target_read_ratio.clamp(0.0, 0.99);
    if target <= 0.0 {
        return 0.0;
    }

    let low = (target - TARGET_READ_RATIO_SPREAD).max(0.0);
    let high = (target + TARGET_READ_RATIO_SPREAD).min(0.99);
    if high <= low {
        return low;
    }

    low + deterministic_ratio_unit(profile) * (high - low)
}

fn deterministic_ratio_unit(profile: &PromptCacheProfile) -> f64 {
    let Some(point) = profile.lookup_points.last() else {
        return 0.5;
    };

    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&point.fingerprint[..8]);
    let raw = u64::from_be_bytes(bytes);
    raw as f64 / u64::MAX as f64
}

fn target_ttl_breakdown(profile: &PromptCacheProfile, creation: i32) -> (i32, i32) {
    if creation <= 0 {
        return (0, 0);
    }
    let ttl = profile
        .breakpoints
        .last()
        .map(|breakpoint| breakpoint.ttl)
        .unwrap_or(DEFAULT_PROMPT_CACHE_TTL);
    if ttl >= HOUR_PROMPT_CACHE_TTL {
        (0, creation)
    } else {
        (creation, 0)
    }
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
    let model = model.to_lowercase().replace('_', "-");
    if model.contains("haiku") {
        if model.contains("3-5") || model.contains("3.5") {
            HAIKU_3_MIN_CACHEABLE_TOKENS
        } else {
            EXTENDED_MIN_CACHEABLE_TOKENS
        }
    } else if model.contains("opus")
        && (model == "opus"
            || model == "opusplan"
            || model.contains("4-5")
            || model.contains("4.5")
            || model.contains("4-6")
            || model.contains("4.6")
            || model.contains("4-7")
            || model.contains("4.7"))
    {
        EXTENDED_MIN_CACHEABLE_TOKENS
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

pub(crate) fn canonicalize_cache_value(value: &Value) -> String {
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
                if is_cache_position_key(key) || is_cache_volatile_key(key, map.get(*key), map) {
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

fn is_cache_volatile_key(
    key: &str,
    value: Option<&Value>,
    object: &serde_json::Map<String, Value>,
) -> bool {
    match key {
        "tool_use_id" | "toolUseId" | "request_id" | "requestId" | "message_id" | "messageId" => {
            true
        }
        "id" => value
            .and_then(Value::as_str)
            .is_some_and(|id| is_cache_volatile_id_value(id, object)),
        _ => false,
    }
}

fn is_cache_volatile_id_value(id: &str, object: &serde_json::Map<String, Value>) -> bool {
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    matches!(kind, "tool_use" | "tool_result" | "message")
        || id.starts_with("toolu_")
        || id.starts_with("srvtoolu_")
        || id.starts_with("msg_")
        || id.starts_with("req_")
        || looks_like_uuid(id)
}

fn looks_like_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (idx, byte) in bytes.iter().enumerate() {
        if matches!(idx, 8 | 13 | 18 | 23) {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
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
    use std::collections::HashMap;

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

    fn test_scope(conversation_id: &str) -> PromptCacheScope {
        PromptCacheScope::new(conversation_id.to_string(), None)
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
        let scope = test_scope("session-a");

        let first = tracker.compute(Some(scope.clone()), Some(&profile), 0.85);
        assert!(first.cache_creation_input_tokens > 0);
        assert_eq!(first.cache_read_input_tokens, 0);

        tracker.update(Some(scope.clone()), Some(&profile), 0.85);
        let second = tracker.compute(Some(scope), Some(&profile), 0.85);
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn high_cache_profile_creates_without_explicit_cache_control() {
        let tracker = PromptCacheTracker::default();
        let req = request(long_text());
        let profile = tracker
            .build_high_cache_profile(&req, 4096)
            .expect("high-cache should synthesize a stable-prefix breakpoint");
        let scope = test_scope("high-cache-session");

        let first = tracker.compute(Some(scope.clone()), Some(&profile), 0.95);
        assert!(first.cache_creation_input_tokens > 0);
        assert_eq!(first.cache_read_input_tokens, 0);

        tracker.update(Some(scope.clone()), Some(&profile), 0.95);
        let second = tracker.compute(Some(scope), Some(&profile), 0.95);
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn min_cacheable_tokens_are_model_specific() {
        assert_eq!(
            min_cacheable_tokens_for_model("claude-haiku-4-5-20251001"),
            4096
        );
        assert_eq!(min_cacheable_tokens_for_model("claude-haiku-4.5"), 4096);
        assert_eq!(min_cacheable_tokens_for_model("claude-haiku-3.5"), 2048);
        assert_eq!(
            min_cacheable_tokens_for_model("claude-sonnet-4-5-20250929"),
            1024
        );
        assert_eq!(min_cacheable_tokens_for_model("claude-opus-4-7"), 4096);
    }

    #[test]
    fn high_cache_profile_uses_resolved_upstream_model_threshold() {
        let tracker = PromptCacheTracker::default();
        let mut req = request(long_text());
        req.model = "haiku".to_string();
        let profile = tracker
            .build_high_cache_profile_for_model(&req, 3_500, "claude-haiku-4-5-20251001")
            .expect("high-cache should still build a profile for supported Haiku");
        let scope = test_scope("haiku-threshold-session");

        let short = tracker.compute(Some(scope.clone()), Some(&profile), 0.95);
        assert_eq!(
            short.cache_creation_input_tokens, 0,
            "Haiku 4.5 must not simulate cache below its 4096 token minimum"
        );

        let profile = tracker
            .build_high_cache_profile_for_model(&req, 8_000, "claude-haiku-4-5-20251001")
            .expect("high-cache should build a profile for a longer Haiku request");
        let long = tracker.compute(Some(scope), Some(&profile), 0.95);
        assert!(
            long.cache_creation_input_tokens >= 4096,
            "Haiku 4.5 should simulate cache only once the request clears the official minimum"
        );
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
    fn tool_cache_control_creates_profile_and_second_request_reads() {
        let tracker = PromptCacheTracker::default();
        let mut req = request("short system".to_string());
        req.tools = Some(vec![Tool {
            tool_type: None,
            name: "Read".to_string(),
            description: long_text(),
            input_schema: HashMap::new(),
            max_uses: None,
            cache_control: Some(json!({"type": "ephemeral"})),
        }]);
        let profile = tracker
            .build_profile(&req, 4096)
            .expect("tool cache_control should create a cache profile");
        let scope = test_scope("tool-cache-session");

        let first = tracker.compute(Some(scope.clone()), Some(&profile), 0.85);
        assert!(first.cache_creation_input_tokens > 0);
        assert_eq!(first.cache_read_input_tokens, 0);

        tracker.update(Some(scope.clone()), Some(&profile), 0.85);
        let second = tracker.compute(Some(scope), Some(&profile), 0.85);
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn current_scope_is_session_only_across_credentials_and_models() {
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
        let scope_a = test_scope("a");
        let scope_b = test_scope("a");
        tracker.update(Some(scope_a), Some(&profile), 0.85);
        let usage = tracker.compute(Some(scope_b), Some(&profile), 0.85);
        assert!(usage.cache_read_input_tokens > 0);
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
        let scope = test_scope("ttl-session");

        let usage = tracker.compute(Some(scope), Some(&profile), 0.85);
        assert!(usage.cache_creation_input_tokens > 0);
        assert_eq!(usage.cache_creation_5m_input_tokens, 0);
        assert!(usage.cache_creation_1h_input_tokens > 0);
    }

    #[test]
    fn growing_conversation_reads_previous_prefix_when_cache_control_moves_forward() {
        let tracker = PromptCacheTracker::default();
        let scope = test_scope("growing-session");
        let shared_prefix = "stable project context and tool transcript ".repeat(500);
        let new_tail = "new user turn and assistant result ".repeat(500);

        let mut first_req = request(long_text());
        first_req.messages[0].content = json!([
            {
                "type": "text",
                "text": shared_prefix,
                "cache_control": {"type": "ephemeral"}
            }
        ]);
        let first_profile = tracker.build_profile(&first_req, 4096).unwrap();
        let first = tracker.compute(Some(scope.clone()), Some(&first_profile), 0.85);
        assert!(first.cache_creation_input_tokens > 0);
        assert_eq!(first.cache_read_input_tokens, 0);
        tracker.update(Some(scope.clone()), Some(&first_profile), 0.85);

        let mut second_req = request(long_text());
        second_req.messages[0].content = json!([
            {
                "type": "text",
                "text": shared_prefix
            },
            {
                "type": "text",
                "text": new_tail,
                "cache_control": {"type": "ephemeral"}
            }
        ]);
        let second_profile = tracker.build_profile(&second_req, 8192).unwrap();
        let second = tracker.compute(Some(scope), Some(&second_profile), 0.85);

        assert!(
            second.cache_read_input_tokens > 0,
            "the previous cache breakpoint should be reusable even when the new cache_control marker moves forward"
        );
        assert!(
            second.cache_creation_input_tokens > 0,
            "only the newly extended prefix should be created"
        );
    }

    #[test]
    fn local_prompt_cache_can_target_ninety_five_percent_without_cross_scope_reads() {
        let tracker = PromptCacheTracker::default();
        let scope = test_scope("target-ratio-session");
        let mut req = request("cacheable target ratio block ".repeat(4000));
        req.messages[0].content = json!([
            {
                "type": "text",
                "text": "cacheable target ratio block ".repeat(4000),
                "cache_control": {"type": "ephemeral"}
            }
        ]);
        let profile = tracker.build_profile(&req, 120_000).unwrap();

        let first = tracker.compute(Some(scope.clone()), Some(&profile), 0.95);
        assert!(first.cache_creation_input_tokens >= 110_000);
        assert_eq!(first.cache_read_input_tokens, 0);

        tracker.update(Some(scope.clone()), Some(&profile), 0.95);
        let second = tracker.compute(Some(scope.clone()), Some(&profile), 0.95);
        assert!(second.cache_read_input_tokens >= 110_000);
        assert_eq!(second.cache_creation_input_tokens, 0);

        let same_session = test_scope("target-ratio-session");
        let reused = tracker.compute(Some(same_session), Some(&profile), 0.95);
        assert!(
            reused.cache_read_input_tokens >= 110_000,
            "local prompt cache is session-scoped and should reuse across credential/model changes"
        );
    }

    #[test]
    fn route_namespace_prevents_cross_route_cache_reads() {
        let tracker = PromptCacheTracker::default();
        let req = request("route namespace cache block ".repeat(4000));
        let profile = tracker
            .build_high_cache_profile(&req, 120_000)
            .expect("high-cache profile");
        let default_scope = test_scope("route-namespace-session");
        let custom_scope = PromptCacheScope {
            route_namespace: Some("/dfcache/team-a".to_string()),
            ..default_scope.clone()
        };

        tracker.update(Some(default_scope.clone()), Some(&profile), 0.95);

        let default_usage = tracker.compute(Some(default_scope), Some(&profile), 0.95);
        let custom_usage = tracker.compute(Some(custom_scope), Some(&profile), 0.95);

        assert!(default_usage.cache_read_input_tokens > 0);
        assert_eq!(custom_usage.cache_read_input_tokens, 0);
        assert!(custom_usage.cache_creation_input_tokens > 0);
    }

    #[test]
    fn kiro_rs_tool_first_miss_then_success_commit_hits() {
        let tracker = PromptCacheTracker::default();
        let scope = test_scope("kiro-session");
        let mut first_req = request("stable system prompt ".repeat(300));
        first_req.messages = vec![
            Message {
                role: "user".to_string(),
                content: json!("stable history question"),
            },
            Message {
                role: "assistant".to_string(),
                content: json!("stable history answer"),
            },
            Message {
                role: "user".to_string(),
                content: json!("current question"),
            },
        ];

        let first = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope.clone()),
            &first_req,
            12_000,
            &first_req.model,
            PromptCacheBounds::default(),
            KiroRsToolCachePolicy::default(),
        );
        assert!(first.usage().cache_creation_input_tokens > 0);
        assert_eq!(first.usage().cache_read_input_tokens, 0);

        tracker.commit_kiro_rs_tool_success_with_bounds(
            Some(scope.clone()),
            &first,
            PromptCacheBounds::default(),
        );

        let second = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope),
            &first_req,
            12_000,
            &first_req.model,
            PromptCacheBounds::default(),
            KiroRsToolCachePolicy::default(),
        );
        assert!(second.usage().cache_read_input_tokens > 0);
        assert_eq!(second.usage().cache_creation_input_tokens, 0);
    }

    #[test]
    fn kiro_rs_tool_failed_request_does_not_commit_segments() {
        let tracker = PromptCacheTracker::default();
        let scope = test_scope("kiro-failed-session");
        let mut req = request("stable system prompt ".repeat(300));
        req.messages = vec![
            Message {
                role: "user".to_string(),
                content: json!("stable history question"),
            },
            Message {
                role: "assistant".to_string(),
                content: json!("stable history answer"),
            },
            Message {
                role: "user".to_string(),
                content: json!("current question"),
            },
        ];

        let failed = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope.clone()),
            &req,
            12_000,
            &req.model,
            PromptCacheBounds::default(),
            KiroRsToolCachePolicy::default(),
        );
        assert!(failed.usage().cache_creation_input_tokens > 0);
        assert_eq!(failed.usage().cache_read_input_tokens, 0);

        let retry = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope),
            &req,
            12_000,
            &req.model,
            PromptCacheBounds::default(),
            KiroRsToolCachePolicy::default(),
        );
        assert_eq!(retry.usage().cache_read_input_tokens, 0);
        assert!(retry.usage().cache_creation_input_tokens > 0);
    }

    #[test]
    fn kiro_rs_tool_skips_dynamic_system_before_first_cache_control() {
        let tracker = PromptCacheTracker::default();
        let scope = test_scope("kiro-dynamic-system");
        let make_req = |dynamic: &str| MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 1024,
            messages: vec![
                Message {
                    role: "user".to_string(),
                    content: json!("stable history question"),
                },
                Message {
                    role: "assistant".to_string(),
                    content: json!("stable history answer"),
                },
                Message {
                    role: "user".to_string(),
                    content: json!("current question"),
                },
            ],
            stream: false,
            system: Some(vec![
                SystemMessage {
                    text: dynamic.to_string(),
                    cache_control: None,
                },
                SystemMessage {
                    text: "stable cached system ".repeat(300),
                    cache_control: Some(json!({"type": "ephemeral"})),
                },
            ]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let first_req = make_req("time=1000");
        let first = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope.clone()),
            &first_req,
            12_000,
            &first_req.model,
            PromptCacheBounds::default(),
            KiroRsToolCachePolicy::default(),
        );
        tracker.commit_kiro_rs_tool_success_with_bounds(
            Some(scope.clone()),
            &first,
            PromptCacheBounds::default(),
        );

        let second_req = make_req("time=2000");
        let second = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope),
            &second_req,
            12_000,
            &second_req.model,
            PromptCacheBounds::default(),
            KiroRsToolCachePolicy::default(),
        );
        assert!(
            second.usage().cache_read_input_tokens > 0,
            "dynamic system text before the first cache_control must not poison Kiro-RS-Tool cache hits"
        );
    }

    #[test]
    fn kiro_rs_tool_single_current_message_without_prefix_is_uncached() {
        let tracker = PromptCacheTracker::default();
        let req = MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 1024,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("only current question"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let plan = tracker.compute_kiro_rs_tool_with_bounds(
            Some(test_scope("kiro-single-message")),
            &req,
            1_000,
            &req.model,
            PromptCacheBounds::default(),
            KiroRsToolCachePolicy::default(),
        );

        assert_eq!(plan.usage().cache_read_input_tokens, 0);
        assert_eq!(plan.usage().cache_creation_input_tokens, 0);
    }

    #[test]
    fn kiro_rs_tool_cache_respects_global_entry_bounds() {
        let tracker = PromptCacheTracker::default();
        let bounds = PromptCacheBounds::from_config(10, 2, 3600, 0);
        for idx in 0..5 {
            let scope = test_scope(&format!("kiro-bounds-{idx}"));
            let mut req = request(format!("stable system prompt {idx} ").repeat(300));
            req.messages = vec![
                Message {
                    role: "user".to_string(),
                    content: json!(format!("stable history question {idx}")),
                },
                Message {
                    role: "assistant".to_string(),
                    content: json!(format!("stable history answer {idx}")),
                },
                Message {
                    role: "user".to_string(),
                    content: json!("current question"),
                },
            ];
            let plan = tracker.compute_kiro_rs_tool_with_bounds(
                Some(scope.clone()),
                &req,
                12_000,
                &req.model,
                bounds,
                KiroRsToolCachePolicy::default(),
            );
            tracker.commit_kiro_rs_tool_success_with_bounds(Some(scope), &plan, bounds);
        }

        assert!(tracker.entry_count() <= 2);
    }

    #[test]
    fn kiro_rs_tool_coverage_ratio_caps_committed_reads() {
        let tracker = PromptCacheTracker::default();
        let scope = test_scope("kiro-coverage-ratio");
        let mut req = request("stable system prompt ".repeat(300));
        req.messages = vec![
            Message {
                role: "user".to_string(),
                content: json!("stable history question"),
            },
            Message {
                role: "assistant".to_string(),
                content: json!("stable history answer"),
            },
            Message {
                role: "user".to_string(),
                content: json!("current question"),
            },
        ];
        let policy = KiroRsToolCachePolicy {
            coverage_ratio: 0.5,
            ..KiroRsToolCachePolicy::default()
        };

        let first = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope.clone()),
            &req,
            12_000,
            &req.model,
            PromptCacheBounds::default(),
            policy,
        );
        let first_create = first.usage().cache_creation_input_tokens;
        assert!(first_create > 0);
        assert_eq!(first.usage().cache_read_input_tokens, 0);
        tracker.commit_kiro_rs_tool_success_with_bounds(
            Some(scope.clone()),
            &first,
            PromptCacheBounds::default(),
        );

        let second = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope),
            &req,
            12_000,
            &req.model,
            PromptCacheBounds::default(),
            policy,
        );
        assert_eq!(second.usage().cache_read_input_tokens, first_create);
        assert_eq!(second.usage().cache_creation_input_tokens, 0);
    }

    #[test]
    fn kiro_rs_tool_creation_cap_commits_only_created_tokens() {
        let tracker = PromptCacheTracker::default();
        let scope = test_scope("kiro-creation-cap");
        let mut req = request("stable system prompt with enough tokens ".repeat(2000));
        req.messages = vec![
            Message {
                role: "user".to_string(),
                content: json!("stable history question"),
            },
            Message {
                role: "assistant".to_string(),
                content: json!("stable history answer"),
            },
            Message {
                role: "user".to_string(),
                content: json!("current question"),
            },
        ];
        let policy = KiroRsToolCachePolicy {
            max_new_creation_tokens_per_request: 2_048,
            ..KiroRsToolCachePolicy::default()
        };

        let first = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope.clone()),
            &req,
            20_000,
            &req.model,
            PromptCacheBounds::default(),
            policy,
        );
        assert_eq!(first.usage().cache_creation_input_tokens, 2_048);
        assert_eq!(first.usage().cache_read_input_tokens, 0);
        tracker.commit_kiro_rs_tool_success_with_bounds(
            Some(scope.clone()),
            &first,
            PromptCacheBounds::default(),
        );

        let second = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope),
            &req,
            20_000,
            &req.model,
            PromptCacheBounds::default(),
            policy,
        );
        assert_eq!(second.usage().cache_read_input_tokens, 2_048);
        assert_eq!(second.usage().cache_creation_input_tokens, 2_048);
    }

    #[test]
    fn kiro_rs_tool_can_disable_incremental_creation_after_read() {
        let tracker = PromptCacheTracker::default();
        let scope = test_scope("kiro-no-incremental-create");
        let mut req = request("stable system prompt with enough tokens ".repeat(2000));
        req.messages = vec![
            Message {
                role: "user".to_string(),
                content: json!("stable history question"),
            },
            Message {
                role: "assistant".to_string(),
                content: json!("stable history answer"),
            },
            Message {
                role: "user".to_string(),
                content: json!("current question"),
            },
        ];
        let first_policy = KiroRsToolCachePolicy {
            max_new_creation_tokens_per_request: 2_048,
            ..KiroRsToolCachePolicy::default()
        };
        let second_policy = KiroRsToolCachePolicy {
            incremental_create_enabled: false,
            ..KiroRsToolCachePolicy::default()
        };

        let first = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope.clone()),
            &req,
            20_000,
            &req.model,
            PromptCacheBounds::default(),
            first_policy,
        );
        tracker.commit_kiro_rs_tool_success_with_bounds(
            Some(scope.clone()),
            &first,
            PromptCacheBounds::default(),
        );

        let second = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope),
            &req,
            20_000,
            &req.model,
            PromptCacheBounds::default(),
            second_policy,
        );
        assert_eq!(second.usage().cache_read_input_tokens, 2_048);
        assert_eq!(second.usage().cache_creation_input_tokens, 0);
    }

    #[test]
    fn kiro_rs_tool_max_coverage_tokens_caps_default_creation() {
        let tracker = PromptCacheTracker::default();
        let scope = test_scope("kiro-max-coverage");
        let mut req = request("stable system prompt with enough tokens ".repeat(2000));
        req.messages = vec![
            Message {
                role: "user".to_string(),
                content: json!("stable history question"),
            },
            Message {
                role: "assistant".to_string(),
                content: json!("stable history answer"),
            },
            Message {
                role: "user".to_string(),
                content: json!("current question"),
            },
        ];
        let policy = KiroRsToolCachePolicy {
            max_coverage_tokens: 3_000,
            ..KiroRsToolCachePolicy::default()
        };

        let first = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope.clone()),
            &req,
            20_000,
            &req.model,
            PromptCacheBounds::default(),
            policy,
        );
        assert_eq!(first.usage().cache_creation_input_tokens, 3_000);
        tracker.commit_kiro_rs_tool_success_with_bounds(
            Some(scope.clone()),
            &first,
            PromptCacheBounds::default(),
        );

        let second = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope),
            &req,
            20_000,
            &req.model,
            PromptCacheBounds::default(),
            policy,
        );
        assert_eq!(second.usage().cache_read_input_tokens, 3_000);
        assert_eq!(second.usage().cache_creation_input_tokens, 0);
    }

    #[test]
    fn kiro_rs_tool_current_user_prefix_is_opt_in() {
        let tracker = PromptCacheTracker::default();
        let req = MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 1024,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("current question stable prefix ".repeat(600)),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let default_plan = tracker.compute_kiro_rs_tool_with_bounds(
            Some(test_scope("kiro-current-user-default")),
            &req,
            10_000,
            &req.model,
            PromptCacheBounds::default(),
            KiroRsToolCachePolicy::default(),
        );
        assert_eq!(default_plan.usage().cache_creation_input_tokens, 0);
        assert_eq!(default_plan.usage().cache_read_input_tokens, 0);

        let policy = KiroRsToolCachePolicy {
            cache_current_user_stable_prefix: true,
            current_user_stable_prefix_max_tokens: 1_500,
            ..KiroRsToolCachePolicy::default()
        };
        let scope = test_scope("kiro-current-user-opt-in");
        let first = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope.clone()),
            &req,
            10_000,
            &req.model,
            PromptCacheBounds::default(),
            policy,
        );
        assert!(first.usage().cache_creation_input_tokens > 0);
        assert!(first.usage().cache_creation_input_tokens <= 1_500);
        tracker.commit_kiro_rs_tool_success_with_bounds(
            Some(scope.clone()),
            &first,
            PromptCacheBounds::default(),
        );

        let second = tracker.compute_kiro_rs_tool_with_bounds(
            Some(scope),
            &req,
            10_000,
            &req.model,
            PromptCacheBounds::default(),
            policy,
        );
        assert_eq!(
            second.usage().cache_read_input_tokens,
            first.usage().cache_creation_input_tokens
        );
    }

    #[test]
    fn volatile_tool_use_ids_do_not_break_prompt_cache_fingerprint() {
        let tracker = PromptCacheTracker::default();
        let mut first_req = request("stable system prompt ".repeat(500));
        first_req.messages[0].content = json!([
            {
                "type": "tool_use",
                "id": "toolu_first",
                "name": "Read",
                "input": {"file_path": "Cargo.toml"}
            },
            {
                "type": "text",
                "text": "cacheable response context ".repeat(700),
                "cache_control": {"type": "ephemeral"}
            }
        ]);
        let mut second_req = first_req.clone();
        second_req.messages[0].content = json!([
            {
                "type": "tool_use",
                "id": "toolu_second",
                "name": "Read",
                "input": {"file_path": "Cargo.toml"}
            },
            {
                "type": "text",
                "text": "cacheable response context ".repeat(700),
                "cache_control": {"type": "ephemeral"}
            }
        ]);

        let first_profile = tracker
            .build_profile(&first_req, 8_192)
            .expect("first profile");
        let second_profile = tracker
            .build_profile(&second_req, 8_192)
            .expect("second profile");
        assert_eq!(
            first_profile.cache_jitter_seed(),
            second_profile.cache_jitter_seed()
        );

        let scope = test_scope("volatile-tool-id-session");
        tracker.update(Some(scope.clone()), Some(&first_profile), 0.85);
        let usage = tracker.compute(Some(scope), Some(&second_profile), 0.85);
        assert!(
            usage.cache_read_input_tokens > 0,
            "tool_use ids change between turns and must not invalidate the stable prompt prefix"
        );
    }

    #[test]
    fn user_text_content_is_not_treated_as_volatile_metadata() {
        let first = canonicalize_cache_value(&json!({
            "type": "text",
            "text": "The literal request_id req_first is part of user content."
        }));
        let second = canonicalize_cache_value(&json!({
            "type": "text",
            "text": "The literal request_id req_second is part of user content."
        }));
        assert_ne!(first, second);

        let stable_business_id = canonicalize_cache_value(&json!({
            "type": "text",
            "id": "customer-plan-a",
            "text": "same content"
        }));
        let changed_business_id = canonicalize_cache_value(&json!({
            "type": "text",
            "id": "customer-plan-b",
            "text": "same content"
        }));
        assert_ne!(stable_business_id, changed_business_id);
    }

    #[test]
    fn prompt_cache_enforces_per_scope_entry_limit() {
        let tracker = PromptCacheTracker::default();
        let bounds = PromptCacheBounds {
            max_entries_per_account: 2,
            max_entries_global: 100,
            entry_ttl: std::time::Duration::from_secs(3_600),
            estimated_bytes_limit: 0,
        };

        let scope = test_scope("bounded-session");
        for suffix in ["a", "b", "c"] {
            let mut req = request(format!("cacheable {suffix} ").repeat(900));
            req.messages[0].content = json!([
                {
                    "type": "text",
                    "text": format!("cacheable body {suffix} ").repeat(900),
                    "cache_control": {"type": "ephemeral"}
                }
            ]);
            let profile = tracker.build_profile(&req, 8_192).expect("profile");
            tracker.update_with_bounds(Some(scope.clone()), Some(&profile), 0.85, bounds);
        }

        assert!(
            tracker.entry_count() <= 2,
            "per-scope cache bound must cap retained fingerprints"
        );
    }

    #[test]
    fn prompt_cache_enforces_global_entry_limit_and_estimated_bytes_limit() {
        let tracker = PromptCacheTracker::default();
        let bounds = PromptCacheBounds {
            max_entries_per_account: 0,
            max_entries_global: 10,
            entry_ttl: std::time::Duration::from_secs(3_600),
            estimated_bytes_limit: 3 * ESTIMATED_ENTRY_BYTES,
        };

        for credential_id in 1..=6 {
            let mut req = request(format!("global cacheable {credential_id} ").repeat(900));
            req.messages[0].content = json!([
                {
                    "type": "text",
                    "text": format!("global cacheable body {credential_id} ").repeat(900),
                    "cache_control": {"type": "ephemeral"}
                }
            ]);
            let profile = tracker.build_profile(&req, 8_192).expect("profile");
            let scope = test_scope(&format!("global-session-{credential_id}"));
            tracker.update_with_bounds(Some(scope), Some(&profile), 0.85, bounds);
        }

        assert!(
            tracker.entry_count() <= 3,
            "estimated byte limit should reduce the effective global entry cap"
        );
    }

    #[test]
    fn target_read_ratio_is_a_bounded_effective_range_not_an_exact_percent() {
        let tracker = PromptCacheTracker::default();
        let mut req = request("cacheable bounded ratio block ".repeat(4000));
        req.messages[0].content = json!([
            {
                "type": "text",
                "text": "cacheable bounded ratio block ".repeat(4000),
                "cache_control": {"type": "ephemeral"}
            }
        ]);
        let profile = tracker.build_profile(&req, 120_000).unwrap();

        let ratio = effective_cache_read_ratio(&profile, 0.95);

        assert!(
            (0.92..=0.98).contains(&ratio),
            "ratio={} should stay within 95% +/- 3%",
            ratio
        );
        assert!(
            (ratio - 0.95).abs() > 0.0001,
            "ratio={} should not be pinned to exactly 95%",
            ratio
        );
    }
}
