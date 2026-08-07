use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::anthropic::inference_attempt_budget::DEFAULT_AUXILIARY_UPSTREAM_MAX_CONCURRENT_REQUESTS;

pub(crate) const DEFAULT_TOKEN_REFRESH_MAX_RPM: u32 = 60;
pub(crate) const MIN_TOKEN_REFRESH_MAX_RPM: u32 = 1;
pub(crate) const MAX_TOKEN_REFRESH_MAX_RPM: u32 = 6_000;
pub(crate) const DEFAULT_TOKEN_REFRESH_BURST: u32 = 8;
pub(crate) const MIN_TOKEN_REFRESH_BURST: u32 = 1;
pub(crate) const MAX_TOKEN_REFRESH_BURST: u32 = 256;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum TlsBackend {
    Rustls,
    NativeTls,
}

impl Default for TlsBackend {
    fn default() -> Self {
        Self::Rustls
    }
}

/// 路径级 prompt-cache usage 模式。
///
/// 该枚举仍作为内部请求链路标记使用。入口路径只负责定位配置；
/// 缓存模式由 `cachePolicy` 的默认值和路径覆盖项决定。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PromptCacheSimulationMode {
    Disabled,
    HighCache,
}

impl Default for PromptCacheSimulationMode {
    fn default() -> Self {
        Self::HighCache
    }
}

/// 上游请求压缩配置。
///
/// 默认总开关关闭；如果启用，默认只执行低风险的 whitespace 压缩。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompressionConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_true")]
    pub whitespace_compression: bool,
}

/// Kiro payload shaping 配置。
///
/// 该配置只处理旧历史和可安全压缩的冗余内容；默认不截断当前用户消息、
/// 当前合法 tool_result、当前 document/PDF 或当前图片。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OversizedImageHandling {
    #[default]
    DropWithPlaceholder,
    Reject,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageProcessingMode {
    #[default]
    Safe,
    Light,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImageProcessingConfig {
    #[serde(default)]
    pub mode: ImageProcessingMode,

    #[serde(default = "default_true")]
    pub safe_materialize_file_sources: bool,

    #[serde(default = "default_true")]
    pub safe_download_remote_sources: bool,

    #[serde(default = "default_true")]
    pub safe_normalize_base64_media_types: bool,
}

impl Default for ImageProcessingConfig {
    fn default() -> Self {
        Self {
            mode: ImageProcessingMode::Safe,
            safe_materialize_file_sources: true,
            safe_download_remote_sources: true,
            safe_normalize_base64_media_types: true,
        }
    }
}

impl ImageProcessingConfig {
    pub fn normalized(self) -> Self {
        match self.mode {
            ImageProcessingMode::Safe => self,
            ImageProcessingMode::Light => Self {
                mode: ImageProcessingMode::Light,
                safe_materialize_file_sources: false,
                safe_download_remote_sources: false,
                safe_normalize_base64_media_types: false,
            },
        }
    }
}

pub const DEFAULT_TOOL_SCHEMA_KEY_VALIDATION_REGEX: &str = "^[a-zA-Z0-9_.-]{1,64}$";

fn default_tool_schema_key_validation_regex() -> String {
    DEFAULT_TOOL_SCHEMA_KEY_VALIDATION_REGEX.to_string()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolSchemaKeyMappingMode {
    /// 清洗不符合目标正则的 schema property key，并在响应 tool_use.input 中映射回原始 key。
    #[default]
    Sanitize,
    /// 检测到不符合目标正则的 schema property key 时直接本地拒绝，不发给上游。
    Reject,
    /// 完全保持旧行为，不校验、不清洗、不反向映射。
    Disabled,
}

/// 本地 Anthropic -> Kiro body 转换能力开关。
///
/// 这些开关只影响本地凭据路径的 Kiro 协议转换器。外部池 raw body 透传不会进入
/// 这些转换阶段；外部池 normalized body 仍按外部池自己的 body/model/usage 配置处理。
/// 默认全部开启以保持旧行为。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BodyConversionConfig {
    /// 规范化工具 input_schema，移除 Kiro/上游容易拒绝的 OpenAPI/Zod/MCP 扩展字段。
    #[serde(default = "default_true")]
    pub tool_schema_normalization: bool,

    /// 将不符合 Kiro 工具名约束的名称清洗/缩短，并维护响应反向映射。
    #[serde(default = "default_true")]
    pub tool_name_mapping: bool,

    /// 工具 input_schema property key 映射策略。
    ///
    /// `sanitize` 仅清洗不匹配 `tool_schema_key_validation_regex` 的 key，合法 key 原样保留且不建映射；
    /// `reject` 在发现非法 key 时本地 400；`disabled` 保持旧行为。
    #[serde(default)]
    pub tool_schema_key_mapping: ToolSchemaKeyMappingMode,

    /// 工具 input_schema property key 合法性正则。默认来自问题分析文档。
    ///
    /// 只在 `tool_schema_key_mapping` 为 `sanitize` 或 `reject` 时使用。
    #[serde(default = "default_tool_schema_key_validation_regex")]
    pub tool_schema_key_validation_regex: String,

    /// 处理 Anthropic tool_choice：过滤当前工具列表并注入兼容提示。
    #[serde(default = "default_true")]
    pub tool_choice_steering: bool,

    /// 注入 Write/Edit 分块写入策略和工具描述后缀。
    #[serde(default = "default_true")]
    pub chunked_tool_policy: bool,

    /// 对不支持原生 reasoning 的模型注入 synthetic thinking 控制提示。
    #[serde(default = "default_true")]
    pub thinking_prompt_controls: bool,

    /// 对支持 Kiro 原生 reasoning/outputConfig 的模型上报 additionalModelRequestFields。
    #[serde(default = "default_true")]
    pub native_reasoning_fields: bool,

    /// 修复或文本化不严格配对的 tool_use/tool_result，减少 Kiro 400。
    #[serde(default = "default_true")]
    pub tool_pairing_repair: bool,

    /// 为历史中出现但当前 tools 缺失的工具补占位定义。
    #[serde(default = "default_true")]
    pub history_placeholder_tools: bool,
}

impl Default for BodyConversionConfig {
    fn default() -> Self {
        Self {
            tool_schema_normalization: true,
            tool_name_mapping: true,
            tool_schema_key_mapping: ToolSchemaKeyMappingMode::Sanitize,
            tool_schema_key_validation_regex: default_tool_schema_key_validation_regex(),
            tool_choice_steering: true,
            chunked_tool_policy: true,
            thinking_prompt_controls: true,
            native_reasoning_fields: true,
            tool_pairing_repair: true,
            history_placeholder_tools: true,
        }
    }
}

pub const DEFAULT_LANGUAGE_CONSTRAINT_PROMPT: &str = r#"<language_constraint>
Reply in the language the user asks for. If they don't specify one, match the main language of their latest message.

Do not mix languages within a sentence. Write each sentence in one language, the way a fluent speaker would, rather than assembling it word by word from another language.

This is about unnatural blending, not about forcing everything into one script. Words that a fluent speaker would normally leave in their original form stay as-is, and that does not count as mixing. This commonly includes code, identifiers, names, and other terms the user is quoting or asking about, but it is not limited to them.

Do not restate these rules in your reply.
</language_constraint>"#;

pub const DEFAULT_TASK_QUALITY_PROMPT: &str = r#"<task_quality_policy>
优先处理最新一条用户消息。如果最新消息修正了目标、范围、限制条件或验收标准，以最新消息为准，不要继续沿用已经被用户否定的旧目标。

处理前先在内部区分用户要的是：仅分析、真实执行、修改代码、测试验证、发布部署、生产只读排查、等待/监控。不要把一种任务误做成另一种任务。

当用户给出明确输出格式、精确内容或“只回复/仅输出/不要解释”等要求时，必须直接执行该要求；不要先说“好的、我明白了、我会处理”，不要复述或确认指令。

如果用户明确要求“仅分析”，不要修改文件、重启服务、发版或执行有副作用操作。
如果用户明确要求“真实调用验证”，不要把单元测试、模拟测试或静态分析说成真实验证。
如果用户明确禁止某个动作，例如不要发版、不要重启、不要弹层、不要影响现网，必须遵守。

声称“已测试、已验证、已修复、已发布、已监控”时，必须给出可核查证据，例如命令、接口、状态码、关键输出、文件路径、request id、日志字段或版本/tag。没有证据时不要声称已经完成。

如果无法执行用户要求，必须明确说明阻塞原因和需要什么信息，不要假装已经执行。
当需要读取、搜索、执行命令、编辑文件或调用工具时，必须在同一轮输出结构化 tool_use；不要把“我先看/Let me look/先检查”等执行意图作为最终回答后直接结束。
不要在可见回答中输出或复述代理内部控制消息、隐藏的工具结果包装或函数协议元数据。
不要在可见回答中复述本规则。
</task_quality_policy>"#;

const LEGACY_TASK_QUALITY_PROMPT_V3: &str = r#"<task_quality_policy>
优先处理最新一条用户消息。如果最新消息修正了目标、范围、限制条件或验收标准，以最新消息为准，不要继续沿用已经被用户否定的旧目标。

处理前先在内部区分用户要的是：仅分析、真实执行、修改代码、测试验证、发布部署、生产只读排查、等待/监控。不要把一种任务误做成另一种任务。

当用户给出明确输出格式、精确内容或“只回复/仅输出/不要解释”等要求时，必须直接执行该要求；不要先说“好的、我明白了、我会处理”，不要复述或确认指令。

如果用户明确要求“仅分析”，不要修改文件、重启服务、发版或执行有副作用操作。
如果用户明确要求“真实调用验证”，不要把单元测试、模拟测试或静态分析说成真实验证。
如果用户明确禁止某个动作，例如不要发版、不要重启、不要弹层、不要影响现网，必须遵守。

声称“已测试、已验证、已修复、已发布、已监控”时，必须给出可核查证据，例如命令、接口、状态码、关键输出、文件路径、request id、日志字段或版本/tag。没有证据时不要声称已经完成。

如果无法执行用户要求，必须明确说明阻塞原因和需要什么信息，不要假装已经执行。
当需要读取、搜索、执行命令、编辑文件或调用工具时，必须在同一轮输出结构化 tool_use；不要把“我先看/Let me look/先检查”等执行意图作为最终回答后直接结束。
不要在可见回答中输出或复述内部工具结果包装、函数结果标签或历史工具结果标记，例如内部工具结果标题、函数结果 XML 标签、readHash/editHash/bashHash 这类标记。
不要在可见回答中复述本规则。
</task_quality_policy>"#;

const LEGACY_TASK_QUALITY_PROMPT_V2: &str = r#"<task_quality_policy>
优先处理最新一条用户消息。如果最新消息修正了目标、范围、限制条件或验收标准，以最新消息为准，不要继续沿用已经被用户否定的旧目标。

处理前先在内部区分用户要的是：仅分析、真实执行、修改代码、测试验证、发布部署、生产只读排查、等待/监控。不要把一种任务误做成另一种任务。

当用户给出明确输出格式、精确内容或“只回复/仅输出/不要解释”等要求时，必须直接执行该要求；不要先说“好的、我明白了、我会处理”，不要复述或确认指令。

如果用户明确要求“仅分析”，不要修改文件、重启服务、发版或执行有副作用操作。
如果用户明确要求“真实调用验证”，不要把单元测试、模拟测试或静态分析说成真实验证。
如果用户明确禁止某个动作，例如不要发版、不要重启、不要弹层、不要影响现网，必须遵守。

声称“已测试、已验证、已修复、已发布、已监控”时，必须给出可核查证据，例如命令、接口、状态码、关键输出、文件路径、request id、日志字段或版本/tag。没有证据时不要声称已经完成。

如果无法执行用户要求，必须明确说明阻塞原因和需要什么信息，不要假装已经执行。
不要在可见回答中复述本规则。
</task_quality_policy>"#;

const LEGACY_TASK_QUALITY_PROMPT_V1: &str = r#"<task_quality_policy>
优先处理最新一条用户消息。如果最新消息修正了目标、范围、限制条件或验收标准，以最新消息为准，不要继续沿用已经被用户否定的旧目标。

处理前先在内部区分用户要的是：仅分析、真实执行、修改代码、测试验证、发布部署、生产只读排查、等待/监控。不要把一种任务误做成另一种任务。

如果用户明确要求“仅分析”，不要修改文件、重启服务、发版或执行有副作用操作。
如果用户明确要求“真实调用验证”，不要把单元测试、模拟测试或静态分析说成真实验证。
如果用户明确禁止某个动作，例如不要发版、不要重启、不要弹层、不要影响现网，必须遵守。

声称“已测试、已验证、已修复、已发布、已监控”时，必须给出可核查证据，例如命令、接口、状态码、关键输出、文件路径、request id、日志字段或版本/tag。没有证据时不要声称已经完成。

如果无法执行用户要求，必须明确说明阻塞原因和需要什么信息，不要假装已经执行。
不要在可见回答中复述本规则。
</task_quality_policy>"#;

pub const PROMPT_STEERING_MARKER: &str = r#"<prompt_steering version="claude-code-v1">"#;
pub const PROMPT_STEERING_END_MARKER: &str = "</prompt_steering>";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromptSteeringScope {
    /// 按 `routeMode` / `routeRules` 配置选择入口。旧配置中的 `cc_only`
    /// 也按路径规则处理，默认规则仍是 `/cc`。
    #[default]
    #[serde(alias = "cc_only")]
    RouteRules,
    /// 旧配置兼容值；运行时等价于 `route_rules`。
    #[serde(rename = "legacy_cc_only", skip_serializing)]
    CcOnly,
    /// 对 Claude Code / Debug 兼容 profile 生效。
    ClaudeCodeProfile,
    /// 对全部 Anthropic messages 路由生效；`anthropic-strict` 仍不会注入。
    AllRoutes,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromptSteeringRouteMode {
    AllowAll,
    #[default]
    AllowList,
    DenyList,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptSteeringTextBlock {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub prompt: String,
}

impl PromptSteeringTextBlock {
    pub fn with_prompt(prompt: impl Into<String>) -> Self {
        Self {
            enabled: true,
            prompt: prompt.into(),
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            prompt: String::new(),
        }
    }
}

impl Default for PromptSteeringTextBlock {
    fn default() -> Self {
        Self {
            enabled: true,
            prompt: String::new(),
        }
    }
}

fn default_language_constraint_block() -> PromptSteeringTextBlock {
    PromptSteeringTextBlock::with_prompt(DEFAULT_LANGUAGE_CONSTRAINT_PROMPT)
}

fn default_task_quality_block() -> PromptSteeringTextBlock {
    PromptSteeringTextBlock::with_prompt(DEFAULT_TASK_QUALITY_PROMPT)
}

fn default_custom_prompt_block() -> PromptSteeringTextBlock {
    PromptSteeringTextBlock::disabled()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptSteeringToggle {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for PromptSteeringToggle {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChunkedWritePromptSteeringConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub system_prompt_enabled: bool,
    #[serde(default = "default_true")]
    pub tool_description_enabled: bool,
}

impl Default for ChunkedWritePromptSteeringConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            system_prompt_enabled: true,
            tool_description_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptSteeringConfig {
    /// 总提示词引导开关。关闭后不注入 language/task/custom、tool_choice、thinking 或
    /// Write/Edit 分块兼容提示；客户端已经提供的结构化字段仍按原始请求语义保留。
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub scope: PromptSteeringScope,
    #[serde(default)]
    pub route_mode: PromptSteeringRouteMode,
    #[serde(default = "default_prompt_steering_route_rules")]
    pub route_rules: Vec<String>,
    #[serde(default = "default_true")]
    pub apply_to_external_pool: bool,
    #[serde(default = "default_true")]
    pub apply_to_count_tokens: bool,
    #[serde(default = "default_language_constraint_block")]
    pub language_constraint: PromptSteeringTextBlock,
    #[serde(default = "default_task_quality_block")]
    pub task_quality: PromptSteeringTextBlock,
    #[serde(default)]
    pub tool_choice: PromptSteeringToggle,
    #[serde(default)]
    pub chunked_write: ChunkedWritePromptSteeringConfig,
    #[serde(default)]
    pub thinking: PromptSteeringToggle,
    #[serde(default = "default_custom_prompt_block")]
    pub custom: PromptSteeringTextBlock,
}

impl Default for PromptSteeringConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scope: PromptSteeringScope::RouteRules,
            route_mode: PromptSteeringRouteMode::AllowList,
            route_rules: default_prompt_steering_route_rules(),
            apply_to_external_pool: true,
            apply_to_count_tokens: true,
            language_constraint: default_language_constraint_block(),
            task_quality: default_task_quality_block(),
            tool_choice: PromptSteeringToggle::default(),
            chunked_write: ChunkedWritePromptSteeringConfig::default(),
            thinking: PromptSteeringToggle::default(),
            custom: default_custom_prompt_block(),
        }
    }
}

impl PromptSteeringConfig {
    pub fn normalized(mut self) -> Self {
        if self.scope == PromptSteeringScope::CcOnly {
            self.scope = PromptSteeringScope::RouteRules;
        }
        self.route_rules = normalize_route_rules(&self.route_rules);
        if self.route_mode == PromptSteeringRouteMode::AllowList && self.route_rules.is_empty() {
            self.route_rules = default_prompt_steering_route_rules();
        }
        if self.language_constraint.prompt.trim().is_empty() {
            self.language_constraint.prompt = DEFAULT_LANGUAGE_CONSTRAINT_PROMPT.to_string();
        } else {
            self.language_constraint.prompt = self.language_constraint.prompt.trim().to_string();
        }
        if self.task_quality.prompt.trim().is_empty() {
            self.task_quality.prompt = DEFAULT_TASK_QUALITY_PROMPT.to_string();
        } else {
            self.task_quality.prompt = self.task_quality.prompt.trim().to_string();
        }
        self.custom.prompt = self.custom.prompt.trim().to_string();
        self
    }
}

fn default_prompt_steering_route_rules() -> Vec<String> {
    vec!["/cc".to_string()]
}

pub const DEFAULT_MISSING_MAX_TOKENS_VALUE: i32 = 20_480;
pub const MAX_MISSING_MAX_TOKENS_VALUE: i32 = 200_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MissingMaxTokensPolicy {
    Reject,
    #[default]
    DefaultValue,
}

/// 入口 Messages 请求缺少顶层 max_tokens 时的兼容策略。
///
/// Anthropic Messages 请求使用 max_tokens 表示本次输出上限。默认补一个较小正数，
/// 只为兼容缺字段客户端；不使用 0，也不补过大的值，避免改变成本和输出语义。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MissingMaxTokensConfig {
    #[serde(default)]
    pub policy: MissingMaxTokensPolicy,

    #[serde(default = "default_missing_max_tokens_value")]
    pub default_value: i32,
}

impl Default for MissingMaxTokensConfig {
    fn default() -> Self {
        Self {
            policy: MissingMaxTokensPolicy::DefaultValue,
            default_value: default_missing_max_tokens_value(),
        }
    }
}

impl MissingMaxTokensConfig {
    pub fn normalized(self) -> Self {
        let default_value = if (1..=MAX_MISSING_MAX_TOKENS_VALUE).contains(&self.default_value) {
            self.default_value
        } else {
            default_missing_max_tokens_value()
        };
        Self {
            policy: self.policy,
            default_value,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_MISSING_MAX_TOKENS_VALUE).contains(&self.default_value) {
            return Err(format!(
                "missingMaxTokens.defaultValue 必须在 1 到 {} 之间",
                MAX_MISSING_MAX_TOKENS_VALUE
            ));
        }
        Ok(())
    }
}

/// 调度容量加权配置。
///
/// 默认关闭；关闭时请求热路径不会为了容量加权额外估算 token，也不会改变并发/RPM
/// 口径。开启后，调用方在已经完成必要 body 处理时传入一个粗略 token 量级，
/// 调度器按 tiers 映射成容量单位。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WeightedCapacityConfig {
    #[serde(default)]
    pub enabled: bool,

    /// 单请求最多消耗多少容量单位，防止极端上下文导致 Redis 写入和调度计数放大。
    #[serde(default = "default_weighted_capacity_max_units")]
    pub max_units_per_request: u32,

    /// token 阈值到容量单位的映射。按 `minTokens` 升序匹配最后一个命中的 tier。
    #[serde(default = "default_weighted_capacity_tiers")]
    pub tiers: Vec<WeightedCapacityTier>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WeightedCapacityTier {
    #[serde(default)]
    pub min_tokens: u32,
    #[serde(default = "default_weighted_capacity_unit")]
    pub units: u32,
}

impl Default for WeightedCapacityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_units_per_request: default_weighted_capacity_max_units(),
            tiers: default_weighted_capacity_tiers(),
        }
    }
}

impl WeightedCapacityConfig {
    pub fn normalized(&self) -> Self {
        let max_units_per_request = self.max_units_per_request.clamp(1, 64);
        let mut tiers: Vec<_> = self
            .tiers
            .iter()
            .copied()
            .filter(|tier| tier.units > 0)
            .collect();
        if tiers.is_empty() {
            tiers = default_weighted_capacity_tiers();
        }
        tiers.sort_by_key(|tier| tier.min_tokens);
        tiers.dedup_by_key(|tier| tier.min_tokens);
        for tier in &mut tiers {
            tier.units = tier.units.clamp(1, max_units_per_request);
        }
        Self {
            enabled: self.enabled,
            max_units_per_request,
            tiers,
        }
    }

    pub fn units_for_tokens(&self, tokens: u32) -> u32 {
        if !self.enabled {
            return 1;
        }
        let normalized = self.normalized();
        normalized
            .tiers
            .iter()
            .filter(|tier| tokens >= tier.min_tokens)
            .map(|tier| tier.units)
            .last()
            .unwrap_or(1)
            .clamp(1, normalized.max_units_per_request)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_units_per_request == 0 || self.max_units_per_request > 64 {
            return Err("weightedCapacity.maxUnitsPerRequest 必须在 1 到 64 之间".to_string());
        }
        let mut seen = BTreeSet::new();
        for tier in &self.tiers {
            if tier.units == 0 || tier.units > self.max_units_per_request {
                return Err(
                    "weightedCapacity.tiers[].units 必须大于 0 且不超过 maxUnitsPerRequest"
                        .to_string(),
                );
            }
            if !seen.insert(tier.min_tokens) {
                return Err("weightedCapacity.tiers[].minTokens 不能重复".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PayloadShapingConfig {
    #[serde(default = "default_payload_shaping_enabled")]
    pub enabled: bool,

    #[serde(default = "default_true")]
    pub truncate_historical_tool_results: bool,

    #[serde(default = "default_historical_tool_result_max_chars")]
    pub historical_tool_result_max_chars: usize,

    #[serde(default = "default_historical_tool_result_head_lines")]
    pub historical_tool_result_head_lines: usize,

    #[serde(default = "default_historical_tool_result_tail_lines")]
    pub historical_tool_result_tail_lines: usize,

    #[serde(default = "default_true")]
    pub discard_historical_thinking: bool,

    #[serde(default = "default_true")]
    pub compress_tool_definitions: bool,

    #[serde(default = "default_tool_definitions_budget_bytes")]
    pub tool_definitions_budget_bytes: usize,

    #[serde(default = "default_tool_description_max_chars")]
    pub tool_description_max_chars: usize,

    #[serde(default = "default_tool_schema_annotation_max_chars")]
    pub tool_schema_annotation_max_chars: usize,

    #[serde(default = "default_true")]
    pub web_fetch_trim_enabled: bool,

    #[serde(default = "default_web_fetch_body_max_chars")]
    pub web_fetch_body_max_chars: usize,

    #[serde(default)]
    pub fit_current_payload_to_budget: bool,

    #[serde(default)]
    pub truncate_current_tool_results: bool,

    #[serde(default = "default_current_tool_result_max_chars")]
    pub current_tool_result_max_chars: usize,

    #[serde(default)]
    pub truncate_current_user_content: bool,

    #[serde(default = "default_current_user_content_max_chars")]
    pub current_user_content_max_chars: usize,

    #[serde(default)]
    pub truncate_current_documents: bool,

    #[serde(default = "default_current_document_max_chars")]
    pub current_document_max_chars: usize,

    #[serde(default)]
    pub truncate_current_images: bool,

    #[serde(default = "default_current_images_max_bytes")]
    pub current_images_max_bytes: usize,

    #[serde(default)]
    pub oversized_image_handling: OversizedImageHandling,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolFormatDebugConfig {
    /// 是否在 Kiro 上游返回 tool-use 格式错误时记录内部诊断。
    #[serde(default = "default_tool_format_debug_enabled")]
    pub enabled: bool,
    /// JSONL 诊断文件目录。仅用于内部排查，不返回给下游。
    #[serde(default = "default_tool_format_debug_dir")]
    pub dir: String,
    /// 非阻塞写盘队列容量。队列满时丢弃诊断，不阻塞主请求。
    #[serde(default = "default_tool_format_debug_channel_capacity")]
    pub channel_capacity: usize,
    /// 单条 JSONL 最大字节数，超过后会丢弃 samples 保留聚合指标。
    #[serde(default = "default_tool_format_debug_max_record_bytes")]
    pub max_record_bytes: usize,
    /// 每类异常最多记录多少个样本。
    #[serde(default = "default_tool_format_debug_max_samples_per_kind")]
    pub max_samples_per_kind: usize,
    /// 同类记录限流窗口秒数。
    #[serde(default = "default_tool_format_debug_window_secs")]
    pub window_secs: u64,
    /// 每个精确 fingerprint 在窗口内最多写几条完整诊断。
    #[serde(default = "default_tool_format_debug_max_records_per_fingerprint")]
    pub max_records_per_fingerprint: u32,
    /// 每个结构 group 在窗口内最多写几条完整诊断。
    #[serde(default = "default_tool_format_debug_max_records_per_group")]
    pub max_records_per_group: u32,
    /// 全局窗口内最多写几条完整诊断。
    #[serde(default = "default_tool_format_debug_max_records_global")]
    pub max_records_global: u32,
    /// 诊断字段中的字符串最大字节数。
    #[serde(default = "default_tool_format_debug_max_string_bytes")]
    pub max_string_bytes: usize,
    /// 是否在采样命中的 tool-use 格式错误诊断中记录实际发送失败的 Kiro 请求体。
    #[serde(default = "default_tool_format_debug_capture_request_body")]
    pub capture_request_body: bool,
    /// 单条诊断中最多保留多少字节的请求体内容。
    #[serde(default = "default_tool_format_debug_max_request_body_bytes")]
    pub max_request_body_bytes: usize,
    /// 同一限流窗口内最多有多少条诊断可以包含请求体内容。
    #[serde(default = "default_tool_format_debug_max_request_body_records_per_window")]
    pub max_request_body_records_per_window: u32,
    /// 诊断文件按时间滚动的间隔秒数。
    #[serde(default = "default_tool_format_debug_roll_interval_secs")]
    pub roll_interval_secs: u64,
    /// 单个诊断文件最大字节数，超过后在同一时间分片内递增序号滚动。
    #[serde(default = "default_tool_format_debug_max_file_bytes")]
    pub max_file_bytes: u64,
}

impl Default for ToolFormatDebugConfig {
    fn default() -> Self {
        Self {
            enabled: default_tool_format_debug_enabled(),
            dir: default_tool_format_debug_dir(),
            channel_capacity: default_tool_format_debug_channel_capacity(),
            max_record_bytes: default_tool_format_debug_max_record_bytes(),
            max_samples_per_kind: default_tool_format_debug_max_samples_per_kind(),
            window_secs: default_tool_format_debug_window_secs(),
            max_records_per_fingerprint: default_tool_format_debug_max_records_per_fingerprint(),
            max_records_per_group: default_tool_format_debug_max_records_per_group(),
            max_records_global: default_tool_format_debug_max_records_global(),
            max_string_bytes: default_tool_format_debug_max_string_bytes(),
            capture_request_body: default_tool_format_debug_capture_request_body(),
            max_request_body_bytes: default_tool_format_debug_max_request_body_bytes(),
            max_request_body_records_per_window:
                default_tool_format_debug_max_request_body_records_per_window(),
            roll_interval_secs: default_tool_format_debug_roll_interval_secs(),
            max_file_bytes: default_tool_format_debug_max_file_bytes(),
        }
    }
}

impl ToolFormatDebugConfig {
    pub fn normalized(&self) -> Self {
        let max_record_bytes = self.max_record_bytes.clamp(1024, 1024 * 1024);
        let max_buffered_records_by_bytes = ((64 * 1024 * 1024) / max_record_bytes).max(1);
        let channel_capacity = self
            .channel_capacity
            .clamp(1, 1024)
            .min(max_buffered_records_by_bytes);

        Self {
            enabled: self.enabled,
            dir: if self.dir.trim().is_empty() {
                default_tool_format_debug_dir()
            } else {
                self.dir.trim().to_string()
            },
            channel_capacity,
            max_record_bytes,
            max_samples_per_kind: self.max_samples_per_kind.min(100),
            window_secs: self.window_secs.clamp(1, 86_400),
            max_records_per_fingerprint: self.max_records_per_fingerprint.min(1_000),
            max_records_per_group: self.max_records_per_group.min(10_000),
            max_records_global: self.max_records_global.min(10_000),
            max_string_bytes: self.max_string_bytes.clamp(32, 4096),
            capture_request_body: self.capture_request_body,
            max_request_body_bytes: normalize_tool_format_debug_request_body_bytes(
                self.max_request_body_bytes,
            ),
            max_request_body_records_per_window: self.max_request_body_records_per_window.min(100),
            roll_interval_secs: self.roll_interval_secs.clamp(60, 86_400),
            max_file_bytes: normalize_tool_format_debug_max_file_bytes(self.max_file_bytes),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PayloadShapingConfigPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub truncate_historical_tool_results: Option<bool>,
    #[serde(default)]
    pub historical_tool_result_max_chars: Option<usize>,
    #[serde(default)]
    pub historical_tool_result_head_lines: Option<usize>,
    #[serde(default)]
    pub historical_tool_result_tail_lines: Option<usize>,
    #[serde(default)]
    pub discard_historical_thinking: Option<bool>,
    #[serde(default)]
    pub compress_tool_definitions: Option<bool>,
    #[serde(default)]
    pub tool_definitions_budget_bytes: Option<usize>,
    #[serde(default)]
    pub tool_description_max_chars: Option<usize>,
    #[serde(default)]
    pub tool_schema_annotation_max_chars: Option<usize>,
    #[serde(default)]
    pub web_fetch_trim_enabled: Option<bool>,
    #[serde(default)]
    pub web_fetch_body_max_chars: Option<usize>,
    #[serde(default)]
    pub fit_current_payload_to_budget: Option<bool>,
    #[serde(default)]
    pub truncate_current_tool_results: Option<bool>,
    #[serde(default)]
    pub current_tool_result_max_chars: Option<usize>,
    #[serde(default)]
    pub truncate_current_user_content: Option<bool>,
    #[serde(default)]
    pub current_user_content_max_chars: Option<usize>,
    #[serde(default)]
    pub truncate_current_documents: Option<bool>,
    #[serde(default)]
    pub current_document_max_chars: Option<usize>,
    #[serde(default)]
    pub truncate_current_images: Option<bool>,
    #[serde(default)]
    pub current_images_max_bytes: Option<usize>,
    #[serde(default)]
    pub oversized_image_handling: Option<OversizedImageHandling>,
}

impl PayloadShapingConfigPatch {
    pub fn apply_to(self, mut config: PayloadShapingConfig) -> PayloadShapingConfig {
        if let Some(value) = self.enabled {
            config.enabled = value;
        }
        if let Some(value) = self.truncate_historical_tool_results {
            config.truncate_historical_tool_results = value;
        }
        if let Some(value) = self.historical_tool_result_max_chars {
            config.historical_tool_result_max_chars = value;
        }
        if let Some(value) = self.historical_tool_result_head_lines {
            config.historical_tool_result_head_lines = value;
        }
        if let Some(value) = self.historical_tool_result_tail_lines {
            config.historical_tool_result_tail_lines = value;
        }
        if let Some(value) = self.discard_historical_thinking {
            config.discard_historical_thinking = value;
        }
        if let Some(value) = self.compress_tool_definitions {
            config.compress_tool_definitions = value;
        }
        if let Some(value) = self.tool_definitions_budget_bytes {
            config.tool_definitions_budget_bytes = value;
        }
        if let Some(value) = self.tool_description_max_chars {
            config.tool_description_max_chars = value;
        }
        if let Some(value) = self.tool_schema_annotation_max_chars {
            config.tool_schema_annotation_max_chars = value;
        }
        if let Some(value) = self.web_fetch_trim_enabled {
            config.web_fetch_trim_enabled = value;
        }
        if let Some(value) = self.web_fetch_body_max_chars {
            config.web_fetch_body_max_chars = value;
        }
        if let Some(value) = self.fit_current_payload_to_budget {
            config.fit_current_payload_to_budget = value;
        }
        if let Some(value) = self.truncate_current_tool_results {
            config.truncate_current_tool_results = value;
        }
        if let Some(value) = self.current_tool_result_max_chars {
            config.current_tool_result_max_chars = value;
        }
        if let Some(value) = self.truncate_current_user_content {
            config.truncate_current_user_content = value;
        }
        if let Some(value) = self.current_user_content_max_chars {
            config.current_user_content_max_chars = value;
        }
        if let Some(value) = self.truncate_current_documents {
            config.truncate_current_documents = value;
        }
        if let Some(value) = self.current_document_max_chars {
            config.current_document_max_chars = value;
        }
        if let Some(value) = self.truncate_current_images {
            config.truncate_current_images = value;
        }
        if let Some(value) = self.current_images_max_bytes {
            config.current_images_max_bytes = value;
        }
        if let Some(value) = self.oversized_image_handling {
            config.oversized_image_handling = value;
        }
        config
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReportedUsageFieldMode {
    Raw,
    Preserve,
    SampleMax,
    SampleTarget,
}

impl Default for ReportedUsageFieldMode {
    fn default() -> Self {
        Self::Preserve
    }
}

/// 单个 usage 字段的下游上报策略。
///
/// 这些策略只用于把内部计算出的 usage 整理成下游响应和后台记录看到的 usage；
/// 不参与 prompt-cache tracker、reader 命中或上游请求计算。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReportedUsageFieldPolicy {
    #[serde(default)]
    pub mode: ReportedUsageFieldMode,

    /// `sample-max` 的上限。
    #[serde(default)]
    pub max_tokens: i32,

    /// `sample-target` 的中心目标值。
    #[serde(default)]
    pub target_tokens: i32,

    /// `sample-target` 的常规最大倍率。writer 默认 1.1，即约 110%。
    #[serde(default = "default_reported_usage_normal_max_multiplier")]
    pub normal_max_multiplier: f64,

    /// input 被压低后的差值是否转入缓存口径。
    ///
    /// 有 cache-read 证据时转入 cache_read_input_tokens；没有 read 证据时转入
    /// cache_creation_input_tokens，避免首轮/无 read 请求把真实输入差额直接丢掉。
    #[serde(default)]
    pub move_delta_to_cache_read: bool,
}

impl Default for ReportedUsageFieldPolicy {
    fn default() -> Self {
        Self {
            mode: ReportedUsageFieldMode::Preserve,
            max_tokens: 0,
            target_tokens: 0,
            normal_max_multiplier: default_reported_usage_normal_max_multiplier(),
            move_delta_to_cache_read: false,
        }
    }
}

impl ReportedUsageFieldPolicy {
    pub fn preserve() -> Self {
        Self::default()
    }

    pub fn raw() -> Self {
        Self {
            mode: ReportedUsageFieldMode::Raw,
            ..Self::default()
        }
    }

    pub fn sample_max(max_tokens: i32) -> Self {
        Self {
            mode: ReportedUsageFieldMode::SampleMax,
            max_tokens,
            ..Self::default()
        }
    }

    pub fn sample_input_max(max_tokens: i32) -> Self {
        Self {
            move_delta_to_cache_read: true,
            ..Self::sample_max(max_tokens)
        }
    }

    pub fn sample_target(target_tokens: i32) -> Self {
        Self {
            mode: ReportedUsageFieldMode::SampleTarget,
            target_tokens,
            ..Self::default()
        }
    }

    pub fn sample_target_with_multiplier(target_tokens: i32, normal_max_multiplier: f64) -> Self {
        Self {
            normal_max_multiplier,
            ..Self::sample_target(target_tokens)
        }
    }

    pub fn normalized(&self) -> Self {
        let mut normalized = self.clone();
        normalized.max_tokens = normalized.max_tokens.max(0);
        normalized.target_tokens = normalized.target_tokens.max(0);
        normalized.normal_max_multiplier =
            normalize_reported_usage_normal_max_multiplier(normalized.normal_max_multiplier);
        normalized
    }

    fn validate(&self, label: &str) -> Result<(), String> {
        if self.max_tokens < 0 {
            return Err(format!("{} 的 sample-max 上限不能小于 0", label));
        }
        if self.target_tokens < 0 {
            return Err(format!("{} 的 sample-target 目标值不能小于 0", label));
        }
        if !self.normal_max_multiplier.is_finite() || self.normal_max_multiplier < 1.0 {
            return Err(format!(
                "{} 的 sample-target 最大倍率必须大于或等于 1",
                label
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReportedUsagePathPolicy {
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 命中该路径的非流式请求是否透传上游 usage。
    ///
    /// 该字段保留旧的 usage projection 命名以兼容现有配置。开启后，非流式请求不进入
    /// 本系统 usage 整形、input 采样和补偿，也不会读取或写入本地 prompt-cache 状态；
    /// 外部池非流式响应会保持上游 usage 原样返回，本地凭证会使用上游 metadata 原始 usage。
    /// 流式请求不受影响。
    /// 全局策略模板里开启会影响继承该模板的路径，路径级配置可单独覆盖。
    #[serde(default)]
    pub skip_non_stream_usage_projection: bool,

    /// 最终下游上报的 cache_read_input_tokens 上限。
    ///
    /// 该限制在 input 差值转入 cache read 之后执行，只向下裁剪，不会抬高小值。
    /// 0 表示关闭最终守护。
    #[serde(default = "default_final_cache_read_max_tokens")]
    pub final_cache_read_max_tokens: i32,

    /// 读取缓存最终上限的确定性扣减下限。
    #[serde(default)]
    pub final_cache_read_jitter_min_tokens: i32,

    /// 读取缓存最终上限的确定性扣减上限。
    #[serde(default)]
    pub final_cache_read_jitter_max_tokens: i32,

    /// 最终下游上报的 cache_creation_input_tokens 上限。
    ///
    /// 该限制在 input 差值转入 cache creation 之后执行，只向下裁剪，不会抬高小值。
    /// 0 表示关闭最终守护。
    #[serde(default = "default_final_cache_creation_max_tokens")]
    pub final_cache_creation_max_tokens: i32,

    /// 写入缓存最终上限的确定性扣减下限。
    #[serde(default = "default_reported_usage_final_cache_creation_jitter_min_tokens")]
    pub final_cache_creation_jitter_min_tokens: i32,

    /// 写入缓存最终上限的确定性扣减上限。
    #[serde(default = "default_reported_usage_final_cache_creation_jitter_max_tokens")]
    pub final_cache_creation_jitter_max_tokens: i32,

    /// 是否启用 output 字段最终限制逻辑。
    ///
    /// 关闭后，只执行 output 字段自身的 raw/preserve/sample-* 改写，不再执行放大或最终上限裁剪。
    #[serde(default = "default_true")]
    pub final_output_guard_enabled: bool,

    /// output 字段完成 raw/preserve/sample-* 改写后的放大阈值。
    ///
    /// 0 表示关闭。大于该阈值时才按 output_uplift_percent 放大。
    #[serde(default = "default_reported_usage_output_uplift_min_tokens")]
    pub output_uplift_min_tokens: i32,

    /// output 字段完成 raw/preserve/sample-* 改写后的放大百分比。
    ///
    /// 0 表示关闭，最大按 200% 归一化。
    #[serde(default = "default_reported_usage_output_uplift_percent")]
    pub output_uplift_percent: u32,

    /// output 字段最终上限。
    ///
    /// 0 表示关闭。生效时会先扣减 final_output_jitter_* 得到有效上限，再向下裁剪。
    #[serde(default = "default_reported_usage_final_output_max_tokens")]
    pub final_output_max_tokens: i32,

    /// output 最终上限的确定性扣减下限。
    #[serde(default = "default_reported_usage_final_output_jitter_min_tokens")]
    pub final_output_jitter_min_tokens: i32,

    /// output 最终上限的确定性扣减上限。
    #[serde(default = "default_reported_usage_final_output_jitter_max_tokens")]
    pub final_output_jitter_max_tokens: i32,

    #[serde(default)]
    pub input: ReportedUsageFieldPolicy,

    #[serde(default)]
    pub output: ReportedUsageFieldPolicy,

    #[serde(default)]
    pub cache_read: ReportedUsageFieldPolicy,

    #[serde(default)]
    pub cache_creation: ReportedUsageFieldPolicy,
}

impl Default for ReportedUsagePathPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            skip_non_stream_usage_projection: false,
            final_cache_read_max_tokens: default_final_cache_read_max_tokens(),
            final_cache_read_jitter_min_tokens: 0,
            final_cache_read_jitter_max_tokens: 0,
            final_cache_creation_max_tokens: default_final_cache_creation_max_tokens(),
            final_cache_creation_jitter_min_tokens:
                default_reported_usage_final_cache_creation_jitter_min_tokens(),
            final_cache_creation_jitter_max_tokens:
                default_reported_usage_final_cache_creation_jitter_max_tokens(),
            final_output_guard_enabled: true,
            output_uplift_min_tokens: default_reported_usage_output_uplift_min_tokens(),
            output_uplift_percent: default_reported_usage_output_uplift_percent(),
            final_output_max_tokens: default_reported_usage_final_output_max_tokens(),
            final_output_jitter_min_tokens: default_reported_usage_final_output_jitter_min_tokens(),
            final_output_jitter_max_tokens: default_reported_usage_final_output_jitter_max_tokens(),
            input: ReportedUsageFieldPolicy::raw(),
            output: ReportedUsageFieldPolicy::raw(),
            cache_read: ReportedUsageFieldPolicy::preserve(),
            cache_creation: ReportedUsageFieldPolicy::preserve(),
        }
    }
}

impl ReportedUsagePathPolicy {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    pub fn normalized(&self) -> Self {
        let final_cache_read_max_tokens = self.final_cache_read_max_tokens.max(0);
        let mut final_cache_read_jitter_min_tokens = self
            .final_cache_read_jitter_min_tokens
            .max(0)
            .min(final_cache_read_max_tokens);
        let final_cache_read_jitter_max_tokens = self
            .final_cache_read_jitter_max_tokens
            .max(0)
            .min(final_cache_read_max_tokens);
        if final_cache_read_jitter_min_tokens > final_cache_read_jitter_max_tokens {
            final_cache_read_jitter_min_tokens = final_cache_read_jitter_max_tokens;
        }
        let final_cache_creation_max_tokens = self.final_cache_creation_max_tokens.max(0);
        let mut final_cache_creation_jitter_min_tokens = self
            .final_cache_creation_jitter_min_tokens
            .max(0)
            .min(final_cache_creation_max_tokens);
        let final_cache_creation_jitter_max_tokens = self
            .final_cache_creation_jitter_max_tokens
            .max(0)
            .min(final_cache_creation_max_tokens);
        if final_cache_creation_jitter_min_tokens > final_cache_creation_jitter_max_tokens {
            final_cache_creation_jitter_min_tokens = final_cache_creation_jitter_max_tokens;
        }
        let final_output_max_tokens = self.final_output_max_tokens.max(0);
        let mut final_output_jitter_min_tokens = self
            .final_output_jitter_min_tokens
            .max(0)
            .min(final_output_max_tokens);
        let final_output_jitter_max_tokens = self
            .final_output_jitter_max_tokens
            .max(0)
            .min(final_output_max_tokens);
        if final_output_jitter_min_tokens > final_output_jitter_max_tokens {
            final_output_jitter_min_tokens = final_output_jitter_max_tokens;
        }

        Self {
            enabled: self.enabled,
            skip_non_stream_usage_projection: self.skip_non_stream_usage_projection,
            final_cache_read_max_tokens,
            final_cache_read_jitter_min_tokens,
            final_cache_read_jitter_max_tokens,
            final_cache_creation_max_tokens,
            final_cache_creation_jitter_min_tokens,
            final_cache_creation_jitter_max_tokens,
            final_output_guard_enabled: self.final_output_guard_enabled,
            output_uplift_min_tokens: self.output_uplift_min_tokens.max(0),
            output_uplift_percent: self.output_uplift_percent.min(200),
            final_output_max_tokens,
            final_output_jitter_min_tokens,
            final_output_jitter_max_tokens,
            input: self.input.normalized(),
            output: self.output.normalized(),
            cache_read: self.cache_read.normalized(),
            cache_creation: self.cache_creation.normalized(),
        }
    }

    fn validate(&self, label: &str) -> Result<(), String> {
        self.input.validate(&format!("{} input", label))?;
        self.output.validate(&format!("{} output", label))?;
        self.cache_read.validate(&format!("{} cacheRead", label))?;
        self.cache_creation
            .validate(&format!("{} cacheCreation", label))?;
        if self.final_cache_read_max_tokens < 0 {
            return Err(format!("{} finalCacheReadMaxTokens 不能小于 0", label));
        }
        if self.final_cache_read_jitter_min_tokens < 0
            || self.final_cache_read_jitter_max_tokens < 0
        {
            return Err(format!(
                "{} finalCacheReadJitterMinTokens 和 finalCacheReadJitterMaxTokens 不能小于 0",
                label
            ));
        }
        if self.final_cache_read_jitter_min_tokens > self.final_cache_read_jitter_max_tokens {
            return Err(format!(
                "{} finalCacheReadJitterMinTokens 不能大于 finalCacheReadJitterMaxTokens",
                label
            ));
        }
        if self.final_cache_read_max_tokens > 0
            && self.final_cache_read_jitter_max_tokens > self.final_cache_read_max_tokens
        {
            return Err(format!(
                "{} finalCacheReadJitterMaxTokens 不能大于 finalCacheReadMaxTokens",
                label
            ));
        }
        if self.final_cache_creation_max_tokens < 0 {
            return Err(format!("{} finalCacheCreationMaxTokens 不能小于 0", label));
        }
        if self.final_cache_creation_jitter_min_tokens < 0
            || self.final_cache_creation_jitter_max_tokens < 0
        {
            return Err(format!(
                "{} finalCacheCreationJitterMinTokens 和 finalCacheCreationJitterMaxTokens 不能小于 0",
                label
            ));
        }
        if self.final_cache_creation_jitter_min_tokens > self.final_cache_creation_jitter_max_tokens
        {
            return Err(format!(
                "{} finalCacheCreationJitterMinTokens 不能大于 finalCacheCreationJitterMaxTokens",
                label
            ));
        }
        if self.final_cache_creation_max_tokens > 0
            && self.final_cache_creation_jitter_max_tokens > self.final_cache_creation_max_tokens
        {
            return Err(format!(
                "{} finalCacheCreationJitterMaxTokens 不能大于 finalCacheCreationMaxTokens",
                label
            ));
        }
        if self.output_uplift_min_tokens < 0 {
            return Err(format!("{} outputUpliftMinTokens 不能小于 0", label));
        }
        if self.final_output_max_tokens < 0 {
            return Err(format!("{} finalOutputMaxTokens 不能小于 0", label));
        }
        if self.final_output_jitter_min_tokens < 0 || self.final_output_jitter_max_tokens < 0 {
            return Err(format!(
                "{} finalOutputJitterMinTokens 和 finalOutputJitterMaxTokens 不能小于 0",
                label
            ));
        }
        if self.final_output_jitter_min_tokens > self.final_output_jitter_max_tokens {
            return Err(format!(
                "{} finalOutputJitterMinTokens 不能大于 finalOutputJitterMaxTokens",
                label
            ));
        }
        if self.final_output_max_tokens > 0
            && self.final_output_jitter_max_tokens > self.final_output_max_tokens
        {
            return Err(format!(
                "{} finalOutputJitterMaxTokens 不能大于 finalOutputMaxTokens",
                label
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReportedUsageConfig {
    #[serde(default)]
    pub default: ReportedUsagePathPolicy,

    #[serde(default)]
    pub path_overrides: BTreeMap<String, ReportedUsagePathPolicy>,
}

impl Default for ReportedUsageConfig {
    fn default() -> Self {
        let mut path_overrides = BTreeMap::new();
        path_overrides.insert(
            "/cc".to_string(),
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                cache_creation: ReportedUsageFieldPolicy::sample_target_with_multiplier(3_000, 1.2),
                ..ReportedUsagePathPolicy::default()
            },
        );
        path_overrides.insert(
            "/ha".to_string(),
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
        );

        Self {
            default: ReportedUsagePathPolicy::default(),
            path_overrides,
        }
    }
}

impl ReportedUsageConfig {
    pub fn normalized(&self) -> Self {
        let path_overrides = self
            .path_overrides
            .iter()
            .filter_map(|(prefix, policy)| {
                normalize_reported_usage_path_prefix(prefix)
                    .map(|prefix| (prefix, policy.normalized()))
            })
            .collect();

        Self {
            default: self.default.normalized(),
            path_overrides,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.default.validate("默认上报策略")?;
        for (prefix, policy) in &self.path_overrides {
            let Some(normalized_prefix) = normalize_reported_usage_path_prefix(prefix) else {
                return Err("路径覆盖前缀不能为空".to_string());
            };
            policy.validate(&format!("路径 {} 上报策略", normalized_prefix))?;
        }
        Ok(())
    }

    pub fn policy_for_path(&self, path: &str) -> ReportedUsagePathPolicy {
        let normalized = self.normalized();
        normalized
            .path_overrides
            .iter()
            .filter(|(prefix, _)| reported_usage_path_matches(prefix, path))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, policy)| policy.clone())
            .unwrap_or(normalized.default)
    }

    pub fn path_override_for_path(&self, path: &str) -> Option<ReportedUsagePathPolicy> {
        let normalized = self.normalized();
        normalized
            .path_overrides
            .iter()
            .filter(|(prefix, _)| reported_usage_path_matches(prefix, path))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, policy)| policy.clone())
    }
}

/// 本地 prompt-cache creation 上报频次控制的状态维度。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheCreationControlScopeMode {
    /// 同一凭据、会话、模型独立控制，最贴近真实账号缓存隔离。
    CredentialConversationModel,
    /// 同一会话、模型共享控制，默认值；跨凭据调度时也会降低重复 creation 上报频次。
    ConversationModel,
}

impl Default for PromptCacheCreationControlScopeMode {
    fn default() -> Self {
        Self::ConversationModel
    }
}

/// 本地 prompt-cache creation 上报频次控制。
///
/// 该配置不改变 `PromptCacheTracker` 的缓存命中/创建计算，只在最终 usage
/// 上报前限制 `cache_creation_input_tokens` 出现频次。被抑制的 creation
/// 默认回到 `input_tokens`，避免只因为隐藏 creation 就降低总输入口径。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptCacheCreationControlConfig {
    /// 总开关。默认开启，用来抑制连续会话里过于频繁的本地模拟 cache creation 上报。
    #[serde(default)]
    pub enabled: bool,

    /// 状态维度。默认按会话/模型跨凭据共享，也可按凭据/会话/模型隔离。
    #[serde(default)]
    pub scope_mode: PromptCacheCreationControlScopeMode,

    /// 同一控制维度下，两次 creation 之间至少间隔多少次成功请求。
    #[serde(default = "default_prompt_cache_creation_min_successes_between")]
    pub min_successful_requests_between_creation: u32,

    /// 同一控制维度下，两次 creation 之间至少间隔多少秒。0 表示不限制。
    #[serde(default = "default_prompt_cache_creation_min_interval_secs")]
    pub min_creation_interval_secs: u64,

    /// 被抑制的 creation 累计到多少 tokens 后才允许下一次 creation。0 表示不限制。
    #[serde(default = "default_prompt_cache_creation_min_delta_tokens")]
    pub min_creation_delta_tokens: i32,

    /// 单次最多允许上报多少 creation tokens。0 表示不限制。
    #[serde(default = "default_prompt_cache_creation_max_tokens_per_event")]
    pub max_creation_tokens_per_event: i32,

    /// creation 额度窗口长度。0 表示关闭窗口额度控制。
    #[serde(default = "default_prompt_cache_creation_budget_window_secs")]
    pub creation_budget_window_secs: u64,

    /// 单个额度窗口内最多允许上报多少 creation tokens。0 表示不限制。
    #[serde(default = "default_prompt_cache_creation_max_tokens_per_window")]
    pub max_creation_tokens_per_window: i32,

    /// 控制器状态空闲多久后过期。0 表示不按空闲时间清理。
    #[serde(default = "default_prompt_cache_creation_expire_after_idle_secs")]
    pub expire_after_idle_secs: u64,
}

impl Default for PromptCacheCreationControlConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scope_mode: PromptCacheCreationControlScopeMode::default(),
            min_successful_requests_between_creation:
                default_prompt_cache_creation_min_successes_between(),
            min_creation_interval_secs: default_prompt_cache_creation_min_interval_secs(),
            min_creation_delta_tokens: default_prompt_cache_creation_min_delta_tokens(),
            max_creation_tokens_per_event: default_prompt_cache_creation_max_tokens_per_event(),
            creation_budget_window_secs: default_prompt_cache_creation_budget_window_secs(),
            max_creation_tokens_per_window: default_prompt_cache_creation_max_tokens_per_window(),
            expire_after_idle_secs: default_prompt_cache_creation_expire_after_idle_secs(),
        }
    }
}

impl PromptCacheCreationControlConfig {
    pub fn normalized(self) -> Self {
        Self {
            enabled: self.enabled,
            scope_mode: self.scope_mode,
            min_successful_requests_between_creation: self.min_successful_requests_between_creation,
            min_creation_interval_secs: self.min_creation_interval_secs,
            min_creation_delta_tokens: self.min_creation_delta_tokens.max(0),
            max_creation_tokens_per_event: self.max_creation_tokens_per_event.max(0),
            creation_budget_window_secs: self.creation_budget_window_secs,
            max_creation_tokens_per_window: self.max_creation_tokens_per_window.max(0),
            expire_after_idle_secs: self.expire_after_idle_secs,
        }
    }

    pub fn validate(self) -> Result<(), String> {
        self.validate_with_label("promptCacheCreationControl")
    }

    fn validate_with_label(self, label: &str) -> Result<(), String> {
        if self.min_creation_delta_tokens < 0 {
            return Err(format!("{label}.minCreationDeltaTokens 不能小于 0"));
        }
        if self.max_creation_tokens_per_event < 0 {
            return Err(format!("{label}.maxCreationTokensPerEvent 不能小于 0"));
        }
        if self.max_creation_tokens_per_window < 0 {
            return Err(format!("{label}.maxCreationTokensPerWindow 不能小于 0"));
        }

        let config = self.normalized();
        if config.min_successful_requests_between_creation > 10_000 {
            return Err(format!(
                "{label}.minSuccessfulRequestsBetweenCreation 不能大于 10000"
            ));
        }
        if config.min_creation_interval_secs > 7 * 24 * 60 * 60 {
            return Err(format!("{label}.minCreationIntervalSecs 不能大于 604800"));
        }
        if config.creation_budget_window_secs > 7 * 24 * 60 * 60 {
            return Err(format!("{label}.creationBudgetWindowSecs 不能大于 604800"));
        }
        if config.expire_after_idle_secs > 30 * 24 * 60 * 60 {
            return Err(format!("{label}.expireAfterIdleSecs 不能大于 2592000"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheSimulationPolicy {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_prompt_cache_target_read_ratio")]
    pub target_read_ratio: f64,
    #[serde(default = "default_prompt_cache_token_scale")]
    pub token_scale: f64,
    #[serde(default = "default_prompt_cache_max_simulated_input_tokens")]
    pub max_simulated_input_tokens: i32,
    #[serde(default = "default_prompt_cache_cap_jitter_min_tokens")]
    pub cap_jitter_min_tokens: i32,
    #[serde(default = "default_prompt_cache_cap_jitter_max_tokens")]
    pub cap_jitter_max_tokens: i32,
    #[serde(default = "default_prompt_cache_scale_min_input_tokens")]
    pub scale_min_input_tokens: i32,
}

impl Default for CacheSimulationPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            target_read_ratio: default_prompt_cache_target_read_ratio(),
            token_scale: default_prompt_cache_token_scale(),
            max_simulated_input_tokens: default_prompt_cache_max_simulated_input_tokens(),
            cap_jitter_min_tokens: default_prompt_cache_cap_jitter_min_tokens(),
            cap_jitter_max_tokens: default_prompt_cache_cap_jitter_max_tokens(),
            scale_min_input_tokens: default_prompt_cache_scale_min_input_tokens(),
        }
    }
}

impl CacheSimulationPolicy {
    pub fn normalized(self) -> Self {
        Self {
            enabled: self.enabled,
            target_read_ratio: if self.target_read_ratio.is_finite() {
                self.target_read_ratio.clamp(0.0, 0.99)
            } else {
                default_prompt_cache_target_read_ratio()
            },
            token_scale: if self.token_scale.is_finite() {
                self.token_scale.clamp(1.0, 3.0)
            } else {
                default_prompt_cache_token_scale()
            },
            max_simulated_input_tokens: self.max_simulated_input_tokens.max(0),
            cap_jitter_min_tokens: self.cap_jitter_min_tokens.max(0),
            cap_jitter_max_tokens: self.cap_jitter_max_tokens.max(0),
            scale_min_input_tokens: self.scale_min_input_tokens.max(0),
        }
    }

    pub fn validate(self, label: &str) -> Result<(), String> {
        if !(0.0..=0.99).contains(&self.target_read_ratio) || !self.target_read_ratio.is_finite() {
            return Err(format!("{label}.targetReadRatio 必须在 0 到 0.99 之间"));
        }
        if !(1.0..=3.0).contains(&self.token_scale) || !self.token_scale.is_finite() {
            return Err(format!("{label}.tokenScale 必须在 1 到 3 之间"));
        }
        if self.max_simulated_input_tokens < 0 {
            return Err(format!("{label}.maxSimulatedInputTokens 不能小于 0"));
        }
        if self.cap_jitter_min_tokens < 0 || self.cap_jitter_max_tokens < 0 {
            return Err(format!(
                "{label}.capJitterMinTokens 和 capJitterMaxTokens 不能小于 0"
            ));
        }
        if self.cap_jitter_min_tokens > self.cap_jitter_max_tokens {
            return Err(format!(
                "{label}.capJitterMinTokens 不能大于 capJitterMaxTokens"
            ));
        }
        if self.scale_min_input_tokens < 0 {
            return Err(format!("{label}.scaleMinInputTokens 不能小于 0"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheSimulationPolicyPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub target_read_ratio: Option<f64>,
    #[serde(default)]
    pub token_scale: Option<f64>,
    #[serde(default)]
    pub max_simulated_input_tokens: Option<i32>,
    #[serde(default)]
    pub cap_jitter_min_tokens: Option<i32>,
    #[serde(default)]
    pub cap_jitter_max_tokens: Option<i32>,
    #[serde(default)]
    pub scale_min_input_tokens: Option<i32>,
}

impl CacheSimulationPolicyPatch {
    fn apply_to(self, mut policy: CacheSimulationPolicy) -> CacheSimulationPolicy {
        if let Some(value) = self.enabled {
            policy.enabled = value;
        }
        if let Some(value) = self.target_read_ratio {
            policy.target_read_ratio = value;
        }
        if let Some(value) = self.token_scale {
            policy.token_scale = value;
        }
        if let Some(value) = self.max_simulated_input_tokens {
            policy.max_simulated_input_tokens = value;
        }
        if let Some(value) = self.cap_jitter_min_tokens {
            policy.cap_jitter_min_tokens = value;
        }
        if let Some(value) = self.cap_jitter_max_tokens {
            policy.cap_jitter_max_tokens = value;
        }
        if let Some(value) = self.scale_min_input_tokens {
            policy.scale_min_input_tokens = value;
        }
        policy.normalized()
    }

    fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.target_read_ratio.is_none()
            && self.token_scale.is_none()
            && self.max_simulated_input_tokens.is_none()
            && self.cap_jitter_min_tokens.is_none()
            && self.cap_jitter_max_tokens.is_none()
            && self.scale_min_input_tokens.is_none()
    }

    fn validate_raw(&self, label: &str) -> Result<(), String> {
        if let Some(value) = self.target_read_ratio {
            if !(0.0..=0.99).contains(&value) || !value.is_finite() {
                return Err(format!("{label}.targetReadRatio 必须在 0 到 0.99 之间"));
            }
        }
        if let Some(value) = self.token_scale {
            if !(1.0..=3.0).contains(&value) || !value.is_finite() {
                return Err(format!("{label}.tokenScale 必须在 1 到 3 之间"));
            }
        }
        if self
            .max_simulated_input_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.maxSimulatedInputTokens 不能小于 0"));
        }
        if self.cap_jitter_min_tokens.is_some_and(|value| value < 0)
            || self.cap_jitter_max_tokens.is_some_and(|value| value < 0)
        {
            return Err(format!(
                "{label}.capJitterMinTokens 和 capJitterMaxTokens 不能小于 0"
            ));
        }
        if self.scale_min_input_tokens.is_some_and(|value| value < 0) {
            return Err(format!("{label}.scaleMinInputTokens 不能小于 0"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CachePointPolicy {
    #[serde(default = "default_kiro_cache_point_enabled")]
    pub enabled: bool,
    #[serde(default = "default_kiro_cache_point_tools_only")]
    pub tools_only: bool,
    #[serde(default = "default_kiro_cache_point_record_plan")]
    pub record_plan: bool,
}

impl Default for CachePointPolicy {
    fn default() -> Self {
        Self {
            enabled: default_kiro_cache_point_enabled(),
            tools_only: default_kiro_cache_point_tools_only(),
            record_plan: default_kiro_cache_point_record_plan(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CachePointPolicyPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub tools_only: Option<bool>,
    #[serde(default)]
    pub record_plan: Option<bool>,
}

impl CachePointPolicyPatch {
    fn apply_to(self, mut policy: CachePointPolicy) -> CachePointPolicy {
        if let Some(value) = self.enabled {
            policy.enabled = value;
        }
        if let Some(value) = self.tools_only {
            policy.tools_only = value;
        }
        if let Some(value) = self.record_plan {
            policy.record_plan = value;
        }
        policy
    }

    fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.tools_only.is_none() && self.record_plan.is_none()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheBoundsPolicy {
    #[serde(default = "default_prompt_cache_max_entries_per_account")]
    pub max_entries_per_account: usize,
    #[serde(default = "default_prompt_cache_max_entries_global")]
    pub max_entries_global: usize,
    #[serde(default = "default_prompt_cache_entry_ttl_secs")]
    pub entry_ttl_secs: u64,
    #[serde(default = "default_prompt_cache_estimated_bytes_limit")]
    pub estimated_bytes_limit: u64,
}

impl Default for CacheBoundsPolicy {
    fn default() -> Self {
        Self {
            max_entries_per_account: default_prompt_cache_max_entries_per_account(),
            max_entries_global: default_prompt_cache_max_entries_global(),
            entry_ttl_secs: default_prompt_cache_entry_ttl_secs(),
            estimated_bytes_limit: default_prompt_cache_estimated_bytes_limit(),
        }
    }
}

impl CacheBoundsPolicy {
    pub fn validate(self, label: &str) -> Result<(), String> {
        if self.max_entries_global > 0 && self.max_entries_per_account > self.max_entries_global {
            return Err(format!(
                "{label}.maxEntriesPerAccount 不能大于 maxEntriesGlobal"
            ));
        }
        if self.entry_ttl_secs == 0 {
            return Err(format!("{label}.entryTtlSecs 必须大于 0"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheBoundsPolicyPatch {
    #[serde(default)]
    pub max_entries_per_account: Option<usize>,
    #[serde(default)]
    pub max_entries_global: Option<usize>,
    #[serde(default)]
    pub entry_ttl_secs: Option<u64>,
    #[serde(default)]
    pub estimated_bytes_limit: Option<u64>,
}

impl CacheBoundsPolicyPatch {
    fn apply_to(self, mut policy: CacheBoundsPolicy) -> CacheBoundsPolicy {
        if let Some(value) = self.max_entries_per_account {
            policy.max_entries_per_account = value;
        }
        if let Some(value) = self.max_entries_global {
            policy.max_entries_global = value;
        }
        if let Some(value) = self.entry_ttl_secs {
            policy.entry_ttl_secs = value;
        }
        if let Some(value) = self.estimated_bytes_limit {
            policy.estimated_bytes_limit = value;
        }
        policy
    }

    fn is_empty(&self) -> bool {
        self.max_entries_per_account.is_none()
            && self.max_entries_global.is_none()
            && self.entry_ttl_secs.is_none()
            && self.estimated_bytes_limit.is_none()
    }

    fn validate_raw(&self, label: &str) -> Result<(), String> {
        if self.entry_ttl_secs == Some(0) {
            return Err(format!("{label}.entryTtlSecs 必须大于 0"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KiroRsToolCachePolicy {
    #[serde(default = "default_kiro_rs_tool_coverage_ratio")]
    pub coverage_ratio: f64,
    #[serde(default)]
    pub max_coverage_tokens: i32,
    #[serde(default = "default_true")]
    pub incremental_create_enabled: bool,
    #[serde(default)]
    pub max_new_creation_tokens_per_request: i32,
    #[serde(default)]
    pub cache_current_user_stable_prefix: bool,
    #[serde(default)]
    pub current_user_stable_prefix_max_tokens: i32,
    #[serde(default = "default_kiro_rs_tool_reported_input_min_tokens")]
    pub reported_input_min_tokens: i32,
    #[serde(default = "default_kiro_rs_tool_reported_input_max_tokens")]
    pub reported_input_max_tokens: i32,
}

impl Default for KiroRsToolCachePolicy {
    fn default() -> Self {
        Self {
            coverage_ratio: default_kiro_rs_tool_coverage_ratio(),
            max_coverage_tokens: 0,
            incremental_create_enabled: true,
            max_new_creation_tokens_per_request: 0,
            cache_current_user_stable_prefix: false,
            current_user_stable_prefix_max_tokens: 0,
            reported_input_min_tokens: default_kiro_rs_tool_reported_input_min_tokens(),
            reported_input_max_tokens: default_kiro_rs_tool_reported_input_max_tokens(),
        }
    }
}

impl KiroRsToolCachePolicy {
    pub fn normalized(mut self) -> Self {
        if !self.coverage_ratio.is_finite() {
            self.coverage_ratio = default_kiro_rs_tool_coverage_ratio();
        }
        self.coverage_ratio = self.coverage_ratio.clamp(0.0, 1.0);
        self.max_coverage_tokens = self.max_coverage_tokens.max(0);
        self.max_new_creation_tokens_per_request = self.max_new_creation_tokens_per_request.max(0);
        self.current_user_stable_prefix_max_tokens =
            self.current_user_stable_prefix_max_tokens.max(0);
        self.reported_input_min_tokens = self.reported_input_min_tokens.max(0);
        self.reported_input_max_tokens = self.reported_input_max_tokens.max(0);
        if self.reported_input_max_tokens > 0
            && self.reported_input_min_tokens > self.reported_input_max_tokens
        {
            self.reported_input_min_tokens = self.reported_input_max_tokens;
        }
        if !self.cache_current_user_stable_prefix {
            self.current_user_stable_prefix_max_tokens = 0;
        }
        self
    }

    pub fn validate(self, label: &str) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.coverage_ratio) || !self.coverage_ratio.is_finite() {
            return Err(format!("{label}.coverageRatio 必须在 0 到 1 之间"));
        }
        if self.max_coverage_tokens < 0 {
            return Err(format!("{label}.maxCoverageTokens 不能小于 0"));
        }
        if self.max_new_creation_tokens_per_request < 0 {
            return Err(format!("{label}.maxNewCreationTokensPerRequest 不能小于 0"));
        }
        if self.current_user_stable_prefix_max_tokens < 0 {
            return Err(format!(
                "{label}.currentUserStablePrefixMaxTokens 不能小于 0"
            ));
        }
        if self.reported_input_min_tokens < 0 {
            return Err(format!("{label}.reportedInputMinTokens 不能小于 0"));
        }
        if self.reported_input_max_tokens < 0 {
            return Err(format!("{label}.reportedInputMaxTokens 不能小于 0"));
        }
        if self.reported_input_max_tokens > 0
            && self.reported_input_min_tokens > self.reported_input_max_tokens
        {
            return Err(format!(
                "{label}.reportedInputMinTokens 不能大于 reportedInputMaxTokens"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KiroRsToolCachePolicyPatch {
    #[serde(default)]
    pub coverage_ratio: Option<f64>,
    #[serde(default)]
    pub max_coverage_tokens: Option<i32>,
    #[serde(default)]
    pub incremental_create_enabled: Option<bool>,
    #[serde(default)]
    pub max_new_creation_tokens_per_request: Option<i32>,
    #[serde(default)]
    pub cache_current_user_stable_prefix: Option<bool>,
    #[serde(default)]
    pub current_user_stable_prefix_max_tokens: Option<i32>,
    #[serde(default)]
    pub reported_input_min_tokens: Option<i32>,
    #[serde(default)]
    pub reported_input_max_tokens: Option<i32>,
}

impl KiroRsToolCachePolicyPatch {
    fn apply_to(self, mut policy: KiroRsToolCachePolicy) -> KiroRsToolCachePolicy {
        if let Some(value) = self.coverage_ratio {
            policy.coverage_ratio = value;
        }
        if let Some(value) = self.max_coverage_tokens {
            policy.max_coverage_tokens = value;
        }
        if let Some(value) = self.incremental_create_enabled {
            policy.incremental_create_enabled = value;
        }
        if let Some(value) = self.max_new_creation_tokens_per_request {
            policy.max_new_creation_tokens_per_request = value;
        }
        if let Some(value) = self.cache_current_user_stable_prefix {
            policy.cache_current_user_stable_prefix = value;
        }
        if let Some(value) = self.current_user_stable_prefix_max_tokens {
            policy.current_user_stable_prefix_max_tokens = value;
        }
        if let Some(value) = self.reported_input_min_tokens {
            policy.reported_input_min_tokens = value;
        }
        if let Some(value) = self.reported_input_max_tokens {
            policy.reported_input_max_tokens = value;
        }
        policy.normalized()
    }

    fn is_empty(&self) -> bool {
        self.coverage_ratio.is_none()
            && self.max_coverage_tokens.is_none()
            && self.incremental_create_enabled.is_none()
            && self.max_new_creation_tokens_per_request.is_none()
            && self.cache_current_user_stable_prefix.is_none()
            && self.current_user_stable_prefix_max_tokens.is_none()
            && self.reported_input_min_tokens.is_none()
            && self.reported_input_max_tokens.is_none()
    }

    fn validate_raw(&self, label: &str) -> Result<(), String> {
        if let Some(value) = self.coverage_ratio {
            if !(0.0..=1.0).contains(&value) || !value.is_finite() {
                return Err(format!("{label}.coverageRatio 必须在 0 到 1 之间"));
            }
        }
        if self.max_coverage_tokens.is_some_and(|value| value < 0) {
            return Err(format!("{label}.maxCoverageTokens 不能小于 0"));
        }
        if self
            .max_new_creation_tokens_per_request
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.maxNewCreationTokensPerRequest 不能小于 0"));
        }
        if self
            .current_user_stable_prefix_max_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!(
                "{label}.currentUserStablePrefixMaxTokens 不能小于 0"
            ));
        }
        if self
            .reported_input_min_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.reportedInputMinTokens 不能小于 0"));
        }
        if self
            .reported_input_max_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.reportedInputMaxTokens 不能小于 0"));
        }
        if matches!(
            (self.reported_input_min_tokens, self.reported_input_max_tokens),
            (Some(min), Some(max)) if max > 0 && min > max
        ) {
            return Err(format!(
                "{label}.reportedInputMinTokens 不能大于 reportedInputMaxTokens"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CacheRoutePolicyPatch {
    #[serde(default)]
    pub cache_type: Option<PromptCacheStrategyType>,
    #[serde(default)]
    pub route_namespace: Option<bool>,
    #[serde(default)]
    pub simulation: Option<CacheSimulationPolicyPatch>,
    #[serde(default)]
    pub creation_control: Option<PromptCacheCreationControlConfig>,
    #[serde(default)]
    pub reported_usage: Option<ReportedUsagePathPolicy>,
    #[serde(default)]
    pub cache_point: Option<CachePointPolicyPatch>,
    #[serde(default)]
    pub bounds: Option<CacheBoundsPolicyPatch>,
    #[serde(default)]
    pub kiro_rs_tool: Option<KiroRsToolCachePolicyPatch>,
}

impl CacheRoutePolicyPatch {
    fn explicit_cache_type(&self) -> PromptCacheStrategyType {
        self.cache_type
            .unwrap_or(PromptCacheStrategyType::CurrentHighCache)
    }

    fn apply_fields_to(&self, mut policy: CacheRoutePolicy) -> CacheRoutePolicy {
        if let Some(cache_type) = self.cache_type {
            policy.cache_type = cache_type;
        }
        if let Some(patch) = self.simulation {
            policy.simulation = patch.apply_to(policy.simulation);
        }
        if let Some(config) = self.creation_control {
            policy.creation_control = config.normalized();
        }
        if let Some(reported_usage) = &self.reported_usage {
            policy.reported_usage = reported_usage.normalized();
        }
        if let Some(patch) = self.cache_point {
            policy.cache_point = patch.apply_to(policy.cache_point);
        }
        if let Some(patch) = self.bounds {
            policy.bounds = patch.apply_to(policy.bounds);
        }
        if let Some(patch) = self.kiro_rs_tool {
            policy.kiro_rs_tool = patch.apply_to(policy.kiro_rs_tool);
        }
        policy.normalized()
    }

    fn apply_route_to(&self, mut policy: CacheRoutePolicy) -> CacheRoutePolicy {
        policy.cache_type = self.explicit_cache_type();
        self.apply_fields_for_strategy(policy)
    }

    pub fn validate(&self, label: &str, base: CacheRoutePolicy) -> Result<(), String> {
        if let Some(patch) = &self.simulation {
            patch.validate_raw(&format!("{label}.simulation"))?;
        }
        if let Some(config) = self.creation_control {
            config.validate_with_label(&format!("{label}.creationControl"))?;
        }
        if let Some(reported_usage) = &self.reported_usage {
            reported_usage.validate(&format!("{label}.reportedUsage"))?;
        }
        if let Some(patch) = &self.bounds {
            patch.validate_raw(&format!("{label}.bounds"))?;
        }
        if let Some(patch) = &self.kiro_rs_tool {
            patch.validate_raw(&format!("{label}.kiroRsTool"))?;
        }
        let policy = self.apply_route_to(base);
        policy.validate(label)
    }

    fn apply_fields_for_strategy(&self, policy: CacheRoutePolicy) -> CacheRoutePolicy {
        match policy.cache_type {
            PromptCacheStrategyType::NoCache => self.apply_no_cache_fields_to(policy),
            PromptCacheStrategyType::CurrentHighCache => self.apply_fields_to(policy),
            PromptCacheStrategyType::KiroRsTool => self.apply_kiro_rs_tool_fields_to(policy),
        }
    }

    fn apply_no_cache_fields_to(&self, mut policy: CacheRoutePolicy) -> CacheRoutePolicy {
        policy.reported_usage = ReportedUsagePathPolicy::disabled().normalized();
        policy.normalized()
    }

    fn apply_kiro_rs_tool_fields_to(&self, mut policy: CacheRoutePolicy) -> CacheRoutePolicy {
        if let Some(cache_type) = self.cache_type {
            policy.cache_type = cache_type;
        }
        if let Some(reported_usage) = &self.reported_usage {
            policy.reported_usage = reported_usage.normalized();
        }
        if let Some(patch) = self.cache_point {
            policy.cache_point = patch.apply_to(policy.cache_point);
        }
        if let Some(patch) = self.bounds {
            policy.bounds = patch.apply_to(policy.bounds);
        }
        if let Some(patch) = self.kiro_rs_tool {
            policy.kiro_rs_tool = patch.apply_to(policy.kiro_rs_tool);
        }
        policy.normalized()
    }

    fn affects_cache_state(&self) -> bool {
        self.cache_type
            .is_some_and(|cache_type| cache_type != PromptCacheStrategyType::NoCache)
            || self.route_namespace.is_some()
            || self.simulation.is_some()
            || self.creation_control.is_some()
            || self.cache_point.is_some()
            || self.bounds.is_some()
            || self.kiro_rs_tool.is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.cache_type.is_none()
            && self.route_namespace.is_none()
            && self
                .simulation
                .as_ref()
                .is_none_or(CacheSimulationPolicyPatch::is_empty)
            && self.creation_control.is_none()
            && self.reported_usage.is_none()
            && self
                .cache_point
                .as_ref()
                .is_none_or(CachePointPolicyPatch::is_empty)
            && self
                .bounds
                .as_ref()
                .is_none_or(CacheBoundsPolicyPatch::is_empty)
            && self
                .kiro_rs_tool
                .as_ref()
                .is_none_or(KiroRsToolCachePolicyPatch::is_empty)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheStrategyType {
    NoCache,
    #[default]
    CurrentHighCache,
    KiroRsTool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CachePolicyConfig {
    #[serde(default)]
    pub default: CacheRoutePolicyPatch,
    #[serde(default)]
    pub current_high_cache: CacheRoutePolicyPatch,
    #[serde(default)]
    pub kiro_rs_tool: CacheRoutePolicyPatch,
    #[serde(default)]
    pub path_overrides: BTreeMap<String, CacheRoutePolicyPatch>,
}

impl CachePolicyConfig {
    pub fn with_builtin_path_defaults(mut self) -> Self {
        for prefix in ["/v1", "/cc", "/ha"] {
            self.path_overrides
                .entry(prefix.to_string())
                .or_insert_with(|| CacheRoutePolicyPatch {
                    cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                    route_namespace: Some(false),
                    ..CacheRoutePolicyPatch::default()
                });
        }
        self.path_overrides
            .entry("/na".to_string())
            .or_insert_with(|| CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::NoCache),
                route_namespace: Some(false),
                ..CacheRoutePolicyPatch::default()
            });
        self
    }

    pub fn with_legacy_defined_cache_route_defaults(mut self, routes: &[String]) -> Self {
        for prefix in normalize_defined_cache_routes(routes) {
            self.path_overrides
                .entry(prefix)
                .or_insert_with(|| CacheRoutePolicyPatch {
                    cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                    route_namespace: Some(true),
                    ..CacheRoutePolicyPatch::default()
                });
        }
        self
    }

    pub fn migrate_builtin_no_cache_routes(&mut self) -> bool {
        false
    }

    pub fn normalized(&self) -> Self {
        let path_overrides = self
            .path_overrides
            .iter()
            .filter_map(|(prefix, policy)| {
                normalize_reported_usage_path_prefix(prefix).map(|prefix| (prefix, policy.clone()))
            })
            .filter(|(_, policy)| !policy.is_empty())
            .collect();
        Self {
            default: self.default.clone(),
            current_high_cache: self.current_high_cache.clone(),
            kiro_rs_tool: self.kiro_rs_tool.clone(),
            path_overrides,
        }
    }

    pub fn validate(&self, base: CacheRoutePolicy) -> Result<(), String> {
        self.default
            .validate("当前本地模拟策略兼容参数", base.clone())?;
        self.current_high_cache
            .validate("当前本地模拟策略模板", base.clone())?;
        self.kiro_rs_tool
            .validate("Kiro-RS-Tool 缓存策略模板", base.clone())?;
        for (prefix, policy) in &self.path_overrides {
            let Some(normalized_prefix) = normalize_reported_usage_path_prefix(prefix) else {
                return Err("缓存策略路径前缀不能为空".to_string());
            };
            policy.validate(&format!("路径 {normalized_prefix} 缓存策略"), base.clone())?;
        }
        Ok(())
    }

    fn current_high_cache_template(&self, base: CacheRoutePolicy) -> CacheRoutePolicy {
        let policy = self.default.apply_fields_to(base);
        self.current_high_cache.apply_fields_to(policy)
    }

    fn kiro_rs_tool_template(&self, base: CacheRoutePolicy) -> CacheRoutePolicy {
        let neutral = CacheRoutePolicy {
            cache_type: PromptCacheStrategyType::KiroRsTool,
            simulation: CacheSimulationPolicy {
                enabled: false,
                target_read_ratio: default_prompt_cache_target_read_ratio(),
                token_scale: 1.0,
                max_simulated_input_tokens: 0,
                cap_jitter_min_tokens: 0,
                cap_jitter_max_tokens: 0,
                scale_min_input_tokens: 0,
            }
            .normalized(),
            creation_control: PromptCacheCreationControlConfig {
                enabled: false,
                ..base.creation_control
            }
            .normalized(),
            reported_usage: ReportedUsagePathPolicy::disabled().normalized(),
            cache_point: CachePointPolicy::default(),
            bounds: base.bounds,
            kiro_rs_tool: KiroRsToolCachePolicy::default(),
        };
        self.kiro_rs_tool.apply_kiro_rs_tool_fields_to(neutral)
    }

    fn no_cache_policy(&self, base: CacheRoutePolicy) -> CacheRoutePolicy {
        CacheRoutePolicy {
            cache_type: PromptCacheStrategyType::NoCache,
            simulation: CacheSimulationPolicy {
                enabled: false,
                ..base.simulation
            }
            .normalized(),
            creation_control: PromptCacheCreationControlConfig {
                enabled: false,
                ..base.creation_control
            }
            .normalized(),
            reported_usage: ReportedUsagePathPolicy::disabled().normalized(),
            cache_point: CachePointPolicy {
                enabled: false,
                ..base.cache_point
            },
            bounds: base.bounds,
            kiro_rs_tool: base.kiro_rs_tool,
        }
        .normalized()
    }

    fn strategy_policy(
        &self,
        strategy_type: PromptCacheStrategyType,
        base: CacheRoutePolicy,
    ) -> CacheRoutePolicy {
        match strategy_type {
            PromptCacheStrategyType::NoCache => self.no_cache_policy(base),
            PromptCacheStrategyType::CurrentHighCache => {
                let mut policy = self.current_high_cache_template(base);
                policy.cache_type = PromptCacheStrategyType::CurrentHighCache;
                policy.normalized()
            }
            PromptCacheStrategyType::KiroRsTool => {
                let mut policy = self.kiro_rs_tool_template(base);
                policy.cache_type = PromptCacheStrategyType::KiroRsTool;
                policy.normalized()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CacheRoutePolicy {
    pub cache_type: PromptCacheStrategyType,
    pub simulation: CacheSimulationPolicy,
    pub creation_control: PromptCacheCreationControlConfig,
    pub reported_usage: ReportedUsagePathPolicy,
    pub cache_point: CachePointPolicy,
    pub bounds: CacheBoundsPolicy,
    pub kiro_rs_tool: KiroRsToolCachePolicy,
}

impl CacheRoutePolicy {
    pub fn normalized(mut self) -> Self {
        self.simulation = self.simulation.normalized();
        self.creation_control = self.creation_control.normalized();
        self.reported_usage = self.reported_usage.normalized();
        self.kiro_rs_tool = self.kiro_rs_tool.normalized();
        self
    }

    pub fn validate(&self, label: &str) -> Result<(), String> {
        self.simulation.validate(&format!("{label}.simulation"))?;
        self.creation_control
            .validate()
            .map_err(|err| format!("{label}.creationControl: {err}"))?;
        self.reported_usage
            .validate(&format!("{label}.reportedUsage"))?;
        self.bounds.validate(&format!("{label}.bounds"))?;
        self.kiro_rs_tool.validate(&format!("{label}.kiroRsTool"))?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCacheRoutePolicy {
    pub policy: CacheRoutePolicy,
    pub namespace: Option<String>,
}

pub fn resolve_cache_policy_for_path(
    base: CacheRoutePolicy,
    reported_usage: &ReportedUsageConfig,
    cache_policy: &CachePolicyConfig,
    path: &str,
) -> ResolvedCacheRoutePolicy {
    let cache_policy = cache_policy.normalized();
    let mut path_base = base.clone();

    if let Some(reported_usage) = reported_usage.path_override_for_path(path) {
        path_base.reported_usage = reported_usage.normalized();
    }

    let Some((prefix, override_policy)) = cache_policy
        .path_overrides
        .iter()
        .filter(|(prefix, _)| reported_usage_path_matches(prefix, path))
        .max_by_key(|(prefix, _)| prefix.len())
    else {
        return ResolvedCacheRoutePolicy {
            policy: cache_policy.no_cache_policy(path_base),
            namespace: None,
        };
    };

    let cache_type = override_policy.explicit_cache_type();
    let mut policy = cache_policy.strategy_policy(cache_type, path_base);
    if override_policy.reported_usage.is_none() {
        if let Some(reported_usage) = reported_usage.path_override_for_path(path) {
            policy.reported_usage = reported_usage.normalized();
        }
    }
    let policy = override_policy.apply_fields_for_strategy(policy);
    let namespace = match cache_type {
        PromptCacheStrategyType::NoCache => None,
        PromptCacheStrategyType::KiroRsTool => Some(prefix.clone()),
        PromptCacheStrategyType::CurrentHighCache => override_policy
            .route_namespace
            .unwrap_or_else(|| override_policy.affects_cache_state())
            .then(|| prefix.clone()),
    };

    ResolvedCacheRoutePolicy {
        policy: policy.normalized(),
        namespace,
    }
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            whitespace_compression: true,
        }
    }
}

impl Default for PayloadShapingConfig {
    fn default() -> Self {
        Self {
            enabled: default_payload_shaping_enabled(),
            truncate_historical_tool_results: true,
            historical_tool_result_max_chars: default_historical_tool_result_max_chars(),
            historical_tool_result_head_lines: default_historical_tool_result_head_lines(),
            historical_tool_result_tail_lines: default_historical_tool_result_tail_lines(),
            discard_historical_thinking: true,
            compress_tool_definitions: true,
            tool_definitions_budget_bytes: default_tool_definitions_budget_bytes(),
            tool_description_max_chars: default_tool_description_max_chars(),
            tool_schema_annotation_max_chars: default_tool_schema_annotation_max_chars(),
            web_fetch_trim_enabled: true,
            web_fetch_body_max_chars: default_web_fetch_body_max_chars(),
            fit_current_payload_to_budget: false,
            truncate_current_tool_results: false,
            current_tool_result_max_chars: default_current_tool_result_max_chars(),
            truncate_current_user_content: false,
            current_user_content_max_chars: default_current_user_content_max_chars(),
            truncate_current_documents: false,
            current_document_max_chars: default_current_document_max_chars(),
            truncate_current_images: false,
            current_images_max_bytes: default_current_images_max_bytes(),
            oversized_image_handling: OversizedImageHandling::default(),
        }
    }
}

/// Anthropic compatibility profile.
///
/// `claude-code` keeps the pragmatic rewrites needed by Claude Code CLI and
/// the Kiro upstream. `anthropic-strict` minimizes synthetic protocol and
/// prompt rewrites for detector-style checks. `debug` follows `claude-code`
/// behavior but exposes proxy warning headers by default.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CompatProfile {
    ClaudeCode,
    AnthropicStrict,
    Debug,
}

impl Default for CompatProfile {
    fn default() -> Self {
        Self::ClaudeCode
    }
}

impl CompatProfile {
    pub fn is_strict(self) -> bool {
        matches!(self, Self::AnthropicStrict)
    }

    pub fn is_debug(self) -> bool {
        matches!(self, Self::Debug)
    }

    pub fn allows_unsigned_thinking(self) -> bool {
        !self.is_strict()
    }
}

/// Thinking 触发策略。
///
/// `real_request` 保持客户端请求的真实语义；`always` 在请求未显式关闭
/// thinking 时强制进入可见 thinking 输出。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingTriggerMode {
    RealRequest,
    Always,
}

impl Default for ThinkingTriggerMode {
    fn default() -> Self {
        Self::RealRequest
    }
}

/// Kiro IDE `x-amzn-kiro-agent-mode` header strategy.
///
/// `vibe` preserves the current Kiro IDE / Claude Code compatible behavior.
/// `spec` forces the alternate Kiro planning-oriented mode. `auto` derives the
/// mode from credential protocol metadata: IdC, Enterprise/external IdP and API
/// key credentials stay on `vibe`; social/provider credentials use `spec`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KiroAgentModeStrategy {
    Vibe,
    Spec,
    Auto,
}

impl Default for KiroAgentModeStrategy {
    fn default() -> Self {
        Self::Vibe
    }
}

/// 请求模型解析策略。
///
/// `compatible` 保持当前 Claude Code 兼容行为：允许 `sonnet`、`opus`、
/// `default` 等短别名，也允许把同 family 的旧版/未来模型名映射到当前
/// Kiro 上游可用模型。`alias_only` 只允许精确模型和显式别名，不做
/// 宽松 family 归一化。`exact_only` 只允许模型能力目录里的精确模型 ID。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelResolutionMode {
    Compatible,
    AliasOnly,
    ExactOnly,
}

impl Default for ModelResolutionMode {
    fn default() -> Self {
        Self::Compatible
    }
}

impl ModelResolutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compatible => "compatible",
            Self::AliasOnly => "alias_only",
            Self::ExactOnly => "exact_only",
        }
    }

    pub fn allows_family_fallback(self) -> bool {
        matches!(self, Self::Compatible)
    }
}

/// 模型映射规则类型。
///
/// `version_equivalent` 表示同一上游小版本的不同写法，例如
/// `claude-opus-4-8` -> `claude-opus-4.8`。`alias` 表示短别名或显式别名，
/// 例如 `sonnet` -> 当前可用 Sonnet。`fallback` 表示兜底规则，只在精确和
/// 版本等价都没有命中后生效。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMappingRuleKind {
    VersionEquivalent,
    Alias,
    Fallback,
}

impl Default for ModelMappingRuleKind {
    fn default() -> Self {
        Self::Alias
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelMappingRule {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub kind: ModelMappingRuleKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ModelMappingRule {
    pub fn normalized(mut self) -> Option<Self> {
        self.source = self.source.trim().to_ascii_lowercase();
        self.target = self.target.trim().to_ascii_lowercase();
        self.note = self
            .note
            .and_then(|value| (!value.trim().is_empty()).then(|| value.trim().to_string()));
        (!self.source.is_empty() && !self.target.is_empty()).then_some(self)
    }
}

/// 模型映射配置。
///
/// 解析顺序固定为：上游模型列表精确匹配 -> 版本等价 -> 显式别名 ->
/// 兜底规则 -> 透传。关闭 `enabled`，或关闭自动规则且规则列表为空时，
/// 未精确匹配的模型会直接透传给上游，不再在本地做隐式降级。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelMappingConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_generate_rules: bool,
    #[serde(default)]
    pub rules: Vec<ModelMappingRule>,
}

impl Default for ModelMappingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_generate_rules: true,
            rules: Vec::new(),
        }
    }
}

impl ModelMappingConfig {
    pub fn normalized(mut self) -> Self {
        self.rules = self
            .rules
            .into_iter()
            .filter_map(ModelMappingRule::normalized)
            .collect();
        self
    }
}

/// Kiro payload guard 的大小裁剪触发模式。
///
/// `preemptive` 保持原有行为：发送上游前只要超过 `payloadGuardMaxBytes`
/// 就执行配置的内容整形和裁剪。`on_too_long` 首次请求只做协议修复；
/// 只有上游返回输入过长类错误后，才按 `payloadGuardMaxBytes` 裁剪并重试一次。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayloadGuardMode {
    Preemptive,
    OnTooLong,
}

impl Default for PayloadGuardMode {
    fn default() -> Self {
        Self::OnTooLong
    }
}

/// 外部备用号池并发满时的处理模式。
///
/// `fail_fast` 保持历史行为：当前没有外部池并发槽时立即返回调度不可用。
/// `wait` 会进入外部池独立等待队列，直到拿到外部池槽位或超过配置等待时间。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolCapacityMode {
    FailFast,
    Wait,
}

impl Default for ExternalPoolCapacityMode {
    fn default() -> Self {
        Self::FailFast
    }
}

/// 外部池入口路由控制模式。
///
/// 默认 `allow_all` 保持历史行为。`allow_list` 只允许命中规则的入口进入外部池；
/// `deny_list` 则禁止命中规则的入口进入外部池，其它入口保持可用。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolRouteMode {
    #[default]
    AllowAll,
    AllowList,
    DenyList,
}

/// 外部池返回模型不可用时的冷却范围。
///
/// `model` 是默认值：只短暂避开当前外部池的当前模型，避免一个不支持模型把
/// 整个外部池冷却并把后续请求推入全局排队队列。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolModelUnavailableCooldownMode {
    Disabled,
    #[default]
    Model,
    Pool,
}

/// 外部池流式响应处理模式。
///
/// `event_passthrough` 保持 SSE event 级透传，并会屏蔽上游流式错误事件中的
/// 内部细节。是否改写下游可见 usage 不由这里决定，而由单个外部池的
/// `usageProjectionMode` 决定：`pass_through` 原样返回上游 usage，
/// `current_path_policy` 按入口路径缓存策略整理 usage。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExternalPoolStreamResponseMode {
    #[serde(
        rename = "event_passthrough",
        alias = "event_passthrough_usage_rewrite",
        alias = "event_passthrough_capture"
    )]
    EventPassthrough,
}

impl Default for ExternalPoolStreamResponseMode {
    fn default() -> Self {
        Self::EventPassthrough
    }
}

impl ExternalPoolStreamResponseMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EventPassthrough => "event_passthrough",
        }
    }

    pub fn parse(value: &str) -> Self {
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim() {
            "event_passthrough"
            | "event_passthrough_usage_rewrite"
            | "event_passthrough_capture" => Some(Self::EventPassthrough),
            _ => None,
        }
    }
}

/// 外部备用号池全局策略配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolsConfig {
    #[serde(default)]
    pub external_pools_enabled: bool,
    #[serde(default = "default_external_pool_global_max_concurrent_requests")]
    pub external_pool_global_max_concurrent_requests: u32,
    #[serde(default = "default_external_pool_max_queued_requests")]
    pub external_pool_max_queued_requests: u32,
    /// 外部池估算输入 token 兼容字段。
    ///
    /// 历史版本曾用它在发送前拒绝外部池请求。当前外部池按本地凭证同款语义：
    /// 先按请求体处理配置发送，是否上下文超限以真实上游响应为准。
    #[serde(default = "default_external_pool_max_input_tokens")]
    pub external_pool_max_input_tokens: i32,
    #[serde(default)]
    pub external_pool_capacity_mode: ExternalPoolCapacityMode,
    #[serde(default = "default_external_pool_dispatch_max_wait_secs")]
    pub external_pool_dispatch_max_wait_secs: u64,
    #[serde(default = "default_external_pool_retry_max_attempts")]
    pub external_pool_retry_max_attempts: u32,
    /// 外部池之间故障转移时允许重试的 HTTP 状态码。
    ///
    /// 认证、配额、渠道禁用等错误默认也按临时抖动信号处理；该列表控制
    /// 普通 HTTP 错误是否继续消耗跨池重试预算。默认列表中的 500 作为
    /// 5xx 族兜底，覆盖 523/524/599 等未逐个枚举的上游临时错误；如果
    /// 运营侧显式只配置 502 等具体状态码，则按配置收窄。
    #[serde(default = "default_external_pool_retry_status_codes")]
    pub external_pool_retry_status_codes: Vec<u16>,
    /// 连接、DNS、超时等没有 HTTP 状态码的网络错误是否允许跨池重试。
    #[serde(default = "default_true")]
    pub external_pool_retry_on_network_error: bool,
    /// 成功状态码下返回错误信封、SSE 协议污染等协议错误是否允许跨池重试。
    #[serde(default = "default_true")]
    pub external_pool_retry_on_protocol_error: bool,
    #[serde(default = "default_external_pool_same_pool_retry_count")]
    pub external_pool_same_pool_retry_count: u32,
    #[serde(default = "default_external_pool_same_pool_retry_status_codes")]
    pub external_pool_same_pool_retry_status_codes: Vec<u16>,
    #[serde(default = "default_external_pool_same_pool_retry_delay_ms")]
    pub external_pool_same_pool_retry_delay_ms: u64,
    /// 外部池瞬态失败调度罚分。
    ///
    /// 每一个仍在 Redis 瞬态失败窗口内的失败 streak，都会临时增加池的
    /// 有效优先级。默认 20 可让优先级 1 的故障池在一次可重试失败后让位给
    /// 优先级 10/20 的健康池；填 0 会退回只按配置优先级和负载排序。
    #[serde(default = "default_external_pool_transient_failure_priority_penalty")]
    pub external_pool_transient_failure_priority_penalty: u32,
    #[serde(default)]
    pub external_direct_policy_enabled: bool,
    #[serde(default)]
    pub direct_external_on_local_maintenance: bool,
    #[serde(default)]
    pub direct_external_model_rules: Vec<String>,
    #[serde(default)]
    pub direct_external_path_rules: Vec<String>,
    #[serde(default)]
    pub external_pool_route_mode: ExternalPoolRouteMode,
    #[serde(default)]
    pub external_pool_route_rules: Vec<String>,
    #[serde(default = "default_true")]
    pub fallback_on_local_capacity_exhausted: bool,
    /// Redis scheduler coordination is a local routing failure, so historical
    /// configs that omit this newer field retain the pre-v0.0.108 capacity
    /// fallback behavior. Operators can still explicitly disable it.
    #[serde(default = "default_true")]
    pub fallback_on_scheduler_redis_degraded: bool,
    #[serde(default = "default_true")]
    pub fallback_on_no_available_credentials: bool,
    #[serde(default = "default_true")]
    pub fallback_on_local_transient_exhausted: bool,
    #[serde(default)]
    pub fallback_on_unsupported_model: bool,
    #[serde(default = "default_true")]
    pub local_pool_preflight_enabled: bool,
    #[serde(default = "default_true")]
    pub external_pool_local_rescue_enabled: bool,
    #[serde(default = "default_true")]
    pub external_pool_local_rescue_on_rate_limit: bool,
    #[serde(default = "default_true")]
    pub external_pool_local_rescue_on_timeout: bool,
    #[serde(default = "default_true")]
    pub external_pool_local_rescue_on_capacity: bool,
    #[serde(default = "default_external_pool_local_rescue_max_wait_secs")]
    pub external_pool_local_rescue_max_wait_secs: u64,
    #[serde(default)]
    pub local_pool_circuit_enabled: bool,
    #[serde(default = "default_local_pool_circuit_window_secs")]
    pub local_pool_circuit_window_secs: u64,
    #[serde(default = "default_local_pool_circuit_open_after_failures")]
    pub local_pool_circuit_open_after_failures: u32,
    #[serde(default = "default_local_pool_circuit_require_distinct_credentials")]
    pub local_pool_circuit_require_distinct_credentials: u32,
    #[serde(default = "default_local_pool_circuit_open_secs")]
    pub local_pool_circuit_open_secs: u64,
    #[serde(default)]
    pub external_pool_auto_disable_enabled: bool,
    #[serde(default = "default_true")]
    pub external_pool_auto_disable_on_auth_error: bool,
    #[serde(default = "default_true")]
    pub external_pool_auto_disable_on_security_lock: bool,
    #[serde(default)]
    pub external_pool_auto_disable_on_quota_exhausted: bool,
    #[serde(default)]
    pub external_pool_auto_disable_on_misconfigured_endpoint: bool,
    #[serde(default = "default_external_pool_auto_disable_failure_threshold")]
    pub external_pool_auto_disable_failure_threshold: u32,
    #[serde(default = "default_external_pool_auto_disable_window_secs")]
    pub external_pool_auto_disable_window_secs: u64,
    #[serde(default)]
    pub external_pool_auto_disable_duration_secs: u64,
    /// 外部池限流类错误的历史兼容冷却秒数。
    ///
    /// 当前普通上游错误默认只写短期健康降权，不写池级冷却；该值只在
    /// 严格临时不可调度或兼容诊断路径中作为时间提示。
    #[serde(default = "default_external_pool_rate_limit_cooldown_secs")]
    pub external_pool_rate_limit_cooldown_secs: u64,
    /// 外部池服务器错误的历史兼容冷却秒数。
    #[serde(default = "default_external_pool_server_error_cooldown_secs")]
    pub external_pool_server_error_cooldown_secs: u64,
    /// 外部池网络错误的历史兼容冷却秒数。
    #[serde(default = "default_external_pool_network_error_cooldown_secs")]
    pub external_pool_network_error_cooldown_secs: u64,
    /// 外部池协议错误的历史兼容冷却秒数。
    #[serde(default = "default_external_pool_protocol_error_cooldown_secs")]
    pub external_pool_protocol_error_cooldown_secs: u64,
    #[serde(default)]
    pub external_pool_model_unavailable_cooldown_mode: ExternalPoolModelUnavailableCooldownMode,
    #[serde(default = "default_external_pool_model_unavailable_cooldown_secs")]
    pub external_pool_model_unavailable_cooldown_secs: u64,
    #[serde(default = "default_external_pool_request_timeout_secs")]
    pub external_pool_request_timeout_secs: u64,
    #[serde(default)]
    pub external_pool_stream_request_timeout_secs: u64,
    #[serde(default = "default_external_pool_stream_idle_timeout_secs")]
    pub external_pool_stream_idle_timeout_secs: u64,
    /// 外部池流式响应在有效语义输出前出现 error/断流/idle 时是否允许换池恢复。
    ///
    /// 该开关只覆盖“尚未向下游提交有效内容”的安全窗口。单个外部池可以通过
    /// `preOutputStreamRetryMode` 覆盖为启用或禁用；默认继承全局值。
    #[serde(default = "default_external_pool_stream_pre_output_retry_enabled")]
    pub external_pool_stream_pre_output_retry_enabled: bool,
    #[serde(default = "default_true")]
    pub external_pool_auto_disable_on_channel_disabled: bool,
    #[serde(default = "default_external_pool_usage_projection_uplift_percent")]
    pub external_pool_usage_projection_uplift_percent: u32,
    #[serde(default)]
    pub external_pool_usage_projection_output_uplift_min_tokens: i32,
    #[serde(default)]
    pub external_pool_usage_projection_output_uplift_percent: u32,
    #[serde(default)]
    pub external_pool_stream_response_mode: ExternalPoolStreamResponseMode,
}

impl Default for ExternalPoolsConfig {
    fn default() -> Self {
        Self {
            external_pools_enabled: false,
            external_pool_global_max_concurrent_requests:
                default_external_pool_global_max_concurrent_requests(),
            external_pool_max_queued_requests: default_external_pool_max_queued_requests(),
            external_pool_max_input_tokens: default_external_pool_max_input_tokens(),
            external_pool_capacity_mode: ExternalPoolCapacityMode::default(),
            external_pool_dispatch_max_wait_secs: default_external_pool_dispatch_max_wait_secs(),
            external_pool_retry_max_attempts: default_external_pool_retry_max_attempts(),
            external_pool_retry_status_codes: default_external_pool_retry_status_codes(),
            external_pool_retry_on_network_error: true,
            external_pool_retry_on_protocol_error: true,
            external_pool_same_pool_retry_count: default_external_pool_same_pool_retry_count(),
            external_pool_same_pool_retry_status_codes:
                default_external_pool_same_pool_retry_status_codes(),
            external_pool_same_pool_retry_delay_ms: default_external_pool_same_pool_retry_delay_ms(
            ),
            external_pool_transient_failure_priority_penalty:
                default_external_pool_transient_failure_priority_penalty(),
            external_direct_policy_enabled: false,
            direct_external_on_local_maintenance: false,
            direct_external_model_rules: Vec::new(),
            direct_external_path_rules: Vec::new(),
            external_pool_route_mode: ExternalPoolRouteMode::default(),
            external_pool_route_rules: Vec::new(),
            fallback_on_local_capacity_exhausted: true,
            fallback_on_scheduler_redis_degraded: true,
            fallback_on_no_available_credentials: true,
            fallback_on_local_transient_exhausted: true,
            fallback_on_unsupported_model: false,
            local_pool_preflight_enabled: true,
            external_pool_local_rescue_enabled: true,
            external_pool_local_rescue_on_rate_limit: true,
            external_pool_local_rescue_on_timeout: true,
            external_pool_local_rescue_on_capacity: true,
            external_pool_local_rescue_max_wait_secs:
                default_external_pool_local_rescue_max_wait_secs(),
            local_pool_circuit_enabled: true,
            local_pool_circuit_window_secs: default_local_pool_circuit_window_secs(),
            local_pool_circuit_open_after_failures: default_local_pool_circuit_open_after_failures(
            ),
            local_pool_circuit_require_distinct_credentials:
                default_local_pool_circuit_require_distinct_credentials(),
            local_pool_circuit_open_secs: default_local_pool_circuit_open_secs(),
            external_pool_auto_disable_enabled: false,
            external_pool_auto_disable_on_auth_error: true,
            external_pool_auto_disable_on_security_lock: true,
            external_pool_auto_disable_on_quota_exhausted: false,
            external_pool_auto_disable_on_misconfigured_endpoint: false,
            external_pool_auto_disable_failure_threshold:
                default_external_pool_auto_disable_failure_threshold(),
            external_pool_auto_disable_window_secs: default_external_pool_auto_disable_window_secs(
            ),
            external_pool_auto_disable_duration_secs: 0,
            external_pool_rate_limit_cooldown_secs: default_external_pool_rate_limit_cooldown_secs(
            ),
            external_pool_server_error_cooldown_secs:
                default_external_pool_server_error_cooldown_secs(),
            external_pool_network_error_cooldown_secs:
                default_external_pool_network_error_cooldown_secs(),
            external_pool_protocol_error_cooldown_secs:
                default_external_pool_protocol_error_cooldown_secs(),
            external_pool_model_unavailable_cooldown_mode:
                ExternalPoolModelUnavailableCooldownMode::default(),
            external_pool_model_unavailable_cooldown_secs:
                default_external_pool_model_unavailable_cooldown_secs(),
            external_pool_request_timeout_secs: default_external_pool_request_timeout_secs(),
            external_pool_stream_request_timeout_secs: 0,
            external_pool_stream_idle_timeout_secs: default_external_pool_stream_idle_timeout_secs(
            ),
            external_pool_stream_pre_output_retry_enabled:
                default_external_pool_stream_pre_output_retry_enabled(),
            external_pool_auto_disable_on_channel_disabled: true,
            external_pool_usage_projection_uplift_percent:
                default_external_pool_usage_projection_uplift_percent(),
            external_pool_usage_projection_output_uplift_min_tokens: 0,
            external_pool_usage_projection_output_uplift_percent: 0,
            external_pool_stream_response_mode: ExternalPoolStreamResponseMode::default(),
        }
    }
}

impl ExternalPoolsConfig {
    /// Return a finite external capacity wait even when a legacy or API client
    /// submits zero. Keeping this bound at runtime prevents a stale config from
    /// turning a local scheduler outage into an indefinitely queued request.
    pub fn effective_dispatch_max_wait_secs(&self) -> u64 {
        if self.external_pool_dispatch_max_wait_secs == 0 {
            default_external_pool_dispatch_max_wait_secs()
        } else {
            self.external_pool_dispatch_max_wait_secs
        }
    }

    pub fn external_pool_route_allowed(&self, endpoint: &str) -> bool {
        match self.external_pool_route_mode {
            ExternalPoolRouteMode::AllowAll => true,
            ExternalPoolRouteMode::AllowList => self
                .external_pool_route_rules
                .iter()
                .any(|rule| route_rule_matches(rule, endpoint)),
            ExternalPoolRouteMode::DenyList => !self
                .external_pool_route_rules
                .iter()
                .any(|rule| route_rule_matches(rule, endpoint)),
        }
    }

    pub fn same_pool_retry_status_codes(&self) -> BTreeSet<u16> {
        self.external_pool_same_pool_retry_status_codes
            .iter()
            .copied()
            .filter(|code| (100..=599).contains(code))
            .collect()
    }

    pub fn retry_status_codes(&self) -> BTreeSet<u16> {
        self.external_pool_retry_status_codes
            .iter()
            .copied()
            .filter(|code| (100..=599).contains(code))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostgresConfig {
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default = "default_postgres_max_connections")]
    pub max_connections: u32,
    /// 独立 usage/dashboard/观测 PgSQL 连接池容量。
    ///
    /// 默认仍使用同一个 Postgres URL，但使用单独 sqlx pool，避免慢 usage 写入或
    /// dashboard 聚合耗尽凭据、运行配置与调度关键路径使用的主业务连接池。
    #[serde(default = "default_postgres_usage_max_connections")]
    pub usage_max_connections: u32,
    #[serde(default = "default_true")]
    pub migrate_on_start: bool,
    #[serde(default)]
    pub compress_usage_rollups_on_start: bool,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        Self {
            url: None,
            max_connections: default_postgres_max_connections(),
            usage_max_connections: default_postgres_usage_max_connections(),
            migrate_on_start: true,
            compress_usage_rollups_on_start: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisConfig {
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default = "default_redis_key_prefix")]
    pub key_prefix: String,
}

impl RedisConfig {
    fn configured_url(&self) -> Option<&str> {
        self.url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RedisAuthorityKey {
    Network { host: String, port: u16 },
    UnixSocket(String),
}

fn normalized_redis_authority_host(host: &str) -> String {
    match host.trim_matches(['[', ']']).to_ascii_lowercase().as_str() {
        "localhost" | "127.0.0.1" | "::1" => "<loopback>".to_string(),
        host => host.to_string(),
    }
}

fn redis_authority_key(value: &str) -> Result<RedisAuthorityKey, String> {
    let parsed = url::Url::parse(value).map_err(|_| {
        "Redis URL must be an absolute redis://, rediss://, or socket URL".to_string()
    })?;
    if !matches!(parsed.scheme(), "redis" | "rediss" | "redis+unix" | "unix") {
        return Err(format!(
            "Redis URL must use redis://, rediss://, redis+unix://, or unix:// (got {})",
            parsed.scheme()
        ));
    }
    if let Some(host) = parsed.host_str() {
        let port = parsed.port().unwrap_or(6379);
        return Ok(RedisAuthorityKey::Network {
            host: normalized_redis_authority_host(host),
            port,
        });
    }

    let path = parsed.path().trim();
    if path.is_empty() || path == "/" {
        return Err("Redis URL does not identify a network authority or socket path".to_string());
    }
    let canonical = std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    Ok(RedisAuthorityKey::UnixSocket(canonical))
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: None,
            key_prefix: default_redis_key_prefix(),
        }
    }
}

fn default_observability_redis_config() -> RedisConfig {
    RedisConfig {
        url: None,
        key_prefix: default_observability_redis_key_prefix(),
    }
}

const MAX_REQUEST_API_KEY_RPM: u32 = 1_000_000;
const MAX_REQUEST_API_KEY_CONCURRENT_REQUESTS: u32 = 10_000;
const MAX_REQUEST_API_KEY_QUEUED_REQUESTS: u32 = 100_000;
const MAX_REQUEST_API_KEY_QUEUE_TIMEOUT_MS: u64 = 300_000;

fn default_request_api_key_rpm() -> u32 {
    300
}

fn default_request_api_key_max_concurrent_requests() -> u32 {
    32
}

fn default_request_api_key_max_queued_requests() -> u32 {
    64
}

fn default_request_api_key_queue_timeout_ms() -> u64 {
    1_000
}

/// 每个实例、每个已认证下游请求 API Key 的本地 admission 配置。
///
/// 该状态不跨实例聚合；部署 N 个实例时，同一 Key 的总准入上限最多可近似放大为 N 倍。
///
/// 这是单实例硬限制，不经过 Redis。多实例部署的总上限等于各实例上限之和。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RequestAdmissionConfig {
    /// 每个实例内，每个请求 API Key 每分钟最多进入多少个 `/messages` 请求。0 表示禁用 RPM 限制。
    #[serde(default = "default_request_api_key_rpm")]
    pub rpm: u32,
    /// 每个实例内，每个请求 API Key 最多同时持有多少个 `/messages` response body。0 表示禁用并发限制。
    #[serde(default = "default_request_api_key_max_concurrent_requests")]
    pub max_concurrent_requests: u32,
    /// 并发占满时，每个实例内每个请求 API Key 最多排队多少个请求。0 表示禁用排队并立即返回 429。
    #[serde(default = "default_request_api_key_max_queued_requests")]
    pub max_queued_requests: u32,
    /// 排队最长等待毫秒数。0 表示禁用排队并立即返回 429。
    #[serde(default = "default_request_api_key_queue_timeout_ms")]
    pub queue_timeout_ms: u64,
}

impl Default for RequestAdmissionConfig {
    fn default() -> Self {
        Self {
            rpm: default_request_api_key_rpm(),
            max_concurrent_requests: default_request_api_key_max_concurrent_requests(),
            max_queued_requests: default_request_api_key_max_queued_requests(),
            queue_timeout_ms: default_request_api_key_queue_timeout_ms(),
        }
    }
}

impl RequestAdmissionConfig {
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            rpm: 0,
            max_concurrent_requests: 0,
            max_queued_requests: 0,
            queue_timeout_ms: 0,
        }
    }

    pub fn normalized(self) -> Self {
        let mut normalized = Self {
            rpm: self.rpm.min(MAX_REQUEST_API_KEY_RPM),
            max_concurrent_requests: self
                .max_concurrent_requests
                .min(MAX_REQUEST_API_KEY_CONCURRENT_REQUESTS),
            max_queued_requests: self
                .max_queued_requests
                .min(MAX_REQUEST_API_KEY_QUEUED_REQUESTS),
            queue_timeout_ms: self
                .queue_timeout_ms
                .min(MAX_REQUEST_API_KEY_QUEUE_TIMEOUT_MS),
        };
        if normalized.max_concurrent_requests == 0
            || normalized.max_queued_requests == 0
            || normalized.queue_timeout_ms == 0
        {
            normalized.max_queued_requests = 0;
            normalized.queue_timeout_ms = 0;
        }
        normalized
    }

    pub fn validate(self) -> Result<(), String> {
        if self.rpm > MAX_REQUEST_API_KEY_RPM {
            return Err(format!(
                "requestAdmission.rpm 不能大于 {MAX_REQUEST_API_KEY_RPM}"
            ));
        }
        if self.max_concurrent_requests > MAX_REQUEST_API_KEY_CONCURRENT_REQUESTS {
            return Err(format!(
                "requestAdmission.maxConcurrentRequests 不能大于 {MAX_REQUEST_API_KEY_CONCURRENT_REQUESTS}"
            ));
        }
        if self.max_queued_requests > MAX_REQUEST_API_KEY_QUEUED_REQUESTS {
            return Err(format!(
                "requestAdmission.maxQueuedRequests 不能大于 {MAX_REQUEST_API_KEY_QUEUED_REQUESTS}"
            ));
        }
        if self.queue_timeout_ms > MAX_REQUEST_API_KEY_QUEUE_TIMEOUT_MS {
            return Err(format!(
                "requestAdmission.queueTimeoutMs 不能大于 {MAX_REQUEST_API_KEY_QUEUE_TIMEOUT_MS}"
            ));
        }
        Ok(())
    }

    pub fn enabled(self) -> bool {
        // Queue settings only shape concurrency waiting; they are not an
        // independent admission limit and are canonicalized away when unused.
        self.rpm > 0 || self.max_concurrent_requests > 0
    }
}

fn deserialize_default_on_null<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// KNA 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Runtime config migration marker persisted in PgSQL.
    ///
    /// File configs loaded through `Config::load` are treated as current so
    /// explicit operator choices are not rewritten as legacy defaults.
    #[serde(default)]
    pub runtime_config_migration_version: u32,

    /// PgSQL 配置。服务启动必须可连接；首次启动可从配置文件 bootstrap 运行配置和凭据。
    #[serde(default)]
    pub postgres: PostgresConfig,

    /// Redis 配置。用于运行时缓存、锁和后续跨实例调度状态。
    #[serde(default)]
    pub redis: RedisConfig,

    /// Optional Redis fault domain for usage summaries, statistics, Admin caches, and cleanup.
    ///
    /// This endpoint must not resolve to the same Redis network authority as `redis`: logical
    /// databases and key prefixes still share one Redis event loop and do not isolate scheduler
    /// latency from observability work. When omitted, observability remains PostgreSQL/local-only
    /// and must never fall back to the business Redis.
    #[serde(default = "default_observability_redis_config")]
    pub observability_redis: RedisConfig,

    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_region")]
    pub region: String,

    /// Auth Region（用于 Token 刷新），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,

    /// API Region（用于 API 请求），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,

    #[serde(default = "default_kiro_version")]
    pub kiro_version: String,

    #[serde(default)]
    pub machine_id: Option<String>,

    #[serde(default)]
    pub api_key: Option<String>,

    /// 额外的客户端调用 API Key。
    ///
    /// `apiKey` 保留为历史主 Key；运行时实际允许的调用 Key 为
    /// `apiKey + apiKeys` 规范化后的集合。
    #[serde(default)]
    pub api_keys: Vec<String>,

    #[serde(default = "default_system_version")]
    pub system_version: String,

    #[serde(default = "default_node_version")]
    pub node_version: String,

    #[serde(default = "default_tls_backend")]
    pub tls_backend: TlsBackend,

    /// 外部 count_tokens API 地址（可选）
    #[serde(default)]
    pub count_tokens_api_url: Option<String>,

    /// count_tokens API 密钥（可选）
    #[serde(default)]
    pub count_tokens_api_key: Option<String>,

    /// count_tokens API 认证类型（可选，"x-api-key" 或 "bearer"，默认 "x-api-key"）
    #[serde(default = "default_count_tokens_auth_type")]
    pub count_tokens_auth_type: String,

    /// HTTP 代理地址（可选）
    /// 支持格式: http://host:port, https://host:port, socks5://host:port, socks5h://host:port
    #[serde(default)]
    pub proxy_url: Option<String>,

    /// 代理认证用户名（可选）
    #[serde(default)]
    pub proxy_username: Option<String>,

    /// 代理认证密码（可选）
    #[serde(default)]
    pub proxy_password: Option<String>,

    /// Admin API 密钥（可选，启用 Admin API 功能）
    #[serde(default)]
    pub admin_api_key: Option<String>,

    /// 已认证请求 API Key 的单实例 `/messages` admission 控制。
    ///
    /// 缺少整个字段的旧配置采用保守默认值；RPM 与并发均为 0 时关闭准入限制。
    /// Queue 只修饰并发等待，无独立 enabled 语义，并在无效组合中规范化为 0/0。
    #[serde(default, deserialize_with = "deserialize_default_on_null")]
    pub request_admission: RequestAdmissionConfig,

    /// 单凭据目标请求速率（RPM）。
    ///
    /// `None` 或 `0` 表示禁用本地凭据级限速；`>0` 会按每个凭据计算最小请求间隔，
    /// 并在调度时优先分流到其他可用凭据。
    #[serde(default = "default_credential_rpm")]
    pub credential_rpm: Option<u32>,

    /// 单凭据最大并发请求数。
    ///
    /// `0` 表示不限制；`>0` 时，同一凭据同时在处理的请求达到上限后，
    /// 新请求会优先调度到其他可用凭据。
    #[serde(default = "default_credential_max_concurrent_requests")]
    pub credential_max_concurrent_requests: u32,

    /// 上游瞬态错误没有 Retry-After 时，对单个凭据设置的临时冷却秒数。
    #[serde(default = "default_credential_transient_cooldown_secs")]
    pub credential_transient_cooldown_secs: u64,

    /// 上游 429 没有 Retry-After 时的基础冷却秒数。
    #[serde(default = "default_credential_rate_limit_cooldown_secs")]
    pub credential_rate_limit_cooldown_secs: u64,

    /// 上游 5xx/408 等服务器瞬态错误的基础冷却秒数。
    #[serde(default = "default_credential_server_error_cooldown_secs")]
    pub credential_server_error_cooldown_secs: u64,

    /// 网络发送错误的基础冷却秒数。
    #[serde(default = "default_credential_network_error_cooldown_secs")]
    pub credential_network_error_cooldown_secs: u64,

    /// 流式读取/idle timeout 错误的基础冷却秒数。
    #[serde(default = "default_credential_stream_error_cooldown_secs")]
    pub credential_stream_error_cooldown_secs: u64,

    /// 可重试协议异常或无法分类错误的基础冷却秒数。
    #[serde(default = "default_credential_protocol_error_cooldown_secs")]
    pub credential_protocol_error_cooldown_secs: u64,

    /// 认证失败进入刷新/判定期间的基础冷却秒数。
    #[serde(default = "default_credential_auth_error_cooldown_secs")]
    pub credential_auth_error_cooldown_secs: u64,

    /// 同一凭据连续瞬态失败时的冷却退避倍率。
    #[serde(default = "default_credential_cooldown_backoff_multiplier")]
    pub credential_cooldown_backoff_multiplier: f64,

    /// 冷却退避随机抖动百分比，范围 0..=100。
    #[serde(default = "default_credential_cooldown_jitter_percent")]
    pub credential_cooldown_jitter_percent: u32,

    /// 冷却结束后的降权观察窗口秒数。
    #[serde(default = "default_credential_probation_secs")]
    pub credential_probation_secs: u64,

    /// 单个凭据临时冷却最长秒数，用于限制 Retry-After 头的影响范围。
    #[serde(default = "default_credential_max_cooldown_secs")]
    pub credential_max_cooldown_secs: u64,

    /// 单个请求等待凭据可调度的最长秒数。
    ///
    /// `0` 表示不限制等待时间；`>0` 时，如果所有可用凭据持续处于冷却、
    /// 本地限流或并发占满状态，超过该时间后返回明确的本地调度限流错误。
    #[serde(default = "default_credential_dispatch_max_wait_secs")]
    pub credential_dispatch_max_wait_secs: u64,

    /// Kiro 上游响应头最长等待秒数。
    ///
    /// 只限制请求发出到拿到响应头的阶段；响应头之后的流式 body 读取仍由
    /// Anthropic SSE 层的上游 idle timeout 控制，避免长输出被整段请求超时误杀。
    /// `0` 表示关闭该额外保护，仅使用底层 HTTP client 的全局超时。
    #[serde(default = "default_kiro_upstream_response_timeout_secs")]
    pub kiro_upstream_response_timeout_secs: u64,

    /// Kiro 上游流式响应正文的静默超时秒数。
    ///
    /// 响应头回来后，如果 eventstream 在该时间内没有任何新 chunk，就按上游
    /// stream idle 处理并释放并发占用。`0` 表示使用默认值，避免错误关闭保护。
    #[serde(default = "default_kiro_upstream_stream_idle_timeout_secs")]
    pub kiro_upstream_stream_idle_timeout_secs: u64,

    /// 是否允许流式响应在尚未向下游发送任何 SSE 字节前，对上游流读取/空闲/错误事件进行重试。
    ///
    /// 该开关只覆盖“下游尚未提交”的安全窗口。只要已经发送过 message_start、ping、
    /// text/thinking/tool_use 或 error 等任意 SSE 字节，就不会自动换号重试，避免重复工具调用
    /// 或事件乱序。
    #[serde(default = "default_kiro_upstream_stream_retry_enabled")]
    pub kiro_upstream_stream_retry_enabled: bool,

    /// 单个流式请求在“未向下游提交”窗口内最多尝试多少次上游流。
    ///
    /// 包含首次调用；默认 2 表示最多补一次重试。0/1 都等价于不额外重试。
    #[serde(default = "default_kiro_upstream_stream_retry_max_attempts")]
    pub kiro_upstream_stream_retry_max_attempts: u32,

    /// 单个下游 Messages 请求允许发出的推理上游 HTTP 请求硬上限。
    ///
    /// 本地凭据重试、首输出前流重试、payload/cachePoint 重试、外部池 failover
    /// 和本地 rescue 共享这一预算。该值与账号、外部池数量无关。
    #[serde(default = "default_inference_upstream_max_attempts")]
    pub inference_upstream_max_attempts: u32,

    /// 单个下游请求允许发出的辅助上游 HTTP 请求硬上限。
    ///
    /// Token refresh 与 enterprise profile discovery 共享该 request-scoped 预算；
    /// 不计入 inferenceUpstreamMaxAttempts，也不受 prompt steering 开关控制。
    #[serde(default = "default_auxiliary_upstream_max_attempts")]
    pub auxiliary_upstream_max_attempts: u32,

    /// 单实例同时执行的辅助上游 HTTP 请求硬上限。
    ///
    /// 达到上限时 fail-fast，不进入无界等待队列。该边界独立于普通请求 admission。
    #[serde(default = "default_auxiliary_upstream_max_concurrent_requests")]
    pub auxiliary_upstream_max_concurrent_requests: u32,

    /// Token refresh 辅助通道的 RPM 硬上限。
    ///
    /// 配置 Redis 时这是跨实例共享上限；未配置 Redis 时是单进程上限。
    #[serde(default = "default_token_refresh_max_rpm")]
    pub token_refresh_max_rpm: u32,

    /// Token refresh 通道允许立即消耗的 token bucket 容量。
    #[serde(default = "default_token_refresh_burst")]
    pub token_refresh_burst: u32,

    /// 上游 eventstream idle timeout 发生在下游提交前时是否允许重试。
    #[serde(default = "default_true")]
    pub kiro_upstream_stream_retry_on_idle_timeout: bool,

    /// 上游 body read error 发生在下游提交前时是否允许重试。
    #[serde(default = "default_true")]
    pub kiro_upstream_stream_retry_on_read_error: bool,

    /// 上游 2xx JSON 错误体、流内 error/invalidState 等状态错误发生在下游提交前时是否允许重试。
    #[serde(default = "default_true")]
    pub kiro_upstream_stream_retry_on_status_error: bool,

    /// Kiro 上游基础 URL 覆盖。
    ///
    /// 默认 `None` 时使用官方 `https://q.{region}.amazonaws.com`。仅用于本地压测、
    /// staging 或显式内网代理验证；生产不配置时不会改变官方调用协议。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kiro_upstream_base_url: Option<String>,

    /// 单次上游调用最多尝试多少个凭据/重试轮次。
    ///
    /// `0` 表示自动使用 3 次固定预算，不随凭据池规模增长；
    /// `>0` 表示显式上限，用于限制单次请求在大凭据池下的最长故障转移时间。
    #[serde(default)]
    pub credential_retry_max_attempts: u32,

    /// 是否允许部分提示/协议逻辑 400 错误换未尝试账号重试。
    ///
    /// 默认关闭，保持 400 请求错误直接失败的原语义。
    #[serde(default)]
    pub credential_prompt_logic_retry_enabled: bool,

    /// 提示/协议逻辑 400 错误最多换号重试次数。
    ///
    /// 仅在 `credentialPromptLogicRetryEnabled=true` 时生效；0 表示开启时默认 1 次。
    #[serde(default)]
    pub credential_prompt_logic_retry_max_attempts: u32,

    /// 并发占用 lease 的最大存活秒数。
    ///
    /// `0` 表示不自动回收；`>0` 时，调度前会清理超过该时间仍未释放的占用，
    /// 避免异常路径导致某个凭据永久被视为并发占满。
    #[serde(default = "default_credential_in_flight_lease_max_secs")]
    pub credential_in_flight_lease_max_secs: u64,

    /// 全局最大并发调度请求数。默认给新初始化配置一个硬上限；`0` 表示不限制。
    #[serde(default = "default_dispatch_global_max_concurrent_requests")]
    pub dispatch_global_max_concurrent_requests: u32,

    /// 全局最多允许等待调度容量的请求数。默认限制等待放大；`0` 表示不限制。
    #[serde(default = "default_dispatch_max_queued_requests")]
    pub dispatch_max_queued_requests: u32,

    /// 本地凭据调度容量是否按请求 token 量级加权。
    ///
    /// 默认关闭；关闭时不会在调度前做额外 token 估算，也不会改变并发/RPM 口径。
    /// 开启后只影响本地凭据池，外部池仍按外部池自己的并发配置调度。
    #[serde(default)]
    pub weighted_capacity: WeightedCapacityConfig,

    /// 新凭据预热请求次数。预热期内 balanced 会降低该凭据调度权重，但不会伪造 success_count。
    #[serde(default = "default_credential_warmup_requests")]
    pub credential_warmup_requests: u32,

    /// balanced 模式下预热凭据参与真实业务请求调度的概率百分比。
    #[serde(default = "default_credential_warmup_selection_percent")]
    pub credential_warmup_selection_percent: u32,

    /// 已有非预热凭据可用时，所有预热凭据合计最多承接的流量百分比。
    #[serde(default = "default_credential_warmup_max_selection_percent")]
    pub credential_warmup_max_selection_percent: u32,

    /// 输入压缩配置。默认不启用；启用后默认只做 whitespace 压缩。
    #[serde(default)]
    pub compression: CompressionConfig,

    /// 多模态图片/文件预处理配置。
    ///
    /// safe 保持现有行为：展开本地 file_id、下载安全远程 URL、识别并修正 base64 图片媒体类型。
    /// light 不下载、不展开、不 decode 校正图片，只让 inline base64/data URL 进入协议转换。
    #[serde(default)]
    pub image_processing: ImageProcessingConfig,

    /// 本地 Anthropic -> Kiro 协议转换能力配置。
    #[serde(default)]
    pub body_conversion: BodyConversionConfig,

    /// 提示词引导配置。默认路径规则仍只命中 `/cc`，但实际生效范围由配置决定。
    /// `enabled` 是所有代理新增提示内容的总开关；子开关只在总开关开启时进一步细分。
    #[serde(default)]
    pub prompt_steering: PromptSteeringConfig,

    /// Messages 请求缺少顶层 max_tokens 时的入口兼容策略。
    #[serde(default)]
    pub missing_max_tokens: MissingMaxTokensConfig,

    /// Kiro payload shaping 配置。默认只压缩旧历史和明显冗余，不截断当前输入。
    #[serde(default)]
    pub payload_shaping: PayloadShapingConfig,

    /// 发送 Kiro 上游前启用最终 payload 防护。
    ///
    /// 防护在 Anthropic -> Kiro 转换之后运行，按真实 JSON 字节数裁剪旧历史，
    /// 并修复 Kiro 容易返回 `400 Improperly formed request` 的工具配对边界。
    #[serde(default = "default_payload_guard_enabled")]
    pub payload_guard_enabled: bool,

    /// payload guard 大小裁剪触发模式。
    #[serde(default = "default_payload_guard_mode")]
    pub payload_guard_mode: PayloadGuardMode,

    /// Kiro 上游请求 JSON body 的本地裁剪目标。默认使用保守阈值 450 KiB；
    /// `0` 表示不按大小整形或裁剪，但仍执行协议修复。该字段不是入站
    /// hard limit；无法安全裁剪时会记录 `still_oversized` 并由上游裁决。
    #[serde(default = "default_payload_guard_max_bytes")]
    pub payload_guard_max_bytes: usize,

    /// payload guard 的安全余量字节数。
    ///
    /// 当 `payloadGuardMaxBytes > 0` 时，实际裁剪目标为
    /// `payloadGuardMaxBytes - payloadGuardSafetyMarginBytes`，避免 provider
    /// 层追加 endpoint/profile 等字段后贴近 Kiro 的真实请求体上限。
    #[serde(default = "default_payload_guard_safety_margin_bytes")]
    pub payload_guard_safety_margin_bytes: usize,

    /// payload 超限时是否允许裁剪最旧历史。关闭后只执行轻量协议修复；
    /// 仍超预算的请求会标记 `still_oversized` 并继续透传给 Kiro。
    #[serde(default = "default_payload_guard_trim_history")]
    pub payload_guard_trim_history: bool,

    /// 外部备用池请求是否复用同一套 payload guard / shaping 配置。
    ///
    /// 开启后，外部池转发 Anthropic 请求前也会按 `payloadGuardMode`、
    /// `payloadGuardMaxBytes` 和 `payloadShaping` 处理超长上下文；不会为外部池复制
    /// 第二套阈值或裁剪规则。关闭后外部池保持原始请求体透传。
    #[serde(default = "default_payload_guard_external_enabled")]
    pub payload_guard_external_enabled: bool,

    /// 是否把 Anthropic tool cache_control 转成 Kiro cachePoint 发送给上游。
    ///
    /// 默认关闭；开启后仅对实际发送给 Kiro 的工具定义插入 cachePoint。
    #[serde(default = "default_kiro_cache_point_enabled")]
    pub kiro_cache_point_enabled: bool,

    /// cachePoint 第一阶段只根据工具上的 cache_control 插入，不自动改写系统消息或历史消息。
    #[serde(default = "default_kiro_cache_point_tools_only")]
    pub kiro_cache_point_tools_only: bool,

    /// 是否把 cachePoint 插入计划写入 payload diagnostics，便于定位上游 body invalid。
    #[serde(default = "default_kiro_cache_point_record_plan")]
    pub kiro_cache_point_record_plan: bool,

    /// 负载均衡模式（"priority" 或 "balanced"）
    #[serde(default = "default_load_balancing_mode")]
    pub load_balancing_mode: String,

    /// 健康调度的错误率 EWMA 新样本权重。
    #[serde(default = "default_scheduler_error_ewma_alpha")]
    pub scheduler_error_ewma_alpha: f64,

    /// 健康调度的配置优先级权重。
    #[serde(default = "default_scheduler_priority_weight")]
    pub scheduler_priority_weight: f64,

    /// 健康调度的实时并发负载权重。
    #[serde(default = "default_scheduler_load_weight")]
    pub scheduler_load_weight: f64,

    /// 健康调度的近期错误率权重。
    #[serde(default = "default_scheduler_error_weight")]
    pub scheduler_error_weight: f64,

    /// 健康调度的延迟 EWMA 权重。
    #[serde(default = "default_scheduler_latency_weight")]
    pub scheduler_latency_weight: f64,

    /// 健康调度的恢复观察期惩罚权重。
    #[serde(default = "default_scheduler_probation_weight")]
    pub scheduler_probation_weight: f64,

    /// 健康调度的近期选中压力权重，用于降低短窗口内被过度选中的凭据。
    #[serde(default = "default_scheduler_selection_pressure_weight")]
    pub scheduler_selection_pressure_weight: f64,

    /// 健康调度的总选中次数权重。默认关闭，仅建议作为极弱长期均衡信号。
    #[serde(default = "default_scheduler_total_selection_weight")]
    pub scheduler_total_selection_weight: f64,

    /// 健康模式在得分最佳的前 N 个候选中加权抽样，降低并发抢占集中度。
    #[serde(default = "default_scheduler_top_k")]
    pub scheduler_top_k: u32,

    /// 调度失败诊断中最多记录多少个账号样本。
    ///
    /// 只影响内部 usage/trace 元数据，不影响对下游返回的统一错误。
    #[serde(default = "default_selection_failure_sample_limit")]
    pub selection_failure_sample_limit: usize,

    /// 是否记录调度失败的账号样本明细。
    ///
    /// 关闭后仍会保留原因计数和主原因，但不写入具体账号样本。
    #[serde(default = "default_selection_failure_record_enabled")]
    pub selection_failure_record_enabled: bool,

    /// Anthropic 兼容 profile（默认 claude-code）。
    #[serde(default = "default_compat_profile")]
    pub compat_profile: CompatProfile,

    /// Kiro IDE agent-mode header 策略（默认 vibe，保持现有成功链路）。
    #[serde(default = "default_kiro_agent_mode_strategy")]
    pub kiro_agent_mode_strategy: KiroAgentModeStrategy,

    /// 请求模型解析策略（默认 compatible）。
    ///
    /// 控制 `sonnet`、`opus`、`default` 等短模型名，以及同 family 自动归一化
    /// 是否允许在发送上游前映射为当前 Kiro 可用模型。
    #[serde(default = "default_model_resolution_mode")]
    pub model_resolution_mode: ModelResolutionMode,

    /// 模型映射和兜底规则配置。
    #[serde(default)]
    pub model_mapping: ModelMappingConfig,

    /// 是否开启非流式响应的 thinking 块提取（默认 true）
    ///
    /// 启用后，非流式响应中的 `<thinking>...</thinking>` 标签会被解析为
    /// 独立的 `{"type": "thinking", ...}` 内容块,与流式响应行为一致。
    #[serde(default = "default_extract_thinking")]
    pub extract_thinking: bool,

    /// thinking 触发策略（默认按真实请求触发）。
    #[serde(default = "default_thinking_trigger_mode")]
    pub thinking_trigger_mode: ThinkingTriggerMode,

    /// 本地 prompt-cache 模拟的目标 cache read 中心比例。
    ///
    /// 对 high-cache 生效；读取仍必须命中同一凭据、
    /// 会话、模型下已创建过的缓存前缀，不会凭空制造 cache read。实际比例会
    /// 围绕该值做小范围确定性浮动，避免每次都精确落在同一个百分比。
    #[serde(default = "default_prompt_cache_target_read_ratio")]
    pub prompt_cache_target_read_ratio: f64,

    /// high-cache 模拟专用的 total input 放大倍数。
    ///
    /// 只影响本地 high-cache usage 模拟，不影响真实 Kiro metadata cache。
    #[serde(default = "default_prompt_cache_token_scale")]
    pub prompt_cache_token_scale: f64,

    /// high-cache 模拟 total input 的上限。
    ///
    /// 触顶时会按稳定 fingerprint 做确定性 soft-cap 抖动，避免每次固定卡在上限。
    #[serde(default = "default_prompt_cache_max_simulated_input_tokens")]
    pub prompt_cache_max_simulated_input_tokens: i32,

    /// high-cache 模拟触顶 soft-cap 的最小扣减 token。
    #[serde(default = "default_prompt_cache_cap_jitter_min_tokens")]
    pub prompt_cache_cap_jitter_min_tokens: i32,

    /// high-cache 模拟触顶 soft-cap 的最大扣减 token。
    #[serde(default = "default_prompt_cache_cap_jitter_max_tokens")]
    pub prompt_cache_cap_jitter_max_tokens: i32,

    /// 基础输入达到该门槛后才启用 high-cache token scale，避免短测试请求被放大。
    #[serde(default = "default_prompt_cache_scale_min_input_tokens")]
    pub prompt_cache_scale_min_input_tokens: i32,

    /// 本地 prompt-cache creation 上报频次控制。
    #[serde(default)]
    pub prompt_cache_creation_control: PromptCacheCreationControlConfig,

    /// 本地 prompt-cache 每个账号最多保留的 fingerprint 条目。
    #[serde(default = "default_prompt_cache_max_entries_per_account")]
    pub prompt_cache_max_entries_per_account: usize,

    /// 本地 prompt-cache 全局最多保留的 fingerprint 条目。
    #[serde(default = "default_prompt_cache_max_entries_global")]
    pub prompt_cache_max_entries_global: usize,

    /// 本地 prompt-cache 单条 fingerprint 的最大 TTL 秒数。
    ///
    /// 实际 TTL 会取上游 cache_control TTL 与该值的较小值；默认值不会缩短现有 5m/1h 行为。
    #[serde(default = "default_prompt_cache_entry_ttl_secs")]
    pub prompt_cache_entry_ttl_secs: u64,

    /// 本地 prompt-cache 估算内存上限。
    #[serde(default = "default_prompt_cache_estimated_bytes_limit")]
    pub prompt_cache_estimated_bytes_limit: u64,

    /// 下游 usage 上报整理配置。
    ///
    /// 默认策略先应用，再按路径前缀使用最长匹配覆盖；只影响 response usage
    /// 和后台 usage record，不影响 prompt-cache reader 计算、tracker 更新和上游请求。
    #[serde(default)]
    pub reported_usage: ReportedUsageConfig,

    /// 路径级缓存策略覆盖。
    ///
    /// 旧的全局 prompt-cache/cachePoint/reportedUsage 字段仍作为默认值；这里可以按路径前缀
    /// 覆盖高缓存模拟、写入频次控制、usage 上报和真实 cachePoint 行为。
    #[serde(default)]
    pub cache_policy: CachePolicyConfig,

    /// 自定义 high-cache 路由前缀。
    ///
    /// 只允许 `/dfcache/{name}` 形式，实际模型接口为
    /// `/dfcache/{name}/v1/messages` 等。未列入这里的 `/dfcache/*`
    /// 路径会直接报错，避免误放开未知路由。
    #[serde(default)]
    pub defined_cache_routes: Vec<String>,

    /// 请求级 usage record 内存保留上限。
    #[serde(default = "default_usage_record_limit")]
    pub usage_record_limit: usize,

    /// tool-use 格式错误的内部 JSONL 诊断记录。详细内容不写入 usage 数据库。
    #[serde(default)]
    pub tool_format_debug: ToolFormatDebugConfig,

    /// Admin 高缓存请求阈值。
    #[serde(default = "default_high_cache_threshold")]
    pub high_cache_threshold: i32,

    /// 默认端点名称（凭据未显式指定 endpoint 时使用，默认 "ide"）
    #[serde(default = "default_endpoint")]
    pub default_endpoint: String,

    /// 是否在响应头中暴露代理改写动作（默认 false）。
    ///
    /// 启用后，凡涉及消息合并 / 孤立 tool_use|tool_result 清理 / thinking 覆写等
    /// 代理侧的隐式改写都会通过 `x-kiro-rs-warnings` 响应头汇总反馈，便于排查。
    /// 仅写头，不会修改响应体，对客户端无副作用。
    #[serde(default = "default_expose_proxy_warnings")]
    pub expose_proxy_warnings: bool,

    /// 外部备用号池和直连/预检 fallback 策略。
    #[serde(default)]
    pub external_pools: ExternalPoolsConfig,

    /// 配置文件路径（运行时元数据，不写入 JSON）
    #[serde(skip)]
    config_path: Option<PathBuf>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_region() -> String {
    "us-east-1".to_string()
}

const CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION: u32 = 8;

fn default_kiro_version() -> String {
    "0.11.107".to_string()
}

fn default_system_version() -> String {
    match std::env::consts::OS {
        "macos" => "darwin#24.6.0".to_string(),
        "windows" => "win32#10.0.22631".to_string(),
        other => format!("{other}#unknown"),
    }
}

fn default_node_version() -> String {
    "22.22.0".to_string()
}

fn default_count_tokens_auth_type() -> String {
    "x-api-key".to_string()
}

fn default_tls_backend() -> TlsBackend {
    TlsBackend::Rustls
}

fn default_load_balancing_mode() -> String {
    "priority".to_string()
}

fn default_credential_transient_cooldown_secs() -> u64 {
    10
}

fn default_credential_rate_limit_cooldown_secs() -> u64 {
    30
}

fn default_credential_server_error_cooldown_secs() -> u64 {
    5
}

fn default_credential_network_error_cooldown_secs() -> u64 {
    5
}

fn default_credential_stream_error_cooldown_secs() -> u64 {
    5
}

fn default_credential_protocol_error_cooldown_secs() -> u64 {
    10
}

fn default_credential_auth_error_cooldown_secs() -> u64 {
    10
}

fn default_credential_cooldown_backoff_multiplier() -> f64 {
    2.0
}

fn default_credential_cooldown_jitter_percent() -> u32 {
    20
}

fn default_credential_probation_secs() -> u64 {
    30
}

fn default_credential_max_cooldown_secs() -> u64 {
    300
}

fn default_credential_rpm() -> Option<u32> {
    Some(100)
}

fn default_credential_max_concurrent_requests() -> u32 {
    30
}

fn default_credential_dispatch_max_wait_secs() -> u64 {
    5
}

fn default_kiro_upstream_response_timeout_secs() -> u64 {
    180
}

fn default_kiro_upstream_stream_idle_timeout_secs() -> u64 {
    180
}

fn default_kiro_upstream_stream_retry_enabled() -> bool {
    true
}

fn default_kiro_upstream_stream_retry_max_attempts() -> u32 {
    2
}

pub(crate) fn default_inference_upstream_max_attempts() -> u32 {
    4
}

pub(crate) fn default_auxiliary_upstream_max_attempts() -> u32 {
    2
}

pub(crate) fn default_auxiliary_upstream_max_concurrent_requests() -> u32 {
    DEFAULT_AUXILIARY_UPSTREAM_MAX_CONCURRENT_REQUESTS
}

pub(crate) fn default_token_refresh_max_rpm() -> u32 {
    DEFAULT_TOKEN_REFRESH_MAX_RPM
}

pub(crate) fn default_token_refresh_burst() -> u32 {
    DEFAULT_TOKEN_REFRESH_BURST
}

fn default_credential_in_flight_lease_max_secs() -> u64 {
    900
}

fn default_dispatch_global_max_concurrent_requests() -> u32 {
    512
}

fn default_dispatch_max_queued_requests() -> u32 {
    30
}

fn default_weighted_capacity_unit() -> u32 {
    1
}

fn default_weighted_capacity_max_units() -> u32 {
    8
}

fn default_weighted_capacity_tiers() -> Vec<WeightedCapacityTier> {
    vec![
        WeightedCapacityTier {
            min_tokens: 0,
            units: 1,
        },
        WeightedCapacityTier {
            min_tokens: 100_000,
            units: 2,
        },
        WeightedCapacityTier {
            min_tokens: 300_000,
            units: 4,
        },
        WeightedCapacityTier {
            min_tokens: 700_000,
            units: 8,
        },
    ]
}

fn default_credential_warmup_requests() -> u32 {
    3
}

fn default_credential_warmup_selection_percent() -> u32 {
    5
}

fn default_credential_warmup_max_selection_percent() -> u32 {
    50
}

fn default_scheduler_error_ewma_alpha() -> f64 {
    0.2
}

fn default_scheduler_priority_weight() -> f64 {
    1.0
}

fn default_scheduler_load_weight() -> f64 {
    100.0
}

fn default_scheduler_error_weight() -> f64 {
    100.0
}

fn default_scheduler_latency_weight() -> f64 {
    0.01
}

fn default_scheduler_probation_weight() -> f64 {
    50.0
}

fn default_scheduler_selection_pressure_weight() -> f64 {
    25.0
}

fn default_scheduler_total_selection_weight() -> f64 {
    0.0
}

fn default_scheduler_top_k() -> u32 {
    3
}

fn default_selection_failure_sample_limit() -> usize {
    20
}

fn default_selection_failure_record_enabled() -> bool {
    true
}

fn default_payload_shaping_enabled() -> bool {
    true
}

fn default_historical_tool_result_max_chars() -> usize {
    8_000
}

fn default_historical_tool_result_head_lines() -> usize {
    80
}

fn default_historical_tool_result_tail_lines() -> usize {
    40
}

fn default_tool_definitions_budget_bytes() -> usize {
    20_000
}

fn default_tool_description_max_chars() -> usize {
    4_000
}

fn default_tool_schema_annotation_max_chars() -> usize {
    1_000
}

fn default_web_fetch_body_max_chars() -> usize {
    12_000
}

fn default_current_tool_result_max_chars() -> usize {
    80_000
}

fn default_current_user_content_max_chars() -> usize {
    120_000
}

fn default_current_document_max_chars() -> usize {
    80_000
}

fn default_current_images_max_bytes() -> usize {
    180_000
}

fn default_payload_guard_enabled() -> bool {
    true
}

fn default_missing_max_tokens_value() -> i32 {
    DEFAULT_MISSING_MAX_TOKENS_VALUE
}

fn default_payload_guard_mode() -> PayloadGuardMode {
    PayloadGuardMode::OnTooLong
}

fn default_payload_guard_max_bytes() -> usize {
    450 * 1024
}

fn default_payload_guard_safety_margin_bytes() -> usize {
    32 * 1024
}

fn default_payload_guard_trim_history() -> bool {
    true
}

fn default_payload_guard_external_enabled() -> bool {
    true
}

fn default_kiro_cache_point_enabled() -> bool {
    false
}

fn default_kiro_cache_point_tools_only() -> bool {
    true
}

fn default_kiro_cache_point_record_plan() -> bool {
    true
}

fn default_compat_profile() -> CompatProfile {
    CompatProfile::ClaudeCode
}

fn default_kiro_agent_mode_strategy() -> KiroAgentModeStrategy {
    KiroAgentModeStrategy::Vibe
}

fn default_model_resolution_mode() -> ModelResolutionMode {
    ModelResolutionMode::Compatible
}

fn default_extract_thinking() -> bool {
    true
}

fn default_thinking_trigger_mode() -> ThinkingTriggerMode {
    ThinkingTriggerMode::RealRequest
}

fn default_true() -> bool {
    true
}

fn default_prompt_cache_target_read_ratio() -> f64 {
    0.98
}

fn default_kiro_rs_tool_coverage_ratio() -> f64 {
    1.0
}

fn default_kiro_rs_tool_reported_input_min_tokens() -> i32 {
    32
}

fn default_kiro_rs_tool_reported_input_max_tokens() -> i32 {
    4_096
}

fn default_prompt_cache_token_scale() -> f64 {
    1.6
}

fn default_prompt_cache_max_simulated_input_tokens() -> i32 {
    300_000
}

fn default_prompt_cache_cap_jitter_min_tokens() -> i32 {
    12_000
}

fn default_prompt_cache_cap_jitter_max_tokens() -> i32 {
    24_000
}

fn default_prompt_cache_scale_min_input_tokens() -> i32 {
    20_000
}

fn default_prompt_cache_creation_min_successes_between() -> u32 {
    3
}

fn default_prompt_cache_creation_min_interval_secs() -> u64 {
    60
}

fn default_prompt_cache_creation_min_delta_tokens() -> i32 {
    12_000
}

fn default_prompt_cache_creation_max_tokens_per_event() -> i32 {
    30_000
}

fn default_prompt_cache_creation_budget_window_secs() -> u64 {
    300
}

fn default_prompt_cache_creation_max_tokens_per_window() -> i32 {
    120_000
}

fn default_prompt_cache_creation_expire_after_idle_secs() -> u64 {
    3_600
}

fn default_prompt_cache_max_entries_per_account() -> usize {
    200
}

fn default_prompt_cache_max_entries_global() -> usize {
    20_000
}

fn default_prompt_cache_entry_ttl_secs() -> u64 {
    86_400
}

fn default_prompt_cache_estimated_bytes_limit() -> u64 {
    256 * 1024 * 1024
}

fn default_usage_record_limit() -> usize {
    5000
}

fn default_tool_format_debug_enabled() -> bool {
    true
}

fn default_tool_format_debug_dir() -> String {
    "logs/tool-format-debug".to_string()
}

fn default_tool_format_debug_channel_capacity() -> usize {
    128
}

fn default_tool_format_debug_max_record_bytes() -> usize {
    512 * 1024
}

fn default_tool_format_debug_max_samples_per_kind() -> usize {
    5
}

fn default_tool_format_debug_window_secs() -> u64 {
    600
}

fn default_tool_format_debug_max_records_per_fingerprint() -> u32 {
    3
}

fn default_tool_format_debug_max_records_per_group() -> u32 {
    20
}

fn default_tool_format_debug_max_records_global() -> u32 {
    200
}

fn default_tool_format_debug_max_string_bytes() -> usize {
    256
}

fn default_tool_format_debug_capture_request_body() -> bool {
    true
}

fn default_tool_format_debug_max_request_body_bytes() -> usize {
    384 * 1024
}

fn default_tool_format_debug_max_request_body_records_per_window() -> u32 {
    20
}

fn default_tool_format_debug_roll_interval_secs() -> u64 {
    30 * 60
}

fn default_tool_format_debug_max_file_bytes() -> u64 {
    100 * 1024 * 1024
}

fn normalize_tool_format_debug_request_body_bytes(value: usize) -> usize {
    if value == 0 {
        0
    } else {
        value.clamp(1024, 2 * 1024 * 1024)
    }
}

fn normalize_tool_format_debug_max_file_bytes(value: u64) -> u64 {
    if value == 0 {
        default_tool_format_debug_max_file_bytes()
    } else {
        value.clamp(1024 * 1024, 1024 * 1024 * 1024)
    }
}

fn default_high_cache_threshold() -> i32 {
    10_000
}

fn default_endpoint() -> String {
    crate::kiro::endpoint::ide::IDE_ENDPOINT_NAME.to_string()
}

fn default_expose_proxy_warnings() -> bool {
    false
}

fn default_local_pool_circuit_window_secs() -> u64 {
    60
}

fn default_local_pool_circuit_open_after_failures() -> u32 {
    3
}

fn default_local_pool_circuit_require_distinct_credentials() -> u32 {
    2
}

fn default_local_pool_circuit_open_secs() -> u64 {
    30
}

fn default_external_pool_auto_disable_failure_threshold() -> u32 {
    1
}

fn default_external_pool_auto_disable_window_secs() -> u64 {
    60
}

fn default_external_pool_global_max_concurrent_requests() -> u32 {
    512
}

fn default_external_pool_max_queued_requests() -> u32 {
    10
}

fn default_external_pool_dispatch_max_wait_secs() -> u64 {
    5
}

fn default_external_pool_retry_max_attempts() -> u32 {
    3
}

fn default_external_pool_retry_status_codes() -> Vec<u16> {
    // 500 is treated as the default 5xx family retry guard by retry_pipeline.
    vec![408, 425, 429, 500, 502, 503, 504, 529]
}

fn default_external_pool_same_pool_retry_count() -> u32 {
    1
}

fn default_external_pool_same_pool_retry_status_codes() -> Vec<u16> {
    // 500 is treated as the default 5xx family retry guard by retry_pipeline.
    vec![408, 425, 429, 500, 502, 503, 504, 529]
}

fn default_external_pool_same_pool_retry_delay_ms() -> u64 {
    500
}

fn default_external_pool_transient_failure_priority_penalty() -> u32 {
    20
}

fn default_external_pool_max_input_tokens() -> i32 {
    1_000_000
}

fn default_external_pool_local_rescue_max_wait_secs() -> u64 {
    15
}

fn default_external_pool_rate_limit_cooldown_secs() -> u64 {
    30
}

fn default_external_pool_server_error_cooldown_secs() -> u64 {
    10
}

fn default_external_pool_network_error_cooldown_secs() -> u64 {
    10
}

fn default_external_pool_protocol_error_cooldown_secs() -> u64 {
    10
}

fn default_external_pool_model_unavailable_cooldown_secs() -> u64 {
    10
}

fn default_external_pool_request_timeout_secs() -> u64 {
    180
}

fn default_external_pool_stream_idle_timeout_secs() -> u64 {
    180
}

fn default_external_pool_stream_pre_output_retry_enabled() -> bool {
    true
}

fn default_external_pool_usage_projection_uplift_percent() -> u32 {
    25
}

fn default_postgres_max_connections() -> u32 {
    10
}

fn default_postgres_usage_max_connections() -> u32 {
    4
}

fn default_redis_key_prefix() -> String {
    "kiro_rs:local".to_string()
}

fn default_observability_redis_key_prefix() -> String {
    "kiro_rs:observability".to_string()
}

fn default_reported_usage_normal_max_multiplier() -> f64 {
    1.1
}

fn default_final_cache_read_max_tokens() -> i32 {
    700_000
}

fn default_final_cache_creation_max_tokens() -> i32 {
    400_000
}

fn default_reported_usage_final_cache_creation_jitter_min_tokens() -> i32 {
    20_000
}

fn default_reported_usage_final_cache_creation_jitter_max_tokens() -> i32 {
    45_000
}

fn default_reported_usage_output_uplift_min_tokens() -> i32 {
    1_000
}

fn default_reported_usage_output_uplift_percent() -> u32 {
    50
}

fn default_reported_usage_final_output_max_tokens() -> i32 {
    200_000
}

fn default_reported_usage_final_output_jitter_min_tokens() -> i32 {
    5_000
}

fn default_reported_usage_final_output_jitter_max_tokens() -> i32 {
    12_000
}

fn normalize_reported_usage_normal_max_multiplier(value: f64) -> f64 {
    if value.is_finite() && value >= 1.0 {
        value.min(10.0)
    } else {
        default_reported_usage_normal_max_multiplier()
    }
}

fn normalize_reported_usage_path_prefix(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_slash = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };
    let normalized = with_slash.trim_end_matches('/').to_string();
    Some(if normalized.is_empty() {
        "/".to_string()
    } else {
        normalized
    })
}

fn reported_usage_path_matches(prefix: &str, path: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

pub fn normalize_defined_cache_route(route: &str) -> Option<String> {
    let trimmed = route.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_slash = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };
    let normalized = with_slash.trim_end_matches('/').to_ascii_lowercase();
    let name = normalized.strip_prefix("/dfcache/")?;
    if name.is_empty()
        || name.contains('/')
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return None;
    }
    Some(format!("/dfcache/{name}"))
}

pub fn normalize_defined_cache_routes(routes: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    routes
        .iter()
        .filter_map(|route| normalize_defined_cache_route(route))
        .filter(|route| seen.insert(route.clone()))
        .collect()
}

pub(crate) fn normalize_route_rule(rule: &str) -> Option<String> {
    let trimmed = rule.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "*" {
        return Some("*".to_string());
    }
    let with_slash = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    let normalized = with_slash.trim_end_matches('/').to_ascii_lowercase();
    Some(if normalized.is_empty() {
        "/".to_string()
    } else {
        normalized
    })
}

pub(crate) fn normalize_route_rules(rules: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    rules
        .iter()
        .filter_map(|rule| normalize_route_rule(rule))
        .filter(|rule| seen.insert(rule.clone()))
        .collect()
}

pub(crate) fn route_rule_matches(rule: &str, endpoint: &str) -> bool {
    let Some(rule) = normalize_route_rule(rule) else {
        return false;
    };
    if rule == "*" {
        return true;
    }
    let endpoint = endpoint.trim().to_ascii_lowercase();
    if endpoint == rule {
        return true;
    }
    let Some(rest) = endpoint.strip_prefix(&rule) else {
        return false;
    };
    rule.ends_with('/') || rest.starts_with('/')
}

fn normalize_request_api_keys(keys: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut normalized = Vec::new();
    for key in keys {
        let key = key.as_ref().trim();
        if key.is_empty() || normalized.iter().any(|existing| existing == key) {
            continue;
        }
        normalized.push(key.to_string());
    }
    normalized
}

impl Default for Config {
    fn default() -> Self {
        Self {
            runtime_config_migration_version: CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION,
            postgres: PostgresConfig::default(),
            redis: RedisConfig::default(),
            observability_redis: default_observability_redis_config(),
            host: default_host(),
            port: default_port(),
            region: default_region(),
            auth_region: None,
            api_region: None,
            kiro_version: default_kiro_version(),
            machine_id: None,
            api_key: None,
            api_keys: Vec::new(),
            system_version: default_system_version(),
            node_version: default_node_version(),
            tls_backend: default_tls_backend(),
            count_tokens_api_url: None,
            count_tokens_api_key: None,
            count_tokens_auth_type: default_count_tokens_auth_type(),
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            admin_api_key: None,
            request_admission: RequestAdmissionConfig::default(),
            credential_rpm: default_credential_rpm(),
            credential_max_concurrent_requests: default_credential_max_concurrent_requests(),
            credential_transient_cooldown_secs: default_credential_transient_cooldown_secs(),
            credential_rate_limit_cooldown_secs: default_credential_rate_limit_cooldown_secs(),
            credential_server_error_cooldown_secs: default_credential_server_error_cooldown_secs(),
            credential_network_error_cooldown_secs: default_credential_network_error_cooldown_secs(
            ),
            credential_stream_error_cooldown_secs: default_credential_stream_error_cooldown_secs(),
            credential_protocol_error_cooldown_secs:
                default_credential_protocol_error_cooldown_secs(),
            credential_auth_error_cooldown_secs: default_credential_auth_error_cooldown_secs(),
            credential_cooldown_backoff_multiplier: default_credential_cooldown_backoff_multiplier(
            ),
            credential_cooldown_jitter_percent: default_credential_cooldown_jitter_percent(),
            credential_probation_secs: default_credential_probation_secs(),
            credential_max_cooldown_secs: default_credential_max_cooldown_secs(),
            credential_dispatch_max_wait_secs: default_credential_dispatch_max_wait_secs(),
            kiro_upstream_response_timeout_secs: default_kiro_upstream_response_timeout_secs(),
            kiro_upstream_stream_idle_timeout_secs: default_kiro_upstream_stream_idle_timeout_secs(
            ),
            kiro_upstream_stream_retry_enabled: default_kiro_upstream_stream_retry_enabled(),
            kiro_upstream_stream_retry_max_attempts:
                default_kiro_upstream_stream_retry_max_attempts(),
            inference_upstream_max_attempts: default_inference_upstream_max_attempts(),
            auxiliary_upstream_max_attempts: default_auxiliary_upstream_max_attempts(),
            auxiliary_upstream_max_concurrent_requests:
                default_auxiliary_upstream_max_concurrent_requests(),
            token_refresh_max_rpm: default_token_refresh_max_rpm(),
            token_refresh_burst: default_token_refresh_burst(),
            kiro_upstream_stream_retry_on_idle_timeout: true,
            kiro_upstream_stream_retry_on_read_error: true,
            kiro_upstream_stream_retry_on_status_error: true,
            kiro_upstream_base_url: None,
            credential_retry_max_attempts: 0,
            credential_prompt_logic_retry_enabled: false,
            credential_prompt_logic_retry_max_attempts: 0,
            credential_in_flight_lease_max_secs: default_credential_in_flight_lease_max_secs(),
            dispatch_global_max_concurrent_requests:
                default_dispatch_global_max_concurrent_requests(),
            dispatch_max_queued_requests: default_dispatch_max_queued_requests(),
            weighted_capacity: WeightedCapacityConfig::default(),
            credential_warmup_requests: default_credential_warmup_requests(),
            credential_warmup_selection_percent: default_credential_warmup_selection_percent(),
            credential_warmup_max_selection_percent:
                default_credential_warmup_max_selection_percent(),
            compression: CompressionConfig::default(),
            image_processing: ImageProcessingConfig::default(),
            body_conversion: BodyConversionConfig::default(),
            prompt_steering: PromptSteeringConfig::default(),
            missing_max_tokens: MissingMaxTokensConfig::default(),
            payload_shaping: PayloadShapingConfig::default(),
            payload_guard_enabled: default_payload_guard_enabled(),
            payload_guard_mode: default_payload_guard_mode(),
            payload_guard_max_bytes: default_payload_guard_max_bytes(),
            payload_guard_safety_margin_bytes: default_payload_guard_safety_margin_bytes(),
            payload_guard_trim_history: default_payload_guard_trim_history(),
            payload_guard_external_enabled: default_payload_guard_external_enabled(),
            kiro_cache_point_enabled: default_kiro_cache_point_enabled(),
            kiro_cache_point_tools_only: default_kiro_cache_point_tools_only(),
            kiro_cache_point_record_plan: default_kiro_cache_point_record_plan(),
            load_balancing_mode: default_load_balancing_mode(),
            scheduler_error_ewma_alpha: default_scheduler_error_ewma_alpha(),
            scheduler_priority_weight: default_scheduler_priority_weight(),
            scheduler_load_weight: default_scheduler_load_weight(),
            scheduler_error_weight: default_scheduler_error_weight(),
            scheduler_latency_weight: default_scheduler_latency_weight(),
            scheduler_probation_weight: default_scheduler_probation_weight(),
            scheduler_selection_pressure_weight: default_scheduler_selection_pressure_weight(),
            scheduler_total_selection_weight: default_scheduler_total_selection_weight(),
            scheduler_top_k: default_scheduler_top_k(),
            selection_failure_sample_limit: default_selection_failure_sample_limit(),
            selection_failure_record_enabled: default_selection_failure_record_enabled(),
            compat_profile: default_compat_profile(),
            kiro_agent_mode_strategy: default_kiro_agent_mode_strategy(),
            model_resolution_mode: default_model_resolution_mode(),
            model_mapping: ModelMappingConfig::default(),
            extract_thinking: default_extract_thinking(),
            thinking_trigger_mode: default_thinking_trigger_mode(),
            prompt_cache_target_read_ratio: default_prompt_cache_target_read_ratio(),
            prompt_cache_token_scale: default_prompt_cache_token_scale(),
            prompt_cache_max_simulated_input_tokens:
                default_prompt_cache_max_simulated_input_tokens(),
            prompt_cache_cap_jitter_min_tokens: default_prompt_cache_cap_jitter_min_tokens(),
            prompt_cache_cap_jitter_max_tokens: default_prompt_cache_cap_jitter_max_tokens(),
            prompt_cache_scale_min_input_tokens: default_prompt_cache_scale_min_input_tokens(),
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_max_entries_per_account: default_prompt_cache_max_entries_per_account(),
            prompt_cache_max_entries_global: default_prompt_cache_max_entries_global(),
            prompt_cache_entry_ttl_secs: default_prompt_cache_entry_ttl_secs(),
            prompt_cache_estimated_bytes_limit: default_prompt_cache_estimated_bytes_limit(),
            reported_usage: ReportedUsageConfig::default(),
            cache_policy: CachePolicyConfig::default(),
            defined_cache_routes: Vec::new(),
            usage_record_limit: default_usage_record_limit(),
            tool_format_debug: ToolFormatDebugConfig::default(),
            high_cache_threshold: default_high_cache_threshold(),
            default_endpoint: default_endpoint(),
            expose_proxy_warnings: default_expose_proxy_warnings(),
            external_pools: ExternalPoolsConfig::default(),
            config_path: None,
        }
    }
}

impl Config {
    /// 获取默认配置文件路径
    pub fn default_config_path() -> &'static str {
        "config.json"
    }

    /// 获取有效的 Auth Region（用于 Token 刷新）
    /// 优先使用 auth_region，未配置时回退到 region
    pub fn effective_auth_region(&self) -> &str {
        self.auth_region.as_deref().unwrap_or(&self.region)
    }

    /// 获取有效的 API Region（用于 API 请求）
    /// 优先使用 api_region，未配置时回退到 region
    pub fn effective_api_region(&self) -> &str {
        self.api_region.as_deref().unwrap_or(&self.region)
    }

    pub fn legacy_cache_route_policy_default(&self) -> CacheRoutePolicy {
        CacheRoutePolicy {
            cache_type: PromptCacheStrategyType::CurrentHighCache,
            simulation: CacheSimulationPolicy {
                enabled: true,
                target_read_ratio: self.prompt_cache_target_read_ratio,
                token_scale: self.prompt_cache_token_scale,
                max_simulated_input_tokens: self.prompt_cache_max_simulated_input_tokens,
                cap_jitter_min_tokens: self.prompt_cache_cap_jitter_min_tokens,
                cap_jitter_max_tokens: self.prompt_cache_cap_jitter_max_tokens,
                scale_min_input_tokens: self.prompt_cache_scale_min_input_tokens,
            }
            .normalized(),
            creation_control: self.prompt_cache_creation_control.normalized(),
            reported_usage: self.reported_usage.default.normalized(),
            cache_point: CachePointPolicy {
                enabled: self.kiro_cache_point_enabled,
                tools_only: self.kiro_cache_point_tools_only,
                record_plan: self.kiro_cache_point_record_plan,
            },
            bounds: CacheBoundsPolicy {
                max_entries_per_account: self.prompt_cache_max_entries_per_account,
                max_entries_global: self.prompt_cache_max_entries_global,
                entry_ttl_secs: self.prompt_cache_entry_ttl_secs,
                estimated_bytes_limit: self.prompt_cache_estimated_bytes_limit,
            },
            kiro_rs_tool: KiroRsToolCachePolicy::default(),
        }
        .normalized()
    }

    #[cfg(test)]
    pub fn cache_policy_for_path(&self, path: &str) -> ResolvedCacheRoutePolicy {
        resolve_cache_policy_for_path(
            self.legacy_cache_route_policy_default(),
            &self.reported_usage,
            &self
                .cache_policy
                .clone()
                .with_builtin_path_defaults()
                .with_legacy_defined_cache_route_defaults(&self.defined_cache_routes),
            path,
        )
    }

    /// 返回规范化后的客户端调用 API Key 列表。
    ///
    /// 历史配置只包含 `apiKey`；新配置可以额外包含 `apiKeys`。这里统一去空、trim、去重，
    /// 并保留原始顺序，保证旧主 key 仍排在第一位。
    pub fn request_api_keys(&self) -> Vec<String> {
        normalize_request_api_keys(
            self.api_key
                .iter()
                .chain(self.api_keys.iter())
                .map(String::as_str),
        )
    }

    /// Validate that observability Redis cannot share the scheduler Redis fault domain.
    ///
    /// Redis logical databases and key prefixes still execute on the same single-threaded
    /// server. Treating `redis.url` and `observabilityRedis.url` as different merely because
    /// their paths or prefixes differ would therefore preserve the scheduler/usage race this
    /// setting is intended to prevent.
    pub fn validate_redis_fault_domains(&self) -> Result<(), String> {
        let Some(observability_url) = self.observability_redis.configured_url() else {
            return Ok(());
        };
        let Some(business_url) = self.redis.configured_url() else {
            return Err(
                "observabilityRedis.url requires redis.url so the two Redis fault domains can be validated"
                    .to_string(),
            );
        };
        let business = redis_authority_key(business_url)?;
        let observability = redis_authority_key(observability_url)?;
        if business == observability {
            return Err(
                "redis.url and observabilityRedis.url must use distinct Redis authorities; changing DB or keyPrefix is not sufficient"
                    .to_string(),
            );
        }
        Ok(())
    }

    /// 按兼容格式写回客户端调用 API Key。
    ///
    /// 第一个 key 写入历史字段 `apiKey`，其余写入 `apiKeys`，这样旧版本/旧脚本仍能读取主 key。
    pub fn set_request_api_keys(&mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) {
        let mut keys =
            normalize_request_api_keys(keys.into_iter().map(|key| key.as_ref().to_string()));
        self.api_key = keys.first().cloned();
        if !keys.is_empty() {
            keys.remove(0);
        }
        self.api_keys = keys;
    }

    /// 如果数据库 runtime config 缺少客户端调用 Key，则从文件配置补齐一次。
    pub fn fill_missing_access_keys_from(&mut self, file_config: &Config) -> bool {
        let mut changed = false;
        if self.request_api_keys().is_empty() {
            let file_keys = file_config.request_api_keys();
            if !file_keys.is_empty() {
                self.set_request_api_keys(file_keys);
                changed = true;
            }
        }
        if self
            .admin_api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .is_none()
        {
            if let Some(admin_key) = file_config
                .admin_api_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
            {
                self.admin_api_key = Some(admin_key.to_string());
                changed = true;
            }
        }
        changed
    }

    /// Apply one-way runtime config migrations that keep old PgSQL configs aligned
    /// with current built-in route semantics.
    pub fn apply_runtime_config_migrations(&mut self) -> bool {
        let mut changed = self.cache_policy.migrate_builtin_no_cache_routes();
        if self.runtime_config_migration_version < 1 {
            if self.payload_guard_mode == PayloadGuardMode::Preemptive {
                self.payload_guard_mode = PayloadGuardMode::OnTooLong;
            }
            self.runtime_config_migration_version = 1;
            changed = true;
        }
        if self.runtime_config_migration_version < 2 {
            if self.prompt_steering.task_quality.prompt == LEGACY_TASK_QUALITY_PROMPT_V1 {
                self.prompt_steering.task_quality.prompt =
                    DEFAULT_TASK_QUALITY_PROMPT.trim().to_string();
            }
            self.runtime_config_migration_version = 2;
            changed = true;
        }
        if self.runtime_config_migration_version < 3 {
            if self.prompt_steering.task_quality.prompt == LEGACY_TASK_QUALITY_PROMPT_V2 {
                self.prompt_steering.task_quality.prompt =
                    DEFAULT_TASK_QUALITY_PROMPT.trim().to_string();
            }
            self.runtime_config_migration_version = 3;
            changed = true;
        }
        if self.runtime_config_migration_version < 4 {
            if self.prompt_steering.task_quality.prompt == LEGACY_TASK_QUALITY_PROMPT_V3 {
                self.prompt_steering.task_quality.prompt =
                    DEFAULT_TASK_QUALITY_PROMPT.trim().to_string();
            }
            self.runtime_config_migration_version = 4;
            changed = true;
        }
        if self.runtime_config_migration_version < 5 {
            // v0.0.108 split Redis coordinator failures out of the existing
            // capacity fallback switch and materialized the new flag as false.
            // Preserve the broad fallback intent of old external-pool configs
            // once; an operator can explicitly disable it again after v5.
            if self.external_pools.external_pools_enabled
                && self.external_pools.fallback_on_local_capacity_exhausted
                && self.external_pools.fallback_on_no_available_credentials
                && self.external_pools.fallback_on_local_transient_exhausted
                && !self.external_pools.fallback_on_scheduler_redis_degraded
            {
                self.external_pools.fallback_on_scheduler_redis_degraded = true;
            }
            // An unbounded external capacity wait can amplify a local Redis
            // outage into permanently queued requests. Zero now migrates to
            // the documented safe default; runtime also applies this bound.
            if self.external_pools.external_pool_capacity_mode == ExternalPoolCapacityMode::Wait
                && self.external_pools.external_pool_dispatch_max_wait_secs == 0
            {
                self.external_pools.external_pool_dispatch_max_wait_secs =
                    default_external_pool_dispatch_max_wait_secs();
            }
            self.runtime_config_migration_version = 5;
            changed = true;
        }
        if self.runtime_config_migration_version < 6 {
            // Both frontend defaults could persist the v3 built-in fingerprint prompt after
            // the original v4 migration had already completed. Replace only the exact built-in
            // bytes so user-edited prompts remain authoritative.
            if self.prompt_steering.task_quality.prompt == LEGACY_TASK_QUALITY_PROMPT_V3 {
                self.prompt_steering.task_quality.prompt =
                    DEFAULT_TASK_QUALITY_PROMPT.trim().to_string();
            }
            self.runtime_config_migration_version = 6;
            changed = true;
        }
        if self.runtime_config_migration_version < 7 {
            // Historical rollup compression is an offline maintenance operation. A live
            // instance can block usage writers long enough for their bounded retries to drop
            // records, so an old persisted startup toggle must not reactivate that path.
            self.postgres.compress_usage_rollups_on_start = false;
            self.runtime_config_migration_version = 7;
            changed = true;
        }
        if self.runtime_config_migration_version < 8 {
            // Risk-control bursts must fail safe. Older persisted runtime configs materialized
            // this field as false because the original circuit only fed external direct policy;
            // v8 makes it a local-account safety circuit as well.
            self.external_pools.local_pool_circuit_enabled = true;
            self.runtime_config_migration_version = 8;
            changed = true;
        }
        changed
    }

    /// 从文件加载配置
    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            // 配置文件不存在时仍应用部署环境变量；否则纯环境部署无法启用独立存储故障域。
            let mut config = Self::default();
            config.apply_env_overrides();
            config
                .validate_redis_fault_domains()
                .map_err(anyhow::Error::msg)?;
            config.config_path = Some(path.to_path_buf());
            return Ok(config);
        }

        let content = fs::read_to_string(path)?;
        let mut config: Config = serde_json::from_str(&content)?;
        config.runtime_config_migration_version = CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION;
        config.apply_env_overrides();
        config
            .validate_redis_fault_domains()
            .map_err(anyhow::Error::msg)?;
        config.config_path = Some(path.to_path_buf());
        Ok(config)
    }

    /// Reapply process-owned storage endpoints after loading the persisted runtime config.
    ///
    /// Runtime configuration lives in PostgreSQL, while deployment URLs remain process
    /// authority. Applying these overrides again prevents a stale persisted URL from selecting
    /// the wrong observability fault domain on restart.
    pub(crate) fn apply_storage_env_overrides(&mut self) {
        self.apply_env_overrides();
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(url) = std::env::var("KIRO_RS_POSTGRES_URL") {
            if !url.trim().is_empty() {
                self.postgres.url = Some(url);
            }
        }
        if let Some(parsed) = std::env::var("KIRO_RS_POSTGRES_MAX_CONNECTIONS")
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            self.postgres.max_connections = parsed.max(1);
        }
        if let Some(parsed) = std::env::var("KIRO_RS_POSTGRES_USAGE_MAX_CONNECTIONS")
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            self.postgres.usage_max_connections = parsed.max(1);
        }
        if let Ok(value) = std::env::var("KIRO_RS_POSTGRES_MIGRATE_ON_START") {
            if let Some(parsed) = parse_env_bool(&value) {
                self.postgres.migrate_on_start = parsed;
            }
        }
        if let Ok(value) = std::env::var("KIRO_RS_POSTGRES_COMPRESS_USAGE_ROLLUPS_ON_START") {
            if let Some(parsed) = parse_env_bool(&value) {
                self.postgres.compress_usage_rollups_on_start = parsed;
            }
        }
        if let Ok(url) = std::env::var("KIRO_RS_REDIS_URL") {
            if !url.trim().is_empty() {
                self.redis.url = Some(url);
            }
        }
        if let Ok(url) = std::env::var("KIRO_RS_OBSERVABILITY_REDIS_URL") {
            if !url.trim().is_empty() {
                self.observability_redis.url = Some(url);
            }
        }
        if let Ok(prefix) = std::env::var("KIRO_RS_OBSERVABILITY_REDIS_KEY_PREFIX") {
            if !prefix.trim().is_empty() {
                self.observability_redis.key_prefix = prefix;
            }
        }
    }

    /// 设置运行时配置路径元数据。数据库加载的配置没有对应文件路径。
    pub(crate) fn set_config_path_for_runtime(&mut self, path: Option<PathBuf>) {
        self.config_path = path;
    }
}

fn parse_env_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Some(true),
        "0" | "false" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observability_redis_is_optional_and_uses_its_own_prefix() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert!(config.observability_redis.url.is_none());
        assert_eq!(
            config.observability_redis.key_prefix,
            "kiro_rs:observability"
        );
        assert!(config.validate_redis_fault_domains().is_ok());
    }

    #[test]
    fn observability_redis_rejects_the_business_authority_even_with_another_db_or_prefix() {
        for observability_url in [
            "redis://cache.internal:6379/0",
            "redis://cache.internal:6379/15",
            "rediss://CACHE.INTERNAL:6379/7",
        ] {
            let mut config = Config::default();
            config.redis.url = Some("redis://cache.internal:6379/0".to_string());
            config.redis.key_prefix = "business".to_string();
            config.observability_redis.url = Some(observability_url.to_string());
            config.observability_redis.key_prefix = "observability".to_string();

            let error = config.validate_redis_fault_domains().unwrap_err();
            assert!(error.contains("distinct Redis authorities"), "{error}");
        }

        for observability_url in ["redis://localhost:6379/15", "redis://[::1]:6379/15"] {
            let mut config = Config::default();
            config.redis.url = Some("redis://127.0.0.1:6379/0".to_string());
            config.observability_redis.url = Some(observability_url.to_string());
            assert!(
                config
                    .validate_redis_fault_domains()
                    .unwrap_err()
                    .contains("distinct Redis authorities")
            );
        }
    }

    #[test]
    fn observability_redis_accepts_a_distinct_network_authority() {
        for observability_url in [
            "redis://cache.internal:6380/0",
            "redis://metrics.internal:6379/0",
        ] {
            let mut config = Config::default();
            config.redis.url = Some("redis://cache.internal:6379/0".to_string());
            config.observability_redis.url = Some(observability_url.to_string());
            assert!(
                config.validate_redis_fault_domains().is_ok(),
                "{observability_url} should be a distinct authority"
            );
        }
    }

    #[test]
    fn observability_redis_requires_a_valid_business_authority() {
        let mut missing_business = Config::default();
        missing_business.observability_redis.url =
            Some("redis://metrics.internal:6379/0".to_string());
        assert!(
            missing_business
                .validate_redis_fault_domains()
                .unwrap_err()
                .contains("requires redis.url")
        );

        let mut invalid_observability = Config::default();
        invalid_observability.redis.url = Some("redis://cache.internal:6379/0".to_string());
        invalid_observability.observability_redis.url = Some("not a URL".to_string());
        assert!(
            invalid_observability
                .validate_redis_fault_domains()
                .unwrap_err()
                .contains("absolute")
        );
    }

    #[test]
    fn default_compat_profile_is_claude_code() {
        assert_eq!(Config::default().compat_profile, CompatProfile::ClaudeCode);
    }

    #[test]
    fn default_kiro_agent_mode_strategy_preserves_vibe() {
        assert_eq!(
            Config::default().kiro_agent_mode_strategy,
            KiroAgentModeStrategy::Vibe
        );
    }

    #[test]
    fn default_prompt_cache_target_read_ratio_is_98_percent() {
        assert_eq!(Config::default().prompt_cache_target_read_ratio, 0.98);
    }

    #[test]
    fn weighted_capacity_defaults_disabled_and_costs_one_unit() {
        let config = Config::default();

        assert!(!config.weighted_capacity.enabled);
        assert_eq!(config.weighted_capacity.max_units_per_request, 8);
        assert_eq!(config.weighted_capacity.units_for_tokens(0), 1);
        assert_eq!(config.weighted_capacity.units_for_tokens(1_000_000), 1);
    }

    #[test]
    fn weighted_capacity_deserializes_missing_field_as_disabled() {
        let config: Config = serde_json::from_str("{}").unwrap();

        assert!(!config.weighted_capacity.enabled);
        assert_eq!(config.weighted_capacity.units_for_tokens(700_000), 1);
    }

    #[test]
    fn weighted_capacity_enabled_maps_token_tiers() {
        let config = WeightedCapacityConfig {
            enabled: true,
            max_units_per_request: 8,
            tiers: vec![
                WeightedCapacityTier {
                    min_tokens: 0,
                    units: 1,
                },
                WeightedCapacityTier {
                    min_tokens: 100_000,
                    units: 2,
                },
                WeightedCapacityTier {
                    min_tokens: 300_000,
                    units: 4,
                },
                WeightedCapacityTier {
                    min_tokens: 700_000,
                    units: 8,
                },
            ],
        };

        assert_eq!(config.units_for_tokens(99_999), 1);
        assert_eq!(config.units_for_tokens(100_000), 2);
        assert_eq!(config.units_for_tokens(450_000), 4);
        assert_eq!(config.units_for_tokens(1_000_000), 8);
    }

    #[test]
    fn default_runtime_controls_are_conservative() {
        let config = Config::default();

        assert_eq!(
            config.runtime_config_migration_version,
            CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION
        );
        assert_eq!(config.postgres.max_connections, 10);
        assert_eq!(config.postgres.usage_max_connections, 4);
        assert!(config.postgres.migrate_on_start);
        assert!(!config.postgres.compress_usage_rollups_on_start);
        assert_eq!(config.redis.key_prefix, "kiro_rs:local");
        assert_eq!(config.credential_rpm, Some(100));
        assert_eq!(config.credential_max_concurrent_requests, 30);
        assert_eq!(config.credential_transient_cooldown_secs, 10);
        assert_eq!(config.credential_rate_limit_cooldown_secs, 30);
        assert_eq!(config.credential_server_error_cooldown_secs, 5);
        assert_eq!(config.credential_auth_error_cooldown_secs, 10);
        assert_eq!(config.credential_cooldown_backoff_multiplier, 2.0);
        assert_eq!(config.credential_probation_secs, 30);
        assert_eq!(config.credential_max_cooldown_secs, 300);
        assert_eq!(config.credential_dispatch_max_wait_secs, 5);
        assert_eq!(config.kiro_upstream_response_timeout_secs, 180);
        assert_eq!(config.kiro_upstream_stream_idle_timeout_secs, 180);
        assert!(config.kiro_upstream_stream_retry_enabled);
        assert_eq!(config.kiro_upstream_stream_retry_max_attempts, 2);
        assert!(config.kiro_upstream_stream_retry_on_idle_timeout);
        assert!(config.kiro_upstream_stream_retry_on_read_error);
        assert!(config.kiro_upstream_stream_retry_on_status_error);
        assert_eq!(config.kiro_upstream_base_url, None);
        assert_eq!(config.credential_retry_max_attempts, 0);
        assert_eq!(config.credential_in_flight_lease_max_secs, 900);
        assert_eq!(config.dispatch_global_max_concurrent_requests, 512);
        assert_eq!(config.dispatch_max_queued_requests, 30);
        assert!(!config.weighted_capacity.enabled);
        assert_eq!(config.credential_warmup_requests, 3);
        assert_eq!(config.credential_warmup_selection_percent, 5);
        assert_eq!(config.credential_warmup_max_selection_percent, 50);
        assert!(config.payload_guard_enabled);
        assert_eq!(config.payload_guard_mode, PayloadGuardMode::OnTooLong);
        assert_eq!(config.payload_guard_max_bytes, 450 * 1024);
        assert_eq!(config.payload_guard_safety_margin_bytes, 32 * 1024);
        assert!(config.payload_guard_trim_history);
        assert!(config.payload_guard_external_enabled);
        assert!(!config.kiro_cache_point_enabled);
        assert!(config.kiro_cache_point_tools_only);
        assert!(config.kiro_cache_point_record_plan);
        assert!(config.payload_shaping.enabled);
        assert!(config.payload_shaping.truncate_historical_tool_results);
        assert_eq!(
            config.payload_shaping.historical_tool_result_max_chars,
            8_000
        );
        assert_eq!(config.payload_shaping.tool_definitions_budget_bytes, 20_000);
        assert_eq!(config.payload_shaping.web_fetch_body_max_chars, 12_000);
        assert!(!config.payload_shaping.fit_current_payload_to_budget);
        assert!(!config.payload_shaping.truncate_current_user_content);
        assert!(!config.payload_shaping.truncate_current_tool_results);
        assert_eq!(config.scheduler_selection_pressure_weight, 25.0);
        assert_eq!(config.scheduler_total_selection_weight, 0.0);
        assert_eq!(config.scheduler_top_k, 3);
        assert_eq!(config.selection_failure_sample_limit, 20);
        assert!(config.selection_failure_record_enabled);
        assert_eq!(config.prompt_cache_max_entries_per_account, 200);
        assert_eq!(config.prompt_cache_max_entries_global, 20_000);
        assert_eq!(config.prompt_cache_entry_ttl_secs, 86_400);
        assert_eq!(config.prompt_cache_estimated_bytes_limit, 256 * 1024 * 1024);
        assert!(!config.external_pools.external_pools_enabled);
        assert_eq!(
            config.external_pools.external_pool_capacity_mode,
            ExternalPoolCapacityMode::FailFast
        );
        assert_eq!(
            config.external_pools.external_pool_dispatch_max_wait_secs,
            5
        );
        assert_eq!(
            config
                .external_pools
                .external_pool_global_max_concurrent_requests,
            512
        );
        assert_eq!(config.external_pools.external_pool_max_queued_requests, 10);
        assert_eq!(config.external_pools.external_pool_retry_max_attempts, 3);
        assert_eq!(
            config.external_pools.external_pool_retry_status_codes,
            vec![408, 425, 429, 500, 502, 503, 504, 529]
        );
        assert!(config.external_pools.external_pool_retry_on_network_error);
        assert!(config.external_pools.external_pool_retry_on_protocol_error);
        assert_eq!(config.external_pools.external_pool_same_pool_retry_count, 1);
        assert_eq!(
            config
                .external_pools
                .external_pool_same_pool_retry_status_codes,
            vec![408, 425, 429, 500, 502, 503, 504, 529]
        );
        assert_eq!(
            config.external_pools.external_pool_same_pool_retry_delay_ms,
            500
        );
        assert_eq!(
            config
                .external_pools
                .external_pool_transient_failure_priority_penalty,
            20
        );
        assert_eq!(
            config.external_pools.external_pool_max_input_tokens,
            1_000_000
        );
        assert_eq!(
            config.external_pools.external_pool_stream_response_mode,
            ExternalPoolStreamResponseMode::EventPassthrough
        );
        assert!(!config.compression.enabled);
        assert!(config.compression.whitespace_compression);
        assert_eq!(
            config
                .reported_usage
                .policy_for_path("/cc/v1/messages")
                .input
                .max_tokens,
            96
        );
        let cc_policy = config.reported_usage.policy_for_path("/cc/v1/messages");
        assert_eq!(
            cc_policy.cache_creation.mode,
            ReportedUsageFieldMode::SampleTarget
        );
        assert_eq!(cc_policy.cache_creation.target_tokens, 3_000);
        assert_eq!(cc_policy.cache_creation.normal_max_multiplier, 1.2);
        let ha_policy = config.reported_usage.policy_for_path("/ha/v1/messages");
        assert_eq!(ha_policy.input.mode, ReportedUsageFieldMode::SampleMax);
        assert_eq!(ha_policy.input.max_tokens, 96);
        assert!(ha_policy.input.move_delta_to_cache_read);
        assert_eq!(
            ha_policy.cache_creation.mode,
            ReportedUsageFieldMode::Preserve
        );
        assert!(!config.reported_usage.path_overrides.contains_key("/na"));
        assert!(
            config
                .reported_usage
                .policy_for_path("/na/v1/messages")
                .enabled
        );
    }

    #[test]
    fn initial_config_deserialization_uses_scheduler_safe_defaults() {
        let config: Config = serde_json::from_str("{}").unwrap();

        assert_eq!(config.request_admission, RequestAdmissionConfig::default());
        assert_eq!(config.credential_rpm, Some(100));
        assert_eq!(config.credential_max_concurrent_requests, 30);
        assert_eq!(config.credential_dispatch_max_wait_secs, 5);
        assert_eq!(config.dispatch_global_max_concurrent_requests, 512);
        assert_eq!(config.dispatch_max_queued_requests, 30);
        assert_eq!(
            config
                .external_pools
                .external_pool_global_max_concurrent_requests,
            512
        );
        assert_eq!(config.external_pools.external_pool_max_queued_requests, 10);
        assert_eq!(config.external_pools.external_pool_retry_max_attempts, 3);
        assert_eq!(config.external_pools.external_pool_same_pool_retry_count, 1);
        assert_eq!(
            config
                .external_pools
                .external_pool_transient_failure_priority_penalty,
            20
        );
        assert_eq!(
            config.external_pools.external_pool_dispatch_max_wait_secs,
            5
        );
    }

    #[test]
    fn parse_env_bool_accepts_common_values() {
        assert_eq!(parse_env_bool("true"), Some(true));
        assert_eq!(parse_env_bool("1"), Some(true));
        assert_eq!(parse_env_bool("YES"), Some(true));
        assert_eq!(parse_env_bool("off"), Some(false));
        assert_eq!(parse_env_bool("0"), Some(false));
        assert_eq!(parse_env_bool("maybe"), None);
    }

    #[test]
    fn default_prompt_cache_token_amplification_is_configured() {
        let config = Config::default();

        assert_eq!(config.prompt_cache_token_scale, 1.6);
        assert_eq!(config.prompt_cache_max_simulated_input_tokens, 300_000);
        assert_eq!(config.prompt_cache_cap_jitter_min_tokens, 12_000);
        assert_eq!(config.prompt_cache_cap_jitter_max_tokens, 24_000);
        assert_eq!(config.prompt_cache_scale_min_input_tokens, 20_000);
    }

    #[test]
    fn compat_profile_deserializes_from_camel_case_config() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "compatProfile": "anthropic-strict",
                "kiroAgentModeStrategy": "auto"
            }"#,
        )
        .unwrap();

        assert_eq!(config.compat_profile, CompatProfile::AnthropicStrict);
        assert_eq!(config.kiro_agent_mode_strategy, KiroAgentModeStrategy::Auto);
    }

    #[test]
    fn request_api_keys_support_legacy_single_key() {
        let config: Config = serde_json::from_str(r#"{"apiKey":" sk-legacy "}"#).unwrap();

        assert_eq!(config.request_api_keys(), vec!["sk-legacy".to_string()]);
    }

    #[test]
    fn request_admission_has_conservative_defaults_and_explicit_zero_disables() {
        let fresh = Config::default();
        assert_eq!(fresh.request_admission, RequestAdmissionConfig::default());
        assert_eq!(fresh.request_admission.rpm, 300);
        assert_eq!(fresh.request_admission.max_concurrent_requests, 32);
        assert_eq!(fresh.request_admission.max_queued_requests, 64);
        assert_eq!(fresh.request_admission.queue_timeout_ms, 1_000);
        let serialized = serde_json::to_value(&fresh).unwrap();
        assert_eq!(serialized["requestAdmission"]["rpm"], 300);
        assert_eq!(
            serde_json::from_value::<Config>(serialized)
                .unwrap()
                .request_admission,
            fresh.request_admission
        );

        let legacy: Config = serde_json::from_str(r#"{"apiKey":"sk-legacy"}"#).unwrap();
        assert_eq!(legacy.request_admission, RequestAdmissionConfig::default());

        let legacy_null: Config =
            serde_json::from_str(r#"{"apiKey":"sk-legacy","requestAdmission":null}"#).unwrap();
        assert_eq!(
            legacy_null.request_admission,
            RequestAdmissionConfig::default()
        );

        let partial: Config = serde_json::from_str(
            r#"{
                "apiKey":"sk-test",
                "requestAdmission": { "rpm": 0 }
            }"#,
        )
        .unwrap();
        assert_eq!(partial.request_admission.rpm, 0);
        assert_eq!(partial.request_admission.max_concurrent_requests, 32);
        assert_eq!(partial.request_admission.max_queued_requests, 64);
        assert_eq!(partial.request_admission.queue_timeout_ms, 1_000);
        assert!(partial.request_admission.normalized().enabled());

        let queue_only: Config = serde_json::from_str(
            r#"{
                "requestAdmission": {
                    "rpm": 0,
                    "maxConcurrentRequests": 0,
                    "maxQueuedRequests": 64,
                    "queueTimeoutMs": 1000
                }
            }"#,
        )
        .unwrap();
        assert!(!queue_only.request_admission.enabled());
        assert_eq!(
            queue_only.request_admission.normalized(),
            RequestAdmissionConfig::disabled()
        );

        let partial_queue_disabled: Config = serde_json::from_str(
            r#"{
                "requestAdmission": {
                    "rpm": 300,
                    "maxConcurrentRequests": 32,
                    "maxQueuedRequests": 0
                }
            }"#,
        )
        .unwrap();
        assert_eq!(
            partial_queue_disabled.request_admission.queue_timeout_ms,
            1_000
        );
        assert_eq!(
            partial_queue_disabled
                .request_admission
                .normalized()
                .queue_timeout_ms,
            0
        );

        let disabled: Config = serde_json::from_str(
            r#"{
                "apiKey":"sk-test",
                "requestAdmission": {
                    "rpm": 0,
                    "maxConcurrentRequests": 0,
                    "maxQueuedRequests": 0,
                    "queueTimeoutMs": 0
                }
            }"#,
        )
        .unwrap();
        assert_eq!(
            disabled.request_admission,
            RequestAdmissionConfig::disabled()
        );
        assert!(!disabled.request_admission.enabled());
        assert_eq!(
            disabled.request_admission.normalized(),
            disabled.request_admission
        );
    }

    #[test]
    fn request_admission_rejects_admin_values_above_hard_bounds() {
        let mut config = RequestAdmissionConfig::default();
        config.rpm = MAX_REQUEST_API_KEY_RPM + 1;
        assert!(config.validate().is_err());
        assert_eq!(config.normalized().rpm, MAX_REQUEST_API_KEY_RPM);

        let mut config = RequestAdmissionConfig::default();
        config.max_concurrent_requests = MAX_REQUEST_API_KEY_CONCURRENT_REQUESTS + 1;
        assert!(config.validate().is_err());

        let mut config = RequestAdmissionConfig::default();
        config.max_queued_requests = MAX_REQUEST_API_KEY_QUEUED_REQUESTS + 1;
        assert!(config.validate().is_err());

        let mut config = RequestAdmissionConfig::default();
        config.queue_timeout_ms = MAX_REQUEST_API_KEY_QUEUE_TIMEOUT_MS + 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn request_api_keys_merge_and_deduplicate_primary_and_extra_keys() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-primary",
                "apiKeys": ["sk-extra", "sk-primary", "", " sk-third "]
            }"#,
        )
        .unwrap();

        assert_eq!(
            config.request_api_keys(),
            vec![
                "sk-primary".to_string(),
                "sk-extra".to_string(),
                "sk-third".to_string()
            ]
        );
    }

    #[test]
    fn set_request_api_keys_preserves_legacy_primary_field() {
        let mut config = Config::default();
        config.set_request_api_keys([" sk-a ", "sk-b", "sk-a"]);

        assert_eq!(config.api_key.as_deref(), Some("sk-a"));
        assert_eq!(config.api_keys, vec!["sk-b".to_string()]);
    }

    #[test]
    fn fill_missing_access_keys_from_file_does_not_override_existing_database_keys() {
        let mut database = Config::default();
        database.set_request_api_keys(["sk-db"]);
        database.admin_api_key = Some("sk-admin-db".to_string());
        let mut file = Config::default();
        file.set_request_api_keys(["sk-file"]);
        file.admin_api_key = Some("sk-admin-file".to_string());

        assert!(!database.fill_missing_access_keys_from(&file));
        assert_eq!(database.request_api_keys(), vec!["sk-db".to_string()]);
        assert_eq!(database.admin_api_key.as_deref(), Some("sk-admin-db"));
    }

    #[test]
    fn fill_missing_access_keys_from_file_initializes_historical_database_config() {
        let mut database = Config::default();
        let mut file = Config::default();
        file.set_request_api_keys(["sk-file", "sk-extra"]);
        file.admin_api_key = Some("sk-admin-file".to_string());

        assert!(database.fill_missing_access_keys_from(&file));
        assert_eq!(
            database.request_api_keys(),
            vec!["sk-file".to_string(), "sk-extra".to_string()]
        );
        assert_eq!(database.admin_api_key.as_deref(), Some("sk-admin-file"));
    }

    #[test]
    fn external_pool_capacity_mode_deserializes_with_compatible_defaults() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "externalPools": {
                    "externalPoolsEnabled": true,
                    "externalPoolMaxQueuedRequests": 25
                }
            }"#,
        )
        .unwrap();

        assert!(config.external_pools.external_pools_enabled);
        assert!(
            config.external_pools.fallback_on_scheduler_redis_degraded,
            "a config that predates the dedicated flag must retain legacy capacity fallback"
        );
        assert_eq!(
            config.external_pools.external_pool_capacity_mode,
            ExternalPoolCapacityMode::FailFast
        );
        assert_eq!(
            config.external_pools.external_pool_dispatch_max_wait_secs,
            5
        );
        assert_eq!(
            config.external_pools.external_pool_request_timeout_secs,
            180
        );
        assert_eq!(
            config
                .external_pools
                .external_pool_stream_request_timeout_secs,
            0
        );
        assert_eq!(
            config.external_pools.external_pool_stream_idle_timeout_secs,
            180
        );
        assert!(config.external_pools.external_pool_local_rescue_enabled);
        assert!(
            config
                .external_pools
                .external_pool_local_rescue_on_rate_limit
        );
        assert!(config.external_pools.external_pool_local_rescue_on_timeout);
        assert!(config.external_pools.external_pool_local_rescue_on_capacity);
        assert_eq!(
            config
                .external_pools
                .external_pool_local_rescue_max_wait_secs,
            15
        );
        assert!(
            config
                .external_pools
                .external_pool_auto_disable_on_channel_disabled
        );
        assert_eq!(config.external_pools.external_pool_max_queued_requests, 25);
        assert_eq!(
            config.external_pools.external_pool_stream_response_mode,
            ExternalPoolStreamResponseMode::EventPassthrough
        );
        assert_eq!(
            config
                .external_pools
                .external_pool_model_unavailable_cooldown_mode,
            ExternalPoolModelUnavailableCooldownMode::Model
        );
        assert_eq!(
            config
                .external_pools
                .external_pool_model_unavailable_cooldown_secs,
            10
        );

        let wait_config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "externalPools": {
                    "externalPoolCapacityMode": "wait",
                    "externalPoolDispatchMaxWaitSecs": 3,
                    "externalPoolModelUnavailableCooldownMode": "pool",
                    "externalPoolModelUnavailableCooldownSecs": 7
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            wait_config.external_pools.external_pool_capacity_mode,
            ExternalPoolCapacityMode::Wait
        );
        assert_eq!(
            wait_config
                .external_pools
                .external_pool_dispatch_max_wait_secs,
            3
        );
        assert_eq!(
            wait_config
                .external_pools
                .external_pool_model_unavailable_cooldown_mode,
            ExternalPoolModelUnavailableCooldownMode::Pool
        );
        assert_eq!(
            wait_config
                .external_pools
                .external_pool_model_unavailable_cooldown_secs,
            7
        );
    }

    #[test]
    fn external_pool_scheduler_redis_fallback_preserves_explicit_boolean_values() {
        let missing: Config = serde_json::from_value(serde_json::json!({
            "runtimeConfigMigrationVersion": CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION,
            "externalPools": {
                "externalPoolsEnabled": true
            }
        }))
        .unwrap();
        assert!(missing.external_pools.fallback_on_scheduler_redis_degraded);

        let explicit_false: Config = serde_json::from_value(serde_json::json!({
            "runtimeConfigMigrationVersion": CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION,
            "externalPools": {
                "externalPoolsEnabled": true,
                "fallbackOnSchedulerRedisDegraded": false
            }
        }))
        .unwrap();
        assert!(
            !explicit_false
                .external_pools
                .fallback_on_scheduler_redis_degraded
        );

        let explicit_true: Config = serde_json::from_value(serde_json::json!({
            "runtimeConfigMigrationVersion": CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION,
            "externalPools": {
                "externalPoolsEnabled": true,
                "fallbackOnSchedulerRedisDegraded": true
            }
        }))
        .unwrap();
        assert!(
            explicit_true
                .external_pools
                .fallback_on_scheduler_redis_degraded
        );
    }

    #[test]
    fn external_pool_capacity_wait_is_always_bounded() {
        let mut config = ExternalPoolsConfig::default();
        assert_eq!(config.effective_dispatch_max_wait_secs(), 5);

        config.external_pool_dispatch_max_wait_secs = 0;
        assert_eq!(config.effective_dispatch_max_wait_secs(), 5);

        config.external_pool_dispatch_max_wait_secs = 7;
        assert_eq!(config.effective_dispatch_max_wait_secs(), 7);
    }

    #[test]
    fn external_pool_route_policy_defaults_to_allow_all() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "externalPools": {
                    "externalPoolsEnabled": true
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            config.external_pools.external_pool_route_mode,
            ExternalPoolRouteMode::AllowAll
        );
        assert!(
            config
                .external_pools
                .external_pool_route_allowed("/cc/v1/messages")
        );
        assert!(
            config
                .external_pools
                .external_pool_route_allowed("/dfcache/team-a/v1/messages")
        );
    }

    #[test]
    fn external_pool_route_policy_denies_matching_routes_only() {
        let mut config = ExternalPoolsConfig {
            external_pool_route_mode: ExternalPoolRouteMode::DenyList,
            external_pool_route_rules: vec!["/cc".to_string(), "/dfcache/team-a".to_string()],
            ..ExternalPoolsConfig::default()
        };

        assert!(!config.external_pool_route_allowed("/cc/v1/messages"));
        assert!(!config.external_pool_route_allowed("/dfcache/team-a/v1/messages"));
        assert!(config.external_pool_route_allowed("/v1/messages"));

        config.external_pool_route_rules = vec!["*".to_string()];
        assert!(!config.external_pool_route_allowed("/v1/messages"));
        assert!(!config.external_pool_route_allowed("/ha/v1/messages"));
    }

    #[test]
    fn external_pool_route_policy_allow_list_requires_a_match() {
        let config = ExternalPoolsConfig {
            external_pool_route_mode: ExternalPoolRouteMode::AllowList,
            external_pool_route_rules: vec!["/ha".to_string(), "/dfcache/team-b".to_string()],
            ..ExternalPoolsConfig::default()
        };

        assert!(config.external_pool_route_allowed("/ha/v1/messages"));
        assert!(config.external_pool_route_allowed("/dfcache/team-b/v1/messages"));
        assert!(!config.external_pool_route_allowed("/cc/v1/messages"));
        assert!(!config.external_pool_route_allowed("/v1/messages"));
    }

    #[test]
    fn external_pool_route_policy_matches_case_insensitively() {
        let config = ExternalPoolsConfig {
            external_pool_route_mode: ExternalPoolRouteMode::AllowList,
            external_pool_route_rules: vec!["/CC/V1".to_string()],
            ..ExternalPoolsConfig::default()
        };

        assert!(config.external_pool_route_allowed("/cc/v1/messages"));
        assert!(!config.external_pool_route_allowed("/ha/v1/messages"));
    }

    #[test]
    fn external_pool_route_policy_does_not_match_mid_path_segments() {
        let config = ExternalPoolsConfig {
            external_pool_route_mode: ExternalPoolRouteMode::AllowList,
            external_pool_route_rules: vec!["/v1".to_string()],
            ..ExternalPoolsConfig::default()
        };

        assert!(config.external_pool_route_allowed("/v1/messages"));
        assert!(!config.external_pool_route_allowed("/cc/v1/messages"));
        assert!(!config.external_pool_route_allowed("/ha/v1/messages"));
    }

    #[test]
    fn external_pool_stream_response_mode_accepts_legacy_capture_value() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "externalPools": {
                    "externalPoolStreamResponseMode": "event_passthrough_capture"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            config.external_pools.external_pool_stream_response_mode,
            ExternalPoolStreamResponseMode::EventPassthrough
        );
    }

    #[test]
    fn reported_usage_deserializes_from_camel_case_config() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "reportedUsage": {
                    "default": {
                        "finalCacheReadMaxTokens": 800000,
                        "finalCacheCreationMaxTokens": 500000,
                        "input": { "mode": "raw" }
                    },
	                    "pathOverrides": {
	                        "cc": {
	                            "finalCacheReadMaxTokens": 300000,
	                            "finalCacheReadJitterMinTokens": 8000,
	                            "finalCacheReadJitterMaxTokens": 24000,
	                            "finalCacheCreationMaxTokens": 180000,
	                            "finalCacheCreationJitterMinTokens": 9000,
	                            "finalCacheCreationJitterMaxTokens": 21000,
	                            "finalOutputGuardEnabled": false,
	                            "outputUpliftMinTokens": 1000,
	                            "outputUpliftPercent": 50,
	                            "finalOutputMaxTokens": 200000,
	                            "finalOutputJitterMinTokens": 5000,
	                            "finalOutputJitterMaxTokens": 12000,
	                            "input": {
	                                "mode": "sample-max",
	                                "maxTokens": 64,
	                                "moveDeltaToCacheRead": true
                            },
                            "cacheCreation": {
                                "mode": "sample-target",
                                "targetTokens": 2048
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            config
                .reported_usage
                .policy_for_path("/v1/messages")
                .input
                .mode,
            ReportedUsageFieldMode::Raw
        );
        assert_eq!(
            config
                .reported_usage
                .policy_for_path("/v1/messages")
                .final_cache_read_max_tokens,
            800_000
        );
        assert_eq!(
            config
                .reported_usage
                .policy_for_path("/v1/messages")
                .final_cache_creation_max_tokens,
            500_000
        );
        let policy = config.reported_usage.policy_for_path("/cc/v1/messages");
        assert_eq!(policy.final_cache_read_max_tokens, 300_000);
        assert_eq!(policy.final_cache_read_jitter_min_tokens, 8_000);
        assert_eq!(policy.final_cache_read_jitter_max_tokens, 24_000);
        assert_eq!(policy.final_cache_creation_max_tokens, 180_000);
        assert_eq!(policy.final_cache_creation_jitter_min_tokens, 9_000);
        assert_eq!(policy.final_cache_creation_jitter_max_tokens, 21_000);
        assert!(!policy.final_output_guard_enabled);
        assert_eq!(policy.output_uplift_min_tokens, 1_000);
        assert_eq!(policy.output_uplift_percent, 50);
        assert_eq!(policy.final_output_max_tokens, 200_000);
        assert_eq!(policy.final_output_jitter_min_tokens, 5_000);
        assert_eq!(policy.final_output_jitter_max_tokens, 12_000);
        assert_eq!(policy.input.mode, ReportedUsageFieldMode::SampleMax);
        assert_eq!(policy.input.max_tokens, 64);
        assert!(policy.input.move_delta_to_cache_read);
        assert_eq!(
            policy.cache_creation.mode,
            ReportedUsageFieldMode::SampleTarget
        );
        assert_eq!(policy.cache_creation.target_tokens, 2048);
    }

    #[test]
    fn prompt_cache_token_amplification_deserializes_from_camel_case_config() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "promptCacheTokenScale": 1.5,
                "promptCacheMaxSimulatedInputTokens": 200000,
                "promptCacheCapJitterMinTokens": 5000,
                "promptCacheCapJitterMaxTokens": 20000,
                "promptCacheScaleMinInputTokens": 10000
            }"#,
        )
        .unwrap();

        assert_eq!(config.prompt_cache_token_scale, 1.5);
        assert_eq!(config.prompt_cache_max_simulated_input_tokens, 200_000);
        assert_eq!(config.prompt_cache_cap_jitter_min_tokens, 5_000);
        assert_eq!(config.prompt_cache_cap_jitter_max_tokens, 20_000);
        assert_eq!(config.prompt_cache_scale_min_input_tokens, 10_000);
    }

    #[test]
    fn prompt_cache_bounds_and_selection_diagnostics_deserialize_from_camel_case_config() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "selectionFailureSampleLimit": 12,
                "selectionFailureRecordEnabled": false,
                "kiroUpstreamStreamIdleTimeoutSecs": 45,
                "kiroCachePointEnabled": true,
                "kiroCachePointToolsOnly": false,
                "kiroCachePointRecordPlan": false,
                "promptCacheMaxEntriesPerAccount": 50,
                "promptCacheMaxEntriesGlobal": 500,
                "promptCacheEntryTtlSecs": 600,
                "promptCacheEstimatedBytesLimit": 1048576
            }"#,
        )
        .unwrap();

        assert_eq!(config.selection_failure_sample_limit, 12);
        assert!(!config.selection_failure_record_enabled);
        assert_eq!(config.kiro_upstream_stream_idle_timeout_secs, 45);
        assert!(config.kiro_cache_point_enabled);
        assert!(!config.kiro_cache_point_tools_only);
        assert!(!config.kiro_cache_point_record_plan);
        assert_eq!(config.prompt_cache_max_entries_per_account, 50);
        assert_eq!(config.prompt_cache_max_entries_global, 500);
        assert_eq!(config.prompt_cache_entry_ttl_secs, 600);
        assert_eq!(config.prompt_cache_estimated_bytes_limit, 1_048_576);
    }

    #[test]
    fn payload_shaping_patch_preserves_unspecified_fields() {
        let base = PayloadShapingConfig {
            enabled: true,
            truncate_historical_tool_results: true,
            historical_tool_result_max_chars: 12_345,
            historical_tool_result_head_lines: 12,
            historical_tool_result_tail_lines: 6,
            discard_historical_thinking: true,
            compress_tool_definitions: true,
            tool_definitions_budget_bytes: 44_000,
            tool_description_max_chars: 3_333,
            tool_schema_annotation_max_chars: 444,
            web_fetch_trim_enabled: true,
            web_fetch_body_max_chars: 8_888,
            fit_current_payload_to_budget: false,
            truncate_current_tool_results: false,
            current_tool_result_max_chars: 77_000,
            truncate_current_user_content: false,
            current_user_content_max_chars: 88_000,
            truncate_current_documents: false,
            current_document_max_chars: 99_000,
            truncate_current_images: false,
            current_images_max_bytes: 111_000,
            oversized_image_handling: OversizedImageHandling::DropWithPlaceholder,
        };

        let patch: PayloadShapingConfigPatch = serde_json::from_value(serde_json::json!({
            "fitCurrentPayloadToBudget": true,
            "currentUserContentMaxChars": 4096,
            "compressToolDefinitions": false
        }))
        .expect("patch");
        let merged = patch.apply_to(base);

        assert!(merged.fit_current_payload_to_budget);
        assert_eq!(merged.current_user_content_max_chars, 4_096);
        assert!(!merged.compress_tool_definitions);
        assert_eq!(merged.historical_tool_result_max_chars, 12_345);
        assert_eq!(merged.tool_definitions_budget_bytes, 44_000);
        assert_eq!(merged.current_images_max_bytes, 111_000);
        assert_eq!(
            merged.oversized_image_handling,
            OversizedImageHandling::DropWithPlaceholder
        );
    }

    #[test]
    fn tool_format_debug_normalization_caps_buffer_memory() {
        let config = ToolFormatDebugConfig {
            channel_capacity: 65_536,
            max_record_bytes: 4 * 1024 * 1024,
            max_records_per_fingerprint: u32::MAX,
            max_records_per_group: u32::MAX,
            max_records_global: u32::MAX,
            max_request_body_records_per_window: u32::MAX,
            ..ToolFormatDebugConfig::default()
        }
        .normalized();

        assert_eq!(config.max_record_bytes, 1024 * 1024);
        assert_eq!(config.channel_capacity, 64);
        assert_eq!(config.max_records_per_fingerprint, 1_000);
        assert_eq!(config.max_records_per_group, 10_000);
        assert_eq!(config.max_records_global, 10_000);
        assert_eq!(config.max_request_body_records_per_window, 100);
    }

    #[test]
    fn reported_usage_default_and_prefix_overrides_match_longest_prefix() {
        let mut reported_usage = ReportedUsageConfig::default();
        reported_usage.path_overrides.insert(
            "/cc/v1".to_string(),
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(32),
                ..ReportedUsagePathPolicy::default()
            },
        );

        assert_eq!(
            reported_usage.policy_for_path("/v1/messages").input.mode,
            ReportedUsageFieldMode::Raw
        );
        assert_eq!(
            reported_usage
                .policy_for_path("/v1/messages")
                .final_cache_read_max_tokens,
            700_000
        );
        assert_eq!(
            reported_usage
                .policy_for_path("/cc/v1/messages")
                .input
                .max_tokens,
            32
        );
        assert_eq!(
            reported_usage.policy_for_path("/cc/other").input.max_tokens,
            96
        );
        assert!(!reported_usage.path_overrides.contains_key("/na"));
        assert!(reported_usage.policy_for_path("/na/v1/messages").enabled);
    }

    #[test]
    fn defined_cache_routes_normalize_and_reject_unsafe_paths() {
        assert_eq!(
            normalize_defined_cache_route("dfcache/CC"),
            Some("/dfcache/cc".to_string())
        );
        assert_eq!(
            normalize_defined_cache_routes(&[
                "/dfcache/aa".to_string(),
                "/dfcache/aa/".to_string(),
                "dfcache/bb".to_string(),
            ]),
            vec!["/dfcache/aa".to_string(), "/dfcache/bb".to_string()]
        );
        assert_eq!(normalize_defined_cache_route("/cc"), None);
        let legacy_route = ["/define", "cache/aa"].concat();
        assert_eq!(normalize_defined_cache_route(&legacy_route), None);
        assert_eq!(normalize_defined_cache_route("/dfcache/a/b"), None);
        assert_eq!(normalize_defined_cache_route("/dfcache/a?b"), None);
    }

    #[test]
    fn cache_policy_resolves_legacy_and_path_overrides_in_order() {
        let mut config = Config::default();
        config.cache_policy.default.simulation = Some(CacheSimulationPolicyPatch {
            target_read_ratio: Some(0.91),
            ..CacheSimulationPolicyPatch::default()
        });
        config.cache_policy.path_overrides.insert(
            "/cc".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::KiroRsTool),
                simulation: Some(CacheSimulationPolicyPatch {
                    enabled: Some(false),
                    token_scale: Some(1.2),
                    ..CacheSimulationPolicyPatch::default()
                }),
                reported_usage: Some(ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(48),
                    ..ReportedUsagePathPolicy::default()
                }),
                cache_point: Some(CachePointPolicyPatch {
                    enabled: Some(true),
                    ..CachePointPolicyPatch::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );

        let cc = config.cache_policy_for_path("/cc/v1/messages");

        assert_eq!(cc.namespace.as_deref(), Some("/cc"));
        assert_eq!(cc.policy.cache_type, PromptCacheStrategyType::KiroRsTool);
        assert!(!cc.policy.simulation.enabled);
        assert_eq!(cc.policy.simulation.token_scale, 1.0);
        assert_eq!(cc.policy.simulation.target_read_ratio, 0.98);
        assert!(cc.policy.reported_usage.enabled);
        assert_eq!(cc.policy.reported_usage.input.max_tokens, 48);
        assert!(cc.policy.cache_point.enabled);

        let ha = config.cache_policy_for_path("/ha/v1/messages");
        assert_eq!(ha.namespace, None);
        assert_eq!(
            ha.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert!(ha.policy.simulation.enabled);
        assert_eq!(ha.policy.simulation.target_read_ratio, 0.91);
        assert_eq!(ha.policy.reported_usage.input.max_tokens, 96);

        let na = config.cache_policy_for_path("/na/v1/messages");
        assert_eq!(na.namespace, None);
        assert_eq!(na.policy.cache_type, PromptCacheStrategyType::NoCache);
        assert!(!na.policy.simulation.enabled);
        assert!(!na.policy.creation_control.enabled);
        assert!(!na.policy.cache_point.enabled);

        let unknown = config.cache_policy_for_path("/reports/v1/messages");
        assert_eq!(unknown.namespace, None);
        assert_eq!(unknown.policy.cache_type, PromptCacheStrategyType::NoCache);
        assert!(!unknown.policy.simulation.enabled);
    }

    #[test]
    fn cache_policy_parameter_matrix_applies_each_strategy_semantics() {
        let mut config = Config::default();
        config.cache_policy.default = CacheRoutePolicyPatch {
            simulation: Some(CacheSimulationPolicyPatch {
                target_read_ratio: Some(0.61),
                token_scale: Some(1.1),
                max_simulated_input_tokens: Some(161_000),
                cap_jitter_min_tokens: Some(1_000),
                cap_jitter_max_tokens: Some(9_000),
                scale_min_input_tokens: Some(6_000),
                ..CacheSimulationPolicyPatch::default()
            }),
            creation_control: Some(PromptCacheCreationControlConfig {
                enabled: true,
                scope_mode: PromptCacheCreationControlScopeMode::ConversationModel,
                min_successful_requests_between_creation: 5,
                min_creation_interval_secs: 55,
                min_creation_delta_tokens: 15_000,
                max_creation_tokens_per_event: 25_000,
                creation_budget_window_secs: 155,
                max_creation_tokens_per_window: 75_000,
                expire_after_idle_secs: 1_555,
            }),
            reported_usage: Some(ReportedUsagePathPolicy {
                output: ReportedUsageFieldPolicy::sample_max(222),
                ..ReportedUsagePathPolicy::default()
            }),
            cache_point: Some(CachePointPolicyPatch {
                enabled: Some(true),
                tools_only: Some(true),
                record_plan: Some(true),
            }),
            bounds: Some(CacheBoundsPolicyPatch {
                max_entries_per_account: Some(11),
                max_entries_global: Some(111),
                entry_ttl_secs: Some(1_111),
                estimated_bytes_limit: Some(11_111),
            }),
            ..CacheRoutePolicyPatch::default()
        };
        config.cache_policy.current_high_cache = CacheRoutePolicyPatch {
            simulation: Some(CacheSimulationPolicyPatch {
                token_scale: Some(1.7),
                cap_jitter_min_tokens: Some(2_000),
                ..CacheSimulationPolicyPatch::default()
            }),
            cache_point: Some(CachePointPolicyPatch {
                tools_only: Some(false),
                ..CachePointPolicyPatch::default()
            }),
            ..CacheRoutePolicyPatch::default()
        };
        config.cache_policy.path_overrides.insert(
            "/matrix".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                route_namespace: None,
                simulation: Some(CacheSimulationPolicyPatch {
                    enabled: Some(true),
                    target_read_ratio: Some(0.77),
                    max_simulated_input_tokens: Some(177_000),
                    scale_min_input_tokens: Some(17_000),
                    ..CacheSimulationPolicyPatch::default()
                }),
                creation_control: Some(PromptCacheCreationControlConfig {
                    enabled: true,
                    scope_mode: PromptCacheCreationControlScopeMode::CredentialConversationModel,
                    min_successful_requests_between_creation: 3,
                    min_creation_interval_secs: 33,
                    min_creation_delta_tokens: 13_000,
                    max_creation_tokens_per_event: 23_000,
                    creation_budget_window_secs: 133,
                    max_creation_tokens_per_window: 73_000,
                    expire_after_idle_secs: 1_333,
                }),
                reported_usage: Some(ReportedUsagePathPolicy {
                    final_cache_read_max_tokens: 333_000,
                    input: ReportedUsageFieldPolicy::sample_input_max(333),
                    output: ReportedUsageFieldPolicy::sample_target(444),
                    cache_read: ReportedUsageFieldPolicy::sample_max(55_000),
                    cache_creation: ReportedUsageFieldPolicy::sample_target_with_multiplier(
                        6_000, 1.4,
                    ),
                    ..ReportedUsagePathPolicy::default()
                }),
                cache_point: Some(CachePointPolicyPatch {
                    enabled: Some(true),
                    tools_only: Some(false),
                    record_plan: Some(false),
                }),
                bounds: Some(CacheBoundsPolicyPatch {
                    max_entries_per_account: Some(7),
                    max_entries_global: Some(70),
                    entry_ttl_secs: Some(700),
                    estimated_bytes_limit: Some(7_000),
                }),
                kiro_rs_tool: Some(KiroRsToolCachePolicyPatch {
                    coverage_ratio: Some(0.7),
                    max_coverage_tokens: Some(70_000),
                    incremental_create_enabled: Some(false),
                    max_new_creation_tokens_per_request: Some(7_000),
                    cache_current_user_stable_prefix: Some(true),
                    current_user_stable_prefix_max_tokens: Some(700),
                    ..Default::default()
                }),
            },
        );
        config.cache_policy.path_overrides.insert(
            "/plain".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::NoCache),
                route_namespace: None,
                simulation: Some(CacheSimulationPolicyPatch {
                    enabled: Some(true),
                    target_read_ratio: Some(0.88),
                    token_scale: Some(2.0),
                    ..CacheSimulationPolicyPatch::default()
                }),
                creation_control: Some(PromptCacheCreationControlConfig::default()),
                reported_usage: Some(ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(111),
                    output: ReportedUsageFieldPolicy::sample_max(22),
                    ..ReportedUsagePathPolicy::default()
                }),
                cache_point: Some(CachePointPolicyPatch {
                    enabled: Some(true),
                    tools_only: Some(false),
                    record_plan: Some(true),
                }),
                bounds: Some(CacheBoundsPolicyPatch {
                    max_entries_per_account: Some(2),
                    max_entries_global: Some(20),
                    entry_ttl_secs: Some(200),
                    estimated_bytes_limit: Some(2_000),
                }),
                kiro_rs_tool: Some(KiroRsToolCachePolicyPatch {
                    coverage_ratio: Some(0.2),
                    ..KiroRsToolCachePolicyPatch::default()
                }),
            },
        );
        config.cache_policy.path_overrides.insert(
            "/tool-matrix".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::KiroRsTool),
                route_namespace: None,
                simulation: Some(CacheSimulationPolicyPatch {
                    enabled: Some(true),
                    token_scale: Some(2.5),
                    ..CacheSimulationPolicyPatch::default()
                }),
                creation_control: Some(PromptCacheCreationControlConfig::default()),
                reported_usage: Some(ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(123),
                    ..ReportedUsagePathPolicy::default()
                }),
                cache_point: Some(CachePointPolicyPatch {
                    enabled: Some(true),
                    tools_only: Some(false),
                    record_plan: Some(true),
                }),
                bounds: Some(CacheBoundsPolicyPatch {
                    max_entries_per_account: Some(5),
                    max_entries_global: Some(50),
                    entry_ttl_secs: Some(500),
                    estimated_bytes_limit: Some(5_000),
                }),
                kiro_rs_tool: Some(KiroRsToolCachePolicyPatch {
                    coverage_ratio: Some(0.5),
                    max_coverage_tokens: Some(50_000),
                    incremental_create_enabled: Some(false),
                    max_new_creation_tokens_per_request: Some(5_000),
                    cache_current_user_stable_prefix: Some(true),
                    current_user_stable_prefix_max_tokens: Some(500),
                    ..Default::default()
                }),
            },
        );

        let high = config.cache_policy_for_path("/matrix/v1/messages");
        assert_eq!(high.namespace.as_deref(), Some("/matrix"));
        assert_eq!(
            high.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert!(high.policy.simulation.enabled);
        assert_eq!(high.policy.simulation.target_read_ratio, 0.77);
        assert_eq!(high.policy.simulation.token_scale, 1.7);
        assert_eq!(high.policy.simulation.max_simulated_input_tokens, 177_000);
        assert_eq!(high.policy.simulation.cap_jitter_min_tokens, 2_000);
        assert_eq!(high.policy.simulation.cap_jitter_max_tokens, 9_000);
        assert_eq!(high.policy.simulation.scale_min_input_tokens, 17_000);
        assert_eq!(
            high.policy.creation_control.scope_mode,
            PromptCacheCreationControlScopeMode::CredentialConversationModel
        );
        assert_eq!(
            high.policy
                .creation_control
                .min_successful_requests_between_creation,
            3
        );
        assert_eq!(high.policy.creation_control.min_creation_interval_secs, 33);
        assert_eq!(
            high.policy.creation_control.min_creation_delta_tokens,
            13_000
        );
        assert_eq!(
            high.policy.creation_control.max_creation_tokens_per_event,
            23_000
        );
        assert_eq!(
            high.policy.creation_control.creation_budget_window_secs,
            133
        );
        assert_eq!(
            high.policy.creation_control.max_creation_tokens_per_window,
            73_000
        );
        assert_eq!(high.policy.creation_control.expire_after_idle_secs, 1_333);
        assert_eq!(
            high.policy.reported_usage.input.mode,
            ReportedUsageFieldMode::SampleMax
        );
        assert_eq!(high.policy.reported_usage.input.max_tokens, 333);
        assert_eq!(
            high.policy.reported_usage.output.mode,
            ReportedUsageFieldMode::SampleTarget
        );
        assert_eq!(high.policy.reported_usage.output.target_tokens, 444);
        assert_eq!(high.policy.reported_usage.cache_read.max_tokens, 55_000);
        assert_eq!(
            high.policy.reported_usage.cache_creation.target_tokens,
            6_000
        );
        assert_eq!(
            high.policy
                .reported_usage
                .cache_creation
                .normal_max_multiplier,
            1.4
        );
        assert_eq!(
            high.policy.reported_usage.final_cache_read_max_tokens,
            333_000
        );
        assert!(high.policy.cache_point.enabled);
        assert!(!high.policy.cache_point.tools_only);
        assert!(!high.policy.cache_point.record_plan);
        assert_eq!(high.policy.bounds.max_entries_per_account, 7);
        assert_eq!(high.policy.bounds.max_entries_global, 70);
        assert_eq!(high.policy.bounds.entry_ttl_secs, 700);
        assert_eq!(high.policy.bounds.estimated_bytes_limit, 7_000);
        assert_eq!(high.policy.kiro_rs_tool.coverage_ratio, 0.7);
        assert_eq!(high.policy.kiro_rs_tool.max_coverage_tokens, 70_000);
        assert!(!high.policy.kiro_rs_tool.incremental_create_enabled);
        assert_eq!(
            high.policy.kiro_rs_tool.max_new_creation_tokens_per_request,
            7_000
        );
        assert!(high.policy.kiro_rs_tool.cache_current_user_stable_prefix);
        assert_eq!(
            high.policy
                .kiro_rs_tool
                .current_user_stable_prefix_max_tokens,
            700
        );

        let plain = config.cache_policy_for_path("/plain/v1/messages");
        assert_eq!(plain.namespace, None);
        assert_eq!(plain.policy.cache_type, PromptCacheStrategyType::NoCache);
        assert!(!plain.policy.simulation.enabled);
        assert_ne!(plain.policy.simulation.token_scale, 2.0);
        assert!(!plain.policy.creation_control.enabled);
        assert!(!plain.policy.cache_point.enabled);
        assert!(!plain.policy.reported_usage.enabled);
        assert_eq!(
            plain.policy.reported_usage.input.mode,
            ReportedUsageFieldMode::Raw
        );
        assert_eq!(
            plain.policy.reported_usage.output.mode,
            ReportedUsageFieldMode::Raw
        );
        assert_ne!(plain.policy.bounds.max_entries_per_account, 2);
        assert_ne!(plain.policy.kiro_rs_tool.coverage_ratio, 0.2);

        let tool = config.cache_policy_for_path("/tool-matrix/v1/messages");
        assert_eq!(tool.namespace.as_deref(), Some("/tool-matrix"));
        assert_eq!(tool.policy.cache_type, PromptCacheStrategyType::KiroRsTool);
        assert!(!tool.policy.simulation.enabled);
        assert_eq!(tool.policy.simulation.token_scale, 1.0);
        assert!(!tool.policy.creation_control.enabled);
        assert!(tool.policy.reported_usage.enabled);
        assert_eq!(tool.policy.reported_usage.input.max_tokens, 123);
        assert!(tool.policy.cache_point.enabled);
        assert!(!tool.policy.cache_point.tools_only);
        assert!(tool.policy.cache_point.record_plan);
        assert_eq!(tool.policy.bounds.max_entries_per_account, 5);
        assert_eq!(tool.policy.bounds.max_entries_global, 50);
        assert_eq!(tool.policy.bounds.entry_ttl_secs, 500);
        assert_eq!(tool.policy.bounds.estimated_bytes_limit, 5_000);
        assert_eq!(tool.policy.kiro_rs_tool.coverage_ratio, 0.5);
        assert_eq!(tool.policy.kiro_rs_tool.max_coverage_tokens, 50_000);
        assert!(!tool.policy.kiro_rs_tool.incremental_create_enabled);
        assert_eq!(
            tool.policy.kiro_rs_tool.max_new_creation_tokens_per_request,
            5_000
        );
        assert!(tool.policy.kiro_rs_tool.cache_current_user_stable_prefix);
        assert_eq!(
            tool.policy
                .kiro_rs_tool
                .current_user_stable_prefix_max_tokens,
            500
        );
    }

    #[test]
    fn cache_policy_keeps_legacy_reported_usage_path_override_after_template_refactor() {
        let current_high_cache_reported_usage = ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::raw(),
            ..ReportedUsagePathPolicy::default()
        };
        let mut config = Config {
            reported_usage: ReportedUsageConfig {
                default: ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::raw(),
                    ..ReportedUsagePathPolicy::default()
                },
                path_overrides: [(
                    "/ha".to_string(),
                    ReportedUsagePathPolicy {
                        input: ReportedUsageFieldPolicy::sample_input_max(500),
                        cache_creation: ReportedUsageFieldPolicy::sample_target(150_000),
                        ..ReportedUsagePathPolicy::default()
                    },
                )]
                .into_iter()
                .collect(),
            },
            cache_policy: CachePolicyConfig {
                default: CacheRoutePolicyPatch {
                    cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                    reported_usage: Some(current_high_cache_reported_usage.clone()),
                    ..CacheRoutePolicyPatch::default()
                },
                current_high_cache: CacheRoutePolicyPatch {
                    cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                    reported_usage: Some(current_high_cache_reported_usage),
                    ..CacheRoutePolicyPatch::default()
                },
                path_overrides: [(
                    "/ha".to_string(),
                    CacheRoutePolicyPatch {
                        cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                        ..CacheRoutePolicyPatch::default()
                    },
                )]
                .into_iter()
                .collect(),
                ..CachePolicyConfig::default()
            },
            ..Config::default()
        };

        let ha = config.cache_policy_for_path("/ha/v1/messages");
        assert_eq!(
            ha.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(
            ha.policy.reported_usage.input.mode,
            ReportedUsageFieldMode::SampleMax
        );
        assert_eq!(ha.policy.reported_usage.input.max_tokens, 500);
        assert_eq!(
            ha.policy.reported_usage.cache_creation.mode,
            ReportedUsageFieldMode::SampleTarget
        );
        assert_eq!(
            ha.policy.reported_usage.cache_creation.target_tokens,
            150_000
        );

        config.cache_policy.path_overrides.insert(
            "/ha".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                reported_usage: Some(ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(42),
                    ..ReportedUsagePathPolicy::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );

        let explicit_ha = config.cache_policy_for_path("/ha/v1/messages");
        assert_eq!(explicit_ha.policy.reported_usage.input.max_tokens, 42);
    }

    #[test]
    fn cache_policy_preserves_cache_type_only_override() {
        let mut config = Config::default();
        config.cache_policy.path_overrides.insert(
            "/tool".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::KiroRsTool),
                ..CacheRoutePolicyPatch::default()
            },
        );

        let normalized = config.cache_policy.normalized();
        assert!(normalized.path_overrides.contains_key("/tool"));

        let resolved = config.cache_policy_for_path("/tool/v1/messages");
        assert_eq!(resolved.namespace.as_deref(), Some("/tool"));
        assert_eq!(
            resolved.policy.cache_type,
            PromptCacheStrategyType::KiroRsTool
        );
        assert!(!resolved.policy.reported_usage.enabled);
        assert!(
            !resolved
                .policy
                .reported_usage
                .skip_non_stream_usage_projection
        );

        let inherited = config.cache_policy_for_path("/cc/v1/messages");
        assert_eq!(inherited.namespace, None);
        assert_eq!(
            inherited.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
    }

    #[test]
    fn kiro_rs_tool_cache_policy_defaults_match_current_behavior() {
        let policy = KiroRsToolCachePolicy::default();

        assert_eq!(policy.coverage_ratio, 1.0);
        assert_eq!(policy.max_coverage_tokens, 0);
        assert!(policy.incremental_create_enabled);
        assert_eq!(policy.max_new_creation_tokens_per_request, 0);
        assert!(!policy.cache_current_user_stable_prefix);
        assert_eq!(policy.current_user_stable_prefix_max_tokens, 0);
        assert_eq!(policy.reported_input_min_tokens, 32);
        assert_eq!(policy.reported_input_max_tokens, 4_096);
    }

    #[test]
    fn kiro_rs_tool_cache_policy_deserializes_template_and_path_patch() {
        let mut config: Config = serde_json::from_value(serde_json::json!({
            "cachePolicy": {
                "kiroRsTool": {
                    "reportedUsage": {
                        "skipNonStreamUsageProjection": true
                    },
                    "kiroRsTool": {
                        "coverageRatio": 0.75,
                        "maxCoverageTokens": 12000,
                        "incrementalCreateEnabled": true,
                        "maxNewCreationTokensPerRequest": 3000,
                        "cacheCurrentUserStablePrefix": true,
                        "currentUserStablePrefixMaxTokens": 1500,
                        "reportedInputMinTokens": 64,
                        "reportedInputMaxTokens": 2048
                    }
                },
                "pathOverrides": {
                    "/dfcache/kiro-param": {
                        "cacheType": "kiro_rs_tool",
                        "kiroRsTool": {
                            "coverageRatio": 0.5,
                            "maxNewCreationTokensPerRequest": 1000,
                            "reportedInputMinTokens": 128
                        }
                    }
                }
            }
        }))
        .expect("deserialize config");
        config.cache_policy = config
            .cache_policy
            .with_builtin_path_defaults()
            .with_legacy_defined_cache_route_defaults(&config.defined_cache_routes)
            .normalized();

        let template = config.cache_policy_for_path("/cc/v1/messages");
        assert_eq!(
            template.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );

        let resolved = config.cache_policy_for_path("/dfcache/kiro-param/v1/messages");
        assert_eq!(resolved.namespace.as_deref(), Some("/dfcache/kiro-param"));
        assert_eq!(
            resolved.policy.cache_type,
            PromptCacheStrategyType::KiroRsTool
        );
        assert!(
            resolved
                .policy
                .reported_usage
                .skip_non_stream_usage_projection
        );
        assert_eq!(resolved.policy.kiro_rs_tool.coverage_ratio, 0.5);
        assert_eq!(resolved.policy.kiro_rs_tool.max_coverage_tokens, 12_000);
        assert_eq!(
            resolved
                .policy
                .kiro_rs_tool
                .max_new_creation_tokens_per_request,
            1_000
        );
        assert!(
            resolved
                .policy
                .kiro_rs_tool
                .cache_current_user_stable_prefix
        );
        assert_eq!(
            resolved
                .policy
                .kiro_rs_tool
                .current_user_stable_prefix_max_tokens,
            1_500
        );
        assert_eq!(resolved.policy.kiro_rs_tool.reported_input_min_tokens, 128);
        assert_eq!(
            resolved.policy.kiro_rs_tool.reported_input_max_tokens,
            2_048
        );
    }

    #[test]
    fn kiro_rs_tool_cache_policy_rejects_invalid_values() {
        let config: Config = serde_json::from_value(serde_json::json!({
            "cachePolicy": {
                "pathOverrides": {
                    "/dfcache/bad": {
                        "cacheType": "kiro_rs_tool",
                        "kiroRsTool": {
                            "coverageRatio": 1.5
                        }
                    }
                }
            }
        }))
        .expect("deserialize config");

        let err = config
            .cache_policy
            .validate(config.legacy_cache_route_policy_default())
            .expect_err("invalid coverage ratio should fail");
        assert!(err.contains("coverageRatio"));

        let invalid_range: Config = serde_json::from_value(serde_json::json!({
            "cachePolicy": {
                "pathOverrides": {
                    "/dfcache/bad-range": {
                        "cacheType": "kiro_rs_tool",
                        "kiroRsTool": {
                            "reportedInputMinTokens": 4096,
                            "reportedInputMaxTokens": 32
                        }
                    }
                }
            }
        }))
        .expect("deserialize config");

        let err = invalid_range
            .cache_policy
            .validate(invalid_range.legacy_cache_route_policy_default())
            .expect_err("invalid reported input range should fail");
        assert!(err.contains("reportedInputMinTokens"));
    }

    #[test]
    fn cache_policy_resolves_path_creation_control_override() {
        let mut config = Config {
            prompt_cache_creation_control: PromptCacheCreationControlConfig {
                enabled: true,
                scope_mode: PromptCacheCreationControlScopeMode::ConversationModel,
                min_successful_requests_between_creation: 7,
                min_creation_interval_secs: 120,
                min_creation_delta_tokens: 30000,
                max_creation_tokens_per_event: 40000,
                creation_budget_window_secs: 600,
                max_creation_tokens_per_window: 200000,
                expire_after_idle_secs: 7200,
            },
            ..Config::default()
        };
        config.cache_policy.path_overrides.insert(
            "/dfcache/team-a".to_string(),
            CacheRoutePolicyPatch {
                creation_control: Some(PromptCacheCreationControlConfig {
                    enabled: true,
                    scope_mode: PromptCacheCreationControlScopeMode::CredentialConversationModel,
                    min_successful_requests_between_creation: 2,
                    min_creation_interval_secs: 30,
                    min_creation_delta_tokens: 8000,
                    max_creation_tokens_per_event: 12000,
                    creation_budget_window_secs: 180,
                    max_creation_tokens_per_window: 50000,
                    expire_after_idle_secs: 900,
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );

        let overridden = config.cache_policy_for_path("/dfcache/team-a/v1/messages");
        assert_eq!(overridden.namespace.as_deref(), Some("/dfcache/team-a"));
        assert_eq!(
            overridden.policy.creation_control.scope_mode,
            PromptCacheCreationControlScopeMode::CredentialConversationModel
        );
        assert_eq!(
            overridden
                .policy
                .creation_control
                .min_successful_requests_between_creation,
            2
        );
        assert_eq!(
            overridden
                .policy
                .creation_control
                .min_creation_interval_secs,
            30
        );

        let inherited = config.cache_policy_for_path("/cc/v1/messages");
        assert_eq!(inherited.namespace, None);
        assert_eq!(
            inherited
                .policy
                .creation_control
                .min_successful_requests_between_creation,
            7
        );
        assert_eq!(
            inherited.policy.creation_control.min_creation_interval_secs,
            120
        );
    }

    #[test]
    fn cache_policy_reported_usage_only_override_does_not_split_cache_scope() {
        let mut config = Config::default();
        config.cache_policy.path_overrides.insert(
            "/reports".to_string(),
            CacheRoutePolicyPatch {
                reported_usage: Some(ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(64),
                    ..ReportedUsagePathPolicy::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );

        let resolved = config.cache_policy_for_path("/reports/v1/messages");

        assert_eq!(resolved.namespace, None);
        assert_eq!(
            resolved.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(resolved.policy.reported_usage.input.max_tokens, 64);
    }

    #[test]
    fn cache_policy_no_cache_route_skips_cache_state_and_is_preserved() {
        let mut config = Config::default();
        config.cache_policy.path_overrides.insert(
            "/plain".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::NoCache),
                ..CacheRoutePolicyPatch::default()
            },
        );

        let normalized = config.cache_policy.normalized();
        assert!(normalized.path_overrides.contains_key("/plain"));

        let resolved = config.cache_policy_for_path("/plain/v1/messages");
        assert_eq!(resolved.namespace, None);
        assert_eq!(resolved.policy.cache_type, PromptCacheStrategyType::NoCache);
        assert!(!resolved.policy.simulation.enabled);
        assert!(!resolved.policy.creation_control.enabled);
        assert!(!resolved.policy.cache_point.enabled);
        assert!(!resolved.policy.reported_usage.enabled);
    }

    #[test]
    fn builtin_route_cache_strategy_is_config_driven() {
        let mut config = Config::default();
        config.cache_policy.path_overrides.insert(
            "/cc".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::NoCache),
                ..CacheRoutePolicyPatch::default()
            },
        );
        config.cache_policy.path_overrides.insert(
            "/na".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                route_namespace: Some(false),
                simulation: Some(CacheSimulationPolicyPatch {
                    enabled: Some(true),
                    target_read_ratio: Some(0.72),
                    ..CacheSimulationPolicyPatch::default()
                }),
                reported_usage: Some(ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(77),
                    ..ReportedUsagePathPolicy::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );
        config.cache_policy.path_overrides.insert(
            "/ha".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                route_namespace: Some(true),
                ..CacheRoutePolicyPatch::default()
            },
        );

        let cc = config.cache_policy_for_path("/cc/v1/messages");
        assert_eq!(cc.policy.cache_type, PromptCacheStrategyType::NoCache);
        assert_eq!(cc.namespace, None);
        assert!(!cc.policy.reported_usage.enabled);

        let na = config.cache_policy_for_path("/na/v1/messages");
        assert_eq!(
            na.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(na.namespace, None);
        assert!(na.policy.simulation.enabled);
        assert_eq!(na.policy.simulation.target_read_ratio, 0.72);
        assert_eq!(na.policy.reported_usage.input.max_tokens, 77);

        let ha = config.cache_policy_for_path("/ha/v1/messages");
        assert_eq!(
            ha.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(ha.namespace.as_deref(), Some("/ha"));
    }

    #[test]
    fn cache_policy_builtin_defaults_do_not_overwrite_existing_path() {
        let mut cache_policy = CachePolicyConfig::default();
        cache_policy.path_overrides.insert(
            "/cc".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::KiroRsTool),
                ..CacheRoutePolicyPatch::default()
            },
        );

        cache_policy.path_overrides.insert(
            "/na".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                simulation: Some(CacheSimulationPolicyPatch {
                    enabled: Some(true),
                    ..CacheSimulationPolicyPatch::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );

        let with_builtins = cache_policy.with_builtin_path_defaults();
        for prefix in ["/v1", "/cc", "/ha", "/na"] {
            assert!(with_builtins.path_overrides.contains_key(prefix));
        }
        assert_eq!(
            with_builtins
                .path_overrides
                .get("/cc")
                .and_then(|policy| policy.cache_type),
            Some(PromptCacheStrategyType::KiroRsTool)
        );
        assert_eq!(
            with_builtins
                .path_overrides
                .get("/na")
                .and_then(|policy| policy.cache_type),
            Some(PromptCacheStrategyType::CurrentHighCache)
        );
        assert!(
            with_builtins
                .path_overrides
                .get("/na")
                .and_then(|policy| policy.simulation)
                .is_some()
        );
    }

    #[test]
    fn runtime_config_migration_preserves_explicit_na_policy() {
        let mut config = Config {
            runtime_config_migration_version: CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION,
            ..Config::default()
        };
        config.cache_policy.path_overrides.insert(
            "/na".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                reported_usage: Some(ReportedUsagePathPolicy::disabled()),
                cache_point: Some(CachePointPolicyPatch {
                    enabled: Some(true),
                    ..CachePointPolicyPatch::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );

        assert!(!config.apply_runtime_config_migrations());
        let na = config
            .cache_policy
            .path_overrides
            .get("/na")
            .expect("/na should be retained as an explicit route");
        assert_eq!(
            na.cache_type,
            Some(PromptCacheStrategyType::CurrentHighCache)
        );
        assert!(na.reported_usage.is_some());
        assert!(na.cache_point.is_some());
    }

    #[test]
    fn runtime_config_migration_rewrites_legacy_payload_guard_default_once() {
        let mut config = Config::default();
        config.runtime_config_migration_version = 0;
        config.payload_guard_mode = PayloadGuardMode::Preemptive;

        assert!(config.apply_runtime_config_migrations());
        assert_eq!(config.payload_guard_mode, PayloadGuardMode::OnTooLong);
        assert_eq!(
            config.runtime_config_migration_version,
            CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION
        );

        config.payload_guard_mode = PayloadGuardMode::Preemptive;
        assert!(!config.apply_runtime_config_migrations());
        assert_eq!(config.payload_guard_mode, PayloadGuardMode::Preemptive);
    }

    #[test]
    fn runtime_config_migration_disables_online_usage_rollup_compression_once() {
        let mut config = Config::default();
        config.runtime_config_migration_version = 6;
        config.postgres.compress_usage_rollups_on_start = true;

        assert!(config.apply_runtime_config_migrations());
        assert_eq!(
            config.runtime_config_migration_version,
            CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION
        );
        assert!(!config.postgres.compress_usage_rollups_on_start);

        config.postgres.compress_usage_rollups_on_start = true;
        assert!(!config.apply_runtime_config_migrations());
        assert!(config.postgres.compress_usage_rollups_on_start);
    }

    #[test]
    fn runtime_config_migration_updates_legacy_default_task_quality_prompt_only() {
        let mut legacy_default = Config::default();
        legacy_default.runtime_config_migration_version = 1;
        legacy_default.prompt_steering.task_quality.prompt =
            LEGACY_TASK_QUALITY_PROMPT_V1.to_string();

        assert!(legacy_default.apply_runtime_config_migrations());
        assert_eq!(
            legacy_default.runtime_config_migration_version,
            CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION
        );
        assert_eq!(
            legacy_default.prompt_steering.task_quality.prompt,
            DEFAULT_TASK_QUALITY_PROMPT.trim()
        );

        let mut legacy_default_v2 = Config::default();
        legacy_default_v2.runtime_config_migration_version = 2;
        legacy_default_v2.prompt_steering.task_quality.prompt =
            LEGACY_TASK_QUALITY_PROMPT_V2.to_string();

        assert!(legacy_default_v2.apply_runtime_config_migrations());
        assert_eq!(
            legacy_default_v2.runtime_config_migration_version,
            CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION
        );
        assert_eq!(
            legacy_default_v2.prompt_steering.task_quality.prompt,
            DEFAULT_TASK_QUALITY_PROMPT.trim()
        );

        let mut custom = Config::default();
        custom.runtime_config_migration_version = 1;
        custom.prompt_steering.task_quality.prompt = "custom task prompt".to_string();

        assert!(custom.apply_runtime_config_migrations());
        assert_eq!(
            custom.runtime_config_migration_version,
            CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION
        );
        assert_eq!(
            custom.prompt_steering.task_quality.prompt,
            "custom task prompt"
        );

        let mut legacy_default_v3 = Config::default();
        legacy_default_v3.runtime_config_migration_version = 3;
        legacy_default_v3.prompt_steering.task_quality.prompt =
            LEGACY_TASK_QUALITY_PROMPT_V3.to_string();

        assert!(legacy_default_v3.apply_runtime_config_migrations());
        assert_eq!(
            legacy_default_v3.runtime_config_migration_version,
            CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION
        );
        assert_eq!(
            legacy_default_v3.prompt_steering.task_quality.prompt,
            DEFAULT_TASK_QUALITY_PROMPT.trim()
        );

        let mut customized_v3 = Config::default();
        customized_v3.runtime_config_migration_version = 3;
        customized_v3.prompt_steering.task_quality.prompt =
            format!("{LEGACY_TASK_QUALITY_PROMPT_V3}\ncustom suffix");

        assert!(customized_v3.apply_runtime_config_migrations());
        assert_eq!(
            customized_v3.prompt_steering.task_quality.prompt,
            format!("{LEGACY_TASK_QUALITY_PROMPT_V3}\ncustom suffix")
        );

        let mut whitespace_customized_v3 = Config::default();
        whitespace_customized_v3.runtime_config_migration_version = 3;
        whitespace_customized_v3.prompt_steering.task_quality.prompt =
            format!(" {LEGACY_TASK_QUALITY_PROMPT_V3}");

        assert!(whitespace_customized_v3.apply_runtime_config_migrations());
        assert_eq!(
            whitespace_customized_v3.prompt_steering.task_quality.prompt,
            format!(" {LEGACY_TASK_QUALITY_PROMPT_V3}")
        );

        let mut persisted_by_old_ui_at_v5 = Config::default();
        persisted_by_old_ui_at_v5.runtime_config_migration_version = 5;
        persisted_by_old_ui_at_v5
            .prompt_steering
            .task_quality
            .prompt = LEGACY_TASK_QUALITY_PROMPT_V3.to_string();
        assert!(persisted_by_old_ui_at_v5.apply_runtime_config_migrations());
        assert_eq!(
            persisted_by_old_ui_at_v5
                .prompt_steering
                .task_quality
                .prompt,
            DEFAULT_TASK_QUALITY_PROMPT.trim()
        );

        for custom_prompt in [
            format!("{LEGACY_TASK_QUALITY_PROMPT_V3}\ncustom suffix"),
            format!(" {LEGACY_TASK_QUALITY_PROMPT_V3}"),
            "operator custom task prompt".to_string(),
        ] {
            let mut customized_at_v5 = Config::default();
            customized_at_v5.runtime_config_migration_version = 5;
            customized_at_v5.prompt_steering.task_quality.prompt = custom_prompt.clone();
            assert!(customized_at_v5.apply_runtime_config_migrations());
            assert_eq!(
                customized_at_v5.prompt_steering.task_quality.prompt, custom_prompt,
                "v6 migration must preserve user-edited prompt bytes"
            );
        }
    }

    #[test]
    fn prompt_master_protocol_capabilities_and_prompt_subtoggles_round_trip_independently() {
        for round in 0..5 {
            for mask in 0_u8..128 {
                let mut config = Config::default();
                config.prompt_steering.enabled = mask & 1 != 0;
                config.body_conversion.tool_choice_steering = mask & 2 != 0;
                config.body_conversion.thinking_prompt_controls = mask & 4 != 0;
                config.body_conversion.chunked_tool_policy = mask & 8 != 0;
                config.prompt_steering.tool_choice.enabled = mask & 16 != 0;
                config.prompt_steering.thinking.enabled = mask & 32 != 0;
                config.prompt_steering.chunked_write.enabled = mask & 64 != 0;

                let encoded = serde_json::to_string(&config)
                    .unwrap_or_else(|error| panic!("round {round}, mask {mask}: {error}"));
                let decoded: Config = serde_json::from_str(&encoded)
                    .unwrap_or_else(|error| panic!("round {round}, mask {mask}: {error}"));

                assert_eq!(
                    decoded.prompt_steering.enabled, config.prompt_steering.enabled,
                    "round {round}, mask {mask}: operator prompt master"
                );
                assert_eq!(
                    decoded.body_conversion.tool_choice_steering,
                    config.body_conversion.tool_choice_steering,
                    "round {round}, mask {mask}: structured tool_choice capability"
                );
                assert_eq!(
                    decoded.body_conversion.thinking_prompt_controls,
                    config.body_conversion.thinking_prompt_controls,
                    "round {round}, mask {mask}: thinking capability"
                );
                assert_eq!(
                    decoded.body_conversion.chunked_tool_policy,
                    config.body_conversion.chunked_tool_policy,
                    "round {round}, mask {mask}: chunked capability"
                );
                assert_eq!(
                    decoded.prompt_steering.tool_choice.enabled,
                    config.prompt_steering.tool_choice.enabled,
                    "round {round}, mask {mask}: tool_choice prompt"
                );
                assert_eq!(
                    decoded.prompt_steering.thinking.enabled,
                    config.prompt_steering.thinking.enabled,
                    "round {round}, mask {mask}: thinking prompt"
                );
                assert_eq!(
                    decoded.prompt_steering.chunked_write.enabled,
                    config.prompt_steering.chunked_write.enabled,
                    "round {round}, mask {mask}: chunked prompt"
                );
            }
        }
    }

    #[test]
    fn runtime_config_migration_restores_legacy_scheduler_fallback_intent_once() {
        for legacy_version in [0, 1, 2, 3, 4] {
            let mut config = Config::default();
            config.runtime_config_migration_version = legacy_version;
            config.external_pools.external_pools_enabled = true;
            config.external_pools.fallback_on_scheduler_redis_degraded = false;

            assert!(config.apply_runtime_config_migrations());
            assert_eq!(
                config.runtime_config_migration_version,
                CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION
            );
            assert!(
                config.external_pools.fallback_on_scheduler_redis_degraded,
                "legacy migration version {legacy_version} must retain broad local fallback intent"
            );

            config.external_pools.fallback_on_scheduler_redis_degraded = false;
            assert!(!config.apply_runtime_config_migrations());
            assert!(
                !config.external_pools.fallback_on_scheduler_redis_degraded,
                "an explicit post-migration opt-out must be stable"
            );
        }
    }

    #[test]
    fn runtime_config_migration_does_not_enable_scheduler_fallback_without_legacy_intent() {
        let mut external_disabled = Config::default();
        external_disabled.runtime_config_migration_version = 4;
        external_disabled.external_pools.external_pools_enabled = false;
        external_disabled
            .external_pools
            .fallback_on_scheduler_redis_degraded = false;
        assert!(external_disabled.apply_runtime_config_migrations());
        assert!(
            !external_disabled
                .external_pools
                .fallback_on_scheduler_redis_degraded
        );

        for disable_legacy_fallback in ["capacity", "no_credentials", "transient"] {
            let mut config = Config::default();
            config.runtime_config_migration_version = 4;
            config.external_pools.external_pools_enabled = true;
            config.external_pools.fallback_on_scheduler_redis_degraded = false;
            match disable_legacy_fallback {
                "capacity" => config.external_pools.fallback_on_local_capacity_exhausted = false,
                "no_credentials" => {
                    config.external_pools.fallback_on_no_available_credentials = false
                }
                "transient" => config.external_pools.fallback_on_local_transient_exhausted = false,
                _ => unreachable!(),
            }

            assert!(config.apply_runtime_config_migrations());
            assert!(
                !config.external_pools.fallback_on_scheduler_redis_degraded,
                "partial legacy fallback policy {disable_legacy_fallback} must not be broadened"
            );
        }

        let mut explicit_true = Config::default();
        explicit_true.runtime_config_migration_version = 4;
        explicit_true.external_pools.external_pools_enabled = true;
        assert!(explicit_true.apply_runtime_config_migrations());
        assert!(
            explicit_true
                .external_pools
                .fallback_on_scheduler_redis_degraded
        );
    }

    #[test]
    fn runtime_config_migration_bounds_legacy_unlimited_external_wait() {
        let mut config = Config::default();
        config.runtime_config_migration_version = 4;
        config.external_pools.external_pool_capacity_mode = ExternalPoolCapacityMode::Wait;
        config.external_pools.external_pool_dispatch_max_wait_secs = 0;

        assert!(config.apply_runtime_config_migrations());
        assert_eq!(
            config.external_pools.external_pool_dispatch_max_wait_secs,
            default_external_pool_dispatch_max_wait_secs()
        );
        assert_eq!(config.external_pools.effective_dispatch_max_wait_secs(), 5);
    }

    #[test]
    fn default_task_quality_prompt_does_not_prime_internal_transcript_fingerprints() {
        for marker in [
            "readHash",
            "editHash",
            "bashHash",
            "Tool results:",
            "Tool results provided",
            "function_results",
        ] {
            assert!(
                !DEFAULT_TASK_QUALITY_PROMPT.contains(marker),
                "default prompt must not teach the model internal marker {marker}"
            );
        }
    }

    #[test]
    fn cache_policy_legacy_defined_cache_routes_default_to_current_strategy() {
        let mut config = Config::default();
        config.defined_cache_routes = vec!["/dfcache/team-a".to_string()];

        let resolved = config.cache_policy_for_path("/dfcache/team-a/v1/messages");

        assert_eq!(
            resolved.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(resolved.namespace.as_deref(), Some("/dfcache/team-a"));
    }

    #[test]
    fn cache_policy_validation_rejects_invalid_path_values() {
        let mut config = Config::default();
        config.cache_policy.path_overrides.insert(
            "/bad".to_string(),
            CacheRoutePolicyPatch {
                simulation: Some(CacheSimulationPolicyPatch {
                    target_read_ratio: Some(1.5),
                    ..CacheSimulationPolicyPatch::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );

        assert!(
            config
                .cache_policy
                .validate(config.legacy_cache_route_policy_default())
                .is_err()
        );

        let mut invalid_prefix = Config::default();
        invalid_prefix
            .cache_policy
            .path_overrides
            .insert(" ".to_string(), CacheRoutePolicyPatch::default());
        assert!(
            invalid_prefix
                .cache_policy
                .validate(invalid_prefix.legacy_cache_route_policy_default())
                .is_err()
        );

        let mut invalid_creation_control = Config::default();
        invalid_creation_control.cache_policy.path_overrides.insert(
            "/bad-creation".to_string(),
            CacheRoutePolicyPatch {
                creation_control: Some(PromptCacheCreationControlConfig {
                    min_creation_delta_tokens: -1,
                    ..PromptCacheCreationControlConfig::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );
        assert!(
            invalid_creation_control
                .cache_policy
                .validate(invalid_creation_control.legacy_cache_route_policy_default())
                .is_err()
        );

        let mut invalid_bounds = Config::default();
        invalid_bounds.cache_policy.path_overrides.insert(
            "/bad-bounds".to_string(),
            CacheRoutePolicyPatch {
                bounds: Some(CacheBoundsPolicyPatch {
                    entry_ttl_secs: Some(0),
                    ..CacheBoundsPolicyPatch::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );
        assert!(
            invalid_bounds
                .cache_policy
                .validate(invalid_bounds.legacy_cache_route_policy_default())
                .is_err()
        );
    }

    #[test]
    fn config_example_bootstraps_desired_reported_usage_defaults() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let example_path = std::path::Path::new(manifest_dir).join("config.example.json");
        let json = std::fs::read_to_string(example_path).unwrap();
        let config: Config = serde_json::from_str(&json).unwrap();

        assert_eq!(config.payload_guard_mode, PayloadGuardMode::OnTooLong);
        assert_eq!(config.inference_upstream_max_attempts, 4);
        assert_eq!(config.auxiliary_upstream_max_attempts, 2);
        assert_eq!(
            config.auxiliary_upstream_max_concurrent_requests,
            DEFAULT_AUXILIARY_UPSTREAM_MAX_CONCURRENT_REQUESTS
        );
        assert_eq!(config.token_refresh_max_rpm, DEFAULT_TOKEN_REFRESH_MAX_RPM);
        assert_eq!(config.token_refresh_burst, DEFAULT_TOKEN_REFRESH_BURST);
        assert!(!config.weighted_capacity.enabled);
        assert_eq!(config.weighted_capacity.max_units_per_request, 8);
        assert_eq!(config.weighted_capacity.units_for_tokens(1_000_000), 1);

        let default_policy = config.reported_usage.policy_for_path("/v1/messages");
        assert_eq!(default_policy.input.mode, ReportedUsageFieldMode::Raw);
        assert_eq!(default_policy.output.mode, ReportedUsageFieldMode::Raw);
        assert_eq!(default_policy.final_cache_read_max_tokens, 700_000);
        assert_eq!(default_policy.final_cache_read_jitter_min_tokens, 0);
        assert_eq!(default_policy.final_cache_read_jitter_max_tokens, 0);
        assert_eq!(default_policy.final_cache_creation_max_tokens, 400_000);
        assert_eq!(
            default_policy.final_cache_creation_jitter_min_tokens,
            20_000
        );
        assert_eq!(
            default_policy.final_cache_creation_jitter_max_tokens,
            45_000
        );
        assert!(default_policy.final_output_guard_enabled);
        assert_eq!(default_policy.output_uplift_min_tokens, 1_000);
        assert_eq!(default_policy.output_uplift_percent, 50);
        assert_eq!(default_policy.final_output_max_tokens, 200_000);
        assert_eq!(default_policy.final_output_jitter_min_tokens, 5_000);
        assert_eq!(default_policy.final_output_jitter_max_tokens, 12_000);
        assert_eq!(
            default_policy.cache_read.mode,
            ReportedUsageFieldMode::Preserve
        );
        assert_eq!(
            default_policy.cache_creation.mode,
            ReportedUsageFieldMode::Preserve
        );

        let cc_policy = config.reported_usage.policy_for_path("/cc/v1/messages");
        assert_eq!(cc_policy.final_cache_read_max_tokens, 700_000);
        assert_eq!(cc_policy.final_cache_creation_max_tokens, 400_000);
        assert_eq!(cc_policy.final_cache_creation_jitter_min_tokens, 20_000);
        assert_eq!(cc_policy.final_cache_creation_jitter_max_tokens, 45_000);
        assert!(cc_policy.final_output_guard_enabled);
        assert_eq!(cc_policy.output_uplift_min_tokens, 1_000);
        assert_eq!(cc_policy.output_uplift_percent, 50);
        assert_eq!(cc_policy.final_output_max_tokens, 200_000);
        assert_eq!(cc_policy.final_output_jitter_min_tokens, 5_000);
        assert_eq!(cc_policy.final_output_jitter_max_tokens, 12_000);
        assert_eq!(cc_policy.input.mode, ReportedUsageFieldMode::SampleMax);
        assert_eq!(cc_policy.input.max_tokens, 96);
        assert!(cc_policy.input.move_delta_to_cache_read);
        assert_eq!(
            cc_policy.cache_creation.mode,
            ReportedUsageFieldMode::SampleTarget
        );
        assert_eq!(cc_policy.cache_creation.target_tokens, 3_000);
        assert_eq!(cc_policy.cache_creation.normal_max_multiplier, 1.2);

        let ha_policy = config.reported_usage.policy_for_path("/ha/v1/messages");
        assert!(ha_policy.final_output_guard_enabled);
        assert_eq!(ha_policy.output_uplift_min_tokens, 1_000);
        assert_eq!(ha_policy.output_uplift_percent, 50);
        assert_eq!(ha_policy.final_output_max_tokens, 200_000);
        assert_eq!(ha_policy.final_output_jitter_min_tokens, 5_000);
        assert_eq!(ha_policy.final_output_jitter_max_tokens, 12_000);
        assert_eq!(ha_policy.input.mode, ReportedUsageFieldMode::SampleMax);
        assert_eq!(
            ha_policy.cache_creation.mode,
            ReportedUsageFieldMode::Preserve
        );

        assert!(!config.reported_usage.path_overrides.contains_key("/na"));
        assert!(
            config
                .reported_usage
                .policy_for_path("/na/v1/messages")
                .enabled
        );
    }

    #[test]
    fn inference_attempt_budget_uses_compatible_default_and_preserves_explicit_value() {
        let historical: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(historical.inference_upstream_max_attempts, 4);

        let explicit: Config =
            serde_json::from_str(r#"{"inferenceUpstreamMaxAttempts":7}"#).unwrap();
        assert_eq!(explicit.inference_upstream_max_attempts, 7);
    }

    #[test]
    fn auxiliary_focus_limits_default_round_trip_and_ignore_prompt_steering_for_five_rounds() {
        for _ in 0..5 {
            let historical: Config = serde_json::from_str("{}").unwrap();
            assert_eq!(historical.auxiliary_upstream_max_attempts, 2);
            assert_eq!(
                historical.auxiliary_upstream_max_concurrent_requests,
                DEFAULT_AUXILIARY_UPSTREAM_MAX_CONCURRENT_REQUESTS
            );
            assert_eq!(
                historical.token_refresh_max_rpm,
                DEFAULT_TOKEN_REFRESH_MAX_RPM
            );
            assert_eq!(historical.token_refresh_burst, DEFAULT_TOKEN_REFRESH_BURST);

            let explicit: Config = serde_json::from_str(
                r#"{
                    "auxiliaryUpstreamMaxAttempts": 7,
                    "auxiliaryUpstreamMaxConcurrentRequests": 31,
                    "tokenRefreshMaxRpm": 120,
                    "tokenRefreshBurst": 16,
                    "promptSteering": {"enabled": false}
                }"#,
            )
            .unwrap();
            assert_eq!(explicit.auxiliary_upstream_max_attempts, 7);
            assert_eq!(explicit.auxiliary_upstream_max_concurrent_requests, 31);
            assert_eq!(explicit.token_refresh_max_rpm, 120);
            assert_eq!(explicit.token_refresh_burst, 16);
            assert!(!explicit.prompt_steering.enabled);

            let round_trip: Config =
                serde_json::from_value(serde_json::to_value(&explicit).unwrap()).unwrap();
            assert_eq!(round_trip.auxiliary_upstream_max_attempts, 7);
            assert_eq!(round_trip.auxiliary_upstream_max_concurrent_requests, 31);
            assert_eq!(round_trip.token_refresh_max_rpm, 120);
            assert_eq!(round_trip.token_refresh_burst, 16);
            assert!(!round_trip.prompt_steering.enabled);
        }
    }
}
