use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::anthropic::types::Model;
use crate::kiro::model::available_models::KiroAvailableModel;
use crate::model::config::{ModelMappingConfig, ModelMappingRuleKind, ModelResolutionMode};

pub const SEED_SOURCE: &str = "kiro-upstream-seed";
pub const KIRO_SOURCE: &str = "kiro-list-available-models";
pub const MANUAL_SOURCE: &str = "manual";
const SEED_JSON: &str = include_str!("../../data/kiro-upstream-models.seed.json");

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
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

impl ModelCapabilitiesStatus {
    pub fn should_refresh_from_seed(&self) -> bool {
        self.source == "built-in"
    }
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
        let models = seed_model_capabilities()
            .into_iter()
            .map(|model| (model.model.clone(), model))
            .collect();
        Self {
            models,
            source: SEED_SOURCE.to_string(),
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
        let models: BTreeMap<String, ModelCapabilityItem> = status
            .models
            .into_iter()
            .filter_map(|item| sanitize_capability_item(item, &status.source))
            .map(|item| (item.model.clone(), item))
            .collect();
        if models.is_empty() {
            return;
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
        let upstream_ids = status
            .models
            .iter()
            .map(|item| item.model.clone())
            .collect::<Vec<_>>();
        let mut models = HashMap::new();
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
        for model in static_anthropic_models() {
            if models.contains_key(&model.id) {
                continue;
            }
            let resolution = resolve_model_with_catalog(&model.id, &upstream_ids);
            if !matches!(
                resolution.source,
                ModelResolutionSource::Unsupported | ModelResolutionSource::PassThrough
            ) {
                models.insert(model.id.clone(), model);
            }
        }
        let mut models: Vec<Model> = models.into_values().collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models
    }

    pub fn max_input_tokens_for(&self, model: &str) -> Option<i32> {
        let model = normalize_model_id(model);
        self.inner
            .read()
            .models
            .get(&model)
            .and_then(|item| item.max_input_tokens)
            .filter(|tokens| *tokens > 0)
    }

    pub fn supports_prompt_caching_for(&self, model: &str) -> Option<bool> {
        let model = normalize_model_id(model);
        self.inner
            .read()
            .models
            .get(&model)
            .and_then(|item| item.supports_prompt_caching)
    }

    #[cfg(test)]
    pub fn resolve_model(&self, requested_model: &str) -> ModelResolution {
        self.resolve_model_with_mode(requested_model, ModelResolutionMode::Compatible)
    }

    #[cfg(test)]
    pub fn resolve_model_with_mode(
        &self,
        requested_model: &str,
        mode: ModelResolutionMode,
    ) -> ModelResolution {
        self.resolve_model_with_mapping(requested_model, mode, &ModelMappingConfig::default())
    }

    pub fn resolve_model_with_mapping(
        &self,
        requested_model: &str,
        mode: ModelResolutionMode,
        model_mapping: &ModelMappingConfig,
    ) -> ModelResolution {
        let inner = self.inner.read();
        let models = inner.models.keys().cloned().collect::<Vec<_>>();
        let manual_models = inner
            .models
            .values()
            .filter(|item| is_manual_source(item.source.as_deref()))
            .map(|item| item.model.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut resolution = resolve_model_with_catalog_mapping_and_mode(
            requested_model,
            &models,
            mode,
            model_mapping,
        );
        if let Some(upstream_model) = resolution.upstream_model.as_deref() {
            if manual_models.contains(upstream_model) {
                resolution.source = ModelResolutionSource::Manual;
                if let Some(note) = resolution.note.take() {
                    resolution.note = Some(format!("{} (manual supplement)", note));
                } else {
                    resolution.note = Some("manual supplement".to_string());
                }
            }
        }
        resolution
    }

    pub fn seed_status() -> ModelCapabilitiesStatus {
        let models = seed_model_capabilities();
        ModelCapabilitiesStatus {
            available: !models.is_empty(),
            source: SEED_SOURCE.to_string(),
            model_count: models.len(),
            last_synced_at: None,
            last_error: None,
            models,
        }
    }

    pub fn sync_from_kiro_models(
        &self,
        models: Vec<KiroAvailableModel>,
    ) -> ModelCapabilitiesStatus {
        let mut merged: BTreeMap<String, ModelCapabilityItem> = BTreeMap::new();
        for model in models {
            if let Some(item) = model_capability_from_kiro(model) {
                merged.insert(item.model.clone(), item);
            }
        }
        let using_seed_fallback = merged.is_empty();
        if merged.is_empty() {
            merged = seed_model_capabilities()
                .into_iter()
                .map(|item| (item.model.clone(), item))
                .collect();
        }
        let mut inner = self.inner.write();
        if !using_seed_fallback && !contains_claude_model_id(merged.keys().map(String::as_str)) {
            let previous_claude_models = inner
                .models
                .values()
                .filter(|item| is_claude_model_id(&item.model))
                .cloned()
                .collect::<Vec<_>>();
            let fallback_claude_models = if previous_claude_models.is_empty() {
                seed_model_capabilities()
                    .into_iter()
                    .filter(|item| is_claude_model_id(&item.model))
                    .collect::<Vec<_>>()
            } else {
                previous_claude_models
            };
            for item in fallback_claude_models {
                merged.entry(item.model.clone()).or_insert(item);
            }
        }
        let manual_models = inner
            .models
            .values()
            .filter(|item| is_manual_source(item.source.as_deref()))
            .cloned()
            .collect::<Vec<_>>();
        for item in manual_models {
            if using_seed_fallback {
                merged.insert(item.model.clone(), item);
            } else {
                merged.entry(item.model.clone()).or_insert(item);
            }
        }
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

    pub fn upsert_manual_model(&self, item: ModelCapabilityItem) -> ModelCapabilitiesStatus {
        let Some(item) = sanitize_capability_item(item, MANUAL_SOURCE) else {
            return self.status();
        };
        let mut inner = self.inner.write();
        inner.models.insert(item.model.clone(), item);
        inner.status()
    }

    pub fn delete_manual_model(&self, model: &str) -> ModelCapabilitiesStatus {
        let model = normalize_model_id(model);
        let mut inner = self.inner.write();
        if inner
            .models
            .get(&model)
            .is_some_and(|item| is_manual_source(item.source.as_deref()))
        {
            inner.models.remove(&model);
        }
        inner.status()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeedModelCapabilities {
    #[allow(dead_code)]
    version: Option<String>,
    #[allow(dead_code)]
    source: Option<String>,
    models: Vec<ModelCapabilityItem>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelResolutionSource {
    ExactUpstream,
    Manual,
    Alias,
    FamilyNormalized,
    PassThrough,
    Unsupported,
}

impl ModelResolutionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactUpstream => "exact_upstream",
            Self::Manual => "manual",
            Self::Alias => "alias",
            Self::FamilyNormalized => "family_normalized",
            Self::PassThrough => "pass_through",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelResolution {
    pub requested_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    pub source: ModelResolutionSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ModelResolution {
    pub fn exact(model: String) -> Self {
        Self {
            requested_model: model.clone(),
            upstream_model: Some(model),
            source: ModelResolutionSource::ExactUpstream,
            note: None,
        }
    }

    pub fn resolved(
        requested_model: String,
        upstream_model: String,
        source: ModelResolutionSource,
    ) -> Self {
        let note = if requested_model == upstream_model {
            None
        } else {
            Some(format!("{} -> {}", requested_model, upstream_model))
        };
        Self {
            requested_model,
            upstream_model: Some(upstream_model),
            source,
            note,
        }
    }

    pub fn unsupported(requested_model: String) -> Self {
        Self {
            requested_model,
            upstream_model: None,
            source: ModelResolutionSource::Unsupported,
            note: None,
        }
    }

    pub fn pass_through(requested_model: String) -> Self {
        Self {
            requested_model: requested_model.clone(),
            upstream_model: Some(requested_model),
            source: ModelResolutionSource::PassThrough,
            note: Some("no mapping rule matched; passing requested model through".to_string()),
        }
    }

    pub fn is_remapped(&self) -> bool {
        self.upstream_model
            .as_deref()
            .is_some_and(|upstream| upstream != self.requested_model)
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
        source: Some(KIRO_SOURCE.to_string()),
    })
}

fn seed_model_capabilities() -> Vec<ModelCapabilityItem> {
    serde_json::from_str::<SeedModelCapabilities>(SEED_JSON)
        .map(|seed| {
            seed.models
                .into_iter()
                .filter(|item| !item.model.trim().is_empty())
                .map(|mut item| {
                    item.model = normalize_model_id(&item.model);
                    if item.display_name.trim().is_empty() {
                        item.display_name = item.model.clone();
                    }
                    if item.supported_input_types.is_empty() {
                        item.supported_input_types = vec!["TEXT".to_string()];
                    }
                    item.source = Some(SEED_SOURCE.to_string());
                    item
                })
                .collect()
        })
        .unwrap_or_else(|err| {
            tracing::warn!(
                "加载内置 Kiro 模型 seed 失败，使用静态兼容模型继续: {}",
                err
            );
            static_model_capabilities()
        })
}

fn sanitize_capability_item(
    mut item: ModelCapabilityItem,
    default_source: &str,
) -> Option<ModelCapabilityItem> {
    item.model = normalize_model_id(&item.model);
    if item.model.is_empty() {
        return None;
    }
    item.display_name = item.display_name.trim().to_string();
    if item.display_name.is_empty() {
        item.display_name = item.model.clone();
    }
    item.description = item
        .description
        .and_then(|value| (!value.trim().is_empty()).then(|| value.trim().to_string()));
    item.max_input_tokens = item.max_input_tokens.filter(|value| *value > 0);
    item.max_output_tokens = item.max_output_tokens.filter(|value| *value > 0);
    item.supported_input_types = normalize_supported_input_types(item.supported_input_types);
    item.source = Some(
        item.source
            .as_deref()
            .filter(|source| !source.trim().is_empty())
            .unwrap_or(default_source)
            .trim()
            .to_string(),
    );
    Some(item)
}

pub fn normalize_supported_input_types(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim().to_ascii_uppercase();
        if value.is_empty() || normalized.contains(&value) {
            continue;
        }
        normalized.push(value);
    }
    if normalized.is_empty() {
        normalized.push("TEXT".to_string());
    }
    normalized
}

pub fn is_manual_source(source: Option<&str>) -> bool {
    source.is_some_and(|source| source.eq_ignore_ascii_case(MANUAL_SOURCE))
}

pub fn normalize_model_id(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

pub fn strip_model_compat_suffixes(model: &str) -> (String, bool) {
    let value = strip_model_1m_suffix(model);
    if let Some(base) = value.strip_suffix("-thinking") {
        (base.to_string(), true)
    } else {
        (value, false)
    }
}

pub fn strip_model_1m_suffix(model: &str) -> String {
    let value = normalize_model_id(model);
    if let Some(base) = value.strip_suffix("[1m]") {
        return base.to_string();
    }
    if let Some(base) = value.strip_suffix("-thinking") {
        if let Some(base_without_1m) = base.strip_suffix("[1m]") {
            return format!("{}-thinking", base_without_1m);
        }
    }
    value
}

pub fn resolve_model_with_catalog(
    requested_model: &str,
    upstream_models: &[String],
) -> ModelResolution {
    resolve_model_with_catalog_and_mode(
        requested_model,
        upstream_models,
        ModelResolutionMode::Compatible,
    )
}

pub fn resolve_model_with_catalog_and_mode(
    requested_model: &str,
    upstream_models: &[String],
    mode: ModelResolutionMode,
) -> ModelResolution {
    resolve_model_with_catalog_mapping_and_mode(
        requested_model,
        upstream_models,
        mode,
        &ModelMappingConfig::default(),
    )
}

pub fn resolve_model_with_catalog_mapping_and_mode(
    requested_model: &str,
    upstream_models: &[String],
    mode: ModelResolutionMode,
    model_mapping: &ModelMappingConfig,
) -> ModelResolution {
    let requested = normalize_model_id(requested_model);
    if requested.is_empty() {
        return ModelResolution::unsupported(requested);
    }
    let model_mapping = model_mapping.clone().normalized();

    let mut available: std::collections::HashSet<String> = upstream_models
        .iter()
        .map(|model| normalize_model_id(model))
        .filter(|model| !model.is_empty())
        .collect();
    if available.is_empty() {
        available.extend(seed_model_capabilities().into_iter().map(|item| item.model));
    }

    if available.contains(&requested) {
        return ModelResolution::exact(requested);
    }

    if mode == ModelResolutionMode::ExactOnly {
        return ModelResolution::unsupported(requested);
    }

    if !model_mapping.enabled {
        return ModelResolution::pass_through(requested);
    }

    if !model_mapping.auto_generate_rules && model_mapping.rules.is_empty() {
        return ModelResolution::pass_through(requested);
    }

    let one_m_base = strip_model_1m_suffix(&requested);
    if one_m_base != requested && available.contains(&one_m_base) {
        return ModelResolution::resolved(requested, one_m_base, ModelResolutionSource::Alias);
    }

    let (base, requested_thinking) = strip_model_compat_suffixes(&requested);
    if requested_thinking && available.contains(&base) {
        return ModelResolution::resolved(requested, base, ModelResolutionSource::Alias);
    }
    if base != requested && available.contains(&base) {
        return ModelResolution::resolved(requested, base, ModelResolutionSource::Alias);
    }

    if let Some(target) = pick_configured_mapping_rule(
        &model_mapping,
        &requested,
        &base,
        &[ModelMappingRuleKind::VersionEquivalent],
    ) {
        return ModelResolution::resolved(requested, target, ModelResolutionSource::Alias);
    }

    if let Some(candidate) = model_mapping
        .auto_generate_rules
        .then(|| pick_version_equivalent_available(&available, &base, requested_thinking))
        .flatten()
    {
        return ModelResolution::resolved(requested, candidate, ModelResolutionSource::Alias);
    }

    if let Some(target) = pick_configured_mapping_rule(
        &model_mapping,
        &requested,
        &base,
        &[ModelMappingRuleKind::Alias],
    ) {
        return ModelResolution::resolved(requested, target, ModelResolutionSource::Alias);
    }

    if let Some(candidate) = explicit_model_alias_families(&base)
        .and_then(|families| pick_family_available(&available, &families))
        .filter(|_| model_mapping.auto_generate_rules)
    {
        return ModelResolution::resolved(requested, candidate, ModelResolutionSource::Alias);
    }

    if let Some(alias) = explicit_model_alias_candidates(&base)
        .and_then(|candidates| pick_available(&available, &candidates))
        .filter(|_| model_mapping.auto_generate_rules)
    {
        return ModelResolution::resolved(requested, alias, ModelResolutionSource::Alias);
    }

    if !mode.allows_family_fallback() {
        return ModelResolution::pass_through(requested);
    }

    if let Some(target) = pick_configured_mapping_rule(
        &model_mapping,
        &requested,
        &base,
        &[ModelMappingRuleKind::Fallback],
    ) {
        return ModelResolution::resolved(
            requested,
            target,
            ModelResolutionSource::FamilyNormalized,
        );
    }

    if is_explicit_claude_minor_version(&base) {
        if let Some(candidate) = compatible_explicit_claude_model_candidates(&base)
            .and_then(|candidates| pick_available(&available, &candidates))
            .filter(|_| model_mapping.auto_generate_rules)
        {
            return ModelResolution::resolved(
                requested,
                candidate,
                ModelResolutionSource::FamilyNormalized,
            );
        }
        return ModelResolution::pass_through(requested);
    }

    if !model_mapping.auto_generate_rules {
        return ModelResolution::pass_through(requested);
    }

    if let Some(candidate) = family_model_candidates(&base)
        .and_then(|candidates| pick_available(&available, &candidates))
    {
        return ModelResolution::resolved(
            requested,
            candidate,
            ModelResolutionSource::FamilyNormalized,
        );
    }

    if let Some(candidate) =
        model_family(&base).and_then(|family| pick_family_available(&available, &[family]))
    {
        return ModelResolution::resolved(
            requested,
            candidate,
            ModelResolutionSource::FamilyNormalized,
        );
    }

    ModelResolution::pass_through(requested)
}

fn explicit_model_alias_candidates(model: &str) -> Option<Vec<&'static str>> {
    match model {
        "auto" => Some(vec![
            "auto",
            "best",
            "default",
            "claude-opus-4.7",
            "claude-opus-4-7",
            "claude-opus-4.6",
            "claude-opus-4-6",
            "claude-sonnet-4.6",
            "claude-sonnet-4-6",
        ]),
        "opus" | "opusplan" => Some(vec![
            "opus",
            "claude-opus-4.7",
            "claude-opus-4-7",
            "claude-opus-4.6",
            "claude-opus-4-6",
            "claude-opus-4.5",
            "claude-opus-4-5-20251101",
        ]),
        "best" => Some(vec![
            "best",
            "auto",
            "opus",
            "claude-opus-4.7",
            "claude-opus-4-7",
            "claude-opus-4.6",
            "claude-opus-4-6",
            "claude-opus-4.5",
            "claude-opus-4-5-20251101",
            "claude-sonnet-4.6",
            "claude-sonnet-4-6",
            "claude-sonnet-4.5",
        ]),
        "default" => Some(vec![
            "default",
            "auto",
            "opus",
            "claude-opus-4.7",
            "claude-opus-4-7",
            "claude-opus-4.6",
            "claude-opus-4-6",
            "claude-opus-4.5",
            "claude-opus-4-5-20251101",
            "claude-sonnet-4.6",
            "claude-sonnet-4-6",
            "claude-sonnet-4.5",
        ]),
        "sonnet" => Some(vec![
            "sonnet",
            "claude-sonnet-4.6",
            "claude-sonnet-4-6",
            "claude-sonnet-4.5",
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4",
        ]),
        "haiku" => Some(vec![
            "haiku",
            "claude-haiku-4.5",
            "claude-haiku-4-5-20251001",
            "claude-haiku-4-5-20251001-thinking",
        ]),
        "claude-sonnet-4-20250514" => Some(vec![
            "claude-sonnet-4",
            "claude-sonnet-4.5",
            "claude-sonnet-4-5-20250929",
        ]),
        "claude-opus-4-20250514" => Some(vec![
            "claude-opus-4",
            "claude-opus-4.5",
            "claude-opus-4-5-20251101",
        ]),
        "claude-opus-4-1-20250805" => Some(vec![
            "claude-opus-4.5",
            "claude-opus-4-5-20251101",
            "claude-opus-4.6",
        ]),
        "claude-3-7-sonnet-20250219" => Some(vec![
            "claude-sonnet-4.5",
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4.6",
        ]),
        "claude-3-5-sonnet-20240620" | "claude-3-5-sonnet-20241022" => Some(vec![
            "claude-sonnet-4.5",
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4",
        ]),
        "claude-3-5-haiku-20241022" => Some(vec![
            "claude-haiku-4.5",
            "claude-haiku-4-5-20251001",
            "haiku",
        ]),
        _ => None,
    }
}

fn compatible_explicit_claude_model_candidates(model: &str) -> Option<Vec<&'static str>> {
    match model {
        "claude-sonnet-4-6" | "claude-sonnet-4.6" => Some(vec![
            "claude-sonnet-4.6",
            "claude-sonnet-4-6",
            "claude-sonnet-4.5",
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4",
        ]),
        "claude-sonnet-4-6-thinking" | "claude-sonnet-4.6-thinking" => Some(vec![
            "claude-sonnet-4-6-thinking",
            "claude-sonnet-4.6-thinking",
            "claude-sonnet-4-5-20250929-thinking",
            "claude-sonnet-4.5",
            "claude-sonnet-4-5-20250929",
        ]),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClaudeMinorVersion {
    family: &'static str,
    major: u32,
    minor: u32,
    uses_dot_minor: bool,
    has_date_suffix: bool,
}

fn is_explicit_claude_minor_version(model: &str) -> bool {
    parse_claude_minor_version(model).is_some()
}

fn pick_configured_mapping_rule(
    model_mapping: &ModelMappingConfig,
    requested: &str,
    base: &str,
    kinds: &[ModelMappingRuleKind],
) -> Option<String> {
    model_mapping
        .rules
        .iter()
        .find(|rule| {
            rule.enabled
                && kinds.contains(&rule.kind)
                && (rule.source == requested || rule.source == base)
        })
        .map(|rule| rule.target.clone())
}

fn pick_version_equivalent_available(
    available: &std::collections::HashSet<String>,
    requested_base: &str,
    requested_thinking: bool,
) -> Option<String> {
    let requested_version = parse_claude_minor_version(requested_base)?;
    let mut candidates = available
        .iter()
        .filter_map(|candidate| {
            let candidate_version = parse_claude_minor_version(candidate)?;
            (candidate_version.family == requested_version.family
                && candidate_version.major == requested_version.major
                && candidate_version.minor == requested_version.minor)
                .then(|| (candidate.clone(), candidate_version))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|(a, a_version), (b, b_version)| {
        version_equivalent_score(a, *a_version, requested_thinking)
            .cmp(&version_equivalent_score(b, *b_version, requested_thinking))
            .then_with(|| a.cmp(b))
    });
    candidates.pop().map(|(candidate, _)| candidate)
}

fn version_equivalent_score(
    model: &str,
    version: ClaudeMinorVersion,
    requested_thinking: bool,
) -> (bool, bool, bool, bool) {
    let candidate_thinking = model.ends_with("-thinking");
    (
        candidate_thinking == requested_thinking,
        !version.has_date_suffix,
        version.uses_dot_minor,
        !candidate_thinking,
    )
}

fn parse_claude_minor_version(model: &str) -> Option<ClaudeMinorVersion> {
    let (base, _) = strip_model_compat_suffixes(model);
    for family in ["opus", "sonnet", "haiku"] {
        let prefix = format!("claude-{}-", family);
        if let Some(rest) = base.strip_prefix(&prefix) {
            return parse_claude_minor_version_rest(family, rest);
        }
    }
    None
}

fn parse_claude_minor_version_rest(family: &'static str, rest: &str) -> Option<ClaudeMinorVersion> {
    let (major, rest) = take_leading_u32(rest)?;
    if let Some(rest) = rest.strip_prefix('.') {
        let (minor, rest) = take_leading_u32(rest)?;
        return Some(ClaudeMinorVersion {
            family,
            major,
            minor,
            uses_dot_minor: true,
            has_date_suffix: has_date_suffix(rest),
        });
    }

    let rest = rest.strip_prefix('-')?;
    let minor_digits = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if minor_digits.is_empty() || minor_digits.len() > 3 {
        return None;
    }
    let minor = minor_digits.parse::<u32>().ok()?;
    let rest = &rest[minor_digits.len()..];
    Some(ClaudeMinorVersion {
        family,
        major,
        minor,
        uses_dot_minor: false,
        has_date_suffix: has_date_suffix(rest),
    })
}

fn take_leading_u32(value: &str) -> Option<(u32, &str)> {
    let digits = value
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    let number = digits.parse::<u32>().ok()?;
    Some((number, &value[digits.len()..]))
}

fn has_date_suffix(rest: &str) -> bool {
    let Some(rest) = rest.strip_prefix('-') else {
        return false;
    };
    let digits = rest.chars().take_while(|ch| ch.is_ascii_digit()).count();
    digits >= 6
}

fn explicit_model_alias_families(model: &str) -> Option<Vec<&'static str>> {
    match model {
        "auto" | "best" | "default" => Some(vec!["opus", "sonnet", "haiku"]),
        "opus" | "opusplan" => Some(vec!["opus"]),
        "sonnet" => Some(vec!["sonnet"]),
        "haiku" => Some(vec!["haiku"]),
        _ => None,
    }
}

fn model_family(model: &str) -> Option<&'static str> {
    if model.contains("sonnet") {
        Some("sonnet")
    } else if model.contains("opus") {
        Some("opus")
    } else if model.contains("haiku") {
        Some("haiku")
    } else {
        None
    }
}

fn family_model_candidates(model: &str) -> Option<Vec<&'static str>> {
    if model.contains("sonnet") {
        if model.contains("4-6") || model.contains("4.6") {
            Some(vec![
                "claude-sonnet-4.6",
                "claude-sonnet-4-6",
                "claude-sonnet-4-6-thinking",
            ])
        } else if model.contains("4-5") || model.contains("4.5") {
            Some(vec![
                "claude-sonnet-4-5-20250929",
                "claude-sonnet-4.5",
                "claude-sonnet-4-5-20250929-thinking",
            ])
        } else if model.contains("4-20250514") || model.contains("sonnet-4") {
            Some(vec![
                "claude-sonnet-4",
                "claude-sonnet-4.5",
                "claude-sonnet-4-5-20250929",
            ])
        } else if model.contains("3-7") || model.contains("3.7") {
            Some(vec![
                "claude-sonnet-4.5",
                "claude-sonnet-4-5-20250929",
                "claude-sonnet-4.6",
            ])
        } else if model.contains("3-5") || model.contains("3.5") {
            Some(vec![
                "claude-sonnet-4.5",
                "claude-sonnet-4-5-20250929",
                "claude-sonnet-4",
            ])
        } else {
            Some(vec![
                "sonnet",
                "claude-sonnet-4.6",
                "claude-sonnet-4-6",
                "claude-sonnet-4.5",
            ])
        }
    } else if model.contains("opus") {
        if model.contains("4-7") || model.contains("4.7") {
            Some(vec![
                "claude-opus-4.7",
                "claude-opus-4-7",
                "claude-opus-4-7-thinking",
            ])
        } else if model.contains("4-6") || model.contains("4.6") {
            Some(vec![
                "claude-opus-4.6",
                "claude-opus-4-6",
                "claude-opus-4-6-thinking",
            ])
        } else if model.contains("4-5") || model.contains("4.5") {
            Some(vec![
                "claude-opus-4-5-20251101",
                "claude-opus-4.5",
                "claude-opus-4-5-20251101-thinking",
            ])
        } else if model.contains("4-1") || model.contains("4-20250514") || model.contains("opus-4")
        {
            Some(vec![
                "claude-opus-4.5",
                "claude-opus-4-5-20251101",
                "claude-opus-4.6",
            ])
        } else {
            Some(vec![
                "opus",
                "claude-opus-4.7",
                "claude-opus-4-7",
                "claude-opus-4.6",
            ])
        }
    } else if model.contains("haiku") {
        Some(vec![
            "claude-haiku-4.5",
            "claude-haiku-4-5-20251001",
            "claude-haiku-4-5-20251001-thinking",
            "haiku",
        ])
    } else {
        None
    }
}

fn pick_family_available(
    available: &std::collections::HashSet<String>,
    families: &[&'static str],
) -> Option<String> {
    for family in families {
        let mut candidates = available
            .iter()
            .filter(|model| model_matches_family(model, family))
            .cloned()
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            continue;
        }
        candidates.sort_by(|a, b| {
            family_candidate_key(a)
                .cmp(&family_candidate_key(b))
                .then_with(|| a.cmp(b))
        });
        if let Some(candidate) = candidates.pop() {
            return Some(candidate);
        }
    }
    None
}

fn model_matches_family(model: &str, family: &str) -> bool {
    model == family || model.starts_with(&format!("claude-{}", family))
}

fn contains_claude_model_id<'a>(models: impl IntoIterator<Item = &'a str>) -> bool {
    models.into_iter().any(is_claude_model_id)
}

fn is_claude_model_id(model: &str) -> bool {
    ["claude-opus-", "claude-sonnet-", "claude-haiku-"]
        .into_iter()
        .any(|prefix| model.starts_with(prefix))
}

fn family_candidate_key(model: &str) -> (bool, bool, Vec<u32>, bool) {
    let versions = model_version_numbers(model);
    (
        !model.ends_with("-thinking"),
        !versions.is_empty(),
        versions,
        model.contains('.'),
    )
}

fn model_version_numbers(model: &str) -> Vec<u32> {
    let mut numbers = Vec::new();
    let mut current = String::new();
    for ch in model.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(value) = current.parse::<u32>() {
                numbers.push(value);
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        if let Ok(value) = current.parse::<u32>() {
            numbers.push(value);
        }
    }
    numbers
}

fn pick_available(
    available: &std::collections::HashSet<String>,
    candidates: &[&'static str],
) -> Option<String> {
    candidates
        .iter()
        .map(|candidate| normalize_model_id(candidate))
        .find(|candidate| available.contains(candidate))
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
            source: Some(SEED_SOURCE.to_string()),
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
    fn sync_from_kiro_models_uses_upstream_models_without_static_merge() {
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
        assert!(!status.models.iter().any(|model| model.model == "opus"));
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

    #[test]
    fn catalog_context_window_for_sonnet_follows_real_upstream_model() {
        let catalog = ModelCapabilitiesCatalog::new();
        catalog.sync_from_kiro_models(vec![
            KiroAvailableModel {
                model_id: "claude-sonnet-4.5".to_string(),
                model_name: Some("Claude Sonnet 4.5".to_string()),
                token_limits: Some(KiroModelTokenLimits {
                    max_input_tokens: Some(200_000),
                    max_output_tokens: Some(64_000),
                }),
                ..Default::default()
            },
            KiroAvailableModel {
                model_id: "claude-sonnet-4.6".to_string(),
                model_name: Some("Claude Sonnet 4.6".to_string()),
                token_limits: Some(KiroModelTokenLimits {
                    max_input_tokens: Some(1_000_000),
                    max_output_tokens: Some(64_000),
                }),
                ..Default::default()
            },
        ]);

        let exact = catalog.resolve_model("claude-sonnet-4.6");
        assert_eq!(exact.upstream_model.as_deref(), Some("claude-sonnet-4.6"));
        assert_eq!(
            exact
                .upstream_model
                .as_deref()
                .and_then(|model| catalog.max_input_tokens_for(model)),
            Some(1_000_000)
        );

        let free_catalog = ModelCapabilitiesCatalog::new();
        free_catalog.sync_from_kiro_models(vec![KiroAvailableModel {
            model_id: "claude-sonnet-4.5".to_string(),
            model_name: Some("Claude Sonnet 4.5".to_string()),
            token_limits: Some(KiroModelTokenLimits {
                max_input_tokens: Some(200_000),
                max_output_tokens: Some(64_000),
            }),
            ..Default::default()
        }]);
        let normalized = free_catalog.resolve_model("claude-sonnet-4-6");
        assert_eq!(
            normalized.upstream_model.as_deref(),
            Some("claude-sonnet-4.5")
        );
        assert_eq!(
            normalized
                .upstream_model
                .as_deref()
                .and_then(|model| free_catalog.max_input_tokens_for(model)),
            Some(200_000)
        );
    }

    #[test]
    fn sync_from_kiro_models_preserves_claude_alias_targets_when_sync_omits_all_claude_models() {
        let catalog = ModelCapabilitiesCatalog::new();
        let status = catalog.sync_from_kiro_models(vec![KiroAvailableModel {
            model_id: "deepseek-3.2".to_string(),
            model_name: Some("Deepseek v3.2".to_string()),
            token_limits: Some(KiroModelTokenLimits {
                max_input_tokens: Some(164_000),
                max_output_tokens: Some(64_000),
            }),
            ..Default::default()
        }]);

        assert_eq!(status.source, KIRO_SOURCE);
        assert!(
            status
                .models
                .iter()
                .any(|model| model.model == "deepseek-3.2")
        );
        assert!(
            status
                .models
                .iter()
                .any(|model| model.model.contains("sonnet"))
        );
        let sonnet = catalog.resolve_model("sonnet");
        assert_eq!(sonnet.source, ModelResolutionSource::Alias);
        assert!(
            sonnet
                .upstream_model
                .as_deref()
                .is_some_and(|model| model.starts_with("claude-sonnet-"))
        );
    }

    #[test]
    fn manual_models_survive_sync_and_same_upstream_model_takes_over() {
        let catalog = ModelCapabilitiesCatalog::new();
        catalog.upsert_manual_model(ModelCapabilityItem {
            model: "claude-opus-5-20270101".to_string(),
            display_name: "Claude Opus 5".to_string(),
            description: None,
            max_input_tokens: Some(1_000_000),
            max_output_tokens: Some(128_000),
            supports_prompt_caching: Some(true),
            supported_input_types: vec!["TEXT".to_string()],
            source: Some(MANUAL_SOURCE.to_string()),
        });

        let status = catalog.sync_from_kiro_models(vec![KiroAvailableModel {
            model_id: "claude-sonnet-4-9-20270101".to_string(),
            model_name: Some("Claude Sonnet 4.9".to_string()),
            token_limits: Some(KiroModelTokenLimits {
                max_input_tokens: Some(1_000_000),
                max_output_tokens: Some(128_000),
            }),
            ..Default::default()
        }]);
        let manual = status
            .models
            .iter()
            .find(|item| item.model == "claude-opus-5-20270101")
            .unwrap();
        assert_eq!(manual.source.as_deref(), Some(MANUAL_SOURCE));
        assert_eq!(
            catalog.resolve_model("claude-opus-5-20270101").source,
            ModelResolutionSource::Manual
        );

        let status = catalog.sync_from_kiro_models(vec![KiroAvailableModel {
            model_id: "claude-opus-5-20270101".to_string(),
            model_name: Some("Claude Opus 5 Upstream".to_string()),
            token_limits: Some(KiroModelTokenLimits {
                max_input_tokens: Some(200_000),
                max_output_tokens: Some(64_000),
            }),
            ..Default::default()
        }]);
        let upstream = status
            .models
            .iter()
            .find(|item| item.model == "claude-opus-5-20270101")
            .unwrap();
        assert_eq!(upstream.source.as_deref(), Some(KIRO_SOURCE));
        assert_eq!(
            catalog.resolve_model("claude-opus-5-20270101").source,
            ModelResolutionSource::ExactUpstream
        );
    }

    #[test]
    fn resolver_maps_legacy_dated_models_to_seeded_kiro_models() {
        let models = seed_model_capabilities()
            .into_iter()
            .map(|item| item.model)
            .collect::<Vec<_>>();

        let sonnet = resolve_model_with_catalog("claude-sonnet-4-20250514", &models);
        assert_eq!(sonnet.upstream_model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(sonnet.source, ModelResolutionSource::Alias);

        let old_sonnet = resolve_model_with_catalog("claude-3-5-sonnet-20241022", &models);
        assert_eq!(
            old_sonnet.upstream_model.as_deref(),
            Some("claude-sonnet-4.5")
        );

        let opus = resolve_model_with_catalog("claude-opus-4-1-20250805", &models);
        assert_eq!(opus.upstream_model.as_deref(), Some("claude-opus-4.5"));
    }

    #[test]
    fn resolver_passes_unknown_family_through_without_mapping_match() {
        let models = seed_model_capabilities()
            .into_iter()
            .map(|item| item.model)
            .collect::<Vec<_>>();

        let result = resolve_model_with_catalog("gpt-4o", &models);
        assert_eq!(result.source, ModelResolutionSource::PassThrough);
        assert_eq!(result.upstream_model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn resolver_preserves_supported_thinking_model_when_only_compat_suffix_is_added() {
        let models = seed_model_capabilities()
            .into_iter()
            .map(|item| item.model)
            .collect::<Vec<_>>();

        let result = resolve_model_with_catalog("claude-opus-4-7-thinking[1m]", &models);
        assert_eq!(
            result.upstream_model.as_deref(),
            Some("claude-opus-4-7-thinking")
        );

        let alternate = resolve_model_with_catalog("claude-opus-4-7[1m]-thinking", &models);
        assert_eq!(
            alternate.upstream_model.as_deref(),
            Some("claude-opus-4-7-thinking")
        );

        let sonnet = resolve_model_with_catalog("claude-sonnet-4-6-thinking", &models);
        assert_eq!(
            sonnet.upstream_model.as_deref(),
            Some("claude-sonnet-4-6-thinking")
        );
    }

    #[test]
    fn resolver_falls_back_to_base_model_when_thinking_variant_is_not_available() {
        let models = vec!["claude-sonnet-4.6".to_string()];

        let result = resolve_model_with_catalog("claude-sonnet-4-6-thinking", &models);

        assert_eq!(result.source, ModelResolutionSource::Alias);
        assert_eq!(result.requested_model, "claude-sonnet-4-6-thinking");
        assert_eq!(result.upstream_model.as_deref(), Some("claude-sonnet-4.6"));
    }

    #[test]
    fn resolver_matches_synced_dot_minor_model_from_dash_request() {
        let models = vec!["claude-opus-4.5".to_string(), "claude-opus-4.8".to_string()];

        let result = resolve_model_with_catalog("claude-opus-4-8", &models);

        assert_eq!(result.source, ModelResolutionSource::Alias);
        assert_eq!(result.upstream_model.as_deref(), Some("claude-opus-4.8"));
    }

    #[test]
    fn resolver_matches_future_synced_minor_versions_without_hardcoded_branches() {
        let models = vec![
            "claude-opus-4.5".to_string(),
            "claude-opus-4.9".to_string(),
            "claude-sonnet-4.10".to_string(),
        ];

        let opus = resolve_model_with_catalog("claude-opus-4-9", &models);
        assert_eq!(opus.source, ModelResolutionSource::Alias);
        assert_eq!(opus.upstream_model.as_deref(), Some("claude-opus-4.9"));

        let sonnet = resolve_model_with_catalog("claude-sonnet-4-10", &models);
        assert_eq!(sonnet.source, ModelResolutionSource::Alias);
        assert_eq!(sonnet.upstream_model.as_deref(), Some("claude-sonnet-4.10"));
    }

    #[test]
    fn resolver_aliases_pick_highest_synced_family_model_before_static_candidates() {
        let models = vec![
            "claude-opus-4.7".to_string(),
            "claude-opus-4.8".to_string(),
            "claude-sonnet-4.6".to_string(),
        ];

        let opus = resolve_model_with_catalog("opus", &models);
        assert_eq!(opus.source, ModelResolutionSource::Alias);
        assert_eq!(opus.upstream_model.as_deref(), Some("claude-opus-4.8"));

        let default = resolve_model_with_catalog("default", &models);
        assert_eq!(default.source, ModelResolutionSource::Alias);
        assert_eq!(default.upstream_model.as_deref(), Some("claude-opus-4.8"));
    }

    #[test]
    fn resolver_preserves_thinking_preference_for_version_equivalent_models() {
        let models = vec![
            "claude-opus-4.8".to_string(),
            "claude-opus-4.8-thinking".to_string(),
        ];

        let result = resolve_model_with_catalog("claude-opus-4-8-thinking[1m]", &models);

        assert_eq!(result.source, ModelResolutionSource::Alias);
        assert_eq!(
            result.upstream_model.as_deref(),
            Some("claude-opus-4.8-thinking")
        );
    }

    #[test]
    fn resolver_does_not_downgrade_explicit_minor_versions_to_older_family_models() {
        let models = vec!["claude-opus-4.5".to_string(), "claude-opus-4.6".to_string()];

        let result = resolve_model_with_catalog("claude-opus-4-8", &models);

        assert_eq!(result.source, ModelResolutionSource::PassThrough);
        assert_eq!(result.upstream_model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn resolver_maps_claude_code_sonnet_46_to_available_sonnet_for_free_pool() {
        let models = vec![
            "auto".to_string(),
            "claude-haiku-4.5".to_string(),
            "claude-sonnet-4".to_string(),
            "claude-sonnet-4.5".to_string(),
            "deepseek-3.2".to_string(),
            "glm-5".to_string(),
        ];

        let dashed = resolve_model_with_catalog("claude-sonnet-4-6", &models);
        assert_eq!(dashed.source, ModelResolutionSource::FamilyNormalized);
        assert_eq!(dashed.upstream_model.as_deref(), Some("claude-sonnet-4.5"));

        let dotted = resolve_model_with_catalog("claude-sonnet-4.6", &models);
        assert_eq!(dotted.source, ModelResolutionSource::FamilyNormalized);
        assert_eq!(dotted.upstream_model.as_deref(), Some("claude-sonnet-4.5"));

        let thinking = resolve_model_with_catalog("claude-sonnet-4-6-thinking", &models);
        assert_eq!(thinking.source, ModelResolutionSource::FamilyNormalized);
        assert_eq!(thinking.requested_model, "claude-sonnet-4-6-thinking");
        assert_eq!(
            thinking.upstream_model.as_deref(),
            Some("claude-sonnet-4.5")
        );
    }

    #[test]
    fn exact_only_still_requires_literal_upstream_model_ids() {
        let models = vec!["claude-opus-4.8".to_string()];

        let result = resolve_model_with_catalog_and_mode(
            "claude-opus-4-8",
            &models,
            ModelResolutionMode::ExactOnly,
        );

        assert_eq!(result.source, ModelResolutionSource::Unsupported);
        assert!(result.upstream_model.is_none());
    }

    #[test]
    fn disabled_mapping_passes_unmatched_models_through_without_auto_rules() {
        let models = vec!["claude-opus-4.8".to_string()];
        let mapping = ModelMappingConfig {
            enabled: false,
            auto_generate_rules: false,
            rules: vec![],
        };

        let result = resolve_model_with_catalog_mapping_and_mode(
            "claude-opus-4-8",
            &models,
            ModelResolutionMode::Compatible,
            &mapping,
        );

        assert_eq!(result.source, ModelResolutionSource::PassThrough);
        assert_eq!(result.upstream_model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn enabled_mapping_without_rules_or_auto_generation_only_keeps_exact_matches() {
        let models = vec!["claude-opus-4.8".to_string()];
        let mapping = ModelMappingConfig {
            enabled: true,
            auto_generate_rules: false,
            rules: vec![],
        };

        let exact = resolve_model_with_catalog_mapping_and_mode(
            "claude-opus-4.8",
            &models,
            ModelResolutionMode::Compatible,
            &mapping,
        );
        assert_eq!(exact.source, ModelResolutionSource::ExactUpstream);
        assert_eq!(exact.upstream_model.as_deref(), Some("claude-opus-4.8"));

        let dashed = resolve_model_with_catalog_mapping_and_mode(
            "claude-opus-4-8",
            &models,
            ModelResolutionMode::Compatible,
            &mapping,
        );
        assert_eq!(dashed.source, ModelResolutionSource::PassThrough);
        assert_eq!(dashed.upstream_model.as_deref(), Some("claude-opus-4-8"));

        let suffixed = resolve_model_with_catalog_mapping_and_mode(
            "claude-opus-4.8[1m]",
            &models,
            ModelResolutionMode::Compatible,
            &mapping,
        );
        assert_eq!(suffixed.source, ModelResolutionSource::PassThrough);
        assert_eq!(
            suffixed.upstream_model.as_deref(),
            Some("claude-opus-4.8[1m]")
        );
    }

    #[test]
    fn configured_mapping_rule_overrides_auto_fallback_rules() {
        let models = vec!["claude-opus-4.8".to_string()];
        let mapping = ModelMappingConfig {
            enabled: true,
            auto_generate_rules: false,
            rules: vec![crate::model::config::ModelMappingRule {
                enabled: true,
                source: "opus".to_string(),
                target: "claude-opus-4.8".to_string(),
                kind: ModelMappingRuleKind::Fallback,
                note: None,
            }],
        };

        let result = resolve_model_with_catalog_mapping_and_mode(
            "opus",
            &models,
            ModelResolutionMode::Compatible,
            &mapping,
        );

        assert_eq!(result.source, ModelResolutionSource::FamilyNormalized);
        assert_eq!(result.upstream_model.as_deref(), Some("claude-opus-4.8"));
    }

    #[test]
    fn resolver_can_follow_future_family_models_after_upstream_sync() {
        let models = vec![
            "claude-sonnet-4-9-20270101".to_string(),
            "claude-opus-5-20270101".to_string(),
        ];

        let sonnet = resolve_model_with_catalog("sonnet", &models);
        assert_eq!(sonnet.source, ModelResolutionSource::Alias);
        assert_eq!(
            sonnet.upstream_model.as_deref(),
            Some("claude-sonnet-4-9-20270101")
        );

        let default = resolve_model_with_catalog("default", &models);
        assert_eq!(default.source, ModelResolutionSource::Alias);
        assert_eq!(
            default.upstream_model.as_deref(),
            Some("claude-opus-5-20270101")
        );
    }

    #[test]
    fn resolver_keeps_common_aliases_usable_when_upstream_aliases_are_absent() {
        let models = vec![
            "claude-opus-4.7".to_string(),
            "claude-sonnet-4.6".to_string(),
            "claude-haiku-4.5".to_string(),
        ];

        let best = resolve_model_with_catalog("best", &models);
        assert_eq!(best.source, ModelResolutionSource::Alias);
        assert_eq!(best.upstream_model.as_deref(), Some("claude-opus-4.7"));

        let default = resolve_model_with_catalog("default", &models);
        assert_eq!(default.source, ModelResolutionSource::Alias);
        assert_eq!(default.upstream_model.as_deref(), Some("claude-opus-4.7"));

        let sonnet = resolve_model_with_catalog("sonnet", &models);
        assert_eq!(sonnet.source, ModelResolutionSource::Alias);
        assert_eq!(sonnet.upstream_model.as_deref(), Some("claude-sonnet-4.6"));

        let haiku = resolve_model_with_catalog("haiku", &models);
        assert_eq!(haiku.source, ModelResolutionSource::Alias);
        assert_eq!(haiku.upstream_model.as_deref(), Some("claude-haiku-4.5"));
    }

    #[test]
    fn resolver_maps_sonnet_alias_to_synced_dot_45_when_alias_absent() {
        let models = vec![
            "auto".to_string(),
            "claude-haiku-4.5".to_string(),
            "claude-sonnet-4".to_string(),
            "claude-sonnet-4.5".to_string(),
            "deepseek-3.2".to_string(),
            "glm-5".to_string(),
            "minimax-m2.1".to_string(),
            "minimax-m2.5".to_string(),
            "qwen3-coder-next".to_string(),
        ];

        let sonnet = resolve_model_with_catalog("sonnet", &models);

        assert_eq!(sonnet.source, ModelResolutionSource::Alias);
        assert_eq!(sonnet.upstream_model.as_deref(), Some("claude-sonnet-4.5"));
    }

    #[test]
    fn model_resolution_modes_keep_compatible_default_behavior() {
        let models = vec!["claude-sonnet-4.6".to_string()];

        let compatible =
            resolve_model_with_catalog_and_mode("sonnet", &models, ModelResolutionMode::Compatible);
        assert_eq!(compatible.source, ModelResolutionSource::Alias);
        assert_eq!(
            compatible.upstream_model.as_deref(),
            Some("claude-sonnet-4.6")
        );

        let alias_only =
            resolve_model_with_catalog_and_mode("sonnet", &models, ModelResolutionMode::AliasOnly);
        assert_eq!(alias_only.source, ModelResolutionSource::Alias);
        assert_eq!(
            alias_only.upstream_model.as_deref(),
            Some("claude-sonnet-4.6")
        );

        let exact_only =
            resolve_model_with_catalog_and_mode("sonnet", &models, ModelResolutionMode::ExactOnly);
        assert_eq!(exact_only.source, ModelResolutionSource::Unsupported);
        assert!(exact_only.upstream_model.is_none());
    }

    #[test]
    fn alias_only_rejects_family_normalized_future_model_names() {
        let models = vec!["claude-sonnet-4.6".to_string()];

        let compatible = resolve_model_with_catalog_and_mode(
            "claude-sonnet-5-20270101",
            &models,
            ModelResolutionMode::Compatible,
        );
        assert_eq!(compatible.source, ModelResolutionSource::FamilyNormalized);
        assert_eq!(
            compatible.upstream_model.as_deref(),
            Some("claude-sonnet-4.6")
        );

        let alias_only = resolve_model_with_catalog_and_mode(
            "claude-sonnet-5-20270101",
            &models,
            ModelResolutionMode::AliasOnly,
        );
        assert_eq!(alias_only.source, ModelResolutionSource::PassThrough);
        assert_eq!(
            alias_only.upstream_model.as_deref(),
            Some("claude-sonnet-5-20270101")
        );
    }

    #[test]
    fn anthropic_models_do_not_advertise_unresolvable_static_models() {
        let catalog = ModelCapabilitiesCatalog::new();
        catalog.sync_from_kiro_models(vec![KiroAvailableModel {
            model_id: "claude-sonnet-4-9-20270101".to_string(),
            model_name: Some("Claude Sonnet 4.9".to_string()),
            token_limits: Some(KiroModelTokenLimits {
                max_input_tokens: Some(1_000_000),
                max_output_tokens: Some(128_000),
            }),
            ..Default::default()
        }]);

        let models = catalog.anthropic_models();
        assert!(models.iter().any(|model| model.id == "sonnet"));
        assert!(!models.iter().any(|model| model.id == "opus"));
        assert!(
            models
                .iter()
                .any(|model| model.id == "claude-sonnet-4-9-20270101")
        );
    }

    #[test]
    fn persisted_status_refreshes_only_legacy_built_in_catalog() {
        let legacy = ModelCapabilitiesStatus {
            available: true,
            source: "built-in".to_string(),
            model_count: 1,
            last_synced_at: None,
            last_error: None,
            models: vec![ModelCapabilityItem {
                model: "claude-sonnet-4-6".to_string(),
                display_name: "Claude Sonnet 4.6".to_string(),
                description: None,
                max_input_tokens: Some(200_000),
                max_output_tokens: Some(64_000),
                supports_prompt_caching: Some(true),
                supported_input_types: vec!["TEXT".to_string()],
                source: None,
            }],
        };
        assert!(legacy.should_refresh_from_seed());

        let synced_without_auto = ModelCapabilitiesStatus {
            source: KIRO_SOURCE.to_string(),
            ..legacy.clone()
        };
        assert!(!synced_without_auto.should_refresh_from_seed());
    }
}
