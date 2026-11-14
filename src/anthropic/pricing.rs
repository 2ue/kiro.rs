use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::cache::CacheUsage;
pub const DEFAULT_PRICING_SOURCE_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
pub const BUILTIN_PRICING_SOURCE_URL: &str = "internal://claude-api-pricing";

pub const FALLBACK_SOURCE: &str = "built-in";
pub const LITELLM_SOURCE: &str = "litellm";
pub const MANUAL_PRICING_SOURCE: &str = "manual";

type ClaudePricingVersion = Option<(u32, u32)>;
type ClaudePricingTier = (&'static str, ClaudePricingVersion);

#[derive(Clone)]
struct PricedModelCandidate {
    source_priority: u8,
    is_alias: bool,
    model: String,
    pricing: ModelPricing,
}

type PriceCandidatesByVersion = BTreeMap<ClaudePricingVersion, Vec<PricedModelCandidate>>;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPricing {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    /// 5-minute cache write (the standard `cache_creation_input_tokens` rate).
    pub cache_creation_input_token_cost: f64,
    /// 1-hour cache write. 0 means "not set": derived as 2x input (Anthropic's 1h write rate).
    #[serde(default)]
    pub cache_creation_1h_input_token_cost: f64,
    pub cache_read_input_token_cost: f64,
}

/// Anthropic prices a 1-hour cache write at 2x the base input rate (5-minute writes: 1.25x).
const ONE_HOUR_CACHE_WRITE_INPUT_MULTIPLIER: f64 = 2.0;

impl ModelPricing {
    pub fn estimate(self, usage: CacheUsage) -> f64 {
        let creation = usage.cache_creation_input_tokens.max(0);
        let creation_1h = usage.cache_creation_1h_input_tokens.max(0).min(creation);
        (usage.input_tokens.max(0) as f64 * self.input_cost_per_token)
            + (usage.output_tokens.max(0) as f64 * self.output_cost_per_token)
            + ((creation - creation_1h) as f64 * self.cache_creation_input_token_cost)
            + (creation_1h as f64 * self.effective_cache_creation_1h_input_token_cost())
            + (usage.cache_read_input_tokens.max(0) as f64 * self.cache_read_input_token_cost)
    }

    /// The 1-hour cache write rate, falling back to 2x input when the source did not set it.
    pub fn effective_cache_creation_1h_input_token_cost(self) -> f64 {
        if self.cache_creation_1h_input_token_cost > 0.0 {
            self.cache_creation_1h_input_token_cost
        } else {
            self.input_cost_per_token * ONE_HOUR_CACHE_WRITE_INPUT_MULTIPLIER
        }
    }

    /// Fills the 1-hour write rate when it is missing, so stored and displayed prices show it.
    pub fn with_derived_cache_creation_1h_cost(mut self) -> Self {
        self.cache_creation_1h_input_token_cost =
            self.effective_cache_creation_1h_input_token_cost();
        self
    }

    pub fn is_usable(self) -> bool {
        let rates = [
            self.input_cost_per_token,
            self.output_cost_per_token,
            self.cache_creation_input_token_cost,
            self.cache_creation_1h_input_token_cost,
            self.cache_read_input_token_cost,
        ];
        rates.iter().all(|rate| rate.is_finite() && *rate >= 0.0)
            && (self.input_cost_per_token > 0.0 || self.output_cost_per_token > 0.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPriceItem {
    pub model: String,
    pub pricing: ModelPricing,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingStatus {
    pub available: bool,
    pub source: String,
    pub source_url: String,
    pub model_count: usize,
    #[serde(default)]
    pub unpriced_models: Vec<String>,
    #[serde(skip_serializing)]
    pub persisted_base_models: Vec<ModelPriceItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub models: Vec<ModelPriceItem>,
}

#[derive(Debug, Clone)]
struct PricingSnapshot {
    /// Public and built-in prices, kept separately from manual overrides.
    prices: HashMap<String, ModelPricing>,
    sources: HashMap<String, String>,
    manual_prices: HashMap<String, ModelPricing>,
    source: String,
    source_url: String,
    last_synced_at: Option<String>,
    last_error: Option<String>,
}

impl PricingSnapshot {
    fn status(&self) -> PricingStatus {
        let mut effective = self.prices.clone();
        effective.extend(
            self.manual_prices
                .iter()
                .map(|(model, pricing)| (model.clone(), *pricing)),
        );
        let mut models: Vec<ModelPriceItem> = effective
            .iter()
            .map(|(model, pricing)| ModelPriceItem {
                model: model.clone(),
                pricing: *pricing,
                source: if self.manual_prices.contains_key(model) {
                    Some(MANUAL_PRICING_SOURCE.to_string())
                } else {
                    self.sources.get(model).cloned()
                },
            })
            .collect();
        models.sort_by(|a, b| a.model.cmp(&b.model));

        let mut persisted_base_models: Vec<ModelPriceItem> = self
            .prices
            .iter()
            .map(|(model, pricing)| ModelPriceItem {
                model: model.clone(),
                pricing: *pricing,
                source: self.sources.get(model).cloned(),
            })
            .collect();
        persisted_base_models.sort_by(|a, b| a.model.cmp(&b.model));

        PricingStatus {
            available: !effective.is_empty(),
            source: self.source.clone(),
            source_url: self.source_url.clone(),
            model_count: effective.len(),
            unpriced_models: known_claude_models()
                .into_iter()
                .filter(|model| !has_direct_price(model, &effective))
                .collect(),
            persisted_base_models,
            last_synced_at: self.last_synced_at.clone(),
            last_error: self.last_error.clone(),
            models,
        }
    }
}

impl Default for PricingSnapshot {
    fn default() -> Self {
        let prices = fallback_prices();
        let sources = prices
            .keys()
            .map(|model| (model.clone(), FALLBACK_SOURCE.to_string()))
            .collect();
        Self {
            prices,
            sources,
            manual_prices: HashMap::new(),
            source: FALLBACK_SOURCE.to_string(),
            source_url: BUILTIN_PRICING_SOURCE_URL.to_string(),
            last_synced_at: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PricingCatalog {
    inner: Arc<RwLock<PricingSnapshot>>,
    mutation_lock: Arc<tokio::sync::Mutex<()>>,
    /// 只在拉取远端价格时才需要。构建 reqwest Client 会加载系统根证书（rustls 下在 macOS
    /// 要读钥匙串，单次约百毫秒且互斥），所以延迟到首次使用，克隆之间共享同一个实例。
    client: Arc<OnceLock<reqwest::Client>>,
}

impl Default for PricingCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl PricingCatalog {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(PricingSnapshot::default())),
            mutation_lock: Arc::new(tokio::sync::Mutex::new(())),
            client: Arc::new(OnceLock::new()),
        }
    }

    /// Serializes DB persistence and in-memory publication across startup and Admin writes.
    pub async fn mutation_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.mutation_lock.lock().await
    }

    pub fn load_persisted_status(&self, status: PricingStatus) {
        let base_models = if status.persisted_base_models.is_empty() {
            status
                .models
                .iter()
                .filter(|item| item.source.as_deref() != Some(MANUAL_PRICING_SOURCE))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            status.persisted_base_models.clone()
        };
        let mut prices = fallback_prices();
        let mut sources = prices
            .keys()
            .map(|model| (model.clone(), FALLBACK_SOURCE.to_string()))
            .collect::<HashMap<_, _>>();
        for item in base_models {
            let model = normalize_pricing_model_id(&item.model);
            prices.insert(model.clone(), item.pricing);
            sources.insert(model, item.source.unwrap_or_else(|| status.source.clone()));
        }
        let mut manual_prices = HashMap::new();
        for item in status
            .models
            .iter()
            .filter(|item| item.source.as_deref() == Some(MANUAL_PRICING_SOURCE))
        {
            manual_prices.insert(normalize_pricing_model_id(&item.model), item.pricing);
        }
        let mut inner = self.inner.write();
        inner.prices = prices;
        inner.sources = sources;
        inner.manual_prices = manual_prices;
        inner.source = status.source;
        inner.source_url = status.source_url;
        inner.last_synced_at = status.last_synced_at;
        inner.last_error = status.last_error;
    }

    pub fn status(&self) -> PricingStatus {
        self.inner.read().status()
    }

    /// Add models from the separately synchronized Kiro capability catalog to the pricing view.
    pub fn status_for_models(&self, models: impl IntoIterator<Item = String>) -> PricingStatus {
        let mut status = self.status();
        let priced = status
            .models
            .iter()
            .map(|item| (item.model.clone(), item.pricing))
            .collect::<HashMap<_, _>>();
        status.unpriced_models.extend(
            models
                .into_iter()
                .map(|model| normalize_pricing_model_id(&model))
                .filter(|model| !model.is_empty() && !has_direct_price(model, &priced)),
        );
        status.unpriced_models.sort();
        status.unpriced_models.dedup();
        status
    }

    /// 按与 `estimate` 相同的候选顺序返回单价；未命中时回退到同计费档最近价格。
    pub fn model_pricing(&self, model: &str) -> Option<ModelPricing> {
        let pricing_candidates = pricing_model_candidates(model);
        let inner = self.inner.read();
        if let Some((_, pricing)) =
            resolve_configured_price(&pricing_candidates, &inner.manual_prices, &inner.prices)
        {
            return Some(pricing);
        }
        let builtin_prices = fallback_prices();
        if let Some(pricing) = pricing_candidates.iter().find_map(|candidate| {
            builtin_prices
                .get(candidate)
                .copied()
                .filter(|pricing| pricing.is_usable())
        }) {
            return Some(pricing);
        }
        nearest_priced_model(model, &inner.manual_prices, &inner.prices, &builtin_prices)
            .map(|(_, pricing)| pricing)
    }

    pub fn estimate(&self, model: &str, usage: CacheUsage) -> PricingEstimate {
        let pricing_candidates = pricing_model_candidates(model);
        let fallback_model = pricing_candidates
            .last()
            .cloned()
            .unwrap_or_else(|| canonical_pricing_model(model));
        let inner = self.inner.read();
        let pricing =
            resolve_configured_price(&pricing_candidates, &inner.manual_prices, &inner.prices);
        let manual_prices = inner.manual_prices.clone();
        let base_prices = inner.prices.clone();
        drop(inner);

        match pricing {
            Some((model, pricing)) => PricingEstimate {
                model,
                available: true,
                cost_usd: pricing.estimate(usage),
            },
            None => {
                let builtin_prices = fallback_prices();
                if let Some((model, pricing)) = pricing_candidates.iter().find_map(|candidate| {
                    builtin_prices
                        .get(candidate)
                        .copied()
                        .filter(|pricing| pricing.is_usable())
                        .map(|pricing| (candidate.clone(), pricing))
                }) {
                    PricingEstimate {
                        model,
                        available: true,
                        cost_usd: pricing.estimate(usage),
                    }
                } else if let Some((model, pricing)) =
                    nearest_priced_model(model, &manual_prices, &base_prices, &builtin_prices)
                {
                    PricingEstimate {
                        model,
                        available: true,
                        cost_usd: pricing.estimate(usage),
                    }
                } else {
                    PricingEstimate {
                        model: fallback_model,
                        available: false,
                        cost_usd: 0.0,
                    }
                }
            }
        }
    }

    pub fn upsert_manual_price(&self, model: &str, pricing: ModelPricing) -> Option<PricingStatus> {
        if !pricing.is_usable() {
            return None;
        }
        let model = normalize_pricing_model_id(model);
        if model.is_empty() {
            return None;
        }
        let mut inner = self.inner.write();
        inner.manual_prices.insert(model, pricing);
        Some(inner.status())
    }

    pub fn delete_manual_price(&self, model: &str) -> PricingStatus {
        let model = normalize_pricing_model_id(model);
        let mut inner = self.inner.write();
        inner.manual_prices.remove(&model);
        inner.status()
    }

    pub async fn sync_for_models(
        &self,
        candidate_models: impl IntoIterator<Item = String>,
    ) -> PricingStatus {
        self.sync_for_models_from_url(DEFAULT_PRICING_SOURCE_URL, candidate_models)
            .await
    }

    async fn sync_for_models_from_url(
        &self,
        url: &str,
        candidate_models: impl IntoIterator<Item = String>,
    ) -> PricingStatus {
        let candidate_models = pricing_sync_candidates(candidate_models);
        match self.fetch_remote_prices(url, &candidate_models).await {
            Ok(prices) => {
                let mut inner = self.inner.write();
                let (merged, sources) = merge_price_sources(&prices);
                inner.prices = merged;
                inner.sources = sources;
                inner.source = LITELLM_SOURCE.to_string();
                inner.source_url = DEFAULT_PRICING_SOURCE_URL.to_string();
                inner.last_synced_at = Some(Utc::now().to_rfc3339());
                inner.last_error = None;
                inner.status()
            }
            Err(err) => {
                tracing::warn!("同步模型价格失败，不影响请求调度: {}", err);
                let mut inner = self.inner.write();
                inner.last_error = Some(err.to_string());
                inner.status()
            }
        }
    }

    async fn fetch_remote_prices(
        &self,
        url: &str,
        candidate_models: &BTreeSet<String>,
    ) -> anyhow::Result<HashMap<String, ModelPricing>> {
        let value: serde_json::Value = self
            .client
            .get_or_init(reqwest::Client::new)
            .get(url)
            .timeout(Duration::from_secs(20))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        parse_litellm_prices(&value, candidate_models)
    }
}

/// Merge one public-source snapshot without allowing it to erase manual or built-in prices.
/// Keeping this pure makes source precedence testable without depending on the network.
fn merge_price_sources(
    remote: &HashMap<String, ModelPricing>,
) -> (HashMap<String, ModelPricing>, HashMap<String, String>) {
    let remote_models = remote.keys().cloned().collect::<BTreeSet<_>>();
    let mut merged = fallback_prices();
    merged.extend(
        remote
            .iter()
            .map(|(model, pricing)| (model.clone(), *pricing)),
    );
    let sources = merged
        .keys()
        .map(|model| {
            let source = if remote_models.contains(model) {
                LITELLM_SOURCE
            } else {
                FALLBACK_SOURCE
            };
            (model.clone(), source.to_string())
        })
        .collect();
    (merged, sources)
}

fn resolve_configured_price(
    candidates: &[String],
    manual: &HashMap<String, ModelPricing>,
    base: &HashMap<String, ModelPricing>,
) -> Option<(String, ModelPricing)> {
    for prices in [manual, base] {
        if let Some((model, pricing)) = candidates.iter().find_map(|candidate| {
            prices
                .get(candidate)
                .copied()
                .filter(|pricing| pricing.is_usable())
                .map(|pricing| (candidate.clone(), pricing))
        }) {
            return Some((model, pricing));
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct PricingEstimate {
    pub model: String,
    pub available: bool,
    pub cost_usd: f64,
}

pub fn normalize_pricing_model_id(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

pub fn canonical_pricing_model(model: &str) -> String {
    let model = model.trim().to_ascii_lowercase();
    let model = model.strip_suffix("[1m]").unwrap_or(&model);
    let model = model.strip_suffix("-thinking").unwrap_or(model);
    let mut parts = model.split('-').collect::<Vec<_>>();
    // Dated Claude ids are request aliases for their versioned pricing entry.
    // Remove any trailing YYYYMMDD segment generically; no model/date allowlist
    // should be needed as upstream catalogs add or retire dated ids.
    if parts
        .last()
        .is_some_and(|value| value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        parts.pop();
    }
    parts.join("-").replace('.', "-")
}

fn parse_litellm_prices(
    value: &serde_json::Value,
    candidate_models: &BTreeSet<String>,
) -> anyhow::Result<HashMap<String, ModelPricing>> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("pricing payload is not an object"))?;
    let mut prices = HashMap::new();
    for model in candidate_models {
        if let Some(entry) = find_litellm_entry(object, model)
            && let Some(pricing) = pricing_from_entry(entry)
        {
            prices.insert(model.clone(), pricing);
        }
    }

    for (key, entry) in object {
        if entry
            .get("litellm_provider")
            .and_then(serde_json::Value::as_str)
            != Some("anthropic")
            || entry.get("mode").and_then(serde_json::Value::as_str) != Some("chat")
        {
            continue;
        }
        let Some(model) = canonical_anthropic_model_id(key) else {
            continue;
        };
        if claude_pricing_tier(&model).is_none() {
            continue;
        }
        if let Some(pricing) = pricing_from_entry(entry) {
            if key == &model {
                prices.insert(model, pricing);
            } else {
                prices.entry(model).or_insert(pricing);
            }
        }
    }

    if prices.is_empty() {
        anyhow::bail!("pricing payload did not contain candidate models");
    }

    Ok(prices)
}

fn find_litellm_entry<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    canonical_model: &str,
) -> Option<&'a serde_json::Value> {
    for key in litellm_candidate_keys(canonical_model) {
        if let Some(value) = object.get(&key)
            && litellm_entry_is_anthropic_chat(&key, value)
            && pricing_from_entry(value).is_some()
        {
            return Some(value);
        }
    }
    None
}

fn litellm_candidate_keys(canonical_model: &str) -> Vec<String> {
    let mut keys = vec![canonical_model.to_string()];
    let dashed_as_dots = canonical_model.replace('-', ".");
    push_candidate_exact(&mut keys, &dashed_as_dots);
    if let Some(dotted_minor) = claude_family_minor_dot_candidate(canonical_model) {
        push_candidate_exact(&mut keys, &dotted_minor);
    }
    push_candidate_exact(&mut keys, &format!("anthropic.{}", canonical_model));
    push_candidate_exact(&mut keys, &format!("anthropic.{}", dashed_as_dots));
    if let Some((family, major, minor)) = claude_family_parts(canonical_model) {
        let dated = match (family, major, minor) {
            ("haiku", 4, 5) => Some(("20251001", "-v1:0")),
            ("sonnet", 4, 5) => Some(("20250929", "-v1:0")),
            ("opus", 4, 5) => Some(("20251101", "-v1:0")),
            ("opus", 4, 6) => Some(("20260205", "-v1")),
            ("opus", 4, 7) => Some(("20260416", "")),
            _ => None,
        };
        if let Some((date, suffix)) = dated {
            let plain = format!("claude-{family}-{major}-{minor}-{date}");
            push_candidate_exact(&mut keys, &plain);
            push_candidate_exact(&mut keys, &format!("anthropic.{plain}{suffix}"));
        }
        if family == "sonnet" && major == 4 && minor == 6 {
            push_candidate_exact(&mut keys, "anthropic.claude-sonnet-4-6");
        }
        if family == "opus" && major == 4 && minor == 6 {
            push_candidate_exact(&mut keys, "anthropic.claude-opus-4-6-v1");
        }
        if family == "opus" && major == 4 && minor == 7 {
            push_candidate_exact(&mut keys, "anthropic.claude-opus-4-7");
        }
    }
    let mut deduped = Vec::with_capacity(keys.len());
    for key in keys {
        if !deduped.contains(&key) {
            deduped.push(key);
        }
    }
    deduped
}

fn litellm_entry_is_anthropic_chat(key: &str, value: &serde_json::Value) -> bool {
    let provider = value
        .get("litellm_provider")
        .and_then(serde_json::Value::as_str);
    let mode = value.get("mode").and_then(serde_json::Value::as_str);
    provider.is_none_or(|provider| provider == "anthropic")
        && mode.is_none_or(|mode| mode == "chat")
        && (provider == Some("anthropic") || key.starts_with("claude-"))
}

fn canonical_anthropic_model_id(key: &str) -> Option<String> {
    let model = key
        .strip_prefix("anthropic/")
        .or_else(|| key.strip_prefix("anthropic."))
        .unwrap_or(key);
    if !model.starts_with("claude-") {
        return None;
    }
    let canonical = canonical_pricing_model(model).replace('.', "-");
    Some(canonical)
}

fn claude_family_parts(model: &str) -> Option<(&str, u32, u32)> {
    let (family, version) = claude_pricing_tier(model)?;
    let (major, minor) = version?;
    Some((family, major, minor))
}

fn pricing_sync_candidates(models: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    let mut candidates = BTreeSet::new();
    for model in models {
        for candidate in pricing_model_candidates(&model) {
            candidates.insert(candidate);
        }
    }
    // Kiro capability discovery is intentionally not the authority for billing models.
    // Include the Claude Code display catalog and the built-in price catalog on every sync.
    candidates.extend(default_pricing_models());
    candidates.extend(super::forced_model::pricing_catalog_model_ids());
    candidates
}

fn known_claude_models() -> BTreeSet<String> {
    default_pricing_models()
        .into_iter()
        .chain(super::forced_model::pricing_catalog_model_ids())
        .map(|model| normalize_pricing_model_id(&model))
        .collect()
}

fn pricing_model_candidates(model: &str) -> Vec<String> {
    let normalized = normalize_pricing_model_id(model);
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    push_candidate(&mut candidates, &normalized);
    let without_1m = normalized.strip_suffix("[1m]").unwrap_or(&normalized);
    push_candidate(&mut candidates, without_1m);

    if let Some(without_thinking) = without_1m.strip_suffix("-thinking") {
        push_candidate(&mut candidates, without_thinking);
    }

    let normalized_dashes = without_1m.replace('.', "-");
    push_candidate(&mut candidates, &normalized_dashes);
    if let Some(without_thinking) = normalized_dashes.strip_suffix("-thinking") {
        push_candidate(&mut candidates, without_thinking);
    }

    let canonical = canonical_pricing_model(&normalized);
    push_candidate(&mut candidates, &canonical);
    if let Some(family_version) = claude_family_version_candidate(&normalized) {
        push_candidate(&mut candidates, &family_version);
    }
    if let Some(family_version) = claude_family_version_candidate(&canonical) {
        push_candidate(&mut candidates, &family_version);
    }

    candidates
}

fn nearest_priced_model(
    model: &str,
    manual: &HashMap<String, ModelPricing>,
    configured: &HashMap<String, ModelPricing>,
    builtins: &HashMap<String, ModelPricing>,
) -> Option<(String, ModelPricing)> {
    let (tier, requested_version) = claude_pricing_tier(model)?;
    let mut candidates: PriceCandidatesByVersion = BTreeMap::new();
    for (prices, priority) in [(builtins, 1), (configured, 2), (manual, 3)] {
        for (candidate, pricing) in prices {
            if !pricing.is_usable() {
                continue;
            }
            let Some((candidate_tier, version)) = claude_pricing_tier(candidate) else {
                continue;
            };
            if candidate_tier == tier {
                candidates
                    .entry(version)
                    .or_default()
                    .push(PricedModelCandidate {
                        source_priority: priority,
                        is_alias: canonical_pricing_model(candidate) != *candidate,
                        model: candidate.clone(),
                        pricing: *pricing,
                    });
            }
        }
    }

    let chosen = match requested_version {
        Some(requested) => candidates
            .range(..=Some(requested))
            .next_back()
            .or_else(|| candidates.iter().next()),
        None => candidates.iter().next_back(),
    }?;
    let candidate = chosen.1.iter().min_by_key(|candidate| {
        (
            std::cmp::Reverse(candidate.source_priority),
            candidate.is_alias,
            candidate.model.as_str(),
        )
    })?;
    Some((candidate.model.clone(), candidate.pricing))
}

fn has_direct_price(model: &str, prices: &HashMap<String, ModelPricing>) -> bool {
    pricing_model_candidates(model).iter().any(|candidate| {
        prices
            .get(candidate)
            .is_some_and(|pricing| pricing.is_usable())
    })
}

/// Maps Claude Code model families to their own nearest priced tier.
/// Fable and Mythos keep separate pricing families instead of borrowing Opus.
fn claude_pricing_tier(model: &str) -> Option<ClaudePricingTier> {
    let normalized = canonical_pricing_model(model).replace('.', "-");
    let rest = normalized.strip_prefix("claude-")?;
    let parts = rest.split('-').collect::<Vec<_>>();
    let family_index = parts
        .iter()
        .position(|part| matches!(*part, "opus" | "sonnet" | "haiku" | "fable" | "mythos"))?;
    let tier = match parts[family_index] {
        "opus" => "opus",
        "sonnet" => "sonnet",
        "haiku" => "haiku",
        "fable" => "fable",
        "mythos" => "mythos",
        _ => return None,
    };
    let prefix_has_version = parts[..family_index]
        .iter()
        .any(|part| part.parse::<u32>().is_ok());
    let version_parts = if prefix_has_version {
        &parts[..family_index]
    } else {
        &parts[family_index + 1..]
    };
    let mut version_numbers = version_parts
        .iter()
        .filter(|part| !part.is_empty() && part.len() <= 3)
        .filter_map(|part| part.parse::<u32>().ok());
    let version = version_numbers.next().map(|major| {
        let minor = version_numbers.next().unwrap_or(0);
        (major, minor)
    });
    Some((tier, version))
}

fn push_candidate(candidates: &mut Vec<String>, candidate: &str) {
    let candidate = normalize_pricing_model_id(candidate);
    push_candidate_exact(candidates, &candidate);

    let dashed = candidate.replace('.', "-");
    push_candidate_exact(candidates, &dashed);

    if let Some(dotted) = claude_family_minor_dot_candidate(&dashed) {
        push_candidate_exact(candidates, &dotted);
    }
}

fn push_candidate_exact(candidates: &mut Vec<String>, candidate: &str) {
    if !candidate.is_empty() && !candidates.iter().any(|item| item == candidate) {
        candidates.push(candidate.to_string());
    }
}

fn claude_family_minor_dot_candidate(model: &str) -> Option<String> {
    let normalized = normalize_pricing_model_id(model);
    let start = normalized.find("claude-")?;
    let prefix = &normalized[..start];
    let rest = &normalized[start + "claude-".len()..];
    let family = ["opus", "sonnet", "haiku", "fable", "mythos"]
        .into_iter()
        .find(|family| rest.starts_with(&format!("{}-", family)))?;
    let version = &rest[family.len() + 1..];
    let (major, after_major) = take_leading_digits(version)?;
    let after_major = after_major.strip_prefix(['-', '.'])?;
    let (minor, tail) = take_leading_digits(after_major)?;
    if major.is_empty() || minor.is_empty() {
        return None;
    }
    Some(format!(
        "{}claude-{}-{}.{}{}",
        prefix, family, major, minor, tail
    ))
}

fn take_leading_digits(value: &str) -> Option<(&str, &str)> {
    let len = value
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .last()
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);
    (len > 0).then(|| value.split_at(len))
}

fn claude_family_version_candidate(model: &str) -> Option<String> {
    let normalized = normalize_pricing_model_id(model).replace('.', "-");
    let start = normalized.find("claude-")?;
    let rest = &normalized[start + "claude-".len()..];
    let family = ["opus", "sonnet", "haiku", "fable", "mythos"]
        .into_iter()
        .find(|family| rest.starts_with(&format!("{}-", family)))?;
    let version = &rest[family.len() + 1..];
    let mut numbers = Vec::new();
    let mut current = String::new();
    for ch in version.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(value) = current.parse::<u32>() {
                numbers.push(value);
            }
            current.clear();
            if numbers.len() >= 2 {
                break;
            }
        }
    }
    if numbers.len() < 2
        && !current.is_empty()
        && let Ok(value) = current.parse::<u32>()
    {
        numbers.push(value);
    }
    if numbers.len() >= 2 {
        Some(format!("claude-{}-{}-{}", family, numbers[0], numbers[1]))
    } else {
        None
    }
}

fn pricing_from_entry(value: &serde_json::Value) -> Option<ModelPricing> {
    let pricing = ModelPricing {
        input_cost_per_token: number_field(value, "input_cost_per_token")?,
        output_cost_per_token: number_field(value, "output_cost_per_token")?,
        cache_creation_input_token_cost: number_field(value, "cache_creation_input_token_cost")
            .unwrap_or_else(|| number_field(value, "input_cost_per_token").unwrap_or(0.0) * 1.25),
        cache_creation_1h_input_token_cost: number_field(
            value,
            "cache_creation_input_token_cost_above_1hr",
        )
        .unwrap_or(0.0),
        cache_read_input_token_cost: number_field(value, "cache_read_input_token_cost")
            .unwrap_or_else(|| number_field(value, "input_cost_per_token").unwrap_or(0.0) * 0.1),
    }
    .with_derived_cache_creation_1h_cost();
    pricing.is_usable().then_some(pricing)
}

fn number_field(value: &serde_json::Value, field: &str) -> Option<f64> {
    value.get(field).and_then(serde_json::Value::as_f64)
}

fn default_pricing_models() -> BTreeSet<String> {
    [
        "claude-haiku-4-5",
        "claude-sonnet-4-5",
        "claude-sonnet-4-6",
        "claude-sonnet-5",
        "claude-sonnet-5-5",
        "claude-opus-4-5",
        "claude-opus-4-6",
        "claude-opus-4-7",
        "claude-opus-5",
        "claude-opus-5-5",
        // These are valid display/计费 families even when the public source has no entry.
        "claude-fable-5",
        "claude-fable-5-1",
        "claude-fable-5-2",
        "claude-mythos-5",
        "claude-mythos-5-1",
        "claude-mythos-5-2",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect()
}

fn fallback_prices() -> HashMap<String, ModelPricing> {
    BTreeMap::from([
        (
            "claude-haiku-4-5",
            ModelPricing {
                input_cost_per_token: 0.000001,
                output_cost_per_token: 0.000005,
                cache_creation_input_token_cost: 0.00000125,
                cache_creation_1h_input_token_cost: 0.000002,
                cache_read_input_token_cost: 0.0000001,
            },
        ),
        (
            "claude-sonnet-4-5",
            ModelPricing {
                input_cost_per_token: 0.000003,
                output_cost_per_token: 0.000015,
                cache_creation_input_token_cost: 0.00000375,
                cache_creation_1h_input_token_cost: 0.000006,
                cache_read_input_token_cost: 0.0000003,
            },
        ),
        (
            "claude-sonnet-4-6",
            ModelPricing {
                input_cost_per_token: 0.000003,
                output_cost_per_token: 0.000015,
                cache_creation_input_token_cost: 0.00000375,
                cache_creation_1h_input_token_cost: 0.000006,
                cache_read_input_token_cost: 0.0000003,
            },
        ),
        (
            "claude-sonnet-5",
            ModelPricing {
                input_cost_per_token: 0.000002,
                output_cost_per_token: 0.00001,
                cache_creation_input_token_cost: 0.0000025,
                cache_creation_1h_input_token_cost: 0.000004,
                cache_read_input_token_cost: 0.0000002,
            },
        ),
        (
            "claude-sonnet-5-5",
            ModelPricing {
                input_cost_per_token: 0.000002,
                output_cost_per_token: 0.00001,
                cache_creation_input_token_cost: 0.0000025,
                cache_creation_1h_input_token_cost: 0.000004,
                cache_read_input_token_cost: 0.0000002,
            },
        ),
        (
            "claude-opus-4-5",
            ModelPricing {
                input_cost_per_token: 0.000005,
                output_cost_per_token: 0.000025,
                cache_creation_input_token_cost: 0.00000625,
                cache_creation_1h_input_token_cost: 0.00001,
                cache_read_input_token_cost: 0.0000005,
            },
        ),
        (
            "claude-opus-4-6",
            ModelPricing {
                input_cost_per_token: 0.000005,
                output_cost_per_token: 0.000025,
                cache_creation_input_token_cost: 0.00000625,
                cache_creation_1h_input_token_cost: 0.00001,
                cache_read_input_token_cost: 0.0000005,
            },
        ),
        (
            "claude-opus-4-7",
            ModelPricing {
                input_cost_per_token: 0.000005,
                output_cost_per_token: 0.000025,
                cache_creation_input_token_cost: 0.00000625,
                cache_creation_1h_input_token_cost: 0.00001,
                cache_read_input_token_cost: 0.0000005,
            },
        ),
        (
            "claude-opus-5",
            ModelPricing {
                input_cost_per_token: 0.000005,
                output_cost_per_token: 0.000025,
                cache_creation_input_token_cost: 0.00000625,
                cache_creation_1h_input_token_cost: 0.00001,
                cache_read_input_token_cost: 0.0000005,
            },
        ),
        (
            "claude-opus-5-5",
            ModelPricing {
                input_cost_per_token: 0.000004,
                output_cost_per_token: 0.00002,
                cache_creation_input_token_cost: 0.000005,
                cache_creation_1h_input_token_cost: 0.000008,
                cache_read_input_token_cost: 0.0000004,
            },
        ),
    ])
    .into_iter()
    .map(|(model, pricing)| (model.to_string(), pricing))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn serve_once(status: u16, body: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).await;
            let reason = if status == 200 {
                "OK"
            } else {
                "Internal Server Error"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        });
        (format!("http://{address}/prices"), task)
    }

    #[test]
    fn canonical_pricing_model_maps_aliases_and_thinking_suffixes() {
        assert_eq!(canonical_pricing_model("opus"), "opus");
        assert_eq!(
            canonical_pricing_model("claude-opus-4-5-20251101-thinking"),
            "claude-opus-4-5"
        );
        assert_eq!(
            canonical_pricing_model("claude-sonnet-4-6[1m]"),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            canonical_pricing_model("claude-sonnet-4-6-thinking[1m]"),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn fallback_pricing_estimates_cache_tokens() {
        let catalog = PricingCatalog::new();
        let usage = CacheUsage {
            total_input_tokens: 100_000,
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_input_tokens: 3_000,
            cache_read_input_tokens: 96_990,
            cache_creation_5m_input_tokens: 3_000,
            cache_creation_1h_input_tokens: 0,
        };

        let estimate = catalog.estimate("claude-sonnet-4-6", usage);

        assert!(estimate.available);
        assert!(estimate.cost_usd > 0.0);
        assert_eq!(estimate.model, "claude-sonnet-4-6");
    }

    #[test]
    fn status_reports_catalog_models_without_prices() {
        let status = PricingCatalog::new().status();
        assert!(
            status
                .unpriced_models
                .contains(&"claude-fable-5-1".to_string())
        );
        assert!(
            status
                .unpriced_models
                .contains(&"claude-mythos-5-2".to_string())
        );
        assert!(
            !status
                .unpriced_models
                .contains(&"claude-opus-5-5".to_string())
        );
    }

    #[test]
    fn status_includes_unpriced_kiro_models_and_recognizes_priced_aliases() {
        let catalog = PricingCatalog::new();
        catalog.upsert_manual_price(
            "claude-fable-5.1",
            ModelPricing {
                input_cost_per_token: 0.00001,
                output_cost_per_token: 0.00005,
                cache_creation_input_token_cost: 0.0000125,
                cache_creation_1h_input_token_cost: 0.00002,
                cache_read_input_token_cost: 0.000001,
            },
        );

        let status = catalog.status_for_models([
            "claude-fable-5-1".to_string(),
            "kiro-custom-model-v1".to_string(),
        ]);

        assert!(
            !status
                .unpriced_models
                .contains(&"claude-fable-5-1".to_string())
        );
        assert!(
            status
                .unpriced_models
                .contains(&"kiro-custom-model-v1".to_string())
        );
    }

    #[test]
    fn unusable_manual_price_does_not_mask_a_valid_price() {
        let catalog = PricingCatalog::new();
        let invalid = ModelPricing {
            input_cost_per_token: -0.000001,
            output_cost_per_token: 0.000005,
            cache_creation_input_token_cost: 0.00000125,
            cache_creation_1h_input_token_cost: 0.000002,
            cache_read_input_token_cost: 0.0000001,
        };
        assert!(!invalid.is_usable());
        catalog
            .inner
            .write()
            .manual_prices
            .insert("custom-unpriced-model".to_string(), invalid);

        assert_eq!(catalog.model_pricing("custom-unpriced-model"), None);
        assert!(
            pricing_from_entry(&serde_json::json!({
                "input_cost_per_token": -0.000001,
                "output_cost_per_token": 0.000005
            }))
            .is_none()
        );
    }

    #[test]
    fn deleting_manual_price_restores_builtin_price_for_that_model() {
        let catalog = PricingCatalog::new();
        let manual = ModelPricing {
            input_cost_per_token: 0.000009,
            output_cost_per_token: 0.000045,
            cache_creation_input_token_cost: 0.00001125,
            cache_creation_1h_input_token_cost: 0.000018,
            cache_read_input_token_cost: 0.0000009,
        };
        catalog.upsert_manual_price("claude-opus-5-5", manual);
        catalog.delete_manual_price("claude-opus-5-5");

        let item = catalog
            .status()
            .models
            .into_iter()
            .find(|item| item.model == "claude-opus-5-5")
            .unwrap();
        assert_eq!(item.source.as_deref(), Some(FALLBACK_SOURCE));
        assert_eq!(item.pricing.input_cost_per_token, 0.000004);
    }

    #[test]
    fn persisted_remote_pricing_keeps_builtin_estimate_fallback() {
        let catalog = PricingCatalog::new();
        catalog.load_persisted_status(PricingStatus {
            available: true,
            source: LITELLM_SOURCE.to_string(),
            source_url: DEFAULT_PRICING_SOURCE_URL.to_string(),
            model_count: 1,
            unpriced_models: Vec::new(),
            persisted_base_models: Vec::new(),
            last_synced_at: Some("2026-06-09T00:00:00Z".to_string()),
            last_error: None,
            models: vec![ModelPriceItem {
                model: "claude-sonnet-4-5".to_string(),
                pricing: ModelPricing {
                    input_cost_per_token: 0.000003,
                    output_cost_per_token: 0.000015,
                    cache_creation_input_token_cost: 0.00000375,
                    cache_creation_1h_input_token_cost: 0.0,
                    cache_read_input_token_cost: 0.0000003,
                },
                source: Some(LITELLM_SOURCE.to_string()),
            }],
        });
        let usage = CacheUsage {
            total_input_tokens: 100_000,
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_input_tokens: 3_000,
            cache_read_input_tokens: 96_990,
            cache_creation_5m_input_tokens: 3_000,
            cache_creation_1h_input_tokens: 0,
        };

        let estimate = catalog.estimate("claude-sonnet-4-6", usage);

        assert!(estimate.available);
        assert!(estimate.cost_usd > 0.0);
        assert_eq!(estimate.model, "claude-sonnet-4-6");
    }

    #[test]
    fn manual_pricing_estimates_exact_model_before_canonical_alias() {
        let catalog = PricingCatalog::new();
        catalog.upsert_manual_price(
            "claude-opus-5-20270101",
            ModelPricing {
                input_cost_per_token: 0.00001,
                output_cost_per_token: 0.00002,
                cache_creation_input_token_cost: 0.0000125,
                cache_creation_1h_input_token_cost: 0.0,
                cache_read_input_token_cost: 0.000001,
            },
        );
        let usage = CacheUsage {
            total_input_tokens: 100,
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };

        let estimate = catalog.estimate("claude-opus-5-20270101", usage);

        assert!(estimate.available);
        assert_eq!(estimate.model, "claude-opus-5-20270101");
        assert!((estimate.cost_usd - 0.002).abs() < f64::EPSILON);
    }

    #[test]
    fn estimate_falls_back_to_same_family_version_price() {
        let catalog = PricingCatalog::new();
        catalog.upsert_manual_price(
            "claude-opus-4-8",
            ModelPricing {
                input_cost_per_token: 0.000005,
                output_cost_per_token: 0.000025,
                cache_creation_input_token_cost: 0.00000625,
                cache_creation_1h_input_token_cost: 0.0,
                cache_read_input_token_cost: 0.0000005,
            },
        );
        let usage = CacheUsage {
            total_input_tokens: 100,
            input_tokens: 100,
            output_tokens: 100,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };

        let estimate = catalog.estimate("claude-opus-4-8-20260529-thinking", usage);

        assert!(estimate.available);
        assert_eq!(estimate.model, "claude-opus-4-8");
        assert!((estimate.cost_usd - 0.003).abs() < f64::EPSILON);
    }

    #[test]
    fn estimate_uses_nearest_lower_price_tier_for_unlisted_claude_models() {
        let catalog = PricingCatalog::new();
        let usage = CacheUsage {
            total_input_tokens: 100,
            input_tokens: 100,
            output_tokens: 100,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };

        let opus = catalog.estimate("claude-opus-6", usage);
        assert!(opus.available);
        assert_eq!(opus.model, "claude-opus-5-5");
        assert!((opus.cost_usd - 0.0024).abs() < f64::EPSILON);
        assert_eq!(
            catalog.model_pricing("claude-opus-6"),
            catalog.model_pricing("claude-opus-5-5")
        );

        for model in ["claude-fable-5-1", "claude-mythos-5-1"] {
            let estimate = catalog.estimate(model, usage);
            assert!(!estimate.available, "{model} must not borrow Opus pricing");
            assert_eq!(estimate.cost_usd, 0.0, "{model}");
            assert!(catalog.model_pricing(model).is_none(), "{model}");
        }

        let sonnet = catalog.estimate("claude-sonnet-6", usage);
        assert!(sonnet.available);
        assert_eq!(sonnet.model, "claude-sonnet-5-5");
        let haiku = catalog.estimate("claude-haiku-6", usage);
        assert!(haiku.available);
        assert_eq!(haiku.model, "claude-haiku-4-5");
    }

    #[test]
    fn nearest_tier_prefers_newest_configured_price_not_a_lower_builtin_price() {
        let catalog = PricingCatalog::new();
        catalog.upsert_manual_price(
            "claude-opus-4-8",
            ModelPricing {
                input_cost_per_token: 0.000007,
                output_cost_per_token: 0.000031,
                cache_creation_input_token_cost: 0.000008,
                cache_creation_1h_input_token_cost: 0.000014,
                cache_read_input_token_cost: 0.0000007,
            },
        );
        let fable_pricing = ModelPricing {
            input_cost_per_token: 0.000007,
            output_cost_per_token: 0.000031,
            cache_creation_input_token_cost: 0.000008,
            cache_creation_1h_input_token_cost: 0.000014,
            cache_read_input_token_cost: 0.0000007,
        };
        catalog.upsert_manual_price("claude-fable-5-1", fable_pricing);
        let estimate = catalog.estimate(
            "claude-fable-6",
            CacheUsage {
                total_input_tokens: 100,
                input_tokens: 100,
                output_tokens: 10,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
            },
        );

        assert_eq!(estimate.model, "claude-fable-5-1");
        assert!((estimate.cost_usd - 0.00101).abs() < f64::EPSILON);
        assert_eq!(catalog.model_pricing("claude-fable-6"), Some(fable_pricing));
    }

    #[test]
    fn nearest_price_uses_numeric_versions_within_the_same_family() {
        let older = ModelPricing {
            input_cost_per_token: 0.00001,
            output_cost_per_token: 0.00005,
            cache_creation_input_token_cost: 0.0000125,
            cache_creation_1h_input_token_cost: 0.00002,
            cache_read_input_token_cost: 0.000001,
        };
        let newer = ModelPricing {
            input_cost_per_token: 0.00002,
            output_cost_per_token: 0.0001,
            cache_creation_input_token_cost: 0.000025,
            cache_creation_1h_input_token_cost: 0.00004,
            cache_read_input_token_cost: 0.000002,
        };
        let configured = HashMap::from([
            ("claude-fable-5-1".to_string(), older),
            ("claude-fable-5-3".to_string(), newer),
        ]);

        assert_eq!(
            nearest_priced_model(
                "claude-fable-5-2",
                &HashMap::new(),
                &configured,
                &HashMap::new(),
            )
            .map(|(model, pricing)| (model, pricing.input_cost_per_token)),
            Some(("claude-fable-5-1".to_string(), older.input_cost_per_token))
        );
        assert_eq!(
            nearest_priced_model(
                "claude-5-2-fable",
                &HashMap::new(),
                &configured,
                &HashMap::new(),
            )
            .map(|(model, pricing)| (model, pricing.input_cost_per_token)),
            Some(("claude-fable-5-1".to_string(), older.input_cost_per_token))
        );
        assert_eq!(
            nearest_priced_model(
                "claude-fable-5-0",
                &HashMap::new(),
                &configured,
                &HashMap::new(),
            )
            .map(|(model, pricing)| (model, pricing.input_cost_per_token)),
            Some(("claude-fable-5-1".to_string(), older.input_cost_per_token))
        );
    }

    #[test]
    fn estimate_matches_dashed_request_to_dotted_price_model() {
        let catalog = PricingCatalog::new();
        catalog.upsert_manual_price(
            "claude-opus-4.8",
            ModelPricing {
                input_cost_per_token: 0.000007,
                output_cost_per_token: 0.000031,
                cache_creation_input_token_cost: 0.000008,
                cache_creation_1h_input_token_cost: 0.0,
                cache_read_input_token_cost: 0.0000007,
            },
        );
        let usage = CacheUsage {
            total_input_tokens: 100,
            input_tokens: 100,
            output_tokens: 10,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };

        let estimate = catalog.estimate("claude-opus-4-8", usage);

        assert!(estimate.available);
        assert_eq!(estimate.model, "claude-opus-4.8");
        assert!((estimate.cost_usd - 0.00101).abs() < f64::EPSILON);
    }

    #[test]
    fn estimate_matches_dotted_request_to_dashed_price_model() {
        let catalog = PricingCatalog::new();
        catalog.upsert_manual_price(
            "claude-opus-4-8",
            ModelPricing {
                input_cost_per_token: 0.000007,
                output_cost_per_token: 0.000031,
                cache_creation_input_token_cost: 0.000008,
                cache_creation_1h_input_token_cost: 0.0,
                cache_read_input_token_cost: 0.0000007,
            },
        );
        let usage = CacheUsage {
            total_input_tokens: 100,
            input_tokens: 100,
            output_tokens: 10,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };

        let estimate = catalog.estimate("claude-opus-4.8", usage);

        assert!(estimate.available);
        assert_eq!(estimate.model, "claude-opus-4-8");
        assert!((estimate.cost_usd - 0.00101).abs() < f64::EPSILON);
    }

    #[test]
    fn dotted_manual_alias_overrides_existing_dashed_builtin_and_public_prices() {
        let catalog = PricingCatalog::new();
        let manual = ModelPricing {
            input_cost_per_token: 0.000009,
            output_cost_per_token: 0.000045,
            cache_creation_input_token_cost: 0.00001125,
            cache_creation_1h_input_token_cost: 0.000018,
            cache_read_input_token_cost: 0.0000009,
        };
        let public = ModelPricing {
            input_cost_per_token: 0.000004,
            output_cost_per_token: 0.00002,
            cache_creation_input_token_cost: 0.000005,
            cache_creation_1h_input_token_cost: 0.000008,
            cache_read_input_token_cost: 0.0000004,
        };
        catalog
            .inner
            .write()
            .prices
            .insert("claude-sonnet-4-5".to_string(), public);
        catalog.upsert_manual_price("claude-sonnet-4.5", manual);

        assert_eq!(catalog.model_pricing("claude-sonnet-4-5"), Some(manual));
        assert_eq!(catalog.model_pricing("claude-sonnet-4.5"), Some(manual));
        assert_eq!(
            catalog
                .estimate(
                    "claude-sonnet-4-5",
                    CacheUsage {
                        total_input_tokens: 1_000_000,
                        input_tokens: 1_000_000,
                        output_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        cache_creation_5m_input_tokens: 0,
                        cache_creation_1h_input_tokens: 0,
                    },
                )
                .cost_usd,
            manual.input_cost_per_token * 1_000_000.0
        );

        catalog
            .inner
            .write()
            .prices
            .insert("claude-fable-5-1".to_string(), public);
        catalog.upsert_manual_price("claude-fable-5.1", manual);
        assert_eq!(catalog.model_pricing("claude-fable-5-2"), Some(manual));
        catalog.delete_manual_price("claude-fable-5.1");
        assert_eq!(catalog.model_pricing("claude-fable-5-2"), Some(public));
    }

    #[test]
    fn litellm_candidate_keys_cover_anthropic_prefixed_models() {
        let sonnet = litellm_candidate_keys("claude-sonnet-4-6");
        assert!(sonnet.contains(&"anthropic.claude-sonnet-4-6".to_string()));

        let opus = litellm_candidate_keys("claude-opus-4-6");
        assert!(opus.contains(&"anthropic.claude-opus-4-6-v1".to_string()));

        let haiku = litellm_candidate_keys("claude-haiku-4-5");
        assert!(haiku.contains(&"anthropic.claude-haiku-4-5-20251001-v1:0".to_string()));
    }

    #[test]
    fn pricing_sync_candidates_include_capability_models_and_family_version_fallbacks() {
        let candidates = pricing_sync_candidates([
            "claude-opus-4-8-thinking".to_string(),
            "claude-opus-4.8".to_string(),
            "claude-opus-4-8-20260529".to_string(),
        ]);

        assert!(candidates.contains("claude-opus-4-8-thinking"));
        assert!(candidates.contains("claude-opus-4-8"));
        assert!(candidates.contains("claude-opus-4-8-20260529"));
        assert!(candidates.contains("claude-fable-5-1"));
        assert!(candidates.contains("claude-mythos-5-2"));
        assert!(candidates.contains("claude-opus-5-5"));
    }

    #[test]
    fn public_feed_discovers_new_anthropic_models_and_rejects_other_providers() {
        let value = serde_json::json!({
            "claude-haiku-6-1": {
                "litellm_provider": "anthropic",
                "mode": "chat",
                "input_cost_per_token": 0.000007,
                "output_cost_per_token": 0.000035
            },
            "anthropic/claude-sonnet-6-1": {
                "litellm_provider": "bedrock",
                "mode": "chat",
                "input_cost_per_token": 0.0007,
                "output_cost_per_token": 0.0035
            },
            "claude-mythos-5-3": {
                "litellm_provider": "anthropic",
                "mode": "embedding",
                "input_cost_per_token": 0.0007,
                "output_cost_per_token": 0.0035
            }
        });

        let prices =
            parse_litellm_prices(&value, &pricing_sync_candidates(std::iter::empty())).unwrap();

        assert_eq!(prices["claude-haiku-6-1"].input_cost_per_token, 0.000007);
        assert!(!prices.contains_key("claude-sonnet-6-1"));
        assert!(!prices.contains_key("claude-mythos-5-3"));
    }

    #[tokio::test]
    async fn pricing_sync_discovers_models_and_preserves_snapshot_on_http_or_payload_failure() {
        let catalog = PricingCatalog::new();
        let previous = ModelPricing {
            input_cost_per_token: 0.000009,
            output_cost_per_token: 0.000045,
            cache_creation_input_token_cost: 0.00001125,
            cache_creation_1h_input_token_cost: 0.000018,
            cache_read_input_token_cost: 0.0000009,
        };
        catalog.upsert_manual_price("claude-haiku-6-1", previous);

        let feed = serde_json::json!({
            "claude-haiku-6-1": {
                "litellm_provider": "anthropic",
                "mode": "chat",
                "input_cost_per_token": 0.000007,
                "output_cost_per_token": 0.000035
            }
        });
        let (url, task) = serve_once(200, serde_json::to_vec(&feed).unwrap()).await;
        let synced = catalog
            .sync_for_models_from_url(&url, Vec::<String>::new())
            .await;
        task.await.unwrap();
        assert!(synced.last_error.is_none());
        assert!(synced.persisted_base_models.iter().any(|item| {
            item.model == "claude-haiku-6-1"
                && item.source.as_deref() == Some(LITELLM_SOURCE)
                && item.pricing.input_cost_per_token == 0.000007
        }));

        let prior_snapshot = catalog
            .status()
            .persisted_base_models
            .into_iter()
            .find(|item| item.model == "claude-haiku-6-1")
            .unwrap();
        for (status, body) in [(500, Vec::new()), (200, b"{".to_vec())] {
            let (url, task) = serve_once(status, body).await;
            let failed = catalog
                .sync_for_models_from_url(&url, Vec::<String>::new())
                .await;
            task.await.unwrap();
            assert!(failed.last_error.is_some());
            let current = catalog
                .status()
                .persisted_base_models
                .into_iter()
                .find(|item| item.model == "claude-haiku-6-1")
                .unwrap();
            assert_eq!(current.pricing, prior_snapshot.pricing);
            assert_eq!(current.source, prior_snapshot.source);
        }
    }

    #[test]
    fn manual_price_overrides_public_price_and_can_be_removed() {
        let manual = ModelPricing {
            input_cost_per_token: 0.000009,
            output_cost_per_token: 0.000045,
            cache_creation_input_token_cost: 0.00001125,
            cache_creation_1h_input_token_cost: 0.000018,
            cache_read_input_token_cost: 0.0000009,
        };
        let public = ModelPricing {
            input_cost_per_token: 0.000002,
            output_cost_per_token: 0.00001,
            cache_creation_input_token_cost: 0.0000025,
            cache_creation_1h_input_token_cost: 0.000004,
            cache_read_input_token_cost: 0.0000001,
        };
        let (base_prices, base_sources) = merge_price_sources(&HashMap::from([
            ("claude-sonnet-5-5".to_string(), public),
            ("claude-fable-5-1".to_string(), manual),
        ]));
        let base_models = base_prices
            .iter()
            .map(|(model, pricing)| ModelPriceItem {
                model: model.clone(),
                pricing: *pricing,
                source: base_sources.get(model).cloned(),
            })
            .collect::<Vec<_>>();
        let mut models = base_models.clone();
        models.push(ModelPriceItem {
            model: "claude-sonnet-5-5".to_string(),
            pricing: manual,
            source: Some(MANUAL_PRICING_SOURCE.to_string()),
        });
        let catalog = PricingCatalog::new();
        catalog.load_persisted_status(PricingStatus {
            available: true,
            source: LITELLM_SOURCE.to_string(),
            source_url: DEFAULT_PRICING_SOURCE_URL.to_string(),
            model_count: models.len(),
            unpriced_models: Vec::new(),
            persisted_base_models: base_models,
            last_synced_at: None,
            last_error: None,
            models,
        });
        let effective = catalog
            .status()
            .models
            .into_iter()
            .find(|item| item.model == "claude-sonnet-5-5")
            .unwrap();
        assert_eq!(effective.pricing, manual);
        assert_eq!(effective.source.as_deref(), Some(MANUAL_PRICING_SOURCE));
        let without_manual = catalog.delete_manual_price("claude-sonnet-5-5");
        let restored = without_manual
            .models
            .into_iter()
            .find(|item| item.model == "claude-sonnet-5-5")
            .unwrap();
        assert_eq!(restored.pricing, public);
        assert_eq!(restored.source.as_deref(), Some(LITELLM_SOURCE));
        assert!(catalog.status().models.iter().any(|item| {
            item.model == "claude-opus-5-5" && item.source.as_deref() == Some(FALLBACK_SOURCE)
        }));
    }

    #[test]
    fn parse_litellm_prices_uses_capability_models_without_static_whitelist() {
        let value = serde_json::json!({
            "claude-opus-4-8": {
                "input_cost_per_token": 0.000005,
                "output_cost_per_token": 0.000025,
                "cache_creation_input_token_cost": 0.00000625,
                "cache_read_input_token_cost": 0.0000005
            }
        });
        let candidates = pricing_sync_candidates(["claude-opus-4-8-thinking".to_string()]);

        let prices = parse_litellm_prices(&value, &candidates).unwrap();

        assert_eq!(
            prices.get("claude-opus-4-8").copied(),
            Some(ModelPricing {
                input_cost_per_token: 0.000005,
                output_cost_per_token: 0.000025,
                cache_creation_input_token_cost: 0.00000625,
                // The source has no 1-hour rate, so it is derived as 2x input.
                cache_creation_1h_input_token_cost: 0.00001,
                cache_read_input_token_cost: 0.0000005,
            })
        );
        assert!(!prices.contains_key("claude-opus-4-7"));
    }

    #[test]
    fn parse_litellm_fable_and_mythos_prices_from_public_catalog() {
        let value = serde_json::json!({
            "claude-fable-5-1": {
                "input_cost_per_token": 0.00001,
                "output_cost_per_token": 0.00005,
                "cache_creation_input_token_cost": 0.0000125,
                "cache_read_input_token_cost": 0.00000025
            },
            "claude-mythos-5-2": {
                "input_cost_per_token": 0.000008,
                "output_cost_per_token": 0.00004,
                "cache_creation_input_token_cost": 0.00001,
                "cache_read_input_token_cost": 0.0000002
            }
        });
        let candidates = pricing_sync_candidates(std::iter::empty());
        let prices = parse_litellm_prices(&value, &candidates).unwrap();

        assert_eq!(prices["claude-fable-5-1"].input_cost_per_token, 0.00001);
        assert_eq!(prices["claude-mythos-5-2"].output_cost_per_token, 0.00004);
    }

    #[test]
    fn litellm_one_hour_cache_write_rate_is_read_and_billed() {
        let value = serde_json::json!({
            "claude-opus-5-5": {
                "input_cost_per_token": 0.000004,
                "output_cost_per_token": 0.00002,
                "cache_creation_input_token_cost": 0.000005,
                "cache_creation_input_token_cost_above_1hr": 0.000008,
                "cache_read_input_token_cost": 0.0000002
            }
        });
        let candidates = pricing_sync_candidates(["claude-opus-5-5".to_string()]);
        let pricing = parse_litellm_prices(&value, &candidates).unwrap()["claude-opus-5-5"];
        assert_eq!(pricing.cache_creation_1h_input_token_cost, 0.000008);

        let write = |five_minutes: i32, one_hour: i32| CacheUsage {
            total_input_tokens: five_minutes + one_hour,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: five_minutes + one_hour,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: five_minutes,
            cache_creation_1h_input_tokens: one_hour,
        };
        let five_minute_cost = pricing.estimate(write(1_000_000, 0));
        let one_hour_cost = pricing.estimate(write(0, 1_000_000));
        assert!((five_minute_cost - 5.0).abs() < 1e-9, "{five_minute_cost}");
        assert!((one_hour_cost - 8.0).abs() < 1e-9, "{one_hour_cost}");
        // Mixed writes bill each part at its own rate.
        assert!((pricing.estimate(write(500_000, 500_000)) - 6.5).abs() < 1e-9);
    }

    #[test]
    fn missing_one_hour_rate_bills_two_times_input() {
        let pricing = ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_creation_input_token_cost: 0.00000375,
            cache_creation_1h_input_token_cost: 0.0,
            cache_read_input_token_cost: 0.0000003,
        };
        assert_eq!(
            pricing.effective_cache_creation_1h_input_token_cost(),
            0.000006
        );
        let usage = CacheUsage {
            total_input_tokens: 1_000_000,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 1_000_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 1_000_000,
        };
        assert!((pricing.estimate(usage) - 6.0).abs() < 1e-9);
        // Built-in prices carry the 1-hour rate explicitly.
        let builtin = fallback_prices()["claude-sonnet-4-5"];
        assert_eq!(builtin.cache_creation_1h_input_token_cost, 0.000006);
    }
}
