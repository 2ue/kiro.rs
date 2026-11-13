//! 强制模型收敛配置。
//!
//! 打开后，所有 Claude 模型请求在入口按规则改写为配置的目标模型（默认 Haiku 4.5），
//! 对外仍以客户端请求的模型（外显模型）表现。与 `modelMapping` 的兼容解析相互独立：
//! 改写后的目标模型交给现有解析器，调度、冷却、转换沿用现状。默认关闭。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::reported_usage_path_matches;

pub const DEFAULT_FORCED_HAIKU_TARGET: &str = "claude-haiku-4-5";

fn default_true() -> bool {
    true
}

fn default_target() -> String {
    DEFAULT_FORCED_HAIKU_TARGET.to_string()
}

fn default_rules() -> Vec<ForcedModelRule> {
    Vec::new()
}

/// 一条收敛规则：外显模型的族命中 `families` 时改发到 `target`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ForcedModelRule {
    #[serde(default)]
    pub families: Vec<String>,
    #[serde(default)]
    pub target: String,
}

/// 单个路由的覆盖；未设置的字段沿用全局值。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ForcedModelRouteOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// 上下文退避参数。窗口取目标模型目录中的 `maxInputTokens`，没有时按 200K。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ForcedContextBackoffConfig {
    /// 预算安全余量比例。
    #[serde(default = "default_safety_margin_ratio")]
    pub safety_margin_ratio: f64,
    /// 预留输出的下限。
    #[serde(default = "default_min_reserved_output_tokens")]
    pub min_reserved_output_tokens: i32,
    /// 清理旧工具结果时保留原文的最近条数。
    #[serde(default = "default_keep_recent_tool_results")]
    pub keep_recent_tool_results: usize,
    /// 清理边界的推进步长（tokens），相邻请求在同一步长内得到相同的处理结果。
    #[serde(default = "default_step_tokens")]
    pub step_tokens: i32,
    /// 预留输出的上限。Kiro 不接收 max_tokens，实际输出很少超过 32K；按客户端的 128K 预留会让
    /// 可用输入只剩窗口的六成多。
    #[serde(default = "default_max_reserved_output_tokens")]
    pub max_reserved_output_tokens: i32,
    /// 摘要检查点：超出预算时把早期对话浓缩成一段记录，而不是直接删掉（关闭时只用硬裁剪）。
    #[serde(default = "default_true")]
    pub summary_enabled: bool,
    /// 估算值超过 `预算 × 该比例` 时在后台预先生成下一个检查点。
    #[serde(default = "default_summary_precompact_ratio")]
    pub summary_precompact_ratio: f64,
    /// 新检查点让 [首条消息 + 摘要 + 尾部] 落到 `预算 × 该比例` 以下。
    #[serde(default = "default_summary_target_ratio")]
    pub summary_target_ratio: f64,
    /// 同步等待摘要的上限（秒），超时就本次走硬裁剪，摘要在后台继续完成。
    #[serde(default = "default_summary_timeout_secs")]
    pub summary_timeout_secs: u64,
}

fn default_safety_margin_ratio() -> f64 {
    0.05
}

fn default_min_reserved_output_tokens() -> i32 {
    8_000
}

fn default_keep_recent_tool_results() -> usize {
    8
}

fn default_step_tokens() -> i32 {
    50_000
}

fn default_max_reserved_output_tokens() -> i32 {
    32_000
}

fn default_summary_precompact_ratio() -> f64 {
    0.75
}

fn default_summary_target_ratio() -> f64 {
    0.45
}

fn default_summary_timeout_secs() -> u64 {
    120
}

impl Default for ForcedContextBackoffConfig {
    fn default() -> Self {
        Self {
            safety_margin_ratio: default_safety_margin_ratio(),
            min_reserved_output_tokens: default_min_reserved_output_tokens(),
            keep_recent_tool_results: default_keep_recent_tool_results(),
            step_tokens: default_step_tokens(),
            max_reserved_output_tokens: default_max_reserved_output_tokens(),
            summary_enabled: true,
            summary_precompact_ratio: default_summary_precompact_ratio(),
            summary_target_ratio: default_summary_target_ratio(),
            summary_timeout_secs: default_summary_timeout_secs(),
        }
    }
}

impl ForcedContextBackoffConfig {
    fn normalized(mut self) -> Self {
        if !self.safety_margin_ratio.is_finite() {
            self.safety_margin_ratio = default_safety_margin_ratio();
        }
        self.safety_margin_ratio = self.safety_margin_ratio.clamp(0.0, 0.5);
        self.min_reserved_output_tokens = self.min_reserved_output_tokens.clamp(1_024, 64_000);
        self.keep_recent_tool_results = self.keep_recent_tool_results.clamp(1, 64);
        self.step_tokens = self.step_tokens.clamp(1_000, 500_000);
        self.max_reserved_output_tokens = self
            .max_reserved_output_tokens
            .clamp(self.min_reserved_output_tokens, 64_000);
        if !self.summary_precompact_ratio.is_finite() {
            self.summary_precompact_ratio = default_summary_precompact_ratio();
        }
        if !self.summary_target_ratio.is_finite() {
            self.summary_target_ratio = default_summary_target_ratio();
        }
        self.summary_precompact_ratio = self.summary_precompact_ratio.clamp(0.3, 0.95);
        self.summary_target_ratio = self
            .summary_target_ratio
            .clamp(0.2, (self.summary_precompact_ratio - 0.1).max(0.2));
        self.summary_timeout_secs = self.summary_timeout_secs.clamp(5, 300);
        self
    }
}

/// 一条知识条目：用户本轮文字命中 `topics` 时，把 `fact` 作为当轮上下文提供给模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ForcedKnowledgeEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 关键词（大小写不敏感，中英文均可）。
    #[serde(default)]
    pub topics: Vec<String>,
    /// `en` / `zh`；空表示不限语言。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default)]
    pub fact: String,
}

impl ForcedKnowledgeEntry {
    fn normalized(mut self) -> Option<Self> {
        self.id = self.id.trim().to_string();
        self.topics = self
            .topics
            .into_iter()
            .map(|topic| topic.trim().to_lowercase())
            .filter(|topic| !topic.is_empty())
            .collect();
        self.locale = self
            .locale
            .map(|locale| locale.trim().to_ascii_lowercase())
            .filter(|locale| !locale.is_empty());
        self.fact = self.fact.trim().to_string();
        (!self.topics.is_empty() && !self.fact.is_empty()).then_some(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ForcedModelMappingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_rules")]
    pub rules: Vec<ForcedModelRule>,
    /// 规则都没命中的 Claude 模型（含短别名）改发到这里。
    #[serde(default = "default_target")]
    pub default_target: String,
    #[serde(default)]
    pub path_overrides: BTreeMap<String, ForcedModelRouteOverride>,
    /// 单个外显模型的精确覆盖，优先于模型族规则和默认目标。
    #[serde(default)]
    pub model_overrides: BTreeMap<String, String>,
    #[serde(default)]
    pub context_backoff: ForcedContextBackoffConfig,
    /// 外显模型为 Opus 5.5 且思考隐藏时，把工具调用之前的文字作为 thinking 块下发。
    #[serde(default = "default_true")]
    pub interleaved_text_as_thinking: bool,
    /// 用户问到相关话题时提供的知识条目；为空时使用内置条目。
    #[serde(default)]
    pub knowledge: Vec<ForcedKnowledgeEntry>,
}

impl Default for ForcedModelMappingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rules: default_rules(),
            default_target: default_target(),
            path_overrides: BTreeMap::new(),
            model_overrides: BTreeMap::new(),
            context_backoff: ForcedContextBackoffConfig::default(),
            interleaved_text_as_thinking: true,
            knowledge: Vec::new(),
        }
    }
}

impl ForcedModelMappingConfig {
    pub fn normalized(mut self) -> Self {
        self.rules = self
            .rules
            .into_iter()
            .filter_map(|mut rule| {
                rule.target = rule.target.trim().to_ascii_lowercase();
                rule.families = rule
                    .families
                    .into_iter()
                    .map(|family| family.trim().to_ascii_lowercase())
                    .filter(|family| !family.is_empty())
                    .collect();
                rule.families.dedup();
                (!rule.target.is_empty() && !rule.families.is_empty()).then_some(rule)
            })
            .collect();
        self.default_target = self.default_target.trim().to_ascii_lowercase();
        if self.default_target.is_empty() {
            self.default_target = default_target();
        }
        self.path_overrides = self
            .path_overrides
            .into_iter()
            .filter_map(|(path, value)| {
                let path = path.trim().trim_end_matches('/').to_string();
                let path = if path.is_empty() {
                    "/".to_string()
                } else if path.starts_with('/') {
                    path
                } else {
                    format!("/{path}")
                };
                (value != ForcedModelRouteOverride::default()).then_some((path, value))
            })
            .collect();
        self.model_overrides = self
            .model_overrides
            .into_iter()
            .filter_map(|(model, target)| {
                let model = model.trim().to_ascii_lowercase().replace('.', "-");
                let target = target.trim().to_ascii_lowercase().replace('.', "-");
                (!model.is_empty() && !target.is_empty()).then_some((model, target))
            })
            .collect();
        self.context_backoff = self.context_backoff.normalized();
        self.knowledge = self
            .knowledge
            .into_iter()
            .filter_map(ForcedKnowledgeEntry::normalized)
            .collect();
        self
    }

    /// 强制收敛是否作用于 `endpoint`：最长匹配的路由覆盖优先，否则取全局开关。
    pub fn applies_to(&self, endpoint: &str) -> bool {
        self.path_overrides
            .iter()
            .filter(|(prefix, _)| reported_usage_path_matches(prefix, endpoint))
            .filter_map(|(prefix, route)| route.enabled.map(|enabled| (prefix.len(), enabled)))
            .max_by_key(|(len, _)| *len)
            .map(|(_, enabled)| enabled)
            .unwrap_or(self.enabled)
    }

    /// 是否有任何路由开启了强制收敛（用于模型列表等无路由上下文的口径）。
    pub fn any_route_enabled(&self) -> bool {
        self.enabled
            || self
                .path_overrides
                .values()
                .any(|route| route.enabled == Some(true))
    }

    /// 配置里出现的全部目标模型（去重）。
    pub fn targets(&self) -> Vec<String> {
        let mut targets: Vec<String> = self
            .rules
            .iter()
            .map(|rule| rule.target.clone())
            .chain(self.model_overrides.values().cloned())
            .chain(std::iter::once(self.default_target.clone()))
            .collect();
        targets.sort();
        targets.dedup();
        targets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_and_route_every_model_to_haiku() {
        let config = ForcedModelMappingConfig::default();
        assert!(!config.enabled);
        assert!(!config.applies_to("/v1/messages"));
        assert!(config.rules.is_empty());
        assert_eq!(config.default_target, DEFAULT_FORCED_HAIKU_TARGET);
        assert!(config.model_overrides.is_empty());
        assert!(config.interleaved_text_as_thinking);
    }

    #[test]
    fn missing_field_deserializes_to_defaults() {
        let config: ForcedModelMappingConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config, ForcedModelMappingConfig::default());
        let config: ForcedModelMappingConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(config.enabled);
        assert_eq!(config.rules, default_rules());
    }

    #[test]
    fn longest_route_override_wins() {
        let mut config = ForcedModelMappingConfig {
            enabled: true,
            ..ForcedModelMappingConfig::default()
        };
        config.path_overrides.insert(
            "/cc".to_string(),
            ForcedModelRouteOverride {
                enabled: Some(false),
            },
        );
        config.path_overrides.insert(
            "/cc/v1/messages".to_string(),
            ForcedModelRouteOverride {
                enabled: Some(true),
            },
        );
        let config = config.normalized();
        assert!(config.applies_to("/v1/messages"));
        assert!(!config.applies_to("/cc/v1/models"));
        assert!(config.applies_to("/cc/v1/messages/count_tokens"));
        assert!(config.applies_to("/cc/v1/messages"));
        // A prefix only matches on a path-segment boundary.
        assert!(config.applies_to("/ccx/v1/messages"));
    }

    #[test]
    fn route_override_can_enable_a_single_route_while_global_is_off() {
        let mut config = ForcedModelMappingConfig::default();
        config.path_overrides.insert(
            "dfcache/eco".to_string(),
            ForcedModelRouteOverride {
                enabled: Some(true),
            },
        );
        let config = config.normalized();
        assert!(config.applies_to("/dfcache/eco/v1/messages"));
        assert!(!config.applies_to("/v1/messages"));
        assert!(config.any_route_enabled());
    }

    #[test]
    fn normalization_drops_empty_rules_and_entries() {
        let config = ForcedModelMappingConfig {
            rules: vec![
                ForcedModelRule {
                    families: vec![" Opus ".to_string(), "".to_string()],
                    target: " Claude-Sonnet-4-5 ".to_string(),
                },
                ForcedModelRule {
                    families: vec![],
                    target: "claude-haiku-4-5".to_string(),
                },
            ],
            default_target: "  ".to_string(),
            model_overrides: BTreeMap::from([(
                " Claude-Opus-5.5 ".to_string(),
                " Claude-Haiku-4.5 ".to_string(),
            )]),
            knowledge: vec![
                ForcedKnowledgeEntry {
                    id: "a".to_string(),
                    enabled: true,
                    topics: vec![" Opus 5.5 ".to_string()],
                    locale: Some(" EN ".to_string()),
                    fact: " fact ".to_string(),
                },
                ForcedKnowledgeEntry {
                    id: "b".to_string(),
                    enabled: true,
                    topics: vec![],
                    locale: None,
                    fact: "no topics".to_string(),
                },
            ],
            ..ForcedModelMappingConfig::default()
        }
        .normalized();
        assert_eq!(config.rules.len(), 1);
        assert_eq!(config.rules[0].families, vec!["opus"]);
        assert_eq!(config.rules[0].target, "claude-sonnet-4-5");
        assert_eq!(config.default_target, DEFAULT_FORCED_HAIKU_TARGET);
        assert_eq!(
            config.model_overrides.get("claude-opus-5-5"),
            Some(&"claude-haiku-4-5".to_string())
        );
        assert_eq!(config.knowledge.len(), 1);
        assert_eq!(config.knowledge[0].topics, vec!["opus 5.5"]);
        assert_eq!(config.knowledge[0].locale.as_deref(), Some("en"));
        assert_eq!(config.knowledge[0].fact, "fact");
    }

    #[test]
    fn backoff_values_are_clamped() {
        let config = ForcedModelMappingConfig {
            context_backoff: ForcedContextBackoffConfig {
                safety_margin_ratio: f64::NAN,
                min_reserved_output_tokens: 1,
                keep_recent_tool_results: 0,
                step_tokens: 1,
                max_reserved_output_tokens: 1,
                summary_enabled: true,
                summary_precompact_ratio: f64::NAN,
                summary_target_ratio: 5.0,
                summary_timeout_secs: 0,
            },
            ..ForcedModelMappingConfig::default()
        }
        .normalized();
        assert_eq!(config.context_backoff.safety_margin_ratio, 0.05);
        assert_eq!(config.context_backoff.min_reserved_output_tokens, 1_024);
        assert_eq!(config.context_backoff.keep_recent_tool_results, 1);
        assert_eq!(config.context_backoff.step_tokens, 1_000);
        assert_eq!(config.context_backoff.max_reserved_output_tokens, 1_024);
        assert_eq!(config.context_backoff.summary_precompact_ratio, 0.75);
        assert_eq!(config.context_backoff.summary_target_ratio, 0.65);
        assert_eq!(config.context_backoff.summary_timeout_secs, 5);
    }

    #[test]
    fn targets_are_deduplicated() {
        let config = ForcedModelMappingConfig::default();
        assert_eq!(
            config.targets(),
            vec![DEFAULT_FORCED_HAIKU_TARGET.to_string()]
        );
    }
}
