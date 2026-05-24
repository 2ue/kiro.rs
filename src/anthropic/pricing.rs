use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::cache::CacheUsage;
use super::converter::map_model;

pub const DEFAULT_PRICING_SOURCE_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

const FALLBACK_SOURCE: &str = "built-in";
const LITELLM_SOURCE: &str = "litellm";

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPriceItem {
    pub model: String,
    pub pricing: ModelPricing,
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
        Self {
            prices: fallback_prices(),
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
        for item in status.models {
            prices.insert(item.model, item.pricing);
        }
        let mut inner = self.inner.write();
        inner.prices = prices;
        inner.source = status.source;
        inner.source_url = status.source_url;
        inner.last_synced_at = status.last_synced_at;
        inner.last_error = status.last_error;
    }

    pub fn status(&self) -> PricingStatus {
        self.inner.read().status()
    }

    pub fn estimate(&self, model: &str, usage: CacheUsage) -> PricingEstimate {
        let canonical_model = canonical_pricing_model(model);
        let pricing = self
            .inner
            .read()
            .prices
            .get(&canonical_model)
            .copied()
            .filter(|pricing| pricing.is_usable());

        match pricing {
            Some(pricing) => PricingEstimate {
                model: canonical_model,
                available: true,
                cost_usd: pricing.estimate(usage),
            },
            None => PricingEstimate {
                model: canonical_model,
                available: false,
                cost_usd: 0.0,
            },
        }
    }

    pub async fn sync(&self) -> PricingStatus {
        match self.fetch_remote_prices().await {
            Ok(prices) => {
                let mut inner = self.inner.write();
                inner.prices = prices;
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

    async fn fetch_remote_prices(&self) -> anyhow::Result<HashMap<String, ModelPricing>> {
        let value: serde_json::Value = self
            .client
            .get(DEFAULT_PRICING_SOURCE_URL)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        parse_litellm_prices(&value)
    }
}

#[derive(Debug, Clone)]
pub struct PricingEstimate {
    pub model: String,
    pub available: bool,
    pub cost_usd: f64,
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
) -> anyhow::Result<HashMap<String, ModelPricing>> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("pricing payload is not an object"))?;
    let mut prices = HashMap::new();
    for model in tracked_pricing_models() {
        if let Some(entry) = find_litellm_entry(object, &model) {
            if let Some(pricing) = pricing_from_entry(entry) {
                prices.insert(model, pricing);
            }
        }
    }

    if prices.is_empty() {
        anyhow::bail!("pricing payload did not contain tracked models");
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

fn tracked_pricing_models() -> BTreeSet<String> {
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
    fn litellm_candidate_keys_cover_anthropic_prefixed_models() {
        let sonnet = litellm_candidate_keys("claude-sonnet-4-6");
        assert!(sonnet.contains(&"anthropic.claude-sonnet-4-6".to_string()));

        let opus = litellm_candidate_keys("claude-opus-4-6");
        assert!(opus.contains(&"anthropic.claude-opus-4-6-v1".to_string()));

        let haiku = litellm_candidate_keys("claude-haiku-4-5");
        assert!(haiku.contains(&"anthropic.claude-haiku-4-5-20251001-v1:0".to_string()));
    }
}
