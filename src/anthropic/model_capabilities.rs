use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::anthropic::types::Model;
use crate::kiro::model::available_models::KiroAvailableModel;

const FALLBACK_SOURCE: &str = "built-in";
const KIRO_SOURCE: &str = "kiro-list-available-models";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilityItem {
    pub model: String,
    pub display_name: String,
    pub description: Option<String>,
    pub max_input_tokens: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub supports_prompt_caching: Option<bool>,
    pub supported_input_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilitiesStatus {
    pub available: bool,
    pub source: String,
    pub model_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub models: Vec<ModelCapabilityItem>,
}

#[derive(Debug, Clone)]
struct ModelCapabilitiesSnapshot {
    models: BTreeMap<String, ModelCapabilityItem>,
    source: String,
    last_synced_at: Option<String>,
    last_error: Option<String>,
}

impl ModelCapabilitiesSnapshot {
    fn status(&self) -> ModelCapabilitiesStatus {
        let models: Vec<ModelCapabilityItem> = self.models.values().cloned().collect();
        ModelCapabilitiesStatus {
            available: !models.is_empty(),
            source: self.source.clone(),
            model_count: models.len(),
            last_synced_at: self.last_synced_at.clone(),
            last_error: self.last_error.clone(),
            models,
        }
    }
}

impl Default for ModelCapabilitiesSnapshot {
    fn default() -> Self {
        let models = static_model_capabilities()
            .into_iter()
            .map(|model| (model.model.clone(), model))
            .collect();
        Self {
            models,
            source: FALLBACK_SOURCE.to_string(),
            last_synced_at: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelCapabilitiesCatalog {
    inner: Arc<RwLock<ModelCapabilitiesSnapshot>>,
}

impl Default for ModelCapabilitiesCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelCapabilitiesCatalog {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ModelCapabilitiesSnapshot::default())),
        }
    }

    pub fn load_persisted_status(&self, status: ModelCapabilitiesStatus) {
        if status.models.is_empty() {
            return;
        }
        let mut models: BTreeMap<String, ModelCapabilityItem> = static_model_capabilities()
            .into_iter()
            .map(|item| (item.model.clone(), item))
            .collect();
        for item in status.models {
            models.insert(item.model.clone(), item);
        }
        let mut inner = self.inner.write();
        inner.models = models;
        inner.source = status.source;
        inner.last_synced_at = status.last_synced_at;
        inner.last_error = status.last_error;
    }

    pub fn status(&self) -> ModelCapabilitiesStatus {
        self.inner.read().status()
    }

    pub fn anthropic_models(&self) -> Vec<Model> {
        let status = self.status();
        let mut models: HashMap<String, Model> = static_anthropic_models()
            .into_iter()
            .map(|model| (model.id.clone(), model))
            .collect();
        for item in status.models {
            let created = model_created_at(&item.model);
            let max_tokens = item.max_output_tokens.unwrap_or(64_000).max(1);
            models.insert(
                item.model.clone(),
                Model {
                    id: item.model,
                    object: "model".to_string(),
                    created,
                    owned_by: "anthropic".to_string(),
                    display_name: item.display_name,
                    model_type: "chat".to_string(),
                    max_tokens,
                },
            );
        }
        let mut models: Vec<Model> = models.into_values().collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models
    }

    pub fn sync_from_kiro_models(
        &self,
        models: Vec<KiroAvailableModel>,
    ) -> ModelCapabilitiesStatus {
        let mut merged: BTreeMap<String, ModelCapabilityItem> = static_model_capabilities()
            .into_iter()
            .map(|item| (item.model.clone(), item))
            .collect();
        for model in models {
            if let Some(item) = model_capability_from_kiro(model) {
                merged.insert(item.model.clone(), item);
            }
        }
        let mut inner = self.inner.write();
        inner.models = merged;
        inner.source = KIRO_SOURCE.to_string();
        inner.last_synced_at = Some(Utc::now().to_rfc3339());
        inner.last_error = None;
        inner.status()
    }

    pub fn record_sync_error(&self, error: impl Into<String>) -> ModelCapabilitiesStatus {
        let mut inner = self.inner.write();
        inner.last_error = Some(error.into());
        inner.status()
    }
}

fn model_capability_from_kiro(model: KiroAvailableModel) -> Option<ModelCapabilityItem> {
    let model_id = model.model_id.trim().to_string();
    if model_id.is_empty() {
        return None;
    }
    let token_limits = model.token_limits;
    let prompt_caching = model.prompt_caching;
    Some(ModelCapabilityItem {
        display_name: model
            .model_name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| model_id.clone()),
        description: model.description,
        max_input_tokens: token_limits
            .as_ref()
            .and_then(|limits| limits.max_input_tokens)
            .filter(|value| *value > 0),
        max_output_tokens: token_limits
            .as_ref()
            .and_then(|limits| limits.max_output_tokens)
            .filter(|value| *value > 0),
        supports_prompt_caching: prompt_caching
            .as_ref()
            .map(|cache| cache.supports_prompt_caching),
        supported_input_types: model.supported_input_types,
        model: model_id,
    })
}

fn static_model_capabilities() -> Vec<ModelCapabilityItem> {
    static_anthropic_models()
        .into_iter()
        .map(|model| ModelCapabilityItem {
            model: model.id,
            display_name: model.display_name,
            description: None,
            max_input_tokens: Some(if model.max_tokens >= 128_000 {
                1_000_000
            } else {
                200_000
            }),
            max_output_tokens: Some(model.max_tokens),
            supports_prompt_caching: Some(true),
            supported_input_types: vec!["TEXT".to_string(), "IMAGE".to_string()],
        })
        .collect()
}

pub fn static_anthropic_models() -> Vec<Model> {
    let specs = [
        ("opus", "Claude Code Alias: Opus", 1_776_276_000, 64_000),
        (
            "opusplan",
            "Claude Code Alias: Opus Plan",
            1_776_276_000,
            64_000,
        ),
        (
            "best",
            "Claude Code Alias: Best Available",
            1_776_276_000,
            64_000,
        ),
        (
            "default",
            "Claude Code Alias: Default",
            1_776_276_000,
            64_000,
        ),
        ("sonnet", "Claude Code Alias: Sonnet", 1_771_286_400, 64_000),
        ("haiku", "Claude Code Alias: Haiku", 1_760_486_400, 64_000),
        ("claude-opus-4-7", "Claude Opus 4.7", 1_776_276_000, 64_000),
        (
            "claude-opus-4-7-thinking",
            "Claude Opus 4.7 (Thinking)",
            1_776_276_000,
            64_000,
        ),
        ("claude-opus-4-6", "Claude Opus 4.6", 1_770_163_200, 64_000),
        (
            "claude-opus-4-6-thinking",
            "Claude Opus 4.6 (Thinking)",
            1_770_163_200,
            64_000,
        ),
        (
            "claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            1_771_286_400,
            64_000,
        ),
        (
            "claude-sonnet-4-6-thinking",
            "Claude Sonnet 4.6 (Thinking)",
            1_771_286_400,
            64_000,
        ),
        (
            "claude-opus-4-5-20251101",
            "Claude Opus 4.5",
            1_763_942_400,
            64_000,
        ),
        (
            "claude-opus-4-5-20251101-thinking",
            "Claude Opus 4.5 (Thinking)",
            1_763_942_400,
            64_000,
        ),
        (
            "claude-sonnet-4-5-20250929",
            "Claude Sonnet 4.5",
            1_759_104_000,
            64_000,
        ),
        (
            "claude-sonnet-4-5-20250929-thinking",
            "Claude Sonnet 4.5 (Thinking)",
            1_759_104_000,
            64_000,
        ),
        (
            "claude-haiku-4-5-20251001",
            "Claude Haiku 4.5",
            1_760_486_400,
            64_000,
        ),
        (
            "claude-haiku-4-5-20251001-thinking",
            "Claude Haiku 4.5 (Thinking)",
            1_760_486_400,
            64_000,
        ),
    ];

    specs
        .into_iter()
        .map(|(id, display_name, created, max_tokens)| Model {
            id: id.to_string(),
            object: "model".to_string(),
            created,
            owned_by: "anthropic".to_string(),
            display_name: display_name.to_string(),
            model_type: "chat".to_string(),
            max_tokens,
        })
        .collect()
}

fn model_created_at(model: &str) -> i64 {
    static_anthropic_models()
        .into_iter()
        .find(|item| item.id == model)
        .map(|item| item.created)
        .unwrap_or_else(|| Utc::now().timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::model::available_models::{KiroAvailableModel, KiroModelTokenLimits};

    #[test]
    fn sync_from_kiro_models_merges_with_static_fallback() {
        let catalog = ModelCapabilitiesCatalog::new();
        let status = catalog.sync_from_kiro_models(vec![KiroAvailableModel {
            model_id: "claude-sonnet-4-9-20270101".to_string(),
            model_name: Some("Claude Sonnet 4.9".to_string()),
            token_limits: Some(KiroModelTokenLimits {
                max_input_tokens: Some(1_000_000),
                max_output_tokens: Some(128_000),
            }),
            ..Default::default()
        }]);

        assert_eq!(status.source, KIRO_SOURCE);
        assert!(status.models.iter().any(|model| model.model == "opus"));
        assert!(
            status
                .models
                .iter()
                .any(|model| model.model == "claude-sonnet-4-9-20270101")
        );
        let anthropic = catalog.anthropic_models();
        let synced = anthropic
            .iter()
            .find(|model| model.id == "claude-sonnet-4-9-20270101")
            .unwrap();
        assert_eq!(synced.max_tokens, 128_000);
    }
}
