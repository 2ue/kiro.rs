//! 提示词注入配置：代理写进上游请求的每一段提示词各有一个开关，和路由的关联只在路由上显式设置。
//!
//! - `defaults`：没有单独设置的路由使用的值；
//! - `pathOverrides`：按路由前缀设置。逐项取最长的、设置了这一项的匹配前缀，都没有设置时用
//!   `defaults`；
//! - `texts`：可编辑文本，缺省时使用 [`catalog`] 里的内置文本。
//!
//! 和提示词无关、但以前和提示词开关绑在一起的功能放在 [`RouteFeaturesConfig`]。

pub mod catalog;
pub mod legacy;
pub(crate) mod migrate;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::model::config::{
    CompatProfile, ThinkingTriggerMode, normalize_reported_usage_path_prefix,
    reported_usage_path_matches,
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

/// 注入项。顺序就是页面上的展示顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InjectionId {
    TaskQuality,
    LanguageConstraint,
    LanguageTurnReminder,
    Custom,
    IdentityPrompt,
    IdentityTurnReminder,
    ChunkedWritePolicy,
    ChunkedWriteToolSuffix,
    ThinkingControls,
    ThinkingOutputPolicy,
    ToolChoicePrefix,
    ToolChoiceTurnReminder,
    WebSearchGuidance,
    EmptyTurnNudge,
}
impl InjectionId {
    pub const ALL: [InjectionId; 14] = [
        Self::TaskQuality,
        Self::LanguageConstraint,
        Self::LanguageTurnReminder,
        Self::Custom,
        Self::IdentityPrompt,
        Self::IdentityTurnReminder,
        Self::ChunkedWritePolicy,
        Self::ChunkedWriteToolSuffix,
        Self::ThinkingControls,
        Self::ThinkingOutputPolicy,
        Self::ToolChoicePrefix,
        Self::ToolChoiceTurnReminder,
        Self::WebSearchGuidance,
        Self::EmptyTurnNudge,
    ];

    fn index(self) -> usize {
        self as usize
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskQuality => "taskQuality",
            Self::LanguageConstraint => "languageConstraint",
            Self::LanguageTurnReminder => "languageTurnReminder",
            Self::Custom => "custom",
            Self::IdentityPrompt => "identityPrompt",
            Self::IdentityTurnReminder => "identityTurnReminder",
            Self::ChunkedWritePolicy => "chunkedWritePolicy",
            Self::ChunkedWriteToolSuffix => "chunkedWriteToolSuffix",
            Self::ThinkingControls => "thinkingControls",
            Self::ThinkingOutputPolicy => "thinkingOutputPolicy",
            Self::ToolChoicePrefix => "toolChoicePrefix",
            Self::ToolChoiceTurnReminder => "toolChoiceTurnReminder",
            Self::WebSearchGuidance => "webSearchGuidance",
            Self::EmptyTurnNudge => "emptyTurnNudge",
        }
    }

    /// 新装默认值。`taskQuality` 另在 `/cc` 上显式开启（见 [`PromptInjectionConfig::default`]）。
    pub fn default_enabled(self) -> bool {
        !matches!(
            self,
            Self::TaskQuality
                | Self::LanguageConstraint
                | Self::LanguageTurnReminder
                | Self::Custom
                | Self::IdentityPrompt
                | Self::IdentityTurnReminder
        )
    }

    /// 能注入到外部池请求里的项。外部池不经过 Kiro 转换，只有写进 system 的这几项能生效。
    pub fn supports_external_pool(self) -> bool {
        matches!(
            self,
            Self::TaskQuality | Self::LanguageConstraint | Self::Custom | Self::IdentityPrompt
        )
    }

    /// `anthropic-strict` 下不注入的项（保持现有覆盖范围）。thinking 控制标签在强制可见
    /// thinking 时例外，由注入点处理。
    pub fn blocked_by_strict(self) -> bool {
        !matches!(
            self,
            Self::IdentityPrompt
                | Self::IdentityTurnReminder
                | Self::WebSearchGuidance
                | Self::EmptyTurnNudge
        )
    }

    /// 这一项可编辑的文本槽位。只读项（协议控制标签）返回空。
    pub fn text_keys(self) -> &'static [TextKey] {
        match self {
            Self::TaskQuality => &[TextKey::TaskQuality],
            Self::LanguageConstraint => &[TextKey::LanguageConstraint],
            Self::LanguageTurnReminder => &[
                TextKey::LanguageTurnReminderExplicit,
                TextKey::LanguageTurnReminderFollowUser,
            ],
            Self::Custom => &[TextKey::Custom],
            Self::IdentityPrompt => &[TextKey::IdentityPrompt],
            Self::IdentityTurnReminder => &[TextKey::IdentityTurnReminder],
            Self::ChunkedWritePolicy => &[TextKey::ChunkedWritePolicy],
            Self::ChunkedWriteToolSuffix => &[
                TextKey::ChunkedWriteToolSuffixWrite,
                TextKey::ChunkedWriteToolSuffixEdit,
            ],
            Self::WebSearchGuidance => &[TextKey::WebSearchGuidance],
            Self::EmptyTurnNudge => &[TextKey::EmptyTurnNudge],
            Self::ThinkingControls
            | Self::ThinkingOutputPolicy
            | Self::ToolChoicePrefix
            | Self::ToolChoiceTurnReminder => &[],
        }
    }
}

/// 可编辑文本槽位。key 形如 `taskQuality`、`languageTurnReminder.explicit`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TextKey {
    #[serde(rename = "taskQuality")]
    TaskQuality,
    #[serde(rename = "languageConstraint")]
    LanguageConstraint,
    #[serde(rename = "languageTurnReminder.explicit")]
    LanguageTurnReminderExplicit,
    #[serde(rename = "languageTurnReminder.followUser")]
    LanguageTurnReminderFollowUser,
    #[serde(rename = "custom")]
    Custom,
    #[serde(rename = "identityPrompt")]
    IdentityPrompt,
    #[serde(rename = "identityTurnReminder")]
    IdentityTurnReminder,
    #[serde(rename = "chunkedWritePolicy")]
    ChunkedWritePolicy,
    #[serde(rename = "chunkedWriteToolSuffix.write")]
    ChunkedWriteToolSuffixWrite,
    #[serde(rename = "chunkedWriteToolSuffix.edit")]
    ChunkedWriteToolSuffixEdit,
    #[serde(rename = "webSearchGuidance")]
    WebSearchGuidance,
    #[serde(rename = "emptyTurnNudge")]
    EmptyTurnNudge,
}

impl TextKey {
    pub const ALL: [TextKey; 12] = [
        Self::TaskQuality,
        Self::LanguageConstraint,
        Self::LanguageTurnReminderExplicit,
        Self::LanguageTurnReminderFollowUser,
        Self::Custom,
        Self::IdentityPrompt,
        Self::IdentityTurnReminder,
        Self::ChunkedWritePolicy,
        Self::ChunkedWriteToolSuffixWrite,
        Self::ChunkedWriteToolSuffixEdit,
        Self::WebSearchGuidance,
        Self::EmptyTurnNudge,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::LanguageTurnReminderExplicit => "客户端声明了语言（{language} 会替换成语言名）",
            Self::LanguageTurnReminderFollowUser => "跟随用户自己的消息",
            Self::ChunkedWriteToolSuffixWrite => "Write 工具",
            Self::ChunkedWriteToolSuffixEdit => "Edit 工具",
            _ => "文本",
        }
    }

    /// 注入时必须保留的片段：控制块清洗靠这些标签识别模型复述的提醒。
    fn required_fragments(self) -> &'static [&'static str] {
        match self {
            Self::LanguageTurnReminderExplicit => {
                &["<language_reminder>", "</language_reminder>", "{language}"]
            }
            Self::LanguageTurnReminderFollowUser => {
                &["<language_reminder>", "</language_reminder>"]
            }
            Self::IdentityTurnReminder => &["<identity_reminder>", "</identity_reminder>"],
            _ => &[],
        }
    }

    fn is_turn_reminder(self) -> bool {
        matches!(
            self,
            Self::LanguageTurnReminderExplicit
                | Self::LanguageTurnReminderFollowUser
                | Self::IdentityTurnReminder
        )
    }

    /// 清空时是否回到内置文本。自定义项清空就是不注入。
    fn empty_means_default(self) -> bool {
        self != Self::Custom
    }
}

/// 一项的全局默认。`externalPool` 只对支持外部池的项有意义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemSetting {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_pool: Option<bool>,
}

/// 一项在某个路由上的设置；没写的字段跟随更短的前缀或全局默认。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_pool: Option<bool>,
}

impl ItemOverride {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.external_pool.is_none()
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
    /// 缺省时为空；新装配置里 `/cc` 的任务质量设置来自 [`PromptInjectionConfig::default`]。
    #[serde(default)]
    pub path_overrides: BTreeMap<String, BTreeMap<InjectionId, ItemOverride>>,
}

fn default_item_settings() -> BTreeMap<InjectionId, ItemSetting> {
    InjectionId::ALL
        .iter()
        .map(|id| {
            (
                *id,
                ItemSetting {
                    enabled: id.default_enabled(),
                    external_pool: id.supports_external_pool().then_some(false),
                },
            )
        })
        .collect()
}

/// 新装时任务质量只在 `/cc` 上开启，这是一条可见、可改的路由设置。
fn default_injection_path_overrides() -> BTreeMap<String, BTreeMap<InjectionId, ItemOverride>> {
    BTreeMap::from([(
        "/cc".to_string(),
        BTreeMap::from([(
            InjectionId::TaskQuality,
            ItemOverride {
                enabled: Some(true),
                external_pool: None,
            },
        )]),
    )])
}

impl Default for PromptInjectionConfig {
    fn default() -> Self {
        Self {
            apply_to_count_tokens: true,
            defaults: default_item_settings(),
            texts: BTreeMap::new(),
            path_overrides: default_injection_path_overrides(),
        }
    }
}

impl PromptInjectionConfig {
    /// 补齐缺失的默认项，去掉不支持外部池的项上的外部池设置，规范化路由前缀，删除空设置和
    /// 与内置文本相同的文本。
    pub fn normalized(mut self) -> Self {
        for id in InjectionId::ALL {
            let setting = self.defaults.entry(id).or_insert(ItemSetting {
                enabled: id.default_enabled(),
                external_pool: None,
            });
            setting.external_pool = id
                .supports_external_pool()
                .then(|| setting.external_pool.unwrap_or(false));
        }
        self.path_overrides = std::mem::take(&mut self.path_overrides)
            .into_iter()
            .filter_map(|(prefix, items)| {
                let prefix = normalize_reported_usage_path_prefix(&prefix)?;
                let items: BTreeMap<_, _> = items
                    .into_iter()
                    .map(|(id, mut item)| {
                        if !id.supports_external_pool() {
                            item.external_pool = None;
                        }
                        (id, item)
                    })
                    .filter(|(_, item)| !item.is_empty())
                    .collect();
                (!items.is_empty()).then_some((prefix, items))
            })
            .collect();
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

    /// 这一项在 `endpoint` 上的开关和外部池开关，以及决定开关的来源。
    pub fn item_setting_for_route(&self, endpoint: &str, id: InjectionId) -> RouteItemSetting {
        let mut enabled = None;
        let mut external_pool = None;
        let mut enabled_len = 0;
        let mut external_len = 0;
        for (prefix, items) in &self.path_overrides {
            let Some(item) = items.get(&id) else {
                continue;
            };
            if !reported_usage_path_matches(prefix, endpoint) {
                continue;
            }
            if let Some(value) = item.enabled
                && (enabled.is_none() || prefix.len() > enabled_len)
            {
                enabled = Some((value, prefix.clone()));
                enabled_len = prefix.len();
            }
            if let Some(value) = item.external_pool
                && (external_pool.is_none() || prefix.len() > external_len)
            {
                external_pool = Some(value);
                external_len = prefix.len();
            }
        }
        let default = self.defaults.get(&id).copied().unwrap_or(ItemSetting {
            enabled: id.default_enabled(),
            external_pool: None,
        });
        let (enabled, source) = match enabled {
            Some((value, prefix)) => (value, Some(prefix)),
            None => (default.enabled, None),
        };
        RouteItemSetting {
            enabled,
            external_pool: external_pool.or(default.external_pool).unwrap_or(false),
            source,
        }
    }
}

impl TextKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskQuality => "taskQuality",
            Self::LanguageConstraint => "languageConstraint",
            Self::LanguageTurnReminderExplicit => "languageTurnReminder.explicit",
            Self::LanguageTurnReminderFollowUser => "languageTurnReminder.followUser",
            Self::Custom => "custom",
            Self::IdentityPrompt => "identityPrompt",
            Self::IdentityTurnReminder => "identityTurnReminder",
            Self::ChunkedWritePolicy => "chunkedWritePolicy",
            Self::ChunkedWriteToolSuffixWrite => "chunkedWriteToolSuffix.write",
            Self::ChunkedWriteToolSuffixEdit => "chunkedWriteToolSuffix.edit",
            Self::WebSearchGuidance => "webSearchGuidance",
            Self::EmptyTurnNudge => "emptyTurnNudge",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteItemSetting {
    pub enabled: bool,
    pub external_pool: bool,
    /// 决定 `enabled` 的路由前缀；`None` 表示来自全局默认。
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpstreamKind {
    Local,
    ExternalPool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// 开关关闭（全局默认或路由设置）。
    Off,
    /// 外部池请求不经过 Kiro 转换，这一项没法注入。
    ExternalPoolUnsupported,
    /// 这一项的外部池开关关闭。
    ExternalPoolOff,
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
    pub fn resolve(
        endpoint: &str,
        upstream: UpstreamKind,
        profile: CompatProfile,
        config: &PromptInjectionConfig,
    ) -> Self {
        let items = InjectionId::ALL
            .iter()
            .map(|id| {
                let setting = config.item_setting_for_route(endpoint, *id);
                let blocked = if !setting.enabled {
                    Some(BlockReason::Off)
                } else if upstream == UpstreamKind::ExternalPool && !id.supports_external_pool() {
                    Some(BlockReason::ExternalPoolUnsupported)
                } else if upstream == UpstreamKind::ExternalPool && !setting.external_pool {
                    Some(BlockReason::ExternalPoolOff)
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
        config.path_overrides.clear();
        for (id, enabled) in changes {
            config.defaults.insert(
                *id,
                ItemSetting {
                    enabled: *enabled,
                    external_pool: id.supports_external_pool().then_some(false),
                },
            );
        }
        Self::resolve("/v1/messages", UpstreamKind::Local, profile, &config)
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
            UpstreamKind::Local,
            CompatProfile::default(),
            &PromptInjectionConfig::default(),
        )
    }
}

/// 按路由控制的功能。这些功能以前和提示词开关绑在一起，本身不注入提示词。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteFeatureSet {
    /// `claude-xxx-thinking` 这类模型名自动补上 thinking 参数。
    #[serde(default = "default_true")]
    pub thinking_model_name: bool,
    #[serde(default)]
    pub thinking_trigger: ThinkingTriggerMode,
    /// 删除回复里暴露上游渠道的句子（"I'm Kiro"、"conflicting instructions" 等）。
    #[serde(default)]
    pub channel_leak_guard: bool,
    /// 流式回复也过滤：每句话检查完才下发，首字会稍晚。
    #[serde(default = "default_true")]
    pub channel_leak_guard_stream: bool,
    /// 原生 web_search 由模型生成搜索词，代理执行搜索，模型根据结果写带引用的回答。
    #[serde(default)]
    pub model_driven_web_search: bool,
}

impl Default for RouteFeatureSet {
    fn default() -> Self {
        Self {
            thinking_model_name: true,
            thinking_trigger: ThinkingTriggerMode::default(),
            channel_leak_guard: false,
            channel_leak_guard_stream: true,
            model_driven_web_search: false,
        }
    }
}

impl RouteFeatureSet {
    pub fn channel_leak_guard_applies(&self, stream: bool) -> bool {
        self.channel_leak_guard && (!stream || self.channel_leak_guard_stream)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteFeatureOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_model_name: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_trigger: Option<ThinkingTriggerMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_leak_guard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_leak_guard_stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_driven_web_search: Option<bool>,
}

impl RouteFeatureOverride {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteFeaturesConfig {
    #[serde(default)]
    pub defaults: RouteFeatureSet,
    #[serde(default)]
    pub path_overrides: BTreeMap<String, RouteFeatureOverride>,
}

impl RouteFeaturesConfig {
    pub fn normalized(mut self) -> Self {
        self.path_overrides = std::mem::take(&mut self.path_overrides)
            .into_iter()
            .filter(|(_, route)| !route.is_empty())
            .filter_map(|(prefix, route)| {
                normalize_reported_usage_path_prefix(&prefix).map(|prefix| (prefix, route))
            })
            .collect();
        self
    }

    /// 逐项取设置了这一项的最长匹配前缀，没有设置时用全局默认。
    pub fn resolve(&self, endpoint: &str) -> RouteFeatureSet {
        let mut matching: Vec<(&String, &RouteFeatureOverride)> = self
            .path_overrides
            .iter()
            .filter(|(prefix, _)| reported_usage_path_matches(prefix, endpoint))
            .collect();
        // 短前缀先应用，长前缀覆盖。
        matching.sort_by_key(|(prefix, _)| prefix.len());
        let mut resolved = self.defaults;
        for (_, route) in matching {
            if let Some(value) = route.thinking_model_name {
                resolved.thinking_model_name = value;
            }
            if let Some(value) = route.thinking_trigger {
                resolved.thinking_trigger = value;
            }
            if let Some(value) = route.channel_leak_guard {
                resolved.channel_leak_guard = value;
            }
            if let Some(value) = route.channel_leak_guard_stream {
                resolved.channel_leak_guard_stream = value;
            }
            if let Some(value) = route.model_driven_web_search {
                resolved.model_driven_web_search = value;
            }
        }
        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_config_fills_missing_items_with_built_in_defaults() {
        let config: PromptInjectionConfig = serde_json::from_value(serde_json::json!({
            "pathOverrides": {"/cc/": {"taskQuality": {"enabled": true}}}
        }))
        .unwrap();
        let config = config.normalized();
        assert_eq!(config.defaults.len(), InjectionId::ALL.len());
        for id in InjectionId::ALL {
            assert_eq!(config.defaults[&id].enabled, id.default_enabled(), "{id:?}");
            assert_eq!(
                config.defaults[&id].external_pool.is_some(),
                id.supports_external_pool()
            );
        }
        assert!(
            config.path_overrides.contains_key("/cc"),
            "prefix is normalized"
        );
        assert_eq!(
            config
                .item_setting_for_route("/cc/v1/messages", InjectionId::TaskQuality)
                .source
                .as_deref(),
            Some("/cc")
        );
    }

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
    fn validation_requires_reminder_tags_and_language_placeholder() {
        let mut config = PromptInjectionConfig::default();
        assert!(config.validate().is_ok());
        config.texts.insert(
            TextKey::LanguageTurnReminderExplicit,
            "<language_reminder>Reply in Chinese.</language_reminder>".into(),
        );
        assert!(config.validate().unwrap_err().contains("{language}"));
        config.texts.insert(
            TextKey::LanguageTurnReminderExplicit,
            "<language_reminder>{language}</language_reminder>".into(),
        );
        config
            .texts
            .insert(TextKey::IdentityTurnReminder, "Be Claude.".into());
        assert!(
            config
                .validate()
                .unwrap_err()
                .contains("<identity_reminder>")
        );
        config.texts.remove(&TextKey::IdentityTurnReminder);
        config
            .texts
            .insert(TextKey::Custom, "x".repeat(MAX_TEXT_CHARS + 1));
        assert!(config.validate().unwrap_err().contains("超过"));
        // Clearing a text restores the built-in one, so an empty edit is not a validation error.
        config.texts.insert(TextKey::Custom, String::new());
        assert!(config.validate().is_ok());
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
