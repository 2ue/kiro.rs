//! 提示词注入配置：代理写进上游请求的每一段提示词各有一个开关，和路由的关联只在路由上显式设置。
//!
//! - `defaults`：没有单独设置的路由使用的值；
//! - `routes[].promptInjection`：命中路由覆盖各项开关，未设置的项继承 `defaults`；
//! - `texts`：可编辑文本，缺省时使用 [`catalog`] 里的内置文本。
//!
//! 和提示词无关、但以前和提示词开关绑在一起的功能放在 [`RouteFeaturesConfig`]。

pub mod catalog;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::model::config::{
    CompatProfile, ImageProcessingConfig, LocalRequestCompatConfig, MissingMaxTokensConfig,
    PayloadGuardMode, PayloadShapingConfig, PayloadTooLongHandling, ThinkingTriggerMode,
};

/// 单项文本上限（字符）。
pub const MAX_TEXT_CHARS: usize = 8_000;
/// 所有可编辑文本合计上限（字符）。
pub const MAX_TOTAL_TEXT_CHARS: usize = 20_000;
/// 每轮提醒的上限（字节）：控制块清洗只识别 1024 字节以内的提醒块，留出语言名替换的余量。
pub const MAX_TURN_REMINDER_BYTES: usize = 1_000;

fn default_true() -> bool {
    true
}

/// 可写入 Anthropic system 的注入项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InjectionId {
    TaskQuality,
    LanguageConstraint,
    Custom,
    IdentityPrompt,
}
impl InjectionId {
    pub const ALL: [Self; 4] = [
        Self::TaskQuality,
        Self::LanguageConstraint,
        Self::Custom,
        Self::IdentityPrompt,
    ];
    fn index(self) -> usize {
        self as usize
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskQuality => "taskQuality",
            Self::LanguageConstraint => "languageConstraint",
            Self::Custom => "custom",
            Self::IdentityPrompt => "identityPrompt",
        }
    }
    pub fn default_enabled(self) -> bool {
        false
    }
    pub fn blocked_by_strict(self) -> bool {
        self != Self::IdentityPrompt
    }
    pub fn text_keys(self) -> &'static [TextKey] {
        match self {
            Self::TaskQuality => &[TextKey::TaskQuality],
            Self::LanguageConstraint => &[TextKey::LanguageConstraint],
            Self::Custom => &[TextKey::Custom],
            Self::IdentityPrompt => &[TextKey::IdentityPrompt],
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextKey {
    TaskQuality,
    LanguageConstraint,
    Custom,
    IdentityPrompt,
}
impl TextKey {
    pub const ALL: [Self; 4] = [
        Self::TaskQuality,
        Self::LanguageConstraint,
        Self::Custom,
        Self::IdentityPrompt,
    ];
    pub fn label(self) -> &'static str {
        self.as_str()
    }
    fn required_fragments(self) -> &'static [&'static str] {
        &[]
    }
    fn is_turn_reminder(self) -> bool {
        false
    }
    fn empty_means_default(self) -> bool {
        self != Self::Custom
    }
}
/// 一项的全局默认。`account` 只对支持账号的项有意义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemSetting {
    pub enabled: bool,
}

/// 一项在某个路由上的设置；没写的字段跟随更短的前缀或全局默认。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl ItemOverride {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptInjectionConfig {
    /// count_tokens 是否计入写进 system 的注入项（任务质量、语言约束、自定义、身份声明）。
    #[serde(default = "default_true")]
    pub apply_to_count_tokens: bool,
    #[serde(default = "default_item_settings")]
    pub defaults: BTreeMap<InjectionId, ItemSetting>,
    /// 改过的文本；没有出现的槽位使用内置文本。
    #[serde(default)]
    pub texts: BTreeMap<TextKey, String>,
}

fn default_item_settings() -> BTreeMap<InjectionId, ItemSetting> {
    InjectionId::ALL
        .iter()
        .map(|id| {
            (
                *id,
                ItemSetting {
                    enabled: id.default_enabled(),
                },
            )
        })
        .collect()
}

impl Default for PromptInjectionConfig {
    fn default() -> Self {
        Self {
            apply_to_count_tokens: true,
            defaults: default_item_settings(),
            texts: BTreeMap::new(),
        }
    }
}

impl PromptInjectionConfig {
    /// 补齐缺失的默认项，去掉不支持账号的项上的账号设置，规范化路由前缀，删除空设置和
    /// 与内置文本相同的文本。
    pub fn normalized(mut self) -> Self {
        for id in InjectionId::ALL {
            self.defaults.entry(id).or_insert(ItemSetting {
                enabled: id.default_enabled(),
            });
        }
        self.texts = std::mem::take(&mut self.texts)
            .into_iter()
            .filter_map(|(key, text)| {
                let text = text.trim();
                (!text.is_empty() && text != catalog::default_text(key))
                    .then(|| (key, text.to_string()))
            })
            .collect();
        self
    }

    /// 保存前的校验：长度、每轮提醒必须保留的标签和占位符。
    pub fn validate(&self) -> Result<(), String> {
        let mut total = 0usize;
        for key in TextKey::ALL {
            let text = self.text(key);
            let chars = text.chars().count();
            if chars > MAX_TEXT_CHARS {
                return Err(format!(
                    "提示词注入文本 {} 超过 {MAX_TEXT_CHARS} 字符（当前 {chars}）",
                    key.as_str()
                ));
            }
            total += chars;
            if text.is_empty() && !key.empty_means_default() {
                continue;
            }
            for fragment in key.required_fragments() {
                if !text.contains(fragment) {
                    return Err(format!(
                        "提示词注入文本 {} 必须保留 {fragment}",
                        key.as_str()
                    ));
                }
            }
            if key.is_turn_reminder() && text.len() > MAX_TURN_REMINDER_BYTES {
                return Err(format!(
                    "提示词注入文本 {} 是每轮提醒，不能超过 {MAX_TURN_REMINDER_BYTES} 字节（当前 {}）",
                    key.as_str(),
                    text.len()
                ));
            }
        }
        if total > MAX_TOTAL_TEXT_CHARS {
            return Err(format!(
                "提示词注入文本合计不能超过 {MAX_TOTAL_TEXT_CHARS} 字符（当前 {total}）"
            ));
        }
        Ok(())
    }

    /// 注入用的文本：改过的文本，或者内置文本。
    pub fn text(&self, key: TextKey) -> &str {
        self.texts
            .get(&key)
            .map(|text| text.trim())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| catalog::default_text(key))
    }

    /// 这一项在 `endpoint` 上的开关和账号开关，以及决定开关的来源。
    pub fn item_setting_for_route(&self, _endpoint: &str, id: InjectionId) -> RouteItemSetting {
        let default = self.defaults.get(&id).copied().unwrap_or(ItemSetting {
            enabled: id.default_enabled(),
        });
        RouteItemSetting {
            enabled: default.enabled,
            source: None,
        }
    }
}

impl TextKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskQuality => "taskQuality",
            Self::LanguageConstraint => "languageConstraint",
            Self::Custom => "custom",
            Self::IdentityPrompt => "identityPrompt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteItemSetting {
    pub enabled: bool,
    /// 决定 `enabled` 的路由前缀；`None` 表示来自全局默认。
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// 开关关闭（全局默认或路由设置）。
    Off,
    /// `anthropic-strict` 不做兼容改写。
    Strict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedItem {
    pub id: InjectionId,
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<BlockReason>,
    /// 决定开关的路由前缀；`None` 表示来自全局默认。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// 一个请求允许注入的项。各注入点只问"这一项允不允许"，内容相关的条件（有没有 Write
/// 工具、tool_choice 是什么）由注入点自己判断。
#[derive(Debug, Clone)]
pub struct ResolvedInjections {
    items: Arc<[ResolvedItem]>,
    texts: Arc<BTreeMap<TextKey, String>>,
}

impl ResolvedInjections {
    pub fn resolve(endpoint: &str, profile: CompatProfile, config: &PromptInjectionConfig) -> Self {
        Self::resolve_route(profile, config, endpoint, &BTreeMap::new())
    }
    pub fn resolve_route(
        profile: CompatProfile,
        config: &PromptInjectionConfig,
        prefix: &str,
        overrides: &BTreeMap<InjectionId, ItemOverride>,
    ) -> Self {
        let items = InjectionId::ALL
            .iter()
            .map(|id| {
                let mut setting = config.item_setting_for_route(prefix, *id);
                if let Some(enabled) = overrides.get(id).and_then(|item| item.enabled) {
                    setting.enabled = enabled;
                    setting.source = Some(prefix.to_string());
                }
                let blocked = if !setting.enabled {
                    Some(BlockReason::Off)
                } else if profile.is_strict() && id.blocked_by_strict() {
                    Some(BlockReason::Strict)
                } else {
                    None
                };
                ResolvedItem {
                    id: *id,
                    allowed: blocked.is_none(),
                    blocked,
                    source: setting.source,
                }
            })
            .collect();
        let texts = TextKey::ALL
            .iter()
            .filter_map(|key| {
                let text = config.text(*key);
                (text != catalog::default_text(*key)).then(|| (*key, text.to_string()))
            })
            .collect();
        Self {
            items,
            texts: Arc::new(texts),
        }
    }

    pub fn allows(&self, id: InjectionId) -> bool {
        self.items[id.index()].allowed
    }

    pub fn item(&self, id: InjectionId) -> &ResolvedItem {
        &self.items[id.index()]
    }

    pub fn items(&self) -> &[ResolvedItem] {
        &self.items
    }

    pub fn text(&self, key: TextKey) -> &str {
        self.texts
            .get(&key)
            .map(String::as_str)
            .unwrap_or_else(|| catalog::default_text(key))
    }
}

#[cfg(test)]
impl ResolvedInjections {
    /// 测试用：新装默认配置下本地 `/v1`，再把 `changes` 里的项设成给定值。
    pub(crate) fn with_items(changes: &[(InjectionId, bool)]) -> Self {
        Self::with_items_and_profile(changes, CompatProfile::default())
    }

    pub(crate) fn with_items_and_profile(
        changes: &[(InjectionId, bool)],
        profile: CompatProfile,
    ) -> Self {
        let mut config = PromptInjectionConfig::default();
        for (id, enabled) in changes {
            config
                .defaults
                .insert(*id, ItemSetting { enabled: *enabled });
        }
        Self::resolve("/v1/messages", profile, &config)
    }

    /// 测试用：在 `with_items` 的基础上改一段文本。
    pub(crate) fn with_items_and_text(
        changes: &[(InjectionId, bool)],
        texts: &[(TextKey, &str)],
    ) -> Self {
        let mut resolved = Self::with_items(changes);
        let mut map = (*resolved.texts).clone();
        for (key, text) in texts {
            map.insert(*key, text.to_string());
        }
        resolved.texts = Arc::new(map);
        resolved
    }

    /// 测试用：全部注入项关闭。
    pub(crate) fn all_off() -> Self {
        let changes: Vec<_> = InjectionId::ALL.iter().map(|id| (*id, false)).collect();
        Self::with_items(&changes)
    }
}

impl Default for ResolvedInjections {
    /// 新装默认配置下本地 `/v1` 的结果。
    fn default() -> Self {
        Self::resolve(
            "/v1/messages",
            CompatProfile::default(),
            &PromptInjectionConfig::default(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct RoutePayloadGuardConfig {
    pub enabled: bool,
    pub mode: PayloadGuardMode,
    pub max_bytes: usize,
    pub safety_margin_bytes: usize,
    pub trim_history: bool,
    pub external_enabled: bool,
    pub too_long_handling: PayloadTooLongHandling,
}
impl Default for RoutePayloadGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: PayloadGuardMode::default(),
            max_bytes: 4 * 1024 * 1024,
            safety_margin_bytes: 64 * 1024,
            trim_history: true,
            external_enabled: true,
            too_long_handling: PayloadTooLongHandling::default(),
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct RouteFeatureSet {
    pub missing_max_tokens: Option<MissingMaxTokensConfig>,
    pub reasoning_compat: Option<LocalRequestCompatConfig>,
    pub history_sanitize: bool,
    pub thinking_model_name: bool,
    pub thinking_trigger: ThinkingTriggerMode,
    pub payload_guard: Option<RoutePayloadGuardConfig>,
    pub payload_shaping: Option<PayloadShapingConfig>,
    pub image_processing: Option<ImageProcessingConfig>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct RouteFeatureOverride {
    #[serde(
        default,
        deserialize_with = "deserialize_feature_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub missing_max_tokens: Option<Option<MissingMaxTokensConfig>>,
    #[serde(
        default,
        deserialize_with = "deserialize_feature_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_compat: Option<Option<LocalRequestCompatConfig>>,
    pub history_sanitize: Option<bool>,
    pub thinking_model_name: Option<bool>,
    pub thinking_trigger: Option<ThinkingTriggerMode>,
    #[serde(
        default,
        deserialize_with = "deserialize_feature_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub payload_guard: Option<Option<RoutePayloadGuardConfig>>,
    #[serde(
        default,
        deserialize_with = "deserialize_feature_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub payload_shaping: Option<Option<PayloadShapingConfig>>,
    #[serde(
        default,
        deserialize_with = "deserialize_feature_override",
        skip_serializing_if = "Option::is_none"
    )]
    pub image_processing: Option<Option<ImageProcessingConfig>>,
}
impl RouteFeatureSet {
    pub fn is_empty(&self) -> bool {
        self.missing_max_tokens.is_none()
            && self.reasoning_compat.is_none()
            && !self.history_sanitize
            && !self.thinking_model_name
            && self.thinking_trigger == ThinkingTriggerMode::Off
            && !self.payload_guard.is_some_and(|config| config.enabled)
            && !self.payload_shaping.is_some_and(|config| config.enabled)
            && self.image_processing.is_none()
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteFeaturesConfig {
    #[serde(default)]
    pub defaults: RouteFeatureSet,
}

impl RouteFeatureOverride {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl RouteFeatureOverride {
    pub fn apply(&self, mut resolved: RouteFeatureSet) -> RouteFeatureSet {
        let route = self;
        if let Some(value) = route.missing_max_tokens {
            resolved.missing_max_tokens = value;
        }
        if let Some(value) = route.reasoning_compat {
            resolved.reasoning_compat = value;
        }
        if let Some(value) = route.history_sanitize {
            resolved.history_sanitize = value;
        }
        if let Some(value) = route.thinking_model_name {
            resolved.thinking_model_name = value;
        }
        if let Some(value) = route.thinking_trigger {
            resolved.thinking_trigger = value;
        }
        if let Some(value) = route.payload_guard {
            resolved.payload_guard = value;
        }
        if let Some(value) = route.payload_shaping {
            resolved.payload_shaping = value;
        }
        if let Some(value) = route.image_processing {
            resolved.image_processing = value;
        }
        resolved
    }
}
impl RouteFeaturesConfig {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(config) = self.defaults.missing_max_tokens {
            config.validate()?;
        }
        if let Some(config) = self.defaults.payload_guard {
            if config.enabled
                && config.max_bytes > 0
                && config.max_bytes.saturating_sub(config.safety_margin_bytes) < 64 * 1024
            {
                return Err("payloadGuard 实际裁剪目标不能小于 65536".to_string());
            }
        }
        Ok(())
    }
    pub fn normalized(self) -> Self {
        self
    }
    pub fn resolve(&self, _endpoint: &str) -> RouteFeatureSet {
        self.defaults
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_items_and_fields_are_rejected() {
        assert!(
            serde_json::from_value::<PromptInjectionConfig>(serde_json::json!({
                "defaults": {"notAnItem": {"enabled": true}}
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PromptInjectionConfig>(serde_json::json!({
                "pathOverrides": {"/cc": {"taskQuality": {"enabeld": true}}}
            }))
            .is_err()
        );
    }

    #[test]
    fn task_quality_keeps_earlier_requests_and_scopes_corrections() {
        let text = catalog::DEFAULT_TASK_QUALITY_PROMPT;
        // Earlier, unrevoked requests stay in force and a correction only covers what it names.
        for phrase in [
            "要求是累加的",
            "没有被明确撤销的要求继续有效",
            "否定只作用于它明确指向的动作或范围",
            "新增需求记下，做完当前这一步再处理",
            "对照用户提出过的全部要求逐条核对",
        ] {
            assert!(text.contains(phrase), "{phrase}");
        }
        // The rules the previous wording existed for are still there.
        for phrase in [
            "修正立即生效，被否定的旧做法不再继续",
            "必须在同一轮输出结构化 tool_use",
            "必须给出可核查证据",
            "不要在可见回答中复述本规则",
        ] {
            assert!(text.contains(phrase), "{phrase}");
        }
        // The current turn's user message is not demoted: Claude Code attaches messages the user
        // sends mid-task next to tool results, so the text must not tell the model to ignore
        // reminders or messages that arrive there.
        for phrase in ["system", "提醒", "reminder", "Tool result received"] {
            assert!(!text.contains(phrase), "{phrase}");
        }
    }

    #[test]
    fn edited_text_equal_to_default_is_not_stored() {
        let mut config = PromptInjectionConfig::default();
        config.texts.insert(
            TextKey::TaskQuality,
            format!("  {}  ", catalog::DEFAULT_TASK_QUALITY_PROMPT),
        );
        assert!(config.normalized().texts.is_empty());
    }
}

fn deserialize_feature_override<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod passthrough_route_tests {
    use super::*;
    #[test]
    fn passthrough_features_default_off_and_longest_prefix_can_disable_defaults() {
        let defaults = RouteFeaturesConfig::default();
        for path in [
            "/v1/messages",
            "/cc/v1/messages",
            "/ha/v1/messages",
            "/na/v1/messages",
        ] {
            assert!(defaults.resolve(path).is_empty());
        }
        let config = crate::model::config::Config::from_json_value(serde_json::json!({
            "routeFeatures": {
                "defaults":{"reasoningCompat":{},"imageProcessing":{},"missingMaxTokens":{}},
                "pathOverrides":{
                    "/cc":{"reasoningCompat":null,"historySanitize":true},
                    "/cc/nested":{"historySanitize":false,"imageProcessing":null}
                }
            }
        }))
        .unwrap();
        let config =
            crate::model::config::Config::from_json_value(serde_json::to_value(config).unwrap())
                .unwrap();
        let compiled = config.routes.compile_with_config(&config);
        let broad = compiled.match_endpoint("/cc/v1/messages").unwrap().features;
        assert!(broad.reasoning_compat.is_none());
        assert!(broad.image_processing.is_some());
        assert!(broad.history_sanitize);
        let nested = compiled
            .match_endpoint("/cc/nested/v1/messages")
            .unwrap()
            .features;
        assert!(nested.reasoning_compat.is_none());
        assert!(nested.image_processing.is_none());
        assert!(!nested.history_sanitize);
        assert!(nested.missing_max_tokens.is_some());
        assert!(
            compiled
                .match_endpoint("/ccc/v1/messages")
                .unwrap()
                .features
                .reasoning_compat
                .is_some()
        );
    }
}
