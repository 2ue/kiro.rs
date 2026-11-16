use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::anthropic::types::Model;
use crate::model::config::{ModelMappingConfig, ModelMappingRuleKind, ModelResolutionMode};

fn default_listed() -> bool {
    true
}

pub const SEED_SOURCE: &str = "anthropic-seed";
pub const MANUAL_SOURCE: &str = "manual";
const SEED_JSON: &str = include_str!("../../data/anthropic-models.seed.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilityItem {
    #[serde(default = "default_listed")]
    pub listed: bool,
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
            .collect::<BTreeMap<_, _>>();
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
        let max_input_by_model = status
            .models
            .iter()
            .filter_map(|item| {
                item.max_input_tokens
                    .filter(|tokens| *tokens > 0)
                    .map(|tokens| (item.model.clone(), tokens))
            })
            .collect::<HashMap<_, _>>();
        let upstream_ids = status
            .models
            .iter()
            .map(|item| item.model.clone())
            .collect::<Vec<_>>();
        let mut models = HashMap::new();
        for item in status.models {
            let created = model_created_at(&item.model);
            let max_tokens = item.max_output_tokens.unwrap_or(64_000).max(1);
            let max_input_tokens = item.max_input_tokens.filter(|tokens| *tokens > 0);
            models.insert(
                item.model.clone(),
                Model {
                    listed: item.listed,
                    id: item.model,
                    object: "model".to_string(),
                    created,
                    created_at: crate::anthropic::types::model_created_at_rfc3339(created),
                    owned_by: "anthropic".to_string(),
                    display_name: item.display_name,
                    model_type: "model".to_string(),
                    max_tokens,
                    max_input_tokens,
                    context_window: max_input_tokens,
                },
            );
        }
        for mut model in static_anthropic_models() {
            if models.contains_key(&model.id) {
                continue;
            }
            let resolution = resolve_model_with_catalog(&model.id, &upstream_ids);
            if resolution
                .upstream_model
                .as_ref()
                .and_then(|id| models.get(id))
                .is_some_and(|entry| !entry.listed)
            {
                continue;
            }
            if !matches!(
                resolution.source,
                ModelResolutionSource::Unsupported | ModelResolutionSource::PassThrough
            ) {
                if let Some(max_input_tokens) = resolution
                    .upstream_model
                    .as_deref()
                    .and_then(|model| max_input_by_model.get(model).copied())
                {
                    model.max_input_tokens = Some(max_input_tokens);
                    model.context_window = Some(max_input_tokens);
                }
                models.insert(model.id.clone(), model);
            }
        }
        let mut models: Vec<Model> = models.into_values().collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models
    }

    /// Claude Code protocol `/v1/models` listing: Claude models only under their Anthropic
    /// hyphenated ids (never 上游's dotted ids), without aliases or `-thinking` / `[1m]`
    /// variants. Those names stay valid as request models; they are just not listed.
    pub fn standard_anthropic_models(&self) -> Vec<Model> {
        standardize_model_listing(
            self.anthropic_models()
                .into_iter()
                .filter(|model| model.listed)
                .collect(),
        )
    }

    /// Looks up one listed model. Accepts the listed id as well as request-side spellings
    /// (dotted 上游 ids, aliases, `-thinking` / `[1m]` variants) and returns the listed entry
    /// they resolve to; unknown models return `None`.
    pub fn standard_anthropic_model(&self, requested_model: &str) -> Option<Model> {
        let models = self.standard_anthropic_models();
        let requested = normalize_model_id(requested_model);
        let (base, _) = strip_model_compat_suffixes(&requested);
        let direct = [requested.clone(), base.clone(), claude_listing_id(&base)];
        if let Some(model) = direct
            .iter()
            .find_map(|id| models.iter().find(|model| &model.id == id))
        {
            return Some(model.clone());
        }
        let upstream_ids = self
            .status()
            .models
            .into_iter()
            .map(|item| item.model)
            .collect::<Vec<_>>();
        let resolution = resolve_model_with_catalog(&base, &upstream_ids);
        if resolution.source == ModelResolutionSource::Unsupported {
            return None;
        }
        // A pass-through of an unknown name resolves to itself, which is not listed.
        let listing_id = standard_listing_id(resolution.upstream_model.as_deref()?)?;
        models.into_iter().find(|model| model.id == listing_id)
    }

    pub fn max_output_tokens_for(&self, model: &str) -> Option<i32> {
        let model = normalize_model_id(model);
        self.inner
            .read()
            .models
            .get(&model)
            .and_then(|item| item.max_output_tokens)
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
        // The built-in seed is protocol/listing knowledge only. It must never become a
        // scheduler target before a real account catalog has been synchronized; otherwise a
        // request could silently acquire a credential for a model that no account advertised.
        let models = inner
            .models
            .values()
            .filter(|item| item.source.as_deref() != Some(SEED_SOURCE))
            .map(|item| item.model.clone())
            .collect::<Vec<_>>();
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
                "加载内置 Anthropic 模型 seed 失败，使用静态兼容模型继续: {}",
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

    let available: std::collections::HashSet<String> = upstream_models
        .iter()
        .map(|model| normalize_model_id(model))
        .filter(|model| !model.is_empty())
        .collect();

    if !model_mapping.enabled {
        if available.contains(&requested) {
            return ModelResolution::exact(requested);
        }
        return if mode == ModelResolutionMode::ExactOnly {
            ModelResolution::unsupported(requested)
        } else {
            ModelResolution::pass_through(requested)
        };
    }

    let (base, requested_thinking) = strip_model_compat_suffixes(&requested);

    // Operator-authored mappings always win over generated compatibility rules,
    // regardless of their declared kind. This keeps an explicit target stable when
    // the upstream catalog later gains another equivalent spelling or version.
    if let Some((target, kind)) = pick_configured_mapping_rule(
        &model_mapping,
        &requested,
        &base,
        &[
            ModelMappingRuleKind::VersionEquivalent,
            ModelMappingRuleKind::Alias,
            ModelMappingRuleKind::Fallback,
        ],
    ) {
        return ModelResolution::resolved(
            requested,
            target,
            match kind {
                ModelMappingRuleKind::Fallback => ModelResolutionSource::FamilyNormalized,
                ModelMappingRuleKind::VersionEquivalent | ModelMappingRuleKind::Alias => {
                    ModelResolutionSource::Alias
                }
            },
        );
    }

    if available.contains(&requested) {
        return ModelResolution::exact(requested);
    }

    if mode == ModelResolutionMode::ExactOnly {
        return ModelResolution::unsupported(requested);
    }

    if !model_mapping.auto_generate_rules && model_mapping.rules.is_empty() {
        return ModelResolution::pass_through(requested);
    }

    if model_mapping.auto_generate_rules {
        let one_m_base = strip_model_1m_suffix(&requested);
        if one_m_base != requested && available.contains(&one_m_base) {
            return ModelResolution::resolved(requested, one_m_base, ModelResolutionSource::Alias);
        }

        if requested_thinking && available.contains(&base) {
            return ModelResolution::resolved(requested, base, ModelResolutionSource::Alias);
        }
        if base != requested && available.contains(&base) {
            return ModelResolution::resolved(requested, base, ModelResolutionSource::Alias);
        }

        // A version-equivalent spelling is safe even when cross-version fallback
        // is disabled: it preserves the requested semantic version.
        if let Some(candidate) =
            pick_version_equivalent_available(&available, &base, requested_thinking)
        {
            return ModelResolution::resolved(requested, candidate, ModelResolutionSource::Alias);
        }

        // Bare aliases prefer their configured/canonical model spelling. Only when that
        // version is absent should the resolver select the highest available model in the
        // requested family.
        if let Some(candidate) = explicit_model_alias_families(&base).and_then(|families| {
            pick_family_available(
                &available,
                &families,
                model_mapping.allow_cross_family_fallback,
            )
        }) {
            return ModelResolution::resolved(requested, candidate, ModelResolutionSource::Alias);
        }
    }

    if !mode.allows_family_fallback() {
        return ModelResolution::pass_through(requested);
    }

    if let Some((target, _)) = pick_configured_mapping_rule(
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

    if !model_mapping.auto_generate_rules {
        return ModelResolution::pass_through(requested);
    }

    if let Some(requested_version) = parse_claude_minor_version(&base) {
        // A syntactically valid Claude version is resolved only against the current account
        // catalog. The resolver deliberately has no built-in "known version" allowlist:
        // Anthropic can add or retire versions without requiring a code change, and the target
        // must always come from `available`, never from a hard-coded model name.
        return match model_mapping
            .allow_version_fallback
            .then(|| {
                pick_closest_family_version(
                    &available,
                    requested_version,
                    model_mapping.allow_cross_family_fallback,
                )
            })
            .flatten()
        {
            Some(candidate) => {
                let mut resolution = ModelResolution::resolved(
                    requested,
                    candidate,
                    ModelResolutionSource::FamilyNormalized,
                );
                resolution.note = resolution.note.map(|note| {
                    format!("{note} (known model unavailable; closest same-family model)")
                });
                resolution
            }
            None => ModelResolution::pass_through(requested),
        };
    }

    if let Some(candidate) = model_family(&base).and_then(|family| {
        pick_family_available(
            &available,
            &[family],
            model_mapping.allow_cross_family_fallback,
        )
    }) {
        return ModelResolution::resolved(
            requested,
            candidate,
            ModelResolutionSource::FamilyNormalized,
        );
    }

    ModelResolution::pass_through(requested)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClaudeMinorVersion {
    family: &'static str,
    major: u32,
    minor: Option<u32>,
    uses_dot_minor: bool,
    has_date_suffix: bool,
}

fn pick_configured_mapping_rule(
    model_mapping: &ModelMappingConfig,
    requested: &str,
    base: &str,
    kinds: &[ModelMappingRuleKind],
) -> Option<(String, ModelMappingRuleKind)> {
    model_mapping
        .rules
        .iter()
        .find(|rule| {
            rule.enabled
                && kinds.contains(&rule.kind)
                && (rule.source == requested || rule.source == base)
        })
        .map(|rule| (rule.target.clone(), rule.kind))
}

fn pick_version_equivalent_available(
    available: &std::collections::HashSet<String>,
    requested_base: &str,
    requested_thinking: bool,
) -> Option<String> {
    let requested_version = parse_claude_minor_version(requested_base)?;
    // A dated major id identifies a retired concrete model, not a spelling
    // variant of the bare major id. It must reach the generic closest-version
    // ranking so a current minor such as 4.5 can win over bare 4.
    if requested_version.has_date_suffix && requested_version.minor.is_none() {
        return None;
    }
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
    for family in ["opus", "sonnet", "haiku", "fable"] {
        let prefix = format!("claude-{}-", family);
        if let Some(rest) = base.strip_prefix(&prefix) {
            return parse_claude_minor_version_rest(family, rest);
        }
    }
    parse_legacy_claude_minor_version(&base)
}

fn parse_claude_minor_version_rest(family: &'static str, rest: &str) -> Option<ClaudeMinorVersion> {
    let (major, rest) = take_leading_u32(rest)?;
    if rest.is_empty() {
        return Some(ClaudeMinorVersion {
            family,
            major,
            minor: None,
            uses_dot_minor: false,
            has_date_suffix: false,
        });
    }
    if let Some(rest) = rest.strip_prefix('.') {
        let (minor, rest) = take_leading_u32(rest)?;
        return Some(ClaudeMinorVersion {
            family,
            major,
            minor: Some(minor),
            uses_dot_minor: true,
            has_date_suffix: has_date_suffix(rest),
        });
    }

    let rest = rest.strip_prefix('-')?;
    let digits = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    if digits.len() >= 6 {
        return (digits.len() == 8 && rest.len() == 8).then_some(ClaudeMinorVersion {
            family,
            major,
            minor: None,
            uses_dot_minor: false,
            has_date_suffix: true,
        });
    }
    if digits.len() > 3 {
        return None;
    }
    let minor = digits.parse::<u32>().ok()?;
    let rest = &rest[digits.len()..];
    Some(ClaudeMinorVersion {
        family,
        major,
        minor: Some(minor),
        uses_dot_minor: false,
        has_date_suffix: has_date_suffix(rest),
    })
}

fn parse_legacy_claude_minor_version(model: &str) -> Option<ClaudeMinorVersion> {
    let rest = model.strip_prefix("claude-")?;
    let parts = rest.split('-').collect::<Vec<_>>();
    if parts.len() < 3 {
        return None;
    }
    if let Some((major, minor)) = parts.first().and_then(|value| value.split_once('.')) {
        let major = major.parse::<u32>().ok()?;
        let minor = minor.parse::<u32>().ok()?;
        let family = match *parts.get(1)? {
            "opus" => "opus",
            "sonnet" => "sonnet",
            "haiku" => "haiku",
            "fable" => "fable",
            _ => return None,
        };
        let date = parts.get(2).copied();
        let has_date_suffix = date.is_some_and(is_yyyymmdd);
        if date.is_some() && !has_date_suffix {
            return None;
        }
        return Some(ClaudeMinorVersion {
            family,
            major,
            minor: Some(minor),
            uses_dot_minor: true,
            has_date_suffix,
        });
    }

    let major = parts.first()?.parse::<u32>().ok()?;
    let minor = parts.get(1)?.parse::<u32>().ok()?;
    let family = match *parts.get(2)? {
        "opus" => "opus",
        "sonnet" => "sonnet",
        "haiku" => "haiku",
        "fable" => "fable",
        _ => return None,
    };
    let date = parts.get(3).copied();
    let has_date_suffix = date.is_some_and(is_yyyymmdd);
    if date.is_some() && !has_date_suffix {
        return None;
    }
    Some(ClaudeMinorVersion {
        family,
        major,
        minor: Some(minor),
        uses_dot_minor: false,
        has_date_suffix,
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

fn is_yyyymmdd(value: &str) -> bool {
    value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// Closest same-family model in `available` by version distance; ties prefer the newer
/// version. Base models win over `-thinking` variants because thinking controls travel
/// separately.
fn pick_closest_family_version(
    available: &HashSet<String>,
    requested: ClaudeMinorVersion,
    allow_cross_family_fallback: bool,
) -> Option<String> {
    let mut candidates = available
        .iter()
        .filter_map(|candidate| {
            let version = parse_claude_minor_version(candidate)?;
            (version.family == requested.family).then_some((candidate, version))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() && allow_cross_family_fallback {
        candidates = available
            .iter()
            .filter_map(|candidate| {
                let version = parse_claude_minor_version(candidate)?;
                Some((candidate, version))
            })
            .collect();
    }
    candidates
        .into_iter()
        .max_by(|(a, a_version), (b, b_version)| {
            let rank = |version: &ClaudeMinorVersion| {
                i64::from(version.major) * 1_000 + i64::from(version.minor.unwrap_or(0))
            };
            let dated_major_minor = |version: &ClaudeMinorVersion| {
                version.family == requested.family
                    && version.major == requested.major
                    && requested.minor.is_none()
                    && requested.has_date_suffix
                    && version.minor.is_some()
            };
            let same_requested_minor = |version: &ClaudeMinorVersion| {
                requested
                    .minor
                    .zip(version.minor)
                    .is_some_and(|(requested, candidate)| requested == candidate)
            };
            let major_distance = |version: &ClaudeMinorVersion| {
                i64::from(version.major).abs_diff(i64::from(requested.major))
            };
            let minor_distance = |version: &ClaudeMinorVersion| {
                requested
                    .minor
                    .unwrap_or_default()
                    .abs_diff(version.minor.unwrap_or_default())
            };
            let key = |model: &str, version: &ClaudeMinorVersion| {
                (
                    version.family == requested.family,
                    dated_major_minor(version),
                    same_requested_minor(version),
                    // Prefer the closest major before comparing minor distance. This keeps a
                    // legacy 3.5 request on a current 4.5 model instead of a bare 4 model, while
                    // still selecting 4.6 for an unavailable 4.8 request.
                    std::cmp::Reverse(major_distance(version)),
                    std::cmp::Reverse(minor_distance(version)),
                    !model.ends_with("-thinking"),
                    rank(version),
                    version_equivalent_score(model, *version, false),
                )
            };
            key(a, a_version)
                .cmp(&key(b, b_version))
                .then_with(|| b.cmp(a))
        })
        .map(|(candidate, _)| candidate.clone())
}

fn explicit_model_alias_families(model: &str) -> Option<Vec<&'static str>> {
    match model {
        "auto" | "best" | "default" => Some(vec!["opus", "sonnet", "haiku"]),
        "opus" | "opusplan" => Some(vec!["opus"]),
        "sonnet" => Some(vec!["sonnet"]),
        "haiku" => Some(vec!["haiku"]),
        "fable" => Some(vec!["fable"]),
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
    } else if is_fable_request_model(model) {
        Some("fable")
    } else {
        None
    }
}

fn is_fable_request_model(model: &str) -> bool {
    model == "fable" || model.starts_with("claude-fable-")
}

fn pick_family_available(
    available: &std::collections::HashSet<String>,
    families: &[&'static str],
    allow_cross_family_fallback: bool,
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
    if allow_cross_family_fallback {
        let mut candidates = available.iter().cloned().collect::<Vec<_>>();
        candidates.sort_by(|a, b| {
            family_candidate_key(a)
                .cmp(&family_candidate_key(b))
                .then_with(|| a.cmp(b))
        });
        return candidates.pop();
    }
    None
}

fn model_matches_family(model: &str, family: &str) -> bool {
    model == family || model.starts_with(&format!("claude-{}", family))
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

fn static_model_capabilities() -> Vec<ModelCapabilityItem> {
    static_anthropic_models()
        .into_iter()
        .map(|model| ModelCapabilityItem {
            listed: true,
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

/// Request-side aliases that are never listed by `/v1/models`.
const MODEL_LISTING_ALIASES: &[&str] = &[
    "auto", "best", "default", "fable", "haiku", "opus", "opusplan", "sonnet",
];

/// 上游 spells Claude versions with dots (`claude-opus-4.6`); the Claude Code protocol
/// uses hyphens (`claude-opus-4-6`).
fn claude_listing_id(model: &str) -> String {
    if model.starts_with("claude-") {
        model.replace('.', "-")
    } else {
        model.to_string()
    }
}

/// Listing id for a catalog entry, or `None` for aliases and compatibility variants.
fn standard_listing_id(model: &str) -> Option<String> {
    let model = normalize_model_id(model);
    if MODEL_LISTING_ALIASES.contains(&model.as_str()) {
        return None;
    }
    // Claude Code 2.1.x validates this explicit thinking alias against `/v1/models`
    // before sending a request. Keep it discoverable while retaining the older
    // request-side alias behavior for the other shorthand names.
    if model == "sonnet-thinking" {
        return Some(model);
    }
    let (base, thinking) = strip_model_compat_suffixes(&model);
    if thinking || base != model {
        return None;
    }
    Some(claude_listing_id(&model))
}

fn standardize_model_listing(models: Vec<Model>) -> Vec<Model> {
    let mut listed: std::collections::BTreeMap<String, Model> = std::collections::BTreeMap::new();
    for mut model in models {
        let Some(id) = standard_listing_id(&model.id) else {
            continue;
        };
        match listed.get_mut(&id) {
            Some(existing) => {
                if existing.max_input_tokens.is_none() {
                    existing.max_input_tokens = model.max_input_tokens;
                    existing.context_window = model.context_window;
                }
            }
            None => {
                model.id = id.clone();
                listed.insert(id, model);
            }
        }
    }
    listed.into_values().collect()
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
        (
            "sonnet-thinking",
            "Claude Code Alias: Sonnet (Thinking)",
            1_771_286_400,
            64_000,
        ),
        ("haiku", "Claude Code Alias: Haiku", 1_760_486_400, 64_000),
        ("claude-opus-4.8", "Claude Opus 4.8", 1_779_926_400, 128_000),
        (
            "claude-opus-4.8-thinking",
            "Claude Opus 4.8 (Thinking)",
            1_779_926_400,
            128_000,
        ),
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
            listed: true,
            id: id.to_string(),
            object: "model".to_string(),
            created,
            created_at: crate::anthropic::types::model_created_at_rfc3339(created),
            owned_by: "anthropic".to_string(),
            display_name: display_name.to_string(),
            model_type: "model".to_string(),
            max_tokens,
            max_input_tokens: None,
            context_window: None,
        })
        .collect()
}

fn model_created_at(model: &str) -> i64 {
    static_anthropic_models()
        .into_iter()
        .find(|item| item.id == model)
        .map(|item| item.created)
        // Stable fallback so repeated /v1/models calls return identical metadata.
        .unwrap_or(UNKNOWN_MODEL_CREATED_AT)
}

/// 2025-01-01T00:00:00Z，用于上游目录中没有发布时间的模型。
const UNKNOWN_MODEL_CREATED_AT: i64 = 1_735_689_600;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_includes_opus_4_8_as_one_m_context_model() {
        let catalog = ModelCapabilitiesCatalog::new();
        let opus = catalog
            .status()
            .models
            .into_iter()
            .find(|model| model.model == "claude-opus-4.8")
            .expect("seed should include claude-opus-4.8");

        assert_eq!(opus.max_input_tokens, Some(1_000_000));
        assert_eq!(opus.max_output_tokens, Some(128_000));

        let exposed = catalog
            .anthropic_models()
            .into_iter()
            .find(|model| model.id == "claude-opus-4.8")
            .expect("models endpoint should expose claude-opus-4.8");
        assert_eq!(exposed.max_input_tokens, Some(1_000_000));
        assert_eq!(exposed.context_window, Some(1_000_000));
    }

    #[test]
    fn fresh_catalog_does_not_dispatch_against_seed_model_ids() {
        let catalog = ModelCapabilitiesCatalog::new();
        let resolution = catalog.resolve_model("sonnet");
        assert_eq!(resolution.source, ModelResolutionSource::PassThrough);
        assert_eq!(resolution.upstream_model.as_deref(), Some("sonnet"));

        let dated = catalog.resolve_model("claude-sonnet-4-20250514");
        assert_eq!(dated.source, ModelResolutionSource::PassThrough);
        assert_eq!(
            dated.upstream_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn resolver_maps_legacy_dated_models_to_seeded_pool_models() {
        let models = seed_model_capabilities()
            .into_iter()
            .map(|item| item.model)
            .collect::<Vec<_>>();

        let sonnet = resolve_model_with_catalog("claude-sonnet-4-20250514", &models);
        assert_eq!(sonnet.upstream_model.as_deref(), Some("claude-sonnet-4.5"));
        assert_eq!(sonnet.source, ModelResolutionSource::FamilyNormalized);

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
    fn resolver_maps_dated_major_to_nearest_current_same_family_model() {
        let models = vec![
            "claude-sonnet-4".to_string(),
            "claude-sonnet-4.5".to_string(),
        ];
        let result = resolve_model_with_catalog("claude-sonnet-4-20250514", &models);
        assert_eq!(result.source, ModelResolutionSource::FamilyNormalized);
        assert_eq!(result.upstream_model.as_deref(), Some("claude-sonnet-4.5"));
    }

    #[test]
    fn resolver_respects_disabled_cross_version_fallback() {
        let models = vec!["claude-sonnet-4.5".to_string()];
        let mapping = ModelMappingConfig {
            allow_version_fallback: false,
            ..ModelMappingConfig::default()
        };
        let result = resolve_model_with_catalog_mapping_and_mode(
            "claude-sonnet-4-20250514",
            &models,
            ModelResolutionMode::Compatible,
            &mapping,
        );
        assert_eq!(result.source, ModelResolutionSource::PassThrough);
        assert_eq!(
            result.upstream_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn explicit_mapping_wins_over_automatic_model_resolution() {
        let models = vec!["claude-sonnet-4.5".to_string()];
        let mapping = ModelMappingConfig {
            rules: vec![crate::model::config::ModelMappingRule {
                enabled: true,
                source: "claude-sonnet-4-20250514".to_string(),
                target: "custom-sonnet".to_string(),
                kind: ModelMappingRuleKind::Alias,
                note: None,
            }],
            ..ModelMappingConfig::default()
        };
        let result = resolve_model_with_catalog_mapping_and_mode(
            "claude-sonnet-4-20250514",
            &models,
            ModelResolutionMode::Compatible,
            &mapping,
        );
        assert_eq!(result.source, ModelResolutionSource::Alias);
        assert_eq!(result.upstream_model.as_deref(), Some("custom-sonnet"));
    }

    #[test]
    fn resolver_does_not_cross_families_by_default() {
        let models = vec!["claude-opus-4.7".to_string()];
        let result = resolve_model_with_catalog("claude-sonnet-4-20250514", &models);
        assert_eq!(result.source, ModelResolutionSource::PassThrough);
        assert_eq!(
            result.upstream_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn resolver_can_cross_families_only_when_enabled() {
        let models = vec!["claude-opus-4.7".to_string()];
        let mapping = ModelMappingConfig {
            allow_cross_family_fallback: true,
            ..ModelMappingConfig::default()
        };
        let result = resolve_model_with_catalog_mapping_and_mode(
            "claude-sonnet-4-20250514",
            &models,
            ModelResolutionMode::Compatible,
            &mapping,
        );
        assert_eq!(result.source, ModelResolutionSource::FamilyNormalized);
        assert_eq!(result.upstream_model.as_deref(), Some("claude-opus-4.7"));
    }

    #[test]
    fn resolver_matches_synced_fable_models_from_official_request_forms() {
        let models = vec!["claude-fable-5".to_string(), "claude-fable-5.1".to_string()];

        let exact = resolve_model_with_catalog("claude-fable-5", &models);
        assert_eq!(exact.source, ModelResolutionSource::ExactUpstream);
        assert_eq!(exact.upstream_model.as_deref(), Some("claude-fable-5"));

        for requested in ["claude-fable-5-thinking", "claude-fable-5[1m]-thinking"] {
            let result = resolve_model_with_catalog(requested, &models);
            assert_eq!(result.source, ModelResolutionSource::Alias, "{requested}");
            assert_eq!(
                result.upstream_model.as_deref(),
                Some("claude-fable-5"),
                "{requested}"
            );
        }

        for requested in [
            "claude-fable-5-1",
            "claude-fable-5-1-thinking",
            "claude-fable-5.1[1m]-thinking",
            "fable",
        ] {
            let result = resolve_model_with_catalog(requested, &models);
            assert_eq!(result.source, ModelResolutionSource::Alias, "{requested}");
            assert_eq!(
                result.upstream_model.as_deref(),
                Some("claude-fable-5.1"),
                "{requested}"
            );
        }

        assert_eq!(
            resolve_model_with_catalog("fable5", &models)
                .upstream_model
                .as_deref(),
            Some("fable5")
        );
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
    fn resolver_aliases_choose_highest_current_model_in_requested_family() {
        let models = vec![
            "claude-opus-4.7".to_string(),
            "claude-opus-4.8".to_string(),
            "claude-sonnet-4.6".to_string(),
            "claude-sonnet-5.5".to_string(),
            "claude-haiku-4.5".to_string(),
            "claude-haiku-5.5".to_string(),
            "claude-fable-5".to_string(),
            "claude-fable-5.1".to_string(),
        ];

        let opus = resolve_model_with_catalog("opus", &models);
        assert_eq!(opus.source, ModelResolutionSource::Alias);
        assert_eq!(opus.upstream_model.as_deref(), Some("claude-opus-4.8"));

        let default = resolve_model_with_catalog("default", &models);
        assert_eq!(default.source, ModelResolutionSource::Alias);
        assert_eq!(default.upstream_model.as_deref(), Some("claude-opus-4.8"));

        let sonnet = resolve_model_with_catalog("sonnet", &models);
        assert_eq!(sonnet.source, ModelResolutionSource::Alias);
        assert_eq!(sonnet.upstream_model.as_deref(), Some("claude-sonnet-5.5"));

        let haiku = resolve_model_with_catalog("haiku", &models);
        assert_eq!(haiku.source, ModelResolutionSource::Alias);
        assert_eq!(haiku.upstream_model.as_deref(), Some("claude-haiku-5.5"));

        let fable = resolve_model_with_catalog("fable", &models);
        assert_eq!(fable.source, ModelResolutionSource::Alias);
        assert_eq!(fable.upstream_model.as_deref(), Some("claude-fable-5.1"));

        let newer_only = resolve_model_with_catalog(
            "sonnet",
            &[
                "claude-sonnet-5".to_string(),
                "claude-sonnet-5.5".to_string(),
            ],
        );
        assert_eq!(newer_only.source, ModelResolutionSource::Alias);
        assert_eq!(
            newer_only.upstream_model.as_deref(),
            Some("claude-sonnet-5.5")
        );
    }

    #[test]
    fn resolver_explicit_versions_prefer_same_family_and_minor_before_newer_models() {
        let models = vec![
            "claude-haiku-4.5".to_string(),
            "claude-haiku-5.5".to_string(),
            "claude-sonnet-4.6".to_string(),
            "claude-sonnet-5.5".to_string(),
            "claude-opus-4.7".to_string(),
            "claude-opus-5.5".to_string(),
            "claude-fable-5".to_string(),
            "claude-fable-5.1".to_string(),
        ];

        for (requested, expected) in [
            ("claude-haiku-4-5-20251001", "claude-haiku-4.5"),
            ("claude-haiku-4-5-20251001-thinking", "claude-haiku-4.5"),
            ("claude-sonnet-4-6", "claude-sonnet-4.6"),
            ("claude-opus-4-7", "claude-opus-4.7"),
            ("claude-fable-5-1", "claude-fable-5.1"),
        ] {
            let result = resolve_model_with_catalog(requested, &models);
            assert_eq!(
                result.upstream_model.as_deref(),
                Some(expected),
                "{requested} must not drift to a newer family model"
            );
        }
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
    fn resolver_maps_known_unavailable_minor_versions_to_closest_family_model() {
        let models = vec!["claude-opus-4.5".to_string(), "claude-opus-4.6".to_string()];

        // claude-opus-4.8 is a known model (static list / seed) that this catalog lacks.
        let result = resolve_model_with_catalog("claude-opus-4-8", &models);

        assert_eq!(result.source, ModelResolutionSource::FamilyNormalized);
        assert_eq!(result.upstream_model.as_deref(), Some("claude-opus-4.6"));
        assert!(result.note.unwrap().contains("closest same-family"));
    }

    #[test]
    fn resolver_uses_same_family_catalog_for_unlisted_minor_versions() {
        let models = vec![
            "claude-opus-4.5".to_string(),
            "claude-sonnet-4.6".to_string(),
        ];

        for (requested, expected) in [
            ("claude-sonnet-4-7", "claude-sonnet-4.6"),
            ("claude-sonnet-4-7-thinking", "claude-sonnet-4.6"),
            ("claude-opus-4-9", "claude-opus-4.5"),
        ] {
            let result = resolve_model_with_catalog(requested, &models);
            assert_eq!(
                result.source,
                ModelResolutionSource::FamilyNormalized,
                "{requested}"
            );
            assert_eq!(result.upstream_model.as_deref(), Some(expected));
        }
        let haiku = resolve_model_with_catalog("claude-haiku-4-6", &models);
        assert_eq!(haiku.source, ModelResolutionSource::PassThrough);
        assert_eq!(haiku.upstream_model.as_deref(), Some("claude-haiku-4-6"));
        for requested in ["claude-foo-9-9", "gpt-4o"] {
            let result = resolve_model_with_catalog(requested, &models);
            assert_eq!(
                result.source,
                ModelResolutionSource::PassThrough,
                "{requested}"
            );
        }
        let fable = resolve_model_with_catalog("claude-fable-5-1", &models);
        assert_eq!(fable.source, ModelResolutionSource::PassThrough);
        assert_eq!(fable.upstream_model.as_deref(), Some("claude-fable-5-1"));
    }

    #[test]
    fn thinking_suffixed_models_split_into_base_model_and_thinking_mode() {
        // A discovered catalog without `-thinking` variants: every `-thinking` request must
        // resolve to the matching base model while the requested name keeps the thinking flag.
        let models = [
            "claude-haiku-4.5",
            "claude-haiku-4-5-20251001",
            "claude-sonnet-4",
            "claude-sonnet-4.5",
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4.6",
            "claude-sonnet-4-6",
            "claude-sonnet-5",
            "claude-sonnet-5.5",
            "claude-haiku-5.5",
            "claude-fable-5",
            "claude-fable-5.1",
            "claude-opus-4.5",
            "claude-opus-4-5-20251101",
            "claude-opus-4.6",
            "claude-opus-4-6",
            "claude-opus-4.7",
            "claude-opus-4-7",
            "claude-opus-4.8",
            "claude-opus-5",
            "claude-opus-5.5",
        ]
        .map(str::to_string)
        .to_vec();
        for (requested, expected_upstream) in [
            ("sonnet-thinking", "claude-sonnet-5.5"),
            ("opus-thinking", "claude-opus-5.5"),
            ("haiku-thinking", "claude-haiku-5.5"),
            ("claude-sonnet-4-5-thinking", "claude-sonnet-4.5"),
            ("claude-opus-4-5-thinking", "claude-opus-4.5"),
            ("claude-haiku-4-5-thinking", "claude-haiku-4.5"),
            ("claude-sonnet-4-6-thinking", "claude-sonnet-4-6"),
            ("claude-opus-4-6-thinking", "claude-opus-4-6"),
            ("claude-opus-4-7-thinking", "claude-opus-4-7"),
            ("claude-opus-4-8-thinking", "claude-opus-4.8"),
            ("claude-sonnet-5-thinking", "claude-sonnet-5"),
            ("claude-sonnet-5-5-thinking", "claude-sonnet-5.5"),
            ("claude-haiku-5-5-thinking", "claude-haiku-5.5"),
            ("claude-fable-5-thinking", "claude-fable-5"),
            ("claude-fable-5-1-thinking", "claude-fable-5.1"),
            ("claude-fable-5.1[1m]-thinking", "claude-fable-5.1"),
            ("claude-opus-5-thinking", "claude-opus-5"),
            ("claude-opus-5-5-thinking", "claude-opus-5.5"),
            ("claude-opus-5.5-thinking", "claude-opus-5.5"),
            (
                "claude-sonnet-4-5-20250929-thinking",
                "claude-sonnet-4-5-20250929",
            ),
            (
                "claude-opus-4-5-20251101-thinking",
                "claude-opus-4-5-20251101",
            ),
            (
                "claude-haiku-4-5-20251001-thinking",
                "claude-haiku-4-5-20251001",
            ),
            ("claude-sonnet-4-20250514-thinking", "claude-sonnet-4.5"),
            ("sonnet[1m]-thinking", "claude-sonnet-5.5"),
            ("opus-thinking[1m]", "claude-opus-5.5"),
            ("claude-sonnet-4-6-thinking[1m]", "claude-sonnet-4-6"),
            ("claude-sonnet-4-6[1m]-thinking", "claude-sonnet-4-6"),
            ("claude-opus-4-8-thinking[1m]", "claude-opus-4.8"),
            ("claude-opus-4-8[1m]-thinking", "claude-opus-4.8"),
            (
                "claude-sonnet-4-5-20250929[1m]-thinking",
                "claude-sonnet-4-5-20250929",
            ),
        ] {
            let result = resolve_model_with_catalog(requested, &models);
            assert_eq!(
                result.upstream_model.as_deref(),
                Some(expected_upstream),
                "{requested}"
            );
            assert!(
                strip_model_compat_suffixes(requested).1,
                "{requested} must keep the thinking mode"
            );
            // Same-spelling and suffix-only requests still need to match a credential listing
            // the resolved base model. Cross-version requests are matched by the resolved
            // catalog id at the scheduler boundary, so the raw dated id is intentionally not
            // used as a credential capability probe here.
            let (requested_base, _) = strip_model_compat_suffixes(requested);
            if requested.starts_with("claude-") && expected_upstream == requested_base {
                assert!(
                    crate::model::model_support::model_is_supported_by_list(
                        &[expected_upstream.to_string()],
                        &[Some(requested)],
                    ),
                    "{requested} must schedule on credentials listing {expected_upstream}"
                );
            }
        }
    }

    #[test]
    fn resolver_uses_catalog_for_any_structurally_valid_claude_version() {
        let models = vec!["claude-sonnet-4.5".to_string()];
        for requested in [
            "claude-sonnet-4-7",
            "claude-sonnet-4-20250514",
            "claude-sonnet-99-1",
        ] {
            let result = resolve_model_with_catalog(requested, &models);
            assert_eq!(
                result.upstream_model.as_deref(),
                Some("claude-sonnet-4.5"),
                "{requested} must use the current same-family catalog"
            );
            assert_eq!(result.source, ModelResolutionSource::FamilyNormalized);
        }
        assert_eq!(
            resolve_model_with_catalog("claude-foo-9-9", &models)
                .upstream_model
                .as_deref(),
            Some("claude-foo-9-9")
        );
    }

    #[test]
    fn resolver_maps_known_sonnet_46_to_closest_free_pool_model() {
        let models = vec![
            "auto".to_string(),
            "claude-haiku-4.5".to_string(),
            "claude-sonnet-4".to_string(),
            "claude-sonnet-4.5".to_string(),
            "deepseek-3.2".to_string(),
            "glm-5".to_string(),
        ];

        for requested in [
            "claude-sonnet-4-6",
            "claude-sonnet-4.6",
            "claude-sonnet-4-6-thinking",
        ] {
            let result = resolve_model_with_catalog(requested, &models);
            assert_eq!(
                result.source,
                ModelResolutionSource::FamilyNormalized,
                "{requested}"
            );
            assert_eq!(result.requested_model, requested);
            assert_eq!(result.upstream_model.as_deref(), Some("claude-sonnet-4.5"));
        }
    }

    #[test]
    fn resolver_allows_explicit_minor_version_spelling_equivalence_only() {
        let models = vec!["claude-sonnet-4.6".to_string()];

        let dashed = resolve_model_with_catalog("claude-sonnet-4-6", &models);
        assert_eq!(dashed.source, ModelResolutionSource::Alias);
        assert_eq!(dashed.upstream_model.as_deref(), Some("claude-sonnet-4.6"));

        let thinking_models = vec!["claude-sonnet-4-6-thinking".to_string()];
        let dotted_thinking =
            resolve_model_with_catalog("claude-sonnet-4.6-thinking", &thinking_models);
        assert_eq!(dotted_thinking.source, ModelResolutionSource::Alias);
        assert_eq!(
            dotted_thinking.upstream_model.as_deref(),
            Some("claude-sonnet-4-6-thinking")
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
            allow_version_fallback: true,
            allow_cross_family_fallback: false,
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
            allow_version_fallback: true,
            allow_cross_family_fallback: false,
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
            allow_version_fallback: true,
            allow_cross_family_fallback: false,
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
    fn standard_models_list_only_hyphenated_claude_ids() {
        let ids: Vec<String> = ModelCapabilitiesCatalog::default()
            .standard_anthropic_models()
            .into_iter()
            .map(|model| model.id)
            .collect();

        assert!(ids.iter().any(|id| id == "claude-opus-4-6"));
        assert!(ids.iter().any(|id| id == "claude-sonnet-4-5"));
        for id in &ids {
            assert!(
                !(id.starts_with("claude-") && id.contains('.')),
                "上游 dotted id listed: {id}"
            );
            assert!(
                id == "sonnet-thinking" || !id.ends_with("-thinking"),
                "unexpected thinking variant listed: {id}"
            );
            assert!(!id.contains("[1m]"), "1m variant listed: {id}");
            assert!(
                !MODEL_LISTING_ALIASES.contains(&id.as_str()),
                "alias listed: {id}"
            );
        }
        let mut deduped = ids.clone();
        deduped.dedup();
        assert_eq!(ids, deduped);
    }

    #[test]
    fn unlisted_catalog_entries_are_not_resynthesized_by_compatibility_models() {
        let catalog = ModelCapabilitiesCatalog::new();
        let mut status = ModelCapabilitiesCatalog::seed_status();
        for item in &mut status.models {
            if item.model.contains("sonnet") {
                item.listed = false;
            }
        }
        catalog.load_persisted_status(status);
        let models = catalog.standard_anthropic_models();
        assert!(!models.is_empty());
        assert!(models.iter().all(|model| !model.id.contains("sonnet")));
        for id in [
            "claude-sonnet-4-5",
            "claude-sonnet-4.5-thinking",
            "sonnet-thinking",
        ] {
            assert!(catalog.standard_anthropic_model(id).is_none(), "{id}");
        }
    }

    #[test]
    fn standard_model_lookup_accepts_request_spellings() {
        let catalog = ModelCapabilitiesCatalog::default();
        let lookup = |id: &str| catalog.standard_anthropic_model(id).map(|model| model.id);

        assert_eq!(
            lookup("claude-opus-4-6").as_deref(),
            Some("claude-opus-4-6")
        );
        assert_eq!(
            lookup("claude-opus-4.6").as_deref(),
            Some("claude-opus-4-6")
        );
        assert_eq!(
            lookup("claude-opus-4-6-thinking").as_deref(),
            Some("claude-opus-4-6")
        );
        assert_eq!(
            lookup("claude-opus-4-6[1m]").as_deref(),
            Some("claude-opus-4-6")
        );
        assert!(lookup("sonnet").is_some_and(|id| id.starts_with("claude-sonnet-")));
        assert_eq!(lookup("claude-nonexistent-9-9"), None);
    }
}
