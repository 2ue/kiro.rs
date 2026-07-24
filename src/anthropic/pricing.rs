use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::cache::CacheUsage;
use super::converter::map_model;

pub const DEFAULT_PRICING_SOURCE_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

pub const FALLBACK_SOURCE: &str = "built-in";
pub const LITELLM_SOURCE: &str = "litellm";
pub const MANUAL_PRICING_SOURCE: &str = "manual";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPricing {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cache_creation_input_token_cost: f64,
    pub cache_read_input_token_cost: f64,
}

impl ModelPricing {
    pub fn estimate(self, usage: CacheUsage) -> f64 {
        (usage.input_tokens.max(0) as f64 * self.input_cost_per_token)
            + (usage.output_tokens.max(0) as f64 * self.output_cost_per_token)
            + (usage.cache_creation_input_tokens.max(0) as f64
                * self.cache_creation_input_token_cost)
            + (usage.cache_read_input_tokens.max(0) as f64 * self.cache_read_input_token_cost)
    }

    fn is_usable(self) -> bool {
        self.input_cost_per_token > 0.0 || self.output_cost_per_token > 0.0
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub models: Vec<ModelPriceItem>,
}

#[derive(Debug, Clone)]
struct PricingSnapshot {
    prices: HashMap<String, ModelPricing>,
    sources: HashMap<String, String>,
    source: String,
    source_url: String,
    last_synced_at: Option<String>,
    last_error: Option<String>,
}

impl PricingSnapshot {
    fn status(&self) -> PricingStatus {
        let mut models: Vec<ModelPriceItem> = self
            .prices
            .iter()
            .map(|(model, pricing)| ModelPriceItem {
                model: model.clone(),
                pricing: *pricing,
                source: self.sources.get(model).cloned(),
            })
            .collect();
        models.sort_by(|a, b| a.model.cmp(&b.model));

        PricingStatus {
            available: !self.prices.is_empty(),
            source: self.source.clone(),
            source_url: self.source_url.clone(),
            model_count: self.prices.len(),
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
            source: FALLBACK_SOURCE.to_string(),
            source_url: DEFAULT_PRICING_SOURCE_URL.to_string(),
            last_synced_at: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PricingCatalog {
    inner: Arc<RwLock<PricingSnapshot>>,
    client: reqwest::Client,
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
            client: reqwest::Client::new(),
        }
    }

    pub fn load_persisted_status(&self, status: PricingStatus) {
        if status.models.is_empty() {
            return;
        }
        let mut prices = HashMap::with_capacity(status.models.len());
        let mut sources = HashMap::with_capacity(status.models.len());
        for item in status.models {
            let model = normalize_pricing_model_id(&item.model);
            prices.insert(model.clone(), item.pricing);
            sources.insert(model, item.source.unwrap_or_else(|| status.source.clone()));
        }
        let mut inner = self.inner.write();
        inner.prices = prices;
        inner.sources = sources;
        inner.source = status.source;
        inner.source_url = status.source_url;
        inner.last_synced_at = status.last_synced_at;
        inner.last_error = status.last_error;
    }

    pub fn status(&self) -> PricingStatus {
        self.inner.read().status()
    }

    pub fn estimate(&self, model: &str, usage: CacheUsage) -> PricingEstimate {
        let pricing_candidates = pricing_model_candidates(model);
        let fallback_model = pricing_candidates
            .last()
            .cloned()
            .unwrap_or_else(|| canonical_pricing_model(model));
        let inner = self.inner.read();
        let pricing = pricing_candidates.iter().find_map(|candidate| {
            inner
                .prices
                .get(candidate)
                .copied()
                .filter(|pricing| pricing.is_usable())
                .map(|pricing| (candidate.clone(), pricing))
        });
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
        inner.prices.insert(model.clone(), pricing);
        inner
            .sources
            .insert(model, MANUAL_PRICING_SOURCE.to_string());
        Some(inner.status())
    }

    pub fn delete_manual_price(&self, model: &str) -> PricingStatus {
        let model = normalize_pricing_model_id(model);
        let mut inner = self.inner.write();
        if inner
            .sources
            .get(&model)
            .is_some_and(|source| source.eq_ignore_ascii_case(MANUAL_PRICING_SOURCE))
        {
            inner.prices.remove(&model);
            inner.sources.remove(&model);
        }
        inner.status()
    }

    pub async fn sync_for_models(
        &self,
        candidate_models: impl IntoIterator<Item = String>,
    ) -> PricingStatus {
        let candidate_models = pricing_sync_candidates(candidate_models);
        match self.fetch_remote_prices(&candidate_models).await {
            Ok(prices) => {
                let mut inner = self.inner.write();
                let remote_models = prices.keys().cloned().collect::<BTreeSet<_>>();
                let mut merged = prices;
                for (model, pricing) in inner
                    .prices
                    .iter()
                    .filter(|(model, _)| {
                        inner.sources.get(*model).is_some_and(|source| {
                            source.eq_ignore_ascii_case(MANUAL_PRICING_SOURCE)
                        })
                    })
                    .map(|(model, pricing)| (model.clone(), *pricing))
                    .collect::<Vec<_>>()
                {
                    merged.entry(model).or_insert(pricing);
                }
                let sources = merged
                    .keys()
                    .map(|model| {
                        let source = if inner.sources.get(model).is_some_and(|source| {
                            source.eq_ignore_ascii_case(MANUAL_PRICING_SOURCE)
                        }) && !remote_models.contains(model)
                        {
                            MANUAL_PRICING_SOURCE
                        } else {
                            LITELLM_SOURCE
                        };
                        (model.clone(), source.to_string())
                    })
                    .collect();
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
        candidate_models: &BTreeSet<String>,
    ) -> anyhow::Result<HashMap<String, ModelPricing>> {
        let value: serde_json::Value = self
            .client
            .get(DEFAULT_PRICING_SOURCE_URL)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        parse_litellm_prices(&value, candidate_models)
    }
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

    map_model(model)
        .unwrap_or_else(|| model.to_string())
        .replace('.', "-")
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
        if let Some(entry) = find_litellm_entry(object, &model) {
            if let Some(pricing) = pricing_from_entry(entry) {
                prices.insert(model.clone(), pricing);
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
        if let Some(value) = object.get(&key) {
            return Some(value);
        }
    }
    None
}

fn litellm_candidate_keys(canonical_model: &str) -> Vec<String> {
    let dotted = canonical_model.replace('-', ".");
    let mut keys = vec![
        canonical_model.to_string(),
        dotted.clone(),
        format!("anthropic.{}", canonical_model),
        format!("anthropic.{}", dotted),
    ];
    match canonical_model {
        "claude-haiku-4-5" => {
            keys.push("claude-haiku-4-5-20251001".to_string());
            keys.push("anthropic.claude-haiku-4-5-20251001-v1:0".to_string());
        }
        "claude-sonnet-4-5" => {
            keys.push("claude-sonnet-4-5-20250929".to_string());
            keys.push("anthropic.claude-sonnet-4-5-20250929-v1:0".to_string());
        }
        "claude-opus-4-5" => {
            keys.push("claude-opus-4-5-20251101".to_string());
            keys.push("anthropic.claude-opus-4-5-20251101-v1:0".to_string());
        }
        "claude-opus-4-6" => {
            keys.push("claude-opus-4-6-20260205".to_string());
            keys.push("anthropic.claude-opus-4-6-v1".to_string());
        }
        "claude-opus-4-7" => {
            keys.push("claude-opus-4-7-20260416".to_string());
            keys.push("anthropic.claude-opus-4-7".to_string());
        }
        "claude-sonnet-4-6" => {
            keys.push("anthropic.claude-sonnet-4-6".to_string());
        }
        _ => {}
    }
    let mut deduped = Vec::with_capacity(keys.len());
    for key in keys {
        if !deduped.contains(&key) {
            deduped.push(key);
        }
    }
    deduped
}

fn pricing_sync_candidates(models: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    let mut candidates = BTreeSet::new();
    for model in models {
        for candidate in pricing_model_candidates(&model) {
            candidates.insert(candidate);
        }
    }
    if candidates.is_empty() {
        candidates.extend(default_pricing_models());
    }
    candidates
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
    let family = ["opus", "sonnet", "haiku"]
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
    let family = ["opus", "sonnet", "haiku"]
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
    if numbers.len() < 2 && !current.is_empty() {
        if let Ok(value) = current.parse::<u32>() {
            numbers.push(value);
        }
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
        cache_read_input_token_cost: number_field(value, "cache_read_input_token_cost")
            .unwrap_or_else(|| number_field(value, "input_cost_per_token").unwrap_or(0.0) * 0.1),
    };
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
        "claude-opus-4-5",
        "claude-opus-4-6",
        "claude-opus-4-7",
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
                cache_read_input_token_cost: 0.0000001,
            },
        ),
        (
            "claude-sonnet-4-5",
            ModelPricing {
                input_cost_per_token: 0.000003,
                output_cost_per_token: 0.000015,
                cache_creation_input_token_cost: 0.00000375,
                cache_read_input_token_cost: 0.0000003,
            },
        ),
        (
            "claude-sonnet-4-6",
            ModelPricing {
                input_cost_per_token: 0.000003,
                output_cost_per_token: 0.000015,
                cache_creation_input_token_cost: 0.00000375,
                cache_read_input_token_cost: 0.0000003,
            },
        ),
        (
            "claude-opus-4-5",
            ModelPricing {
                input_cost_per_token: 0.000005,
                output_cost_per_token: 0.000025,
                cache_creation_input_token_cost: 0.00000625,
                cache_read_input_token_cost: 0.0000005,
            },
        ),
        (
            "claude-opus-4-6",
            ModelPricing {
                input_cost_per_token: 0.000005,
                output_cost_per_token: 0.000025,
                cache_creation_input_token_cost: 0.00000625,
                cache_read_input_token_cost: 0.0000005,
            },
        ),
        (
            "claude-opus-4-7",
            ModelPricing {
                input_cost_per_token: 0.000005,
                output_cost_per_token: 0.000025,
                cache_creation_input_token_cost: 0.00000625,
                cache_read_input_token_cost: 0.0000005,
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

    #[test]
    fn canonical_pricing_model_maps_aliases_and_thinking_suffixes() {
        assert_eq!(canonical_pricing_model("opus"), "claude-opus-4-7");
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
    fn persisted_remote_pricing_keeps_builtin_estimate_fallback() {
        let catalog = PricingCatalog::new();
        catalog.load_persisted_status(PricingStatus {
            available: true,
            source: LITELLM_SOURCE.to_string(),
            source_url: DEFAULT_PRICING_SOURCE_URL.to_string(),
            model_count: 1,
            last_synced_at: Some("2026-06-09T00:00:00Z".to_string()),
            last_error: None,
            models: vec![ModelPriceItem {
                model: "claude-sonnet-4-5".to_string(),
                pricing: ModelPricing {
                    input_cost_per_token: 0.000003,
                    output_cost_per_token: 0.000015,
                    cache_creation_input_token_cost: 0.00000375,
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
    fn estimate_matches_dashed_request_to_dotted_price_model() {
        let catalog = PricingCatalog::new();
        catalog.upsert_manual_price(
            "claude-opus-4.8",
            ModelPricing {
                input_cost_per_token: 0.000007,
                output_cost_per_token: 0.000031,
                cache_creation_input_token_cost: 0.000008,
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
                cache_read_input_token_cost: 0.0000005,
            })
        );
        assert!(!prices.contains_key("claude-opus-4-7"));
    }
}
