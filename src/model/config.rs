use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub use super::routes::{CompiledRoutes, RouteConfig, RoutesConfig};

pub use super::prefix_tool_plus_policy::{
    PrefixToolPlusCachePolicy, SelectedUsageShaping, SuppressedCreationTarget, UsageFieldShaping,
    UsageShapingProfile,
};
pub use super::prompt_injection::{
    InjectionId, PromptInjectionConfig, ResolvedInjections, RouteFeatureSet, RouteFeaturesConfig,
};

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

/// 上游 payload shaping 配置。
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

    /// 是否在发给上游前压缩过大的图片（默认关闭）。打开后，长边超过 `resize_max_long_side` 或
    /// base64 长度超过 `resize_max_bytes` 的 PNG/JPEG/WebP 图片会等比缩小并重新编码为 JPEG；
    /// GIF 不处理，失败时保留原图。只在本地多模态预处理阶段执行：直接转发给账号的请求
    /// （原样转发路径）不经过这一步，保持原图。
    #[serde(default)]
    pub resize_enabled: bool,

    /// 压缩后的长边上限（像素）。
    #[serde(default = "default_image_resize_max_long_side")]
    pub resize_max_long_side: u32,

    /// 压缩目标：base64 编码后的字节数上限。
    #[serde(default = "default_image_resize_max_bytes")]
    pub resize_max_bytes: usize,

    /// 重新编码为 JPEG 时的初始画质（1..=100）。
    #[serde(default = "default_image_resize_jpeg_quality")]
    pub resize_jpeg_quality: u8,
}

pub const IMAGE_RESIZE_MIN_LONG_SIDE: u32 = 256;
pub const IMAGE_RESIZE_MAX_LONG_SIDE: u32 = 8000;
pub const IMAGE_RESIZE_MIN_BYTES: usize = 32 * 1024;
pub const IMAGE_RESIZE_MAX_BYTES: usize = 5 * 1024 * 1024;

fn default_image_resize_max_long_side() -> u32 {
    1568
}

fn default_image_resize_max_bytes() -> usize {
    400_000
}

fn default_image_resize_jpeg_quality() -> u8 {
    85
}

impl Default for ImageProcessingConfig {
    fn default() -> Self {
        Self {
            mode: ImageProcessingMode::Safe,
            safe_materialize_file_sources: true,
            safe_download_remote_sources: true,
            safe_normalize_base64_media_types: true,
            resize_enabled: false,
            resize_max_long_side: default_image_resize_max_long_side(),
            resize_max_bytes: default_image_resize_max_bytes(),
            resize_jpeg_quality: default_image_resize_jpeg_quality(),
        }
    }
}

impl ImageProcessingConfig {
    pub fn normalized(self) -> Self {
        let resize = Self {
            resize_max_long_side: self
                .resize_max_long_side
                .clamp(IMAGE_RESIZE_MIN_LONG_SIDE, IMAGE_RESIZE_MAX_LONG_SIDE),
            resize_max_bytes: self
                .resize_max_bytes
                .clamp(IMAGE_RESIZE_MIN_BYTES, IMAGE_RESIZE_MAX_BYTES),
            resize_jpeg_quality: self.resize_jpeg_quality.clamp(40, 100),
            ..self
        };
        match resize.mode {
            ImageProcessingMode::Safe => resize,
            ImageProcessingMode::Light => Self {
                mode: ImageProcessingMode::Light,
                safe_materialize_file_sources: false,
                safe_download_remote_sources: false,
                safe_normalize_base64_media_types: false,
                ..resize
            },
        }
    }
}

pub const DEFAULT_TOOL_SCHEMA_KEY_VALIDATION_REGEX: &str = "^[a-zA-Z0-9_.-]{1,64}$";

/// 请求参数兼容开关（`localRequestCompat`）。
///
/// 每一项对应一种「客户端发了严格校验不收的写法时怎么处理」的行为：开着就改写后继续，
/// 关掉就交给严格校验（多数情况下返回 400）。已有的兼容行为默认开启，零配置时与之前完全
/// 一致；新增的兼容默认也开启，因为它们只把原本会被拒绝或计费的请求改成可用。
///
/// 改写发生在请求入口、选路之前，本地账号和账号都会收到改写后的请求体；账号的
/// 参数透传方式不受这里影响。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct LocalRequestCompatConfig {
    /// `output_config.effort: "none"`（不区分大小写、忽略首尾空白）视为未指定并删除。已有行为。
    pub drop_effort_none: bool,
    /// `thinking.type: "between_tools"` 改为 `adaptive`。已有行为。
    pub thinking_type_aliases: bool,
    /// thinking 预算与 max_tokens 不匹配时自动修复（预算过小抬到 1024、超出时扩大 max_tokens
    /// 或收紧预算、adaptive/disabled 去掉 budget）。已有行为。
    pub repair_thinking_budget: bool,
    /// `messages` 里的 system/developer 角色并入 system 提示词，其他未知角色丢弃。已有行为。
    pub normalize_message_roles: bool,
    /// 消息 content 的标量、null、单对象、数组里的非对象元素归一成标准形状。已有行为。
    pub normalize_message_content: bool,
    /// 新增：`output_config.effort` 大小写或首尾空白不规范时改成标准写法（`" High "` → `high`）。
    pub normalize_effort_case: bool,
    /// 新增：`output_config.effort` 为空串或不认识的值时删除该字段，按未指定处理，
    /// 不再返回 `output_config.effort must be one of` 的 400。
    pub drop_unknown_effort: bool,
}

impl Default for LocalRequestCompatConfig {
    fn default() -> Self {
        Self {
            drop_effort_none: true,
            thinking_type_aliases: true,
            repair_thinking_budget: true,
            normalize_message_roles: true,
            normalize_message_content: true,
            normalize_effort_case: true,
            drop_unknown_effort: true,
        }
    }
}

impl LocalRequestCompatConfig {
    /// 是否需要在入口改写 effort 或 thinking.type（任一相关开关打开）。
    pub fn rewrites_reasoning_aliases(&self) -> bool {
        self.drop_effort_none
            || self.thinking_type_aliases
            || self.normalize_effort_case
            || self.drop_unknown_effort
    }
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

    pub(crate) fn validate(&self, label: &str) -> Result<(), String> {
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

    /// 命中该路径的非流式请求是否全部透传上游 usage（强制关）。
    ///
    /// 该字段保留旧的 usage projection 命名以兼容现有配置。开启后，非流式请求不进入
    /// 本系统 usage 整形、input 采样和补偿，也不会读取或写入本地 prompt-cache 状态；
    /// 账号非流式响应会保持上游 usage 原样返回，本地凭证会使用上游 metadata 原始 usage。
    /// 关闭时按路径策略整形；是否额外要求官方缓存条件见 `strict_cache_eligibility_non_stream`。
    /// 流式请求不受影响。
    /// 全局策略模板里开启会影响继承该模板的路径，路径级配置可单独覆盖。
    #[serde(default)]
    pub skip_non_stream_usage_projection: bool,

    /// 流式请求是否按官方缓存条件严格检查，默认关闭。
    ///
    /// 开启后，流式请求只有带 cache_control（块级或顶层自动缓存），且断点前内容达到模型最小
    /// 缓存长度时才按路径策略整形，其余透传上游 usage、不读写本地缓存状态。关闭时不看有没有
    /// cache_control，整条请求按已标记处理，直接按路径策略整形；只有整条请求都短于模型最小
    /// 缓存长度时仍透传（官方也不会缓存这种请求）。
    #[serde(default)]
    pub strict_cache_eligibility_stream: bool,

    /// 非流式请求是否按官方缓存条件严格检查，默认关闭。规则同
    /// `strict_cache_eligibility_stream`；`skip_non_stream_usage_projection` 开启时非流式一律透传，
    /// 不再看这个开关。
    #[serde(default)]
    pub strict_cache_eligibility_non_stream: bool,

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
            strict_cache_eligibility_stream: false,
            strict_cache_eligibility_non_stream: false,
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

    /// Whether a request of this kind must meet the official cache conditions (a cache_control
    /// breakpoint and a prefix at the model minimum) before the route strategy shapes it.
    pub fn strict_cache_eligibility(&self, stream: bool) -> bool {
        if stream {
            self.strict_cache_eligibility_stream
        } else {
            self.strict_cache_eligibility_non_stream
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
            strict_cache_eligibility_stream: self.strict_cache_eligibility_stream,
            strict_cache_eligibility_non_stream: self.strict_cache_eligibility_non_stream,
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

    pub(crate) fn validate(&self, label: &str) -> Result<(), String> {
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
}

impl Default for ReportedUsageConfig {
    fn default() -> Self {
        Self {
            default: ReportedUsagePathPolicy::default(),
        }
    }
}
impl ReportedUsageConfig {
    pub fn normalized(&self) -> Self {
        Self {
            default: self.default.normalized(),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        self.default.validate("默认上报策略")
    }
    pub fn policy_for_path(&self, _path: &str) -> ReportedUsagePathPolicy {
        self.default.normalized()
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
pub struct CacheBoundsPolicy {
    /// 每个会话 scope（conversation_id + 路由命名空间）最多保留的条目数；字段名为兼容保留。
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

/// 下游缓存命中率与小值整形。
///
/// 默认关闭，开启后在所有缓存策略（`no_cache` 除外）的最终下游 usage 上执行：
///
/// - 按目标命中率分配读取（口径与 sub2api 一致：`read / (input + read + creation)`）；
/// - 计费不低于原始计费 × `min_cost_ratio`；
/// - 是否写入由 creation control 决定，写入大小受 `min_creation_ratio` 约束；
/// - 非零但过小的缓存读写抬到最小值。
///
/// 优先级：写入频次控制 > 不亏损 > 读写上限 > 命中率。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheHitShapingPolicy {
    /// 总开关，默认关闭。关闭时下游 usage 与 creation control 保持原有逻辑。
    #[serde(default)]
    pub enabled: bool,

    /// 目标命中率下限，范围 0~0.99。会话首请求（没有可读前缀）不受此约束。
    #[serde(default = "default_cache_hit_target_ratio")]
    pub target_hit_ratio: f64,

    /// 在目标值之上的确定性抖动幅度，避免每次都精确等于目标值。范围 0~0.2。
    #[serde(default = "default_cache_hit_ratio_jitter")]
    pub ratio_jitter: f64,

    /// 非零缓存读取的最小值；低于该值时抬到该值。0 表示不处理。
    #[serde(default = "default_cache_hit_min_tokens")]
    pub min_cache_read_tokens: i32,

    /// 非零缓存写入的最小值；低于该值时抬到该值（本次不允许写入时并回 input）。0 表示不处理。
    #[serde(default = "default_cache_hit_min_tokens")]
    pub min_cache_creation_tokens: i32,

    /// 下游计费下限：整形后的计费不低于原始计费的这个倍数，范围 0~5，0 表示不限制。
    #[serde(default = "default_cache_hit_min_cost_ratio")]
    pub min_cost_ratio: f64,

    /// 缓存读取上限：不超过原始输入的这个倍数，范围 0~50，0 表示不限制。
    /// 触及上限后命中率可以低于目标，计费下限仍然生效。
    #[serde(default)]
    pub max_read_multiplier: f64,

    /// 最小写入：放行写入时，写入不低于原始输入的这个比例，范围 0~5，0 表示不限制。
    #[serde(default)]
    pub min_creation_ratio: f64,

    /// 计费下限计入读取，默认关闭。开启后，计费达不到下限时按目标命中率同时增加读取和
    /// 非读取部分，读取的费用也算进下限，同样的命中率下上报的上下文更小。关闭时非读取部分
    /// 先单独满足下限，再在总量之外追加读取到目标命中率（原有行为）。
    /// 只作用于本地链路的命中率整形；上下限优先策略不使用这个开关。
    #[serde(default)]
    pub read_aware_cost_floor: bool,

    /// 计费差额补入 input，默认开启（原有行为）。计费下限仍按原来的方式在读写之间分配；
    /// 写入（和读取）补完仍低于下限时，开启则把剩余差额记入 input，关闭则舍弃剩余差额，
    /// 计费可以低于下限。上下限优先策略关闭时同时不累计差额。
    #[serde(default = "default_true")]
    pub cost_floor_top_up_input: bool,
}

impl Default for CacheHitShapingPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            target_hit_ratio: default_cache_hit_target_ratio(),
            ratio_jitter: default_cache_hit_ratio_jitter(),
            min_cache_read_tokens: default_cache_hit_min_tokens(),
            min_cache_creation_tokens: default_cache_hit_min_tokens(),
            min_cost_ratio: default_cache_hit_min_cost_ratio(),
            max_read_multiplier: 0.0,
            min_creation_ratio: 0.0,
            read_aware_cost_floor: false,
            cost_floor_top_up_input: true,
        }
    }
}

impl CacheHitShapingPolicy {
    pub const MAX_TARGET_HIT_RATIO: f64 = 0.99;
    pub const MAX_RATIO_JITTER: f64 = 0.2;
    pub const MAX_MIN_COST_RATIO: f64 = 5.0;
    pub const MAX_READ_MULTIPLIER: f64 = 50.0;
    pub const MAX_MIN_CREATION_RATIO: f64 = 5.0;

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    pub fn normalized(mut self) -> Self {
        if !self.target_hit_ratio.is_finite() {
            self.target_hit_ratio = default_cache_hit_target_ratio();
        }
        self.target_hit_ratio = self.target_hit_ratio.clamp(0.0, Self::MAX_TARGET_HIT_RATIO);
        if !self.ratio_jitter.is_finite() {
            self.ratio_jitter = default_cache_hit_ratio_jitter();
        }
        self.ratio_jitter = self.ratio_jitter.clamp(0.0, Self::MAX_RATIO_JITTER);
        self.min_cache_read_tokens = self.min_cache_read_tokens.max(0);
        self.min_cache_creation_tokens = self.min_cache_creation_tokens.max(0);
        if !self.min_cost_ratio.is_finite() {
            self.min_cost_ratio = default_cache_hit_min_cost_ratio();
        }
        self.min_cost_ratio = self.min_cost_ratio.clamp(0.0, Self::MAX_MIN_COST_RATIO);
        if !self.max_read_multiplier.is_finite() {
            self.max_read_multiplier = 0.0;
        }
        self.max_read_multiplier = self
            .max_read_multiplier
            .clamp(0.0, Self::MAX_READ_MULTIPLIER);
        if !self.min_creation_ratio.is_finite() {
            self.min_creation_ratio = 0.0;
        }
        self.min_creation_ratio = self
            .min_creation_ratio
            .clamp(0.0, Self::MAX_MIN_CREATION_RATIO);
        self
    }

    pub fn validate(self, label: &str) -> Result<(), String> {
        if !self.target_hit_ratio.is_finite()
            || !(0.0..=Self::MAX_TARGET_HIT_RATIO).contains(&self.target_hit_ratio)
        {
            return Err(format!("{label}.targetHitRatio 必须在 0 到 0.99 之间"));
        }
        if !self.ratio_jitter.is_finite()
            || !(0.0..=Self::MAX_RATIO_JITTER).contains(&self.ratio_jitter)
        {
            return Err(format!("{label}.ratioJitter 必须在 0 到 0.2 之间"));
        }
        if self.min_cache_read_tokens < 0 {
            return Err(format!("{label}.minCacheReadTokens 不能小于 0"));
        }
        if self.min_cache_creation_tokens < 0 {
            return Err(format!("{label}.minCacheCreationTokens 不能小于 0"));
        }
        if !self.min_cost_ratio.is_finite()
            || !(0.0..=Self::MAX_MIN_COST_RATIO).contains(&self.min_cost_ratio)
        {
            return Err(format!("{label}.minCostRatio 必须在 0 到 5 之间"));
        }
        if !self.max_read_multiplier.is_finite()
            || !(0.0..=Self::MAX_READ_MULTIPLIER).contains(&self.max_read_multiplier)
        {
            return Err(format!("{label}.maxReadMultiplier 必须在 0 到 50 之间"));
        }
        if !self.min_creation_ratio.is_finite()
            || !(0.0..=Self::MAX_MIN_CREATION_RATIO).contains(&self.min_creation_ratio)
        {
            return Err(format!("{label}.minCreationRatio 必须在 0 到 5 之间"));
        }
        Ok(())
    }
}

/// 缓存写入时长（5 分钟 / 1 小时）。整次请求的写入只归一种时长：最终下游 usage 的
/// `cache_creation_input_tokens` 全部记入 `cache_creation.ephemeral_5m_input_tokens` 或
/// `ephemeral_1h_input_tokens`。"请求标记"指客户端任一 `cache_control.ttl` 写了 `"1h"`（或大于
/// 5 分钟）或 `"5m"`；只写 `cache_control` 不写 `ttl` 视为未指定。
/// Wire names are spelled out: `rename_all = "snake_case"` would produce `force5m`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum CacheTtlMode {
    /// 跟随请求标记，未指定按 5 分钟（与官方 API 一致）。
    #[default]
    #[serde(rename = "follow_request_default_5m")]
    FollowRequestDefault5m,
    /// 跟随请求标记，未指定按 1 小时。
    #[serde(rename = "follow_request_default_1h")]
    FollowRequestDefault1h,
    /// 忽略请求标记，一律 5 分钟。
    #[serde(rename = "force_5m")]
    Force5m,
    /// 忽略请求标记，一律 1 小时。
    #[serde(rename = "force_1h")]
    Force1h,
    /// 仅账号：渠道返回了 1 小时写入就按 1 小时；渠道只有 5 分钟写入或没有拆分时视为未标记，
    /// 改为跟随请求标记，请求也未指定按 5 分钟。
    #[serde(rename = "upstream_then_request_default_5m")]
    UpstreamThenRequestDefault5m,
}

impl CacheTtlMode {}

/// 路由缓存策略的写入时长设置：本地账号和账号分开配置。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheTtlPolicy {
    #[serde(default)]
    pub account: CacheTtlMode,
}

impl CacheTtlPolicy {
    pub fn validate(self, label: &str) -> Result<(), String> {
        let _ = label;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheTtlPolicyPatch {
    #[serde(default)]
    pub account: Option<CacheTtlMode>,
}

impl CacheTtlPolicyPatch {
    fn apply_to(self, mut policy: CacheTtlPolicy) -> CacheTtlPolicy {
        if let Some(value) = self.account {
            policy.account = value;
        }
        policy
    }

    fn is_empty(&self) -> bool {
        self.account.is_none()
    }

    fn validate_raw(&self, label: &str) -> Result<(), String> {
        let _ = label;
        Ok(())
    }
}

/// 冷请求下游 usage 约束（默认关闭）。
///
/// 开启后，本地前缀追踪本次未命中的请求（新会话首轮、fork / resume 后、服务重启后的首个
/// 请求）在最终下游 usage 上：读取为 0，`input + 写入` 不超过真实输入；input 仍按
/// reportedUsage.input 采样，其余记为写入。命中之后的轮次不受影响。
/// 关闭时与未引入该约束的行为完全一致。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ColdRequestGuardPolicy {
    #[serde(default)]
    pub enabled: bool,
}

impl ColdRequestGuardPolicy {
    pub fn disabled() -> Self {
        Self { enabled: false }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ColdRequestGuardPolicyPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl ColdRequestGuardPolicyPatch {
    fn apply_to(self, mut policy: ColdRequestGuardPolicy) -> ColdRequestGuardPolicy {
        if let Some(value) = self.enabled {
            policy.enabled = value;
        }
        policy
    }

    fn is_empty(&self) -> bool {
        self.enabled.is_none()
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CacheHitShapingPolicyPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub target_hit_ratio: Option<f64>,
    #[serde(default)]
    pub ratio_jitter: Option<f64>,
    #[serde(default)]
    pub min_cache_read_tokens: Option<i32>,
    #[serde(default)]
    pub min_cache_creation_tokens: Option<i32>,
    #[serde(default)]
    pub min_cost_ratio: Option<f64>,
    #[serde(default)]
    pub max_read_multiplier: Option<f64>,
    #[serde(default)]
    pub min_creation_ratio: Option<f64>,
    #[serde(default)]
    pub read_aware_cost_floor: Option<bool>,
    #[serde(default)]
    pub cost_floor_top_up_input: Option<bool>,
}

impl CacheHitShapingPolicyPatch {
    fn apply_to(self, mut policy: CacheHitShapingPolicy) -> CacheHitShapingPolicy {
        if let Some(value) = self.enabled {
            policy.enabled = value;
        }
        if let Some(value) = self.target_hit_ratio {
            policy.target_hit_ratio = value;
        }
        if let Some(value) = self.ratio_jitter {
            policy.ratio_jitter = value;
        }
        if let Some(value) = self.min_cache_read_tokens {
            policy.min_cache_read_tokens = value;
        }
        if let Some(value) = self.min_cache_creation_tokens {
            policy.min_cache_creation_tokens = value;
        }
        if let Some(value) = self.min_cost_ratio {
            policy.min_cost_ratio = value;
        }
        if let Some(value) = self.max_read_multiplier {
            policy.max_read_multiplier = value;
        }
        if let Some(value) = self.min_creation_ratio {
            policy.min_creation_ratio = value;
        }
        if let Some(value) = self.read_aware_cost_floor {
            policy.read_aware_cost_floor = value;
        }
        if let Some(value) = self.cost_floor_top_up_input {
            policy.cost_floor_top_up_input = value;
        }
        policy.normalized()
    }

    fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.target_hit_ratio.is_none()
            && self.ratio_jitter.is_none()
            && self.min_cache_read_tokens.is_none()
            && self.min_cache_creation_tokens.is_none()
            && self.min_cost_ratio.is_none()
            && self.max_read_multiplier.is_none()
            && self.min_creation_ratio.is_none()
            && self.read_aware_cost_floor.is_none()
            && self.cost_floor_top_up_input.is_none()
    }

    fn validate_raw(&self, label: &str) -> Result<(), String> {
        if let Some(value) = self.target_hit_ratio
            && (!value.is_finite()
                || !(0.0..=CacheHitShapingPolicy::MAX_TARGET_HIT_RATIO).contains(&value))
        {
            return Err(format!("{label}.targetHitRatio 必须在 0 到 0.99 之间"));
        }
        if let Some(value) = self.ratio_jitter
            && (!value.is_finite()
                || !(0.0..=CacheHitShapingPolicy::MAX_RATIO_JITTER).contains(&value))
        {
            return Err(format!("{label}.ratioJitter 必须在 0 到 0.2 之间"));
        }
        if self.min_cache_read_tokens.is_some_and(|value| value < 0) {
            return Err(format!("{label}.minCacheReadTokens 不能小于 0"));
        }
        if self
            .min_cache_creation_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.minCacheCreationTokens 不能小于 0"));
        }
        if let Some(value) = self.min_cost_ratio
            && (!value.is_finite()
                || !(0.0..=CacheHitShapingPolicy::MAX_MIN_COST_RATIO).contains(&value))
        {
            return Err(format!("{label}.minCostRatio 必须在 0 到 5 之间"));
        }
        if let Some(value) = self.max_read_multiplier
            && (!value.is_finite()
                || !(0.0..=CacheHitShapingPolicy::MAX_READ_MULTIPLIER).contains(&value))
        {
            return Err(format!("{label}.maxReadMultiplier 必须在 0 到 50 之间"));
        }
        if let Some(value) = self.min_creation_ratio
            && (!value.is_finite()
                || !(0.0..=CacheHitShapingPolicy::MAX_MIN_CREATION_RATIO).contains(&value))
        {
            return Err(format!("{label}.minCreationRatio 必须在 0 到 5 之间"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrefixToolCachePolicy {
    #[serde(default = "default_prefix_tool_coverage_ratio")]
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
    #[serde(default = "default_prefix_tool_reported_input_min_tokens")]
    pub reported_input_min_tokens: i32,
    #[serde(default = "default_prefix_tool_reported_input_max_tokens")]
    pub reported_input_max_tokens: i32,
}

impl Default for PrefixToolCachePolicy {
    fn default() -> Self {
        Self {
            coverage_ratio: default_prefix_tool_coverage_ratio(),
            max_coverage_tokens: 0,
            incremental_create_enabled: true,
            max_new_creation_tokens_per_request: 0,
            cache_current_user_stable_prefix: false,
            current_user_stable_prefix_max_tokens: 0,
            reported_input_min_tokens: default_prefix_tool_reported_input_min_tokens(),
            reported_input_max_tokens: default_prefix_tool_reported_input_max_tokens(),
        }
    }
}

impl PrefixToolCachePolicy {
    pub fn normalized(mut self) -> Self {
        if !self.coverage_ratio.is_finite() {
            self.coverage_ratio = default_prefix_tool_coverage_ratio();
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
pub struct PrefixToolCachePolicyPatch {
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

impl PrefixToolCachePolicyPatch {
    fn apply_to(self, mut policy: PrefixToolCachePolicy) -> PrefixToolCachePolicy {
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
        if let Some(value) = self.coverage_ratio
            && (!(0.0..=1.0).contains(&value) || !value.is_finite())
        {
            return Err(format!("{label}.coverageRatio 必须在 0 到 1 之间"));
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StableSegmentCachePolicy {
    #[serde(default = "default_stable_segment_coverage_ratio")]
    pub coverage_ratio: f64,
    #[serde(default = "default_stable_segment_max_coverage_tokens")]
    pub max_coverage_tokens: i32,
    #[serde(default = "default_stable_segment_min_total_input_tokens")]
    pub min_total_input_tokens: i32,
    #[serde(default = "default_stable_segment_min_segment_tokens")]
    pub min_segment_tokens: i32,
    #[serde(default = "default_true")]
    pub incremental_create_enabled: bool,
    #[serde(default = "default_stable_segment_max_new_creation_tokens_per_request")]
    pub max_new_creation_tokens_per_request: i32,
    #[serde(default = "default_true")]
    pub include_tools: bool,
    #[serde(default = "default_true")]
    pub include_system: bool,
    #[serde(default = "default_true")]
    pub include_history: bool,
    #[serde(default = "default_stable_segment_history_message_limit")]
    pub history_message_limit: usize,
    #[serde(default = "default_true")]
    pub honor_explicit_cache_control: bool,
    #[serde(default = "default_true")]
    pub auto_cache_system: bool,
    #[serde(default = "default_true")]
    pub auto_cache_tools: bool,
    #[serde(default = "default_true")]
    pub auto_cache_history_message_ends: bool,
    #[serde(default)]
    pub cache_current_user_stable_prefix: bool,
    #[serde(default)]
    pub current_user_stable_prefix_max_tokens: i32,
    #[serde(default = "default_stable_segment_default_ttl_secs")]
    pub default_ttl_secs: u64,
    #[serde(default = "default_stable_segment_extended_ttl_secs")]
    pub extended_ttl_secs: u64,
    #[serde(default = "default_stable_segment_reported_input_min_tokens")]
    pub reported_input_min_tokens: i32,
    #[serde(default = "default_stable_segment_reported_input_max_tokens")]
    pub reported_input_max_tokens: i32,
    /// 读写放大倍数（1~3，默认 1 不放大）：真实总输入先乘这个倍数，再按片段覆盖比例归入读写。
    /// 未覆盖部分（input）仍按真实值计算；冷请求不产生读取。
    #[serde(default = "default_stable_segment_token_scale")]
    pub token_scale: f64,
    /// 放大后总输入的软上限，0 表示不限。触到上限时按 capJitter 区间扣减（不超过上限的 8%）。
    #[serde(default)]
    pub max_simulated_input_tokens: i32,
    #[serde(default)]
    pub cap_jitter_min_tokens: i32,
    #[serde(default)]
    pub cap_jitter_max_tokens: i32,
    /// 真实总输入低于这个值时不放大，0 表示总是放大。
    #[serde(default)]
    pub scale_min_input_tokens: i32,
    /// output 放大比例（0~200，默认 0 不放大）。output 超过 outputUpliftMinTokens 时才放大。
    #[serde(default)]
    pub output_uplift_percent: u32,
    /// output 放大门槛，0 表示所有非零 output 都放大。
    #[serde(default)]
    pub output_uplift_min_tokens: i32,
    /// 最终下游读取上限，在命中率整形和冷请求约束之后向下裁剪。0 表示沿用默认上限（70 万）。
    #[serde(default)]
    pub final_cache_read_max_tokens: i32,
    /// 读取上限的确定性扣减区间，避免每次都精确等于上限。
    #[serde(default)]
    pub final_cache_read_jitter_min_tokens: i32,
    #[serde(default)]
    pub final_cache_read_jitter_max_tokens: i32,
    /// 最终下游写入上限。0 表示沿用默认上限（40 万，扣减 2 万~4.5 万）。
    #[serde(default)]
    pub final_cache_creation_max_tokens: i32,
    #[serde(default)]
    pub final_cache_creation_jitter_min_tokens: i32,
    #[serde(default)]
    pub final_cache_creation_jitter_max_tokens: i32,
    /// 最终下游 output 上限（放大之后裁剪）。0 表示不限。
    #[serde(default)]
    pub final_output_max_tokens: i32,
    #[serde(default)]
    pub final_output_jitter_min_tokens: i32,
    #[serde(default)]
    pub final_output_jitter_max_tokens: i32,
}

impl Default for StableSegmentCachePolicy {
    fn default() -> Self {
        Self {
            coverage_ratio: default_stable_segment_coverage_ratio(),
            max_coverage_tokens: default_stable_segment_max_coverage_tokens(),
            min_total_input_tokens: default_stable_segment_min_total_input_tokens(),
            min_segment_tokens: default_stable_segment_min_segment_tokens(),
            incremental_create_enabled: true,
            max_new_creation_tokens_per_request:
                default_stable_segment_max_new_creation_tokens_per_request(),
            include_tools: true,
            include_system: true,
            include_history: true,
            history_message_limit: default_stable_segment_history_message_limit(),
            honor_explicit_cache_control: true,
            auto_cache_system: true,
            auto_cache_tools: true,
            auto_cache_history_message_ends: true,
            cache_current_user_stable_prefix: false,
            current_user_stable_prefix_max_tokens: 0,
            default_ttl_secs: default_stable_segment_default_ttl_secs(),
            extended_ttl_secs: default_stable_segment_extended_ttl_secs(),
            reported_input_min_tokens: default_stable_segment_reported_input_min_tokens(),
            reported_input_max_tokens: default_stable_segment_reported_input_max_tokens(),
            token_scale: default_stable_segment_token_scale(),
            max_simulated_input_tokens: 0,
            cap_jitter_min_tokens: 0,
            cap_jitter_max_tokens: 0,
            scale_min_input_tokens: 0,
            output_uplift_percent: 0,
            output_uplift_min_tokens: 0,
            final_cache_read_max_tokens: 0,
            final_cache_read_jitter_min_tokens: 0,
            final_cache_read_jitter_max_tokens: 0,
            final_cache_creation_max_tokens: 0,
            final_cache_creation_jitter_min_tokens: 0,
            final_cache_creation_jitter_max_tokens: 0,
            final_output_max_tokens: 0,
            final_output_jitter_min_tokens: 0,
            final_output_jitter_max_tokens: 0,
        }
    }
}

impl StableSegmentCachePolicy {
    pub const MAX_TOKEN_SCALE: f64 = 3.0;
    pub const MAX_OUTPUT_UPLIFT_PERCENT: u32 = 200;

    /// 是否配置了读写放大。
    pub fn amplifies_cache(self) -> bool {
        self.token_scale.is_finite() && self.token_scale > 1.0
    }

    /// 是否配置了 output 放大。
    pub fn uplifts_output(self) -> bool {
        self.output_uplift_percent > 0
    }

    pub fn normalized(mut self) -> Self {
        if !self.coverage_ratio.is_finite() {
            self.coverage_ratio = default_stable_segment_coverage_ratio();
        }
        self.coverage_ratio = self.coverage_ratio.clamp(0.0, 1.0);
        self.max_coverage_tokens = self.max_coverage_tokens.max(0);
        self.min_total_input_tokens = self.min_total_input_tokens.max(0);
        self.min_segment_tokens = self.min_segment_tokens.max(0);
        self.max_new_creation_tokens_per_request = self.max_new_creation_tokens_per_request.max(0);
        self.current_user_stable_prefix_max_tokens =
            self.current_user_stable_prefix_max_tokens.max(0);
        self.default_ttl_secs = self.default_ttl_secs.max(1);
        self.extended_ttl_secs = self.extended_ttl_secs.max(self.default_ttl_secs);
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
        self.token_scale = if self.token_scale.is_finite() {
            self.token_scale.clamp(1.0, Self::MAX_TOKEN_SCALE)
        } else {
            default_stable_segment_token_scale()
        };
        self.max_simulated_input_tokens = self.max_simulated_input_tokens.max(0);
        self.cap_jitter_min_tokens = self.cap_jitter_min_tokens.max(0);
        self.cap_jitter_max_tokens = self.cap_jitter_max_tokens.max(0);
        if self.cap_jitter_min_tokens > self.cap_jitter_max_tokens {
            std::mem::swap(
                &mut self.cap_jitter_min_tokens,
                &mut self.cap_jitter_max_tokens,
            );
        }
        self.scale_min_input_tokens = self.scale_min_input_tokens.max(0);
        self.output_uplift_percent = self
            .output_uplift_percent
            .min(Self::MAX_OUTPUT_UPLIFT_PERCENT);
        self.output_uplift_min_tokens = self.output_uplift_min_tokens.max(0);
        for (max, min_jitter, max_jitter) in [
            (
                &mut self.final_cache_read_max_tokens,
                &mut self.final_cache_read_jitter_min_tokens,
                &mut self.final_cache_read_jitter_max_tokens,
            ),
            (
                &mut self.final_cache_creation_max_tokens,
                &mut self.final_cache_creation_jitter_min_tokens,
                &mut self.final_cache_creation_jitter_max_tokens,
            ),
            (
                &mut self.final_output_max_tokens,
                &mut self.final_output_jitter_min_tokens,
                &mut self.final_output_jitter_max_tokens,
            ),
        ] {
            *max = (*max).max(0);
            *min_jitter = (*min_jitter).max(0);
            *max_jitter = (*max_jitter).max(0);
            if *min_jitter > *max_jitter {
                std::mem::swap(min_jitter, max_jitter);
            }
        }
        self
    }

    pub fn validate(self, label: &str) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.coverage_ratio) || !self.coverage_ratio.is_finite() {
            return Err(format!("{label}.coverageRatio 必须在 0 到 1 之间"));
        }
        if !(1.0..=Self::MAX_TOKEN_SCALE).contains(&self.token_scale)
            || !self.token_scale.is_finite()
        {
            return Err(format!("{label}.tokenScale 必须在 1 到 3 之间"));
        }
        for (name, value) in [
            ("maxSimulatedInputTokens", self.max_simulated_input_tokens),
            ("capJitterMinTokens", self.cap_jitter_min_tokens),
            ("capJitterMaxTokens", self.cap_jitter_max_tokens),
            ("scaleMinInputTokens", self.scale_min_input_tokens),
            ("outputUpliftMinTokens", self.output_uplift_min_tokens),
        ] {
            if value < 0 {
                return Err(format!("{label}.{name} 不能小于 0"));
            }
        }
        if self.cap_jitter_min_tokens > self.cap_jitter_max_tokens {
            return Err(format!(
                "{label}.capJitterMinTokens 不能大于 capJitterMaxTokens"
            ));
        }
        if self.output_uplift_percent > Self::MAX_OUTPUT_UPLIFT_PERCENT {
            return Err(format!("{label}.outputUpliftPercent 不能大于 200"));
        }
        if self.final_cache_read_max_tokens < 0 {
            return Err(format!("{label}.finalCacheReadMaxTokens 不能小于 0"));
        }
        if self.final_cache_read_jitter_min_tokens < 0 {
            return Err(format!("{label}.finalCacheReadJitterMinTokens 不能小于 0"));
        }
        if self.final_cache_read_jitter_max_tokens < 0 {
            return Err(format!("{label}.finalCacheReadJitterMaxTokens 不能小于 0"));
        }
        if self.final_cache_creation_max_tokens < 0 {
            return Err(format!("{label}.finalCacheCreationMaxTokens 不能小于 0"));
        }
        if self.final_cache_creation_jitter_min_tokens < 0 {
            return Err(format!(
                "{label}.finalCacheCreationJitterMinTokens 不能小于 0"
            ));
        }
        if self.final_cache_creation_jitter_max_tokens < 0 {
            return Err(format!(
                "{label}.finalCacheCreationJitterMaxTokens 不能小于 0"
            ));
        }
        if self.final_output_max_tokens < 0 {
            return Err(format!("{label}.finalOutputMaxTokens 不能小于 0"));
        }
        if self.final_output_jitter_min_tokens < 0 {
            return Err(format!("{label}.finalOutputJitterMinTokens 不能小于 0"));
        }
        if self.final_output_jitter_max_tokens < 0 {
            return Err(format!("{label}.finalOutputJitterMaxTokens 不能小于 0"));
        }
        if self.final_cache_read_jitter_min_tokens > self.final_cache_read_jitter_max_tokens {
            return Err(format!(
                "{label}.finalCacheReadJitterMinTokens 不能大于 finalCacheReadJitterMaxTokens"
            ));
        }
        if self.final_cache_creation_jitter_min_tokens > self.final_cache_creation_jitter_max_tokens
        {
            return Err(format!(
                "{label}.finalCacheCreationJitterMinTokens 不能大于 finalCacheCreationJitterMaxTokens"
            ));
        }
        if self.final_output_jitter_min_tokens > self.final_output_jitter_max_tokens {
            return Err(format!(
                "{label}.finalOutputJitterMinTokens 不能大于 finalOutputJitterMaxTokens"
            ));
        }
        if self.max_coverage_tokens < 0 {
            return Err(format!("{label}.maxCoverageTokens 不能小于 0"));
        }
        if self.min_total_input_tokens < 0 {
            return Err(format!("{label}.minTotalInputTokens 不能小于 0"));
        }
        if self.min_segment_tokens < 0 {
            return Err(format!("{label}.minSegmentTokens 不能小于 0"));
        }
        if self.max_new_creation_tokens_per_request < 0 {
            return Err(format!("{label}.maxNewCreationTokensPerRequest 不能小于 0"));
        }
        if self.current_user_stable_prefix_max_tokens < 0 {
            return Err(format!(
                "{label}.currentUserStablePrefixMaxTokens 不能小于 0"
            ));
        }
        if self.default_ttl_secs == 0 {
            return Err(format!("{label}.defaultTtlSecs 必须大于 0"));
        }
        if self.extended_ttl_secs < self.default_ttl_secs {
            return Err(format!("{label}.extendedTtlSecs 不能小于 defaultTtlSecs"));
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
pub struct StableSegmentCachePolicyPatch {
    #[serde(default)]
    pub coverage_ratio: Option<f64>,
    #[serde(default)]
    pub max_coverage_tokens: Option<i32>,
    #[serde(default)]
    pub min_total_input_tokens: Option<i32>,
    #[serde(default)]
    pub min_segment_tokens: Option<i32>,
    #[serde(default)]
    pub incremental_create_enabled: Option<bool>,
    #[serde(default)]
    pub max_new_creation_tokens_per_request: Option<i32>,
    #[serde(default)]
    pub include_tools: Option<bool>,
    #[serde(default)]
    pub include_system: Option<bool>,
    #[serde(default)]
    pub include_history: Option<bool>,
    #[serde(default)]
    pub history_message_limit: Option<usize>,
    #[serde(default)]
    pub honor_explicit_cache_control: Option<bool>,
    #[serde(default)]
    pub auto_cache_system: Option<bool>,
    #[serde(default)]
    pub auto_cache_tools: Option<bool>,
    #[serde(default)]
    pub auto_cache_history_message_ends: Option<bool>,
    #[serde(default)]
    pub cache_current_user_stable_prefix: Option<bool>,
    #[serde(default)]
    pub current_user_stable_prefix_max_tokens: Option<i32>,
    #[serde(default)]
    pub default_ttl_secs: Option<u64>,
    #[serde(default)]
    pub extended_ttl_secs: Option<u64>,
    #[serde(default)]
    pub reported_input_min_tokens: Option<i32>,
    #[serde(default)]
    pub reported_input_max_tokens: Option<i32>,
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
    #[serde(default)]
    pub output_uplift_percent: Option<u32>,
    #[serde(default)]
    pub output_uplift_min_tokens: Option<i32>,
    #[serde(default)]
    pub final_cache_read_max_tokens: Option<i32>,
    #[serde(default)]
    pub final_cache_read_jitter_min_tokens: Option<i32>,
    #[serde(default)]
    pub final_cache_read_jitter_max_tokens: Option<i32>,
    #[serde(default)]
    pub final_cache_creation_max_tokens: Option<i32>,
    #[serde(default)]
    pub final_cache_creation_jitter_min_tokens: Option<i32>,
    #[serde(default)]
    pub final_cache_creation_jitter_max_tokens: Option<i32>,
    #[serde(default)]
    pub final_output_max_tokens: Option<i32>,
    #[serde(default)]
    pub final_output_jitter_min_tokens: Option<i32>,
    #[serde(default)]
    pub final_output_jitter_max_tokens: Option<i32>,
}

impl StableSegmentCachePolicyPatch {
    fn apply_to(self, mut policy: StableSegmentCachePolicy) -> StableSegmentCachePolicy {
        if let Some(value) = self.coverage_ratio {
            policy.coverage_ratio = value;
        }
        if let Some(value) = self.max_coverage_tokens {
            policy.max_coverage_tokens = value;
        }
        if let Some(value) = self.min_total_input_tokens {
            policy.min_total_input_tokens = value;
        }
        if let Some(value) = self.min_segment_tokens {
            policy.min_segment_tokens = value;
        }
        if let Some(value) = self.incremental_create_enabled {
            policy.incremental_create_enabled = value;
        }
        if let Some(value) = self.max_new_creation_tokens_per_request {
            policy.max_new_creation_tokens_per_request = value;
        }
        if let Some(value) = self.include_tools {
            policy.include_tools = value;
        }
        if let Some(value) = self.include_system {
            policy.include_system = value;
        }
        if let Some(value) = self.include_history {
            policy.include_history = value;
        }
        if let Some(value) = self.history_message_limit {
            policy.history_message_limit = value;
        }
        if let Some(value) = self.honor_explicit_cache_control {
            policy.honor_explicit_cache_control = value;
        }
        if let Some(value) = self.auto_cache_system {
            policy.auto_cache_system = value;
        }
        if let Some(value) = self.auto_cache_tools {
            policy.auto_cache_tools = value;
        }
        if let Some(value) = self.auto_cache_history_message_ends {
            policy.auto_cache_history_message_ends = value;
        }
        if let Some(value) = self.cache_current_user_stable_prefix {
            policy.cache_current_user_stable_prefix = value;
        }
        if let Some(value) = self.current_user_stable_prefix_max_tokens {
            policy.current_user_stable_prefix_max_tokens = value;
        }
        if let Some(value) = self.default_ttl_secs {
            policy.default_ttl_secs = value;
        }
        if let Some(value) = self.extended_ttl_secs {
            policy.extended_ttl_secs = value;
        }
        if let Some(value) = self.reported_input_min_tokens {
            policy.reported_input_min_tokens = value;
        }
        if let Some(value) = self.reported_input_max_tokens {
            policy.reported_input_max_tokens = value;
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
        if let Some(value) = self.output_uplift_percent {
            policy.output_uplift_percent = value;
        }
        if let Some(value) = self.output_uplift_min_tokens {
            policy.output_uplift_min_tokens = value;
        }
        if let Some(value) = self.final_cache_read_max_tokens {
            policy.final_cache_read_max_tokens = value;
        }
        if let Some(value) = self.final_cache_read_jitter_min_tokens {
            policy.final_cache_read_jitter_min_tokens = value;
        }
        if let Some(value) = self.final_cache_read_jitter_max_tokens {
            policy.final_cache_read_jitter_max_tokens = value;
        }
        if let Some(value) = self.final_cache_creation_max_tokens {
            policy.final_cache_creation_max_tokens = value;
        }
        if let Some(value) = self.final_cache_creation_jitter_min_tokens {
            policy.final_cache_creation_jitter_min_tokens = value;
        }
        if let Some(value) = self.final_cache_creation_jitter_max_tokens {
            policy.final_cache_creation_jitter_max_tokens = value;
        }
        if let Some(value) = self.final_output_max_tokens {
            policy.final_output_max_tokens = value;
        }
        if let Some(value) = self.final_output_jitter_min_tokens {
            policy.final_output_jitter_min_tokens = value;
        }
        if let Some(value) = self.final_output_jitter_max_tokens {
            policy.final_output_jitter_max_tokens = value;
        }
        policy.normalized()
    }

    fn is_empty(&self) -> bool {
        self.coverage_ratio.is_none()
            && self.max_coverage_tokens.is_none()
            && self.min_total_input_tokens.is_none()
            && self.min_segment_tokens.is_none()
            && self.incremental_create_enabled.is_none()
            && self.max_new_creation_tokens_per_request.is_none()
            && self.include_tools.is_none()
            && self.include_system.is_none()
            && self.include_history.is_none()
            && self.history_message_limit.is_none()
            && self.honor_explicit_cache_control.is_none()
            && self.auto_cache_system.is_none()
            && self.auto_cache_tools.is_none()
            && self.auto_cache_history_message_ends.is_none()
            && self.cache_current_user_stable_prefix.is_none()
            && self.current_user_stable_prefix_max_tokens.is_none()
            && self.default_ttl_secs.is_none()
            && self.extended_ttl_secs.is_none()
            && self.reported_input_min_tokens.is_none()
            && self.reported_input_max_tokens.is_none()
            && self.token_scale.is_none()
            && self.max_simulated_input_tokens.is_none()
            && self.cap_jitter_min_tokens.is_none()
            && self.cap_jitter_max_tokens.is_none()
            && self.scale_min_input_tokens.is_none()
            && self.output_uplift_percent.is_none()
            && self.output_uplift_min_tokens.is_none()
            && self.final_cache_read_max_tokens.is_none()
            && self.final_cache_read_jitter_min_tokens.is_none()
            && self.final_cache_read_jitter_max_tokens.is_none()
            && self.final_cache_creation_max_tokens.is_none()
            && self.final_cache_creation_jitter_min_tokens.is_none()
            && self.final_cache_creation_jitter_max_tokens.is_none()
            && self.final_output_max_tokens.is_none()
            && self.final_output_jitter_min_tokens.is_none()
            && self.final_output_jitter_max_tokens.is_none()
    }

    fn validate_raw(&self, label: &str) -> Result<(), String> {
        if let Some(value) = self.coverage_ratio
            && (!(0.0..=1.0).contains(&value) || !value.is_finite())
        {
            return Err(format!("{label}.coverageRatio 必须在 0 到 1 之间"));
        }
        if self.max_coverage_tokens.is_some_and(|value| value < 0) {
            return Err(format!("{label}.maxCoverageTokens 不能小于 0"));
        }
        if self.min_total_input_tokens.is_some_and(|value| value < 0) {
            return Err(format!("{label}.minTotalInputTokens 不能小于 0"));
        }
        if self.min_segment_tokens.is_some_and(|value| value < 0) {
            return Err(format!("{label}.minSegmentTokens 不能小于 0"));
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
        if self.default_ttl_secs == Some(0) {
            return Err(format!("{label}.defaultTtlSecs 必须大于 0"));
        }
        if matches!(
            (self.default_ttl_secs, self.extended_ttl_secs),
            (Some(default_ttl), Some(extended_ttl)) if extended_ttl < default_ttl
        ) {
            return Err(format!("{label}.extendedTtlSecs 不能小于 defaultTtlSecs"));
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
        if let Some(value) = self.token_scale
            && (!(1.0..=StableSegmentCachePolicy::MAX_TOKEN_SCALE).contains(&value)
                || !value.is_finite())
        {
            return Err(format!("{label}.tokenScale 必须在 1 到 3 之间"));
        }
        for (name, value) in [
            ("maxSimulatedInputTokens", self.max_simulated_input_tokens),
            ("capJitterMinTokens", self.cap_jitter_min_tokens),
            ("capJitterMaxTokens", self.cap_jitter_max_tokens),
            ("scaleMinInputTokens", self.scale_min_input_tokens),
            ("outputUpliftMinTokens", self.output_uplift_min_tokens),
        ] {
            if value.is_some_and(|value| value < 0) {
                return Err(format!("{label}.{name} 不能小于 0"));
            }
        }
        if matches!(
            (self.cap_jitter_min_tokens, self.cap_jitter_max_tokens),
            (Some(min), Some(max)) if min > max
        ) {
            return Err(format!(
                "{label}.capJitterMinTokens 不能大于 capJitterMaxTokens"
            ));
        }
        if self
            .output_uplift_percent
            .is_some_and(|value| value > StableSegmentCachePolicy::MAX_OUTPUT_UPLIFT_PERCENT)
        {
            return Err(format!("{label}.outputUpliftPercent 不能大于 200"));
        }
        if self
            .final_cache_read_max_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.finalCacheReadMaxTokens 不能小于 0"));
        }
        if self
            .final_cache_read_jitter_min_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.finalCacheReadJitterMinTokens 不能小于 0"));
        }
        if self
            .final_cache_read_jitter_max_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.finalCacheReadJitterMaxTokens 不能小于 0"));
        }
        if self
            .final_cache_creation_max_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.finalCacheCreationMaxTokens 不能小于 0"));
        }
        if self
            .final_cache_creation_jitter_min_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!(
                "{label}.finalCacheCreationJitterMinTokens 不能小于 0"
            ));
        }
        if self
            .final_cache_creation_jitter_max_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!(
                "{label}.finalCacheCreationJitterMaxTokens 不能小于 0"
            ));
        }
        if self.final_output_max_tokens.is_some_and(|value| value < 0) {
            return Err(format!("{label}.finalOutputMaxTokens 不能小于 0"));
        }
        if self
            .final_output_jitter_min_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.finalOutputJitterMinTokens 不能小于 0"));
        }
        if self
            .final_output_jitter_max_tokens
            .is_some_and(|value| value < 0)
        {
            return Err(format!("{label}.finalOutputJitterMaxTokens 不能小于 0"));
        }
        if matches!((self.final_cache_read_jitter_min_tokens, self.final_cache_read_jitter_max_tokens), (Some(min), Some(max)) if min > max)
        {
            return Err(format!(
                "{label}.finalCacheReadJitterMinTokens 不能大于 finalCacheReadJitterMaxTokens"
            ));
        }
        if matches!((self.final_cache_creation_jitter_min_tokens, self.final_cache_creation_jitter_max_tokens), (Some(min), Some(max)) if min > max)
        {
            return Err(format!(
                "{label}.finalCacheCreationJitterMinTokens 不能大于 finalCacheCreationJitterMaxTokens"
            ));
        }
        if matches!((self.final_output_jitter_min_tokens, self.final_output_jitter_max_tokens), (Some(min), Some(max)) if min > max)
        {
            return Err(format!(
                "{label}.finalOutputJitterMinTokens 不能大于 finalOutputJitterMaxTokens"
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
    pub bounds: Option<CacheBoundsPolicyPatch>,
    #[serde(default)]
    pub prefix_tool: Option<PrefixToolCachePolicyPatch>,
    /// 前缀追踪 Plus 策略参数；整体替换，缺失字段取默认值。
    #[serde(default)]
    pub prefix_tool_plus: Option<PrefixToolPlusCachePolicy>,
    /// Boxed: the patch has ~25 optional fields and `CachePolicyConfig` embeds this struct
    /// several times, which otherwise bloats every `Config` copy on debug-build stacks.
    #[serde(default)]
    pub stable_segment: Option<Box<StableSegmentCachePolicyPatch>>,
    #[serde(default)]
    pub hit_shaping: Option<CacheHitShapingPolicyPatch>,
    #[serde(default)]
    pub cold_request_guard: Option<ColdRequestGuardPolicyPatch>,
    /// 缓存写入时长；所有缓存策略都生效（no_cache 无写入，不受影响）。
    #[serde(default)]
    pub cache_ttl: Option<CacheTtlPolicyPatch>,
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

        if let Some(patch) = self.bounds {
            policy.bounds = patch.apply_to(policy.bounds);
        }
        if let Some(patch) = self.prefix_tool {
            policy.prefix_tool = patch.apply_to(policy.prefix_tool);
        }
        if let Some(patch) = self.stable_segment.as_deref() {
            policy.stable_segment = patch.apply_to(policy.stable_segment);
        }
        if let Some(patch) = self.hit_shaping {
            policy.hit_shaping = patch.apply_to(policy.hit_shaping);
        }
        if let Some(patch) = self.cold_request_guard {
            policy.cold_request_guard = patch.apply_to(policy.cold_request_guard);
        }
        if let Some(patch) = self.cache_ttl {
            policy.cache_ttl = patch.apply_to(policy.cache_ttl);
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
        if let Some(patch) = &self.prefix_tool {
            patch.validate_raw(&format!("{label}.prefixTool"))?;
        }
        if let Some(policy) = &self.prefix_tool_plus {
            policy.validate(&format!("{label}.prefixToolPlus"))?;
        }
        if let Some(patch) = &self.stable_segment {
            patch.validate_raw(&format!("{label}.stableSegment"))?;
        }
        if let Some(patch) = &self.hit_shaping {
            patch.validate_raw(&format!("{label}.hitShaping"))?;
        }
        if let Some(patch) = &self.cache_ttl {
            patch.validate_raw(&format!("{label}.cacheTtl"))?;
        }
        let policy = self.apply_route_to(base);
        policy.validate(label)
    }

    fn apply_fields_for_strategy(&self, policy: CacheRoutePolicy) -> CacheRoutePolicy {
        match policy.cache_type {
            PromptCacheStrategyType::NoCache => self.apply_no_cache_fields_to(policy),
            PromptCacheStrategyType::CurrentHighCache
            | PromptCacheStrategyType::BoundedHighCache => self.apply_fields_to(policy),
            PromptCacheStrategyType::PrefixTool => self.apply_prefix_tool_fields_to(policy),
            PromptCacheStrategyType::PrefixToolPlus => {
                self.apply_prefix_tool_plus_fields_to(policy)
            }
            PromptCacheStrategyType::StableSegmentCache => {
                self.apply_stable_segment_fields_to(policy)
            }
        }
    }

    fn apply_no_cache_fields_to(&self, mut policy: CacheRoutePolicy) -> CacheRoutePolicy {
        policy.reported_usage = ReportedUsagePathPolicy::disabled().normalized();
        policy.normalized()
    }

    fn apply_prefix_tool_fields_to(&self, mut policy: CacheRoutePolicy) -> CacheRoutePolicy {
        if let Some(cache_type) = self.cache_type {
            policy.cache_type = cache_type;
        }
        if let Some(reported_usage) = &self.reported_usage {
            policy.reported_usage = reported_usage.normalized();
        }

        if let Some(patch) = self.bounds {
            policy.bounds = patch.apply_to(policy.bounds);
        }
        if let Some(patch) = self.prefix_tool {
            policy.prefix_tool = patch.apply_to(policy.prefix_tool);
        }
        if let Some(patch) = self.hit_shaping {
            policy.hit_shaping = patch.apply_to(policy.hit_shaping);
        }
        if let Some(patch) = self.cold_request_guard {
            policy.cold_request_guard = patch.apply_to(policy.cold_request_guard);
        }
        if let Some(patch) = self.cache_ttl {
            policy.cache_ttl = patch.apply_to(policy.cache_ttl);
        }
        policy.normalized()
    }

    /// Plus 策略自带整形与写入频次控制，不叠加 hitShaping / coldRequestGuard / reportedUsage。
    fn apply_prefix_tool_plus_fields_to(&self, mut policy: CacheRoutePolicy) -> CacheRoutePolicy {
        if let Some(cache_type) = self.cache_type {
            policy.cache_type = cache_type;
        }

        if let Some(patch) = self.bounds {
            policy.bounds = patch.apply_to(policy.bounds);
        }
        if let Some(plus) = self.prefix_tool_plus {
            policy.prefix_tool_plus = plus.normalized();
        }
        if let Some(patch) = self.cache_ttl {
            policy.cache_ttl = patch.apply_to(policy.cache_ttl);
        }
        policy.normalized()
    }

    fn apply_stable_segment_fields_to(&self, mut policy: CacheRoutePolicy) -> CacheRoutePolicy {
        if let Some(cache_type) = self.cache_type {
            policy.cache_type = cache_type;
        }
        if let Some(reported_usage) = &self.reported_usage {
            policy.reported_usage = reported_usage.normalized();
        }

        if let Some(patch) = self.bounds {
            policy.bounds = patch.apply_to(policy.bounds);
        }
        if let Some(patch) = self.stable_segment.as_deref() {
            policy.stable_segment = patch.apply_to(policy.stable_segment);
        }
        if let Some(patch) = self.hit_shaping {
            policy.hit_shaping = patch.apply_to(policy.hit_shaping);
        }
        if let Some(patch) = self.cold_request_guard {
            policy.cold_request_guard = patch.apply_to(policy.cold_request_guard);
        }
        if let Some(patch) = self.cache_ttl {
            policy.cache_ttl = patch.apply_to(policy.cache_ttl);
        }
        policy.normalized()
    }

    fn affects_cache_state(&self) -> bool {
        self.cache_type
            .is_some_and(|cache_type| cache_type != PromptCacheStrategyType::NoCache)
            || self.route_namespace.is_some()
            || self.simulation.is_some()
            || self.creation_control.is_some()
            || self.bounds.is_some()
            || self.prefix_tool.is_some()
            || self.prefix_tool_plus.is_some()
            || self.stable_segment.is_some()
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
                .bounds
                .as_ref()
                .is_none_or(CacheBoundsPolicyPatch::is_empty)
            && self
                .prefix_tool
                .as_ref()
                .is_none_or(PrefixToolCachePolicyPatch::is_empty)
            && self.prefix_tool_plus.is_none()
            && self
                .stable_segment
                .as_deref()
                .is_none_or(StableSegmentCachePolicyPatch::is_empty)
            && self
                .hit_shaping
                .as_ref()
                .is_none_or(CacheHitShapingPolicyPatch::is_empty)
            && self
                .cold_request_guard
                .as_ref()
                .is_none_or(ColdRequestGuardPolicyPatch::is_empty)
            && self
                .cache_ttl
                .as_ref()
                .is_none_or(CacheTtlPolicyPatch::is_empty)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheStrategyType {
    NoCache,
    #[default]
    CurrentHighCache,
    PrefixTool,
    StableSegmentCache,
    PrefixToolPlus,
    /// 上下限优先的本地模拟缓存：模拟、前缀追踪、reportedUsage、creationControl、hitShaping
    /// 与 `current_high_cache` 相同，但各字段上下限是最终约束，计费只在范围内补，
    /// 补不上的差额跨请求记账。
    BoundedHighCache,
}

impl PromptCacheStrategyType {
    /// 走本地模拟缓存（high cache 模拟、前缀追踪）链路的策略。
    pub fn uses_high_cache_simulation(self) -> bool {
        matches!(self, Self::CurrentHighCache | Self::BoundedHighCache)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CachePolicyConfig {
    #[serde(default)]
    pub default: CacheRoutePolicyPatch,
    #[serde(default)]
    pub current_high_cache: CacheRoutePolicyPatch,
    #[serde(default)]
    pub prefix_tool: CacheRoutePolicyPatch,
    #[serde(default)]
    pub stable_segment_cache: CacheRoutePolicyPatch,
    #[serde(default)]
    pub prefix_tool_plus: CacheRoutePolicyPatch,
    /// 上下限优先的本地模拟缓存策略模板（`bounded_high_cache`），不继承 `default`。
    #[serde(default)]
    pub bounded_high_cache: CacheRoutePolicyPatch,
}

impl CachePolicyConfig {
    pub fn normalized(&self) -> Self {
        self.clone()
    }

    pub fn validate(&self, base: CacheRoutePolicy) -> Result<(), String> {
        self.default
            .validate("当前本地模拟策略兼容参数", base.clone())?;
        self.current_high_cache
            .validate("当前本地模拟策略模板", base.clone())?;
        self.prefix_tool
            .validate("前缀追踪 缓存策略模板", base.clone())?;
        self.stable_segment_cache
            .validate("Stable Segment 缓存策略模板", base.clone())?;
        self.prefix_tool_plus
            .validate("前缀追踪 Plus 缓存策略模板", base.clone())?;
        self.bounded_high_cache
            .validate("上下限优先缓存策略模板", base.clone())?;
        Ok(())
    }

    fn current_high_cache_template(&self, base: CacheRoutePolicy) -> CacheRoutePolicy {
        let policy = self.default.apply_fields_to(base);
        self.current_high_cache.apply_fields_to(policy)
    }

    fn bounded_high_cache_template(&self, base: CacheRoutePolicy) -> CacheRoutePolicy {
        let mut policy = self.bounded_high_cache.apply_fields_to(base);
        policy.cache_type = PromptCacheStrategyType::BoundedHighCache;
        policy.normalized()
    }

    fn prefix_tool_template(&self, base: CacheRoutePolicy) -> CacheRoutePolicy {
        let neutral = CacheRoutePolicy {
            cache_type: PromptCacheStrategyType::PrefixTool,
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

            bounds: base.bounds,
            prefix_tool: PrefixToolCachePolicy::default(),
            stable_segment: base.stable_segment,
            hit_shaping: base.hit_shaping,
            cold_request_guard: base.cold_request_guard,
            prefix_tool_plus: base.prefix_tool_plus,
            cache_ttl: base.cache_ttl,
        };
        self.prefix_tool.apply_prefix_tool_fields_to(neutral)
    }

    fn stable_segment_template(&self, base: CacheRoutePolicy) -> CacheRoutePolicy {
        let neutral = CacheRoutePolicy {
            cache_type: PromptCacheStrategyType::StableSegmentCache,
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

            bounds: base.bounds,
            prefix_tool: base.prefix_tool,
            stable_segment: StableSegmentCachePolicy::default(),
            hit_shaping: base.hit_shaping,
            cold_request_guard: base.cold_request_guard,
            prefix_tool_plus: base.prefix_tool_plus,
            cache_ttl: base.cache_ttl,
        };
        self.stable_segment_cache
            .apply_stable_segment_fields_to(neutral)
    }

    fn prefix_tool_plus_template(&self, base: CacheRoutePolicy) -> CacheRoutePolicy {
        let neutral = CacheRoutePolicy {
            cache_type: PromptCacheStrategyType::PrefixToolPlus,
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

            bounds: base.bounds,
            prefix_tool: base.prefix_tool,
            stable_segment: base.stable_segment,
            hit_shaping: CacheHitShapingPolicy::disabled(),
            cold_request_guard: ColdRequestGuardPolicy::disabled(),
            prefix_tool_plus: PrefixToolPlusCachePolicy::default(),
            cache_ttl: base.cache_ttl,
        };
        self.prefix_tool_plus
            .apply_prefix_tool_plus_fields_to(neutral)
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

            bounds: base.bounds,
            prefix_tool: base.prefix_tool,
            stable_segment: base.stable_segment,
            hit_shaping: CacheHitShapingPolicy::disabled(),
            cold_request_guard: ColdRequestGuardPolicy::disabled(),
            prefix_tool_plus: base.prefix_tool_plus,
            cache_ttl: base.cache_ttl,
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
            PromptCacheStrategyType::PrefixTool => {
                let mut policy = self.prefix_tool_template(base);
                policy.cache_type = PromptCacheStrategyType::PrefixTool;
                policy.normalized()
            }
            PromptCacheStrategyType::StableSegmentCache => {
                let mut policy = self.stable_segment_template(base);
                policy.cache_type = PromptCacheStrategyType::StableSegmentCache;
                policy.normalized()
            }
            PromptCacheStrategyType::PrefixToolPlus => {
                let mut policy = self.prefix_tool_plus_template(base);
                policy.cache_type = PromptCacheStrategyType::PrefixToolPlus;
                policy.normalized()
            }
            PromptCacheStrategyType::BoundedHighCache => self.bounded_high_cache_template(base),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CacheRoutePolicy {
    pub cache_type: PromptCacheStrategyType,
    pub simulation: CacheSimulationPolicy,
    pub creation_control: PromptCacheCreationControlConfig,
    pub reported_usage: ReportedUsagePathPolicy,

    pub bounds: CacheBoundsPolicy,
    pub prefix_tool: PrefixToolCachePolicy,
    pub stable_segment: StableSegmentCachePolicy,
    pub hit_shaping: CacheHitShapingPolicy,
    pub cold_request_guard: ColdRequestGuardPolicy,
    pub prefix_tool_plus: PrefixToolPlusCachePolicy,
    /// 缓存写入时长（5 分钟 / 1 小时），本地账号与账号分开设置；所有缓存策略共用。
    pub cache_ttl: CacheTtlPolicy,
}

impl CacheRoutePolicy {
    pub fn normalized(mut self) -> Self {
        self.simulation = self.simulation.normalized();
        self.creation_control = self.creation_control.normalized();
        self.reported_usage = self.reported_usage.normalized();
        self.prefix_tool = self.prefix_tool.normalized();
        self.stable_segment = self.stable_segment.normalized();
        self.hit_shaping = self.hit_shaping.normalized();
        self.prefix_tool_plus = self.prefix_tool_plus.normalized();
        self
    }

    pub fn validate(&self, label: &str) -> Result<(), String> {
        self.simulation.validate(&format!("{label}.simulation"))?;
        self.hit_shaping.validate(&format!("{label}.hitShaping"))?;
        self.creation_control
            .validate()
            .map_err(|err| format!("{label}.creationControl: {err}"))?;
        self.reported_usage
            .validate(&format!("{label}.reportedUsage"))?;
        self.bounds.validate(&format!("{label}.bounds"))?;
        self.prefix_tool.validate(&format!("{label}.prefixTool"))?;
        self.stable_segment
            .validate(&format!("{label}.stableSegment"))?;
        self.prefix_tool_plus
            .validate(&format!("{label}.prefixToolPlus"))?;
        self.cache_ttl.validate(&format!("{label}.cacheTtl"))?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCacheRoutePolicy {
    pub policy: CacheRoutePolicy,
    pub namespace: Option<String>,
}

pub fn resolve_cache_policy_for_route(
    base: CacheRoutePolicy,
    _reported_usage: &ReportedUsageConfig,
    cache_policy: &CachePolicyConfig,
    route: &RouteConfig,
) -> ResolvedCacheRoutePolicy {
    let cache_policy = cache_policy.normalized();
    let mut path_base = base;
    if let Some(reported_usage) = &route.reported_usage {
        path_base.reported_usage = reported_usage.normalized();
    }
    let prefix = &route.prefix;
    let override_policy = &route.cache_policy;
    let cache_type = override_policy.explicit_cache_type();
    let mut policy = cache_policy.strategy_policy(cache_type, path_base);
    if override_policy.reported_usage.is_none() {
        if let Some(reported_usage) = &route.reported_usage {
            policy.reported_usage = reported_usage.normalized();
        }
    }
    let policy = override_policy.apply_fields_for_strategy(policy);
    let namespace = match cache_type {
        PromptCacheStrategyType::NoCache => None,
        PromptCacheStrategyType::PrefixTool | PromptCacheStrategyType::PrefixToolPlus => {
            Some(prefix.clone())
        }
        PromptCacheStrategyType::StableSegmentCache => override_policy
            .route_namespace
            .unwrap_or_else(|| override_policy.affects_cache_state())
            .then(|| prefix.clone()),
        PromptCacheStrategyType::CurrentHighCache | PromptCacheStrategyType::BoundedHighCache => {
            override_policy
                .route_namespace
                .unwrap_or_else(|| override_policy.affects_cache_state())
                .then(|| prefix.clone())
        }
    };

    ResolvedCacheRoutePolicy {
        policy: policy.normalized(),
        namespace,
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
/// the 上游 upstream. `anthropic-strict` minimizes synthetic protocol and
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
/// `real_request`：客户端请求带 Claude Code 可见 thinking 信号（如 `ultrathink`）时开启
/// thinking 并强制可见输出；`always`：请求未显式关闭 thinking 时一律开启；`off`：只按客户端
/// 显式的 `thinking` 字段处理，代理不自动开启，也不强制可见输出。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingTriggerMode {
    RealRequest,
    Always,
    Off,
}

impl Default for ThinkingTriggerMode {
    fn default() -> Self {
        Self::Off
    }
}

/// 请求模型解析策略。
///
/// `compatible` 保持当前 Claude Code 兼容行为：允许 `sonnet`、`opus`、
/// `default` 等短别名，也允许把同 family 的旧版/未来模型名映射到当前
/// 上游 上游可用模型。`alias_only` 只允许精确模型和显式别名，不做
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
/// 解析顺序固定为：显式映射 -> 上游模型列表精确匹配 -> 后缀兼容 ->
/// 同版本格式兼容 -> 自动别名/同族跨版本 -> 可选跨族兜底 -> 透传。
/// 关闭 `enabled` 后，规则和自动兼容解析都不生效；关闭自动规则且规则列表为空时，
/// 未精确匹配的模型会直接透传给上游。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelMappingConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_generate_rules: bool,
    /// 允许已知 Claude 模型在同族内跨小版本解析到当前可用模型。
    ///
    /// 例如请求一个已下线的 dated Sonnet 4，目录只有 Sonnet 4.5 时，
    /// 可以选择将请求解析到 4.5。关闭后仍保留同版本的点号/短横线兼容。
    #[serde(default = "default_true")]
    pub allow_version_fallback: bool,
    /// 允许在请求模型所属族完全没有可用模型时跨 Claude 族兜底。
    ///
    /// 默认关闭，避免 Sonnet/Opus/Haiku 之间静默改变能力语义。
    #[serde(default)]
    pub allow_cross_family_fallback: bool,
    #[serde(default)]
    pub rules: Vec<ModelMappingRule>,
}

impl Default for ModelMappingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_generate_rules: true,
            allow_version_fallback: true,
            allow_cross_family_fallback: false,
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

/// 上游 payload guard 的大小裁剪触发模式。
///
/// `preemptive` 在 上游 local path 按 `payloadGuardMaxWeight`、
/// 在外部/Anthropic byte path 按 `payloadGuardMaxBytes`，发送上游前执行配置的
/// 内容整形和裁剪。`on_too_long` 首次请求只做协议修复；只有上游返回输入过长类
/// 错误后，才按对应口径裁剪并重试一次。
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

/// 上游判定输入过长后的处理方式（对所有路由生效，与路由挂载的 usage 策略无关）。
///
/// - `client_compaction`（默认）：按 Claude Code 协议返回 `prompt is too long`，由客户端自动压缩后
///   重试；只有 Claude Code 自己的压缩请求仍按 payload guard 裁剪重试，保证压缩本身能完成。
/// - `trim_retry`：旧行为，所有请求都由服务端裁剪历史后重试。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayloadTooLongHandling {
    #[default]
    ClientCompaction,
    TrimRetry,
}

/// 外部备用号池并发满时的处理模式。
///
/// `fail_fast` 保持历史行为：当前没有账号并发槽时立即返回调度不可用。
/// `wait` 会进入账号独立等待队列，直到拿到账号槽位或超过配置等待时间。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapacityMode {
    FailFast,
    Wait,
}

impl Default for CapacityMode {
    fn default() -> Self {
        Self::Wait
    }
}

/// 账号入口路由控制模式。
///
/// 默认 `allow_all` 保持历史行为。`allow_list` 只允许命中规则的入口进入账号；
/// `deny_list` 则禁止命中规则的入口进入账号，其它入口保持可用。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RouteRuleMode {
    #[default]
    AllowAll,
    AllowList,
    DenyList,
}

impl RouteRuleMode {
    pub fn parse(value: &str) -> Self {
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "allow_all" | "allow-all" | "all" => Some(Self::AllowAll),
            "allow_list" | "allow-list" | "allowlist" => Some(Self::AllowList),
            "deny_list" | "deny-list" | "denylist" => Some(Self::DenyList),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowAll => "allow_all",
            Self::AllowList => "allow_list",
            Self::DenyList => "deny_list",
        }
    }
}

/// 账号返回模型不可用时的冷却范围。
///
/// `model` 是默认值：只短暂避开当前账号的当前模型，避免一个不支持模型把
/// 整个账号冷却并把后续请求推入全局排队队列。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelUnavailableCooldownMode {
    Disabled,
    #[default]
    Model,
    Pool,
}

/// 账号流式响应处理模式。
///
/// `event_passthrough` 保持 SSE event 级透传，并会屏蔽上游流式错误事件中的
/// 内部细节。是否改写下游可见 usage 不由这里决定，而由单个账号的
/// `usageProjectionMode` 决定：`pass_through` 原样返回上游 usage，
/// `current_path_policy` 按入口路径缓存策略整理 usage。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StreamResponseMode {
    #[serde(
        rename = "event_passthrough",
        alias = "event_passthrough_usage_rewrite",
        alias = "event_passthrough_capture"
    )]
    EventPassthrough,
}

impl Default for StreamResponseMode {
    fn default() -> Self {
        Self::EventPassthrough
    }
}

impl StreamResponseMode {
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
///
/// 注意：因质量评分权重为 `f64`，本结构体只实现 `PartialEq` 不实现 `Eq`。
/// 既有的配置变更检测（`account_policy_changed`）只依赖 `PartialEq`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountsConfig {
    #[serde(skip)]
    pub transport_proxy_url: Option<String>,
    #[serde(skip)]
    pub transport_proxy_username: Option<String>,
    #[serde(skip)]
    pub transport_proxy_password: Option<String>,
    #[serde(skip)]
    pub routes: RoutesConfig,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_account_global_max_concurrent_requests")]
    pub global_max_concurrent_requests: u32,
    #[serde(default = "default_account_max_queued_requests")]
    pub max_queued_requests: u32,
    #[serde(default)]
    pub capacity_mode: CapacityMode,
    #[serde(default = "default_account_dispatch_max_wait_secs")]
    pub dispatch_max_wait_secs: u64,
    #[serde(default = "default_account_retry_max_attempts")]
    pub retry_max_attempts: u32,
    /// 账号之间故障转移时允许重试的 HTTP 状态码。
    ///
    /// 认证、配额、渠道禁用等错误默认也按临时抖动信号处理；该列表控制
    /// 普通 HTTP 错误是否继续消耗跨池重试预算。默认列表中的 500 作为
    /// 5xx 族兜底，覆盖 523/524/599 等未逐个枚举的上游临时错误；如果
    /// 运营侧显式只配置 502 等具体状态码，则按配置收窄。
    #[serde(default = "default_account_retry_status_codes")]
    pub retry_status_codes: Vec<u16>,
    /// 连接、DNS、超时等没有 HTTP 状态码的网络错误是否允许跨池重试。
    #[serde(default = "default_true")]
    pub retry_on_network_error: bool,
    /// 成功状态码下返回错误信封、SSE 协议污染等协议错误是否允许跨池重试。
    #[serde(default = "default_true")]
    pub retry_on_protocol_error: bool,
    #[serde(default = "default_account_same_pool_retry_count")]
    pub same_pool_retry_count: u32,
    #[serde(default = "default_account_same_pool_retry_status_codes")]
    pub same_pool_retry_status_codes: Vec<u16>,
    #[serde(default = "default_account_same_pool_retry_delay_ms")]
    pub same_pool_retry_delay_ms: u64,
    /// 账号瞬态失败调度罚分。
    ///
    /// 每一个仍在 Redis 瞬态失败窗口内的失败 streak，都会临时增加池的
    /// 有效优先级。默认 20 可让优先级 1 的故障池在一次可重试失败后让位给
    /// 优先级 10/20 的健康池；填 0 会退回只按配置优先级和负载排序。
    #[serde(default = "default_account_transient_failure_priority_penalty")]
    pub transient_failure_priority_penalty: u32,

    // ==================== 质量感知调度（被动采样） ====================
    //
    // 这组配置让外部账号调度从"优先级 + 负载"的字典序排序，升级为把
    // 真实请求观测到的失败率与首字延迟一并纳入的加权评分。
    //
    // 全部数据来自用户真实请求，不做任何主动探测。
    /// 质量感知调度主开关。
    ///
    /// 关闭后完全退回"有效优先级 → 负载"的字典序排序；质量状态仍可随既有
    /// 运行态快照读取，但不参与选择、不展示，也不会写入新的质量样本，
    /// 行为与引入本特性之前的选择结果完全一致，可作为线上快速回退手段。
    #[serde(default = "default_account_quality_aware_scheduling_enabled")]
    pub quality_aware_scheduling_enabled: bool,
    /// 质量 EWMA 平滑系数，有效窗口约为最近 `1/alpha` 个请求。
    #[serde(default = "default_account_quality_ewma_alpha")]
    pub quality_ewma_alpha: f64,
    /// 质量样本有效期；超过该时长无请求则质量数据过期并回到中性值。
    #[serde(default = "default_account_quality_sample_ttl_secs")]
    pub quality_sample_ttl_secs: u64,
    /// 参与质量评分所需的最少样本数；不足时按中性值处理，避免冷启动误判。
    #[serde(default = "default_account_quality_min_samples")]
    pub quality_min_samples: u64,
    /// 优先级权重（分数越低越优先）。
    #[serde(default = "default_account_quality_priority_weight")]
    pub quality_priority_weight: f64,
    /// 负载率权重。
    #[serde(default = "default_account_quality_load_weight")]
    pub quality_load_weight: f64,
    /// 失败率权重。
    ///
    /// 量级刻意显著高于延迟权重：报错是比"慢"严重得多的问题，
    /// 必须能压过首字延迟的倾向，但仍保持在同一个连续可比空间内。
    #[serde(default = "default_account_quality_error_weight")]
    pub quality_error_weight: f64,
    /// 相对首字延迟权重（以候选集中位数为 1.0 的相对值）。
    #[serde(default = "default_account_quality_latency_weight")]
    pub quality_latency_weight: f64,
    /// 临时避让权重。
    #[serde(default = "default_account_quality_probation_weight")]
    pub quality_probation_weight: f64,
    /// 候选池大小 K：在分数最优的 K 个候选里加权随机，避免惊群。
    #[serde(default = "default_account_quality_top_k")]
    pub quality_top_k: u32,
    /// 持续劣化判定窗口。
    #[serde(default = "default_account_degrade_window_secs")]
    pub degrade_window_secs: u64,
    /// 触发临时降级的失败率阈值。
    #[serde(default = "default_account_degrade_error_rate_threshold")]
    pub degrade_error_rate_threshold: f64,
    /// 临时降级时长。
    #[serde(default = "default_account_degrade_probation_secs")]
    pub degrade_probation_secs: u64,
    /// 连续劣化指数退避后的最长降级时长。
    #[serde(default = "default_account_max_probation_secs")]
    pub max_probation_secs: u64,
    /// 降级期保留的探测流量比例（百分比）。
    ///
    /// 这是"不是真冷却"的落地：被降级的账号仍保留少量真实流量，
    /// 否则它永远不会产生新样本来证明自己已经恢复。
    #[serde(default = "default_account_probe_share_percent")]
    pub probe_share_percent: u32,
    /// 恢复爬坡时长；降级到期后流量线性回升，避免瞬间重新打满。
    #[serde(default = "default_account_recovery_ramp_secs")]
    pub recovery_ramp_secs: u64,
    #[serde(default)]
    pub auto_disable_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_disable_on_auth_error: bool,
    #[serde(default = "default_true")]
    pub auto_disable_on_security_lock: bool,
    #[serde(default)]
    pub auto_disable_on_quota_exhausted: bool,
    #[serde(default)]
    pub auto_disable_on_misconfigured_endpoint: bool,
    #[serde(default = "default_account_auto_disable_failure_threshold")]
    pub auto_disable_failure_threshold: u32,
    #[serde(default = "default_account_auto_disable_window_secs")]
    pub auto_disable_window_secs: u64,
    #[serde(default)]
    pub auto_disable_duration_secs: u64,
    /// 账号限流类错误的历史兼容冷却秒数。
    ///
    /// 当前普通上游错误默认只写短期健康降权，不写池级冷却；该值只在
    /// 严格临时不可调度或兼容诊断路径中作为时间提示。
    #[serde(default = "default_account_rate_limit_cooldown_secs")]
    pub rate_limit_cooldown_secs: u64,
    /// 账号服务器错误的历史兼容冷却秒数。
    #[serde(default = "default_account_server_error_cooldown_secs")]
    pub server_error_cooldown_secs: u64,
    /// 账号网络错误的历史兼容冷却秒数。
    #[serde(default = "default_account_network_error_cooldown_secs")]
    pub network_error_cooldown_secs: u64,
    /// 账号协议错误的历史兼容冷却秒数。
    #[serde(default = "default_account_protocol_error_cooldown_secs")]
    pub protocol_error_cooldown_secs: u64,
    #[serde(default)]
    pub model_unavailable_cooldown_mode: ModelUnavailableCooldownMode,
    #[serde(default = "default_account_model_unavailable_cooldown_secs")]
    pub model_unavailable_cooldown_secs: u64,
    #[serde(default = "default_account_request_timeout_secs")]
    pub request_timeout_secs: u64,
    #[serde(default)]
    pub stream_request_timeout_secs: u64,
    #[serde(default = "default_account_stream_idle_timeout_secs")]
    pub stream_idle_timeout_secs: u64,
    /// 账号流式响应首个语义输出（文本、tool_use、结束）的最长等待秒数，从发出请求算起。
    ///
    /// 超时且尚未向下游提交任何内容时，按首输出前失败处理并换池或回落本地；0 表示不限制。
    /// 现网 2026-10-21：同一账号成功请求的总耗时 p95 为 107 秒，而失败请求在无输出状态下
    /// 中位等待 144–261 秒，客户端先放弃。默认 120 秒，覆盖正常长首字，同时远早于上述失败形态。
    #[serde(default = "default_account_stream_first_output_timeout_secs")]
    pub stream_first_output_timeout_secs: u64,
    /// 账号流式响应在有效语义输出前出现 error/断流/idle 时是否允许换池恢复。
    ///
    /// 该开关只覆盖“尚未向下游提交有效内容”的安全窗口。单个账号可以通过
    /// `preOutputStreamRetryMode` 覆盖为启用或禁用；默认继承全局值。
    #[serde(default = "default_account_stream_pre_output_retry_enabled")]
    pub stream_pre_output_retry_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_disable_on_channel_disabled: bool,
    #[serde(default = "default_account_usage_projection_uplift_percent")]
    pub usage_projection_uplift_percent: u32,
    #[serde(default = "default_true")]
    pub usage_projection_cost_floor_enabled: bool,
    #[serde(default = "default_account_usage_projection_cost_floor_margin_percent")]
    pub usage_projection_cost_floor_margin_percent: u32,
    #[serde(default)]
    pub usage_projection_output_uplift_min_tokens: i32,
    #[serde(default)]
    pub usage_projection_output_uplift_percent: u32,
    #[serde(default)]
    pub stream_response_mode: StreamResponseMode,
    /// 账号 usage 诊断文件记录。默认关闭，只用于生产临时排查上游原始
    /// usage/响应形状与本系统解析结果不一致的问题。
    #[serde(default)]
    pub usage_debug_enabled: bool,
    #[serde(default = "default_account_usage_debug_dir")]
    pub usage_debug_dir: String,
    #[serde(default = "default_account_usage_debug_max_body_bytes")]
    pub usage_debug_max_body_bytes: u32,
    #[serde(default = "default_account_usage_debug_max_files")]
    pub usage_debug_max_files: u32,
    /// 账号 wire/交互诊断文件记录。默认关闭，用于临时排查 Claude Code CLI
    /// 空回、流式终止、追问未进入上游请求、以及 downstream SSE 形状问题。
    #[serde(default)]
    pub wire_debug_enabled: bool,
    #[serde(default = "default_account_wire_debug_dir")]
    pub wire_debug_dir: String,
    #[serde(default = "default_account_wire_debug_max_body_bytes")]
    pub wire_debug_max_body_bytes: u32,
    #[serde(default = "default_account_wire_debug_max_files")]
    pub wire_debug_max_files: u32,
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            routes: RoutesConfig::default(),
            transport_proxy_url: None,
            transport_proxy_username: None,
            transport_proxy_password: None,
            enabled: true,
            global_max_concurrent_requests: default_account_global_max_concurrent_requests(),
            max_queued_requests: default_account_max_queued_requests(),
            capacity_mode: CapacityMode::default(),
            dispatch_max_wait_secs: default_account_dispatch_max_wait_secs(),
            retry_max_attempts: default_account_retry_max_attempts(),
            retry_status_codes: default_account_retry_status_codes(),
            retry_on_network_error: true,
            retry_on_protocol_error: true,
            same_pool_retry_count: default_account_same_pool_retry_count(),
            same_pool_retry_status_codes: default_account_same_pool_retry_status_codes(),
            same_pool_retry_delay_ms: default_account_same_pool_retry_delay_ms(),
            transient_failure_priority_penalty: default_account_transient_failure_priority_penalty(
            ),
            quality_aware_scheduling_enabled: default_account_quality_aware_scheduling_enabled(),
            quality_ewma_alpha: default_account_quality_ewma_alpha(),
            quality_sample_ttl_secs: default_account_quality_sample_ttl_secs(),
            quality_min_samples: default_account_quality_min_samples(),
            quality_priority_weight: default_account_quality_priority_weight(),
            quality_load_weight: default_account_quality_load_weight(),
            quality_error_weight: default_account_quality_error_weight(),
            quality_latency_weight: default_account_quality_latency_weight(),
            quality_probation_weight: default_account_quality_probation_weight(),
            quality_top_k: default_account_quality_top_k(),
            degrade_window_secs: default_account_degrade_window_secs(),
            degrade_error_rate_threshold: default_account_degrade_error_rate_threshold(),
            degrade_probation_secs: default_account_degrade_probation_secs(),
            max_probation_secs: default_account_max_probation_secs(),
            probe_share_percent: default_account_probe_share_percent(),
            recovery_ramp_secs: default_account_recovery_ramp_secs(),
            auto_disable_enabled: false,
            auto_disable_on_auth_error: true,
            auto_disable_on_security_lock: true,
            auto_disable_on_quota_exhausted: false,
            auto_disable_on_misconfigured_endpoint: false,
            auto_disable_failure_threshold: default_account_auto_disable_failure_threshold(),
            auto_disable_window_secs: default_account_auto_disable_window_secs(),
            auto_disable_duration_secs: 0,
            rate_limit_cooldown_secs: default_account_rate_limit_cooldown_secs(),
            server_error_cooldown_secs: default_account_server_error_cooldown_secs(),
            network_error_cooldown_secs: default_account_network_error_cooldown_secs(),
            protocol_error_cooldown_secs: default_account_protocol_error_cooldown_secs(),
            model_unavailable_cooldown_mode: ModelUnavailableCooldownMode::default(),
            model_unavailable_cooldown_secs: default_account_model_unavailable_cooldown_secs(),
            request_timeout_secs: default_account_request_timeout_secs(),
            stream_request_timeout_secs: 0,
            stream_idle_timeout_secs: default_account_stream_idle_timeout_secs(),
            stream_first_output_timeout_secs: default_account_stream_first_output_timeout_secs(),
            stream_pre_output_retry_enabled: default_account_stream_pre_output_retry_enabled(),
            auto_disable_on_channel_disabled: true,
            usage_projection_uplift_percent: default_account_usage_projection_uplift_percent(),
            usage_projection_cost_floor_enabled: true,
            usage_projection_cost_floor_margin_percent:
                default_account_usage_projection_cost_floor_margin_percent(),
            usage_projection_output_uplift_min_tokens: 0,
            usage_projection_output_uplift_percent: 0,
            stream_response_mode: StreamResponseMode::default(),
            usage_debug_enabled: false,
            usage_debug_dir: default_account_usage_debug_dir(),
            usage_debug_max_body_bytes: default_account_usage_debug_max_body_bytes(),
            usage_debug_max_files: default_account_usage_debug_max_files(),
            wire_debug_enabled: false,
            wire_debug_dir: default_account_wire_debug_dir(),
            wire_debug_max_body_bytes: default_account_wire_debug_max_body_bytes(),
            wire_debug_max_files: default_account_wire_debug_max_files(),
        }
    }
}

impl AccountsConfig {
    /// Return a finite external capacity wait even when a legacy or API client
    /// submits zero. Keeping this bound at runtime prevents a stale config from
    /// turning a local scheduler outage into an indefinitely queued request.
    pub fn effective_dispatch_max_wait_secs(&self) -> u64 {
        if self.dispatch_max_wait_secs == 0 {
            default_account_dispatch_max_wait_secs()
        } else {
            self.dispatch_max_wait_secs
        }
    }

    pub fn same_pool_retry_status_codes(&self) -> BTreeSet<u16> {
        self.same_pool_retry_status_codes
            .iter()
            .copied()
            .filter(|code| (100..=599).contains(code))
            .collect()
    }

    pub fn retry_status_codes(&self) -> BTreeSet<u16> {
        self.retry_status_codes
            .iter()
            .copied()
            .filter(|code| (100..=599).contains(code))
            .collect()
    }
}

pub(crate) fn route_policy_allows(mode: RouteRuleMode, rules: &[String], endpoint: &str) -> bool {
    match mode {
        RouteRuleMode::AllowAll => true,
        RouteRuleMode::AllowList => rules.iter().any(|rule| route_rule_matches(rule, endpoint)),
        RouteRuleMode::DenyList => !rules.iter().any(|rule| route_rule_matches(rule, endpoint)),
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

/// Optional per-client admission override for a managed request API Key.
///
/// The secret is intentionally kept in the same runtime configuration surface
/// as the legacy `apiKey`/`apiKeys` fields. Existing deployments therefore do
/// not need a migration step: a policy is only applied when its key is present
/// in this list, while all legacy keys continue to use `requestAdmission`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RequestApiKeyPolicy {
    #[serde(default)]
    pub allowed_routes: Vec<String>,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub request_admission: Option<RequestAdmissionConfig>,
    /// Usage caps (billed amount, credits, tokens) per period; empty means unlimited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_limits: Vec<crate::anthropic::request_key_limits::KeyUsageLimit>,
}

impl RequestApiKeyPolicy {
    pub fn normalized(mut self) -> Option<Self> {
        self.api_key = self.api_key.trim().to_string();
        if self.api_key.is_empty() {
            return None;
        }
        self.name = self.name.trim().chars().take(80).collect();
        self.request_admission = self
            .request_admission
            .map(RequestAdmissionConfig::normalized);
        Some(self)
    }
}

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
    #[serde(skip)]
    pub compiled_routes: Option<std::sync::Arc<CompiledRoutes>>,
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

    #[serde(default)]
    pub api_key: Option<String>,

    /// 额外的客户端调用 API Key。
    ///
    /// `apiKey` 保留为历史主 Key；运行时实际允许的调用 Key 为
    /// `apiKey + apiKeys` 规范化后的集合。
    #[serde(default)]
    pub api_keys: Vec<String>,

    #[serde(default = "default_tls_backend")]
    pub tls_backend: TlsBackend,

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

    /// Managed request API Keys. This is additive to the legacy `apiKey` and
    /// `apiKeys` fields; legacy keys remain valid and inherit the global
    /// `requestAdmission` policy unless an override is explicitly present.
    #[serde(default)]
    pub request_api_key_policies: Vec<RequestApiKeyPolicy>,

    /// 首个上游内容到达前，最多静默保留多少秒（0–60，默认 15）。
    ///
    /// 窗口内不向客户端写任何字节，以便上游失败时无感换号重试；超过窗口仍无输出时，
    /// 先发出 message_start 并开始保活，此后不再做首输出前换号。0 表示一直保留到首个输出。
    #[serde(default = "default_stream_pre_output_hold_secs")]
    pub stream_pre_output_hold_secs: u64,

    /// 单个下游 Messages 请求允许发出的推理上游 HTTP 请求硬上限。
    ///
    /// 首输出前流重试、payload 重试和账号 failover 共享这一预算。
    /// 该值与账号数量无关。
    #[serde(default = "default_inference_upstream_max_attempts")]
    pub inference_upstream_max_attempts: u32,

    /// 代理写进上游请求的提示词：每一项的全局默认、可编辑文本和按路由的显式设置。
    #[serde(default)]
    pub prompt_injection: PromptInjectionConfig,

    /// 按路由控制的功能（`-thinking` 模型名、thinking 触发、历史清洗）。
    #[serde(default)]
    pub route_features: RouteFeaturesConfig,

    /// Anthropic 兼容 profile（默认 claude-code）。
    #[serde(default = "default_compat_profile")]
    pub compat_profile: CompatProfile,

    /// 请求模型解析策略（默认 compatible）。
    ///
    /// 控制 `sonnet`、`opus`、`default` 等短模型名，以及同 family 自动归一化
    /// 是否允许在发送上游前映射为已配置的可用模型。
    #[serde(default = "default_model_resolution_mode")]
    pub model_resolution_mode: ModelResolutionMode,

    /// 模型映射和兜底规则配置。
    #[serde(default)]
    pub model_mapping: ModelMappingConfig,

    /// 本地 prompt-cache 模拟的目标 cache read 中心比例。
    ///
    /// 对 high-cache 生效；读取仍必须命中同一凭据、
    /// 会话、模型下已创建过的缓存前缀，不会凭空制造 cache read。实际比例会
    /// 围绕该值做小范围确定性浮动，避免每次都精确落在同一个百分比。
    #[serde(default = "default_prompt_cache_target_read_ratio")]
    pub prompt_cache_target_read_ratio: f64,

    /// high-cache 模拟专用的 total input 放大倍数。
    ///
    /// 只影响本地 high-cache usage 模拟，不影响真实上游缓存。
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

    /// 本地 prompt-cache 每个会话 scope（conversation_id + 路由命名空间）最多保留的 fingerprint
    /// 条目。scope 不含账号，同一会话换号后仍命中同一组条目；键名里的 PerAccount 只为兼容保留。
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
    pub routes: RoutesConfig,

    /// Route scenario most recently applied in full from the admin route scenario page; only
    /// used to show which scenario is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_route_scenario_id: Option<u64>,

    /// 请求级 usage record 内存保留上限。
    #[serde(default = "default_usage_record_limit")]
    pub usage_record_limit: usize,

    /// Admin 高缓存请求阈值。
    #[serde(default = "default_high_cache_threshold")]
    pub high_cache_threshold: i32,

    /// 是否自动清理过期审计日志（含账号异动）。默认关闭：升级后不删除任何已有记录，
    /// 由管理员在审计页确认后手动开启。
    #[serde(default)]
    pub audit_log_retention_enabled: bool,

    /// 审计日志保留天数；开启自动清理后，后台按批删除更早的记录。
    #[serde(default = "default_audit_log_retention_days")]
    pub audit_log_retention_days: u32,

    /// 外部备用号池和直连/预检 fallback 策略。
    #[serde(default)]
    pub accounts: AccountsConfig,

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

const CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION: u32 = 2;

impl Config {}

fn default_tls_backend() -> TlsBackend {
    TlsBackend::Rustls
}

pub const STREAM_PRE_OUTPUT_HOLD_MAX_SECS: u64 = 60;

fn default_stream_pre_output_hold_secs() -> u64 {
    15
}

/// 狂暴模式轮数上限。
///
/// 总尝试量是 `账号数 × region数 × 轮数`，放开轮数会让单个下游请求打出成百上千次
/// 上游调用。管理端校验与运行时 clamp 共用此常量，避免两处阈值漂移。

/// 狂暴模式跨轮间隔上限 60s：sleep 期间并发 lease 仍被占用，不能无限拉长。

pub(crate) fn default_inference_upstream_max_attempts() -> u32 {
    4
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

fn default_compat_profile() -> CompatProfile {
    CompatProfile::ClaudeCode
}

fn default_model_resolution_mode() -> ModelResolutionMode {
    ModelResolutionMode::Compatible
}

fn default_true() -> bool {
    true
}

fn default_prompt_cache_target_read_ratio() -> f64 {
    0.98
}

fn default_cache_hit_target_ratio() -> f64 {
    0.9
}

fn default_cache_hit_ratio_jitter() -> f64 {
    0.04
}

fn default_cache_hit_min_tokens() -> i32 {
    1024
}

fn default_cache_hit_min_cost_ratio() -> f64 {
    1.0
}

fn default_prefix_tool_coverage_ratio() -> f64 {
    1.0
}

fn default_prefix_tool_reported_input_min_tokens() -> i32 {
    32
}

fn default_prefix_tool_reported_input_max_tokens() -> i32 {
    4_096
}

fn default_stable_segment_coverage_ratio() -> f64 {
    0.90
}

fn default_stable_segment_max_coverage_tokens() -> i32 {
    120_000
}

fn default_stable_segment_min_total_input_tokens() -> i32 {
    4_096
}

fn default_stable_segment_min_segment_tokens() -> i32 {
    1_024
}

fn default_stable_segment_max_new_creation_tokens_per_request() -> i32 {
    30_000
}

fn default_stable_segment_history_message_limit() -> usize {
    40
}

fn default_stable_segment_default_ttl_secs() -> u64 {
    300
}

fn default_stable_segment_extended_ttl_secs() -> u64 {
    3_600
}

fn default_stable_segment_reported_input_min_tokens() -> i32 {
    32
}

fn default_stable_segment_reported_input_max_tokens() -> i32 {
    256
}

fn default_stable_segment_token_scale() -> f64 {
    1.0
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

fn default_high_cache_threshold() -> i32 {
    10_000
}

pub const MIN_AUDIT_LOG_RETENTION_DAYS: u32 = 1;
pub const MAX_AUDIT_LOG_RETENTION_DAYS: u32 = 3650;

fn default_audit_log_retention_days() -> u32 {
    15
}

fn default_account_auto_disable_failure_threshold() -> u32 {
    1
}

fn default_account_auto_disable_window_secs() -> u64 {
    60
}

fn default_account_global_max_concurrent_requests() -> u32 {
    1024
}

fn default_account_max_queued_requests() -> u32 {
    256
}

fn default_account_dispatch_max_wait_secs() -> u64 {
    60
}

fn default_account_retry_max_attempts() -> u32 {
    3
}

fn default_account_retry_status_codes() -> Vec<u16> {
    // 500 is treated as the default 5xx family retry guard by retry_pipeline.
    // 404 is included because some upstreams surface model-unavailable and
    // invalid-model failures as exact Not Found responses.
    vec![404, 408, 425, 429, 500, 502, 503, 504, 529]
}

fn default_account_same_pool_retry_count() -> u32 {
    1
}

fn default_account_same_pool_retry_status_codes() -> Vec<u16> {
    // 500 is treated as the default 5xx family retry guard by retry_pipeline.
    vec![408, 425, 429, 500, 502, 503, 504, 529]
}

fn default_account_same_pool_retry_delay_ms() -> u64 {
    500
}

fn default_account_quality_aware_scheduling_enabled() -> bool {
    // This feature changes scheduling decisions and performs background Redis
    // sampling. Keep upgrades backwards-compatible unless an operator opts in.
    false
}

fn default_account_quality_ewma_alpha() -> f64 {
    // 有效窗口约为最近 10 个请求：足够快地跟上真实劣化，
    // 又不会被单次抖动带偏。
    0.2
}

fn default_account_quality_sample_ttl_secs() -> u64 {
    600
}

fn default_account_quality_min_samples() -> u64 {
    5
}

fn default_account_quality_priority_weight() -> f64 {
    1.0
}

fn default_account_quality_load_weight() -> f64 {
    100.0
}

fn default_account_quality_error_weight() -> f64 {
    // 失败率 0→30% 贡献 30 分，约等于 30 级优先级差或 3 倍中位数首字延迟，
    // 保证"报错多"的判断优先级天然高于"首字慢"。
    100.0
}

fn default_account_quality_latency_weight() -> f64 {
    // 相对值制：候选集中位数为 1.0。慢一倍（2.0）贡献 10 分。
    10.0
}

fn default_account_quality_probation_weight() -> f64 {
    50.0
}

fn default_account_quality_top_k() -> u32 {
    3
}

fn default_account_degrade_window_secs() -> u64 {
    120
}

fn default_account_degrade_error_rate_threshold() -> f64 {
    0.5
}

fn default_account_degrade_probation_secs() -> u64 {
    180
}

fn default_account_max_probation_secs() -> u64 {
    900
}

fn default_account_probe_share_percent() -> u32 {
    5
}

fn default_account_recovery_ramp_secs() -> u64 {
    60
}

fn default_account_transient_failure_priority_penalty() -> u32 {
    20
}

fn default_account_rate_limit_cooldown_secs() -> u64 {
    30
}

fn default_account_server_error_cooldown_secs() -> u64 {
    10
}

fn default_account_network_error_cooldown_secs() -> u64 {
    10
}

fn default_account_protocol_error_cooldown_secs() -> u64 {
    10
}

fn default_account_model_unavailable_cooldown_secs() -> u64 {
    10
}

fn default_account_request_timeout_secs() -> u64 {
    180
}

fn default_account_stream_idle_timeout_secs() -> u64 {
    180
}

fn default_account_stream_first_output_timeout_secs() -> u64 {
    45
}

fn default_account_stream_pre_output_retry_enabled() -> bool {
    true
}

fn default_account_usage_projection_uplift_percent() -> u32 {
    25
}

fn default_account_usage_projection_cost_floor_margin_percent() -> u32 {
    10
}

fn default_account_usage_debug_dir() -> String {
    "/tmp/claude-pool/account-usage-debug".to_string()
}

fn default_account_usage_debug_max_body_bytes() -> u32 {
    8 * 1024
}

fn default_account_usage_debug_max_files() -> u32 {
    1_000
}

fn default_account_wire_debug_dir() -> String {
    "/tmp/claude-pool/account-wire-debug".to_string()
}

fn default_account_wire_debug_max_body_bytes() -> u32 {
    8 * 1024
}

fn default_account_wire_debug_max_files() -> u32 {
    1_000
}

fn default_postgres_max_connections() -> u32 {
    16
}

fn default_postgres_usage_max_connections() -> u32 {
    6
}

fn default_redis_key_prefix() -> String {
    "claude_pool:local".to_string()
}

fn default_observability_redis_key_prefix() -> String {
    "claude_pool:observability".to_string()
}

fn default_reported_usage_normal_max_multiplier() -> f64 {
    1.1
}

pub(crate) fn default_final_cache_read_max_tokens() -> i32 {
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

pub(crate) fn normalize_reported_usage_path_prefix(prefix: &str) -> Option<String> {
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

pub(crate) fn reported_usage_path_matches(prefix: &str, path: &str) -> bool {
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

pub fn normalize_legacy_defined_cache_routes(routes: &[String]) -> Vec<String> {
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
            compiled_routes: None,
            runtime_config_migration_version: CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION,
            postgres: PostgresConfig::default(),
            redis: RedisConfig::default(),
            observability_redis: default_observability_redis_config(),
            host: default_host(),
            port: default_port(),
            api_key: None,
            api_keys: Vec::new(),
            tls_backend: default_tls_backend(),
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            admin_api_key: None,
            request_admission: RequestAdmissionConfig::default(),
            request_api_key_policies: Vec::new(),
            stream_pre_output_hold_secs: default_stream_pre_output_hold_secs(),
            inference_upstream_max_attempts: default_inference_upstream_max_attempts(),
            prompt_injection: PromptInjectionConfig::default(),
            route_features: RouteFeaturesConfig::default(),
            compat_profile: default_compat_profile(),
            model_resolution_mode: default_model_resolution_mode(),
            model_mapping: ModelMappingConfig::default(),
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
            routes: RoutesConfig::default(),
            active_route_scenario_id: None,
            usage_record_limit: default_usage_record_limit(),
            high_cache_threshold: default_high_cache_threshold(),
            audit_log_retention_enabled: false,
            audit_log_retention_days: default_audit_log_retention_days(),
            accounts: AccountsConfig::default(),
            config_path: None,
        }
    }
}

impl Config {
    /// 获取默认配置文件路径
    pub fn default_config_path() -> &'static str {
        "config.json"
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

            bounds: CacheBoundsPolicy {
                max_entries_per_account: self.prompt_cache_max_entries_per_account,
                max_entries_global: self.prompt_cache_max_entries_global,
                entry_ttl_secs: self.prompt_cache_entry_ttl_secs,
                estimated_bytes_limit: self.prompt_cache_estimated_bytes_limit,
            },
            prefix_tool: PrefixToolCachePolicy::default(),
            stable_segment: StableSegmentCachePolicy::default(),
            hit_shaping: CacheHitShapingPolicy::default(),
            cold_request_guard: ColdRequestGuardPolicy::default(),
            prefix_tool_plus: PrefixToolPlusCachePolicy::default(),
            cache_ttl: CacheTtlPolicy::default(),
        }
        .normalized()
    }

    pub fn cache_policy_for_path(&self, path: &str) -> ResolvedCacheRoutePolicy {
        self.routes
            .compile_with_config(self)
            .match_endpoint(path)
            .expect("builtin root route")
            .cache_policy
            .clone()
    }

    /// 返回规范化后的客户端调用 API Key 列表。
    ///
    /// 历史配置只包含 `apiKey`；新配置可以额外包含 `apiKeys`。这里统一去空、trim、去重，
    /// 并保留原始顺序，保证旧主 key 仍排在第一位。
    pub fn request_api_keys(&self) -> Vec<String> {
        let mut keys = normalize_request_api_keys(
            self.api_key
                .iter()
                .chain(self.api_keys.iter())
                .map(String::as_str),
        );
        for policy in &self.request_api_key_policies {
            keys.extend(normalize_request_api_keys([policy.api_key.as_str()]));
        }
        normalize_request_api_keys(keys)
    }

    /// Returns normalized managed policies with duplicate secrets collapsed.
    ///
    /// The first entry wins so an operator cannot accidentally change a
    /// client's limits by adding a duplicate later in the file.
    pub fn request_api_key_policies(&self) -> Vec<RequestApiKeyPolicy> {
        let mut seen = BTreeSet::new();
        self.request_api_key_policies
            .iter()
            .filter_map(|policy| policy.clone().normalized())
            .filter(|policy| seen.insert(policy.api_key.clone()))
            .collect()
    }

    pub fn set_request_api_key_policies(
        &mut self,
        policies: impl IntoIterator<Item = RequestApiKeyPolicy>,
    ) {
        let mut seen = BTreeSet::new();
        self.request_api_key_policies = policies
            .into_iter()
            .filter_map(RequestApiKeyPolicy::normalized)
            .filter(|policy| seen.insert(policy.api_key.clone()))
            .collect();
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
        let missing_request_keys = self.request_api_keys().is_empty();
        if missing_request_keys {
            let file_keys = file_config.request_api_keys();
            if !file_keys.is_empty() {
                self.set_request_api_keys(file_keys);
                changed = true;
            }
        }
        if missing_request_keys
            && self.request_api_key_policies.is_empty()
            && !file_config.request_api_key_policies.is_empty()
        {
            self.set_request_api_key_policies(file_config.request_api_key_policies.clone());
            changed = true;
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
        let changed =
            self.runtime_config_migration_version != CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION;
        self.runtime_config_migration_version = CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION;
        changed
    }

    pub fn from_json_value(mut value: serde_json::Value) -> anyhow::Result<Self> {
        super::routes::migrate_path_overrides_to_routes(&mut value);
        Ok(serde_json::from_value(value)?)
    }
    pub fn from_json_str(json: &str) -> anyhow::Result<Self> {
        Self::from_json_value(serde_json::from_str(json)?)
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
        let mut value: serde_json::Value = serde_json::from_str(&content)?;
        super::routes::migrate_path_overrides_to_routes(&mut value);
        let mut config: Config = serde_json::from_value(value)?;
        config
            .routes
            .validate_with_config(&config)
            .map_err(anyhow::Error::msg)?;
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
        if let Ok(url) = std::env::var("CLAUDE_POOL_POSTGRES_URL") {
            if !url.trim().is_empty() {
                self.postgres.url = Some(url);
            }
        }
        if let Some(parsed) = std::env::var("CLAUDE_POOL_POSTGRES_MAX_CONNECTIONS")
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            self.postgres.max_connections = parsed.max(1);
        }
        if let Some(parsed) = std::env::var("CLAUDE_POOL_POSTGRES_USAGE_MAX_CONNECTIONS")
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            self.postgres.usage_max_connections = parsed.max(1);
        }
        if let Ok(value) = std::env::var("CLAUDE_POOL_POSTGRES_MIGRATE_ON_START") {
            if let Some(parsed) = parse_env_bool(&value) {
                self.postgres.migrate_on_start = parsed;
            }
        }
        if let Ok(value) = std::env::var("CLAUDE_POOL_POSTGRES_COMPRESS_USAGE_ROLLUPS_ON_START") {
            if let Some(parsed) = parse_env_bool(&value) {
                self.postgres.compress_usage_rollups_on_start = parsed;
            }
        }
        if let Ok(url) = std::env::var("CLAUDE_POOL_REDIS_URL") {
            if !url.trim().is_empty() {
                self.redis.url = Some(url);
            }
        }
        if let Ok(url) = std::env::var("CLAUDE_POOL_OBSERVABILITY_REDIS_URL") {
            if !url.trim().is_empty() {
                self.observability_redis.url = Some(url);
            }
        }
        if let Ok(prefix) = std::env::var("CLAUDE_POOL_OBSERVABILITY_REDIS_KEY_PREFIX") {
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
    #[derive(Default)]
    struct TestEndpointRule {
        mode: super::RouteRuleMode,
        rules: Vec<String>,
    }
    impl TestEndpointRule {
        fn allows(&self, endpoint: &str) -> bool {
            super::route_policy_allows(self.mode, &self.rules, endpoint)
        }
    }

    use super::*;

    #[test]
    fn observability_redis_is_optional_and_uses_its_own_prefix() {
        let config: Config = Config::from_json_str("{}").unwrap();
        assert!(config.observability_redis.url.is_none());
        assert_eq!(
            config.observability_redis.key_prefix,
            "claude_pool:observability"
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
    fn cache_hit_shaping_round_trips_and_resolves_per_strategy() {
        let json = r#"{
            "pathOverrides": {
                "/dfcache/minea": {
                    "cacheType": "current_high_cache",
                    "hitShaping": { "targetHitRatio": 0.85, "minCacheCreationTokens": 2048 }
                },
                "/dfcache/pool": { "cacheType": "prefix_tool" },
                "/na": { "cacheType": "no_cache" }
            }
        }"#;
        let config = Config::from_json_value(serde_json::json!({"cachePolicy": serde_json::from_str::<serde_json::Value>(json).unwrap()})).unwrap();
        let cache_policy = config.cache_policy.clone();
        let round_trip: CachePolicyConfig =
            serde_json::from_str(&serde_json::to_string(&cache_policy).unwrap()).unwrap();
        assert_eq!(round_trip, cache_policy);

        let minea = config
            .cache_policy_for_path("/dfcache/minea/v1/messages")
            .policy
            .hit_shaping;
        // 没有显式开启时保持关闭。
        assert!(!minea.enabled);
        assert_eq!(minea.target_hit_ratio, 0.85);
        assert_eq!(minea.min_cache_creation_tokens, 2048);
        assert_eq!(minea.min_cache_read_tokens, 1024);

        // 默认关闭，没有单独配置的策略继承默认值。
        assert!(!CacheHitShapingPolicy::default().enabled);
        let pool = config
            .cache_policy_for_path("/dfcache/pool/v1/messages")
            .policy
            .hit_shaping;
        assert_eq!(pool, CacheHitShapingPolicy::default());
        let v1 = config
            .cache_policy_for_path("/v1/messages")
            .policy
            .hit_shaping;
        assert_eq!(v1, CacheHitShapingPolicy::default());

        // no_cache 不整形。
        let na = config
            .cache_policy_for_path("/na/v1/messages")
            .policy
            .hit_shaping;
        assert!(!na.enabled);
    }

    #[test]
    fn cold_request_guard_defaults_off_and_resolves_per_route() {
        let mut config = Config::default();
        assert!(
            !config
                .cache_policy_for_path("/dfcache/minea/v1/messages")
                .policy
                .cold_request_guard
                .enabled
        );
        let patch: CacheRoutePolicyPatch = serde_json::from_value(serde_json::json!({
            "cacheType": "current_high_cache",
            "coldRequestGuard": {"enabled": true}
        }))
        .expect("patch json");
        assert_eq!(
            patch.cold_request_guard,
            Some(ColdRequestGuardPolicyPatch {
                enabled: Some(true)
            })
        );
        assert!(!patch.is_empty());
        config
            .routes
            .route_mut(&"/dfcache/minea".to_string())
            .cache_policy = patch.clone();
        assert!(
            config
                .cache_policy_for_path("/dfcache/minea/v1/messages")
                .policy
                .cold_request_guard
                .enabled
        );
        // 其他路由不受影响；no_cache 路由始终关闭。
        assert!(
            !config
                .cache_policy_for_path("/dfcache/mineb/v1/messages")
                .policy
                .cold_request_guard
                .enabled
        );
        config.routes.route_mut(&"/na".to_string()).cache_policy = CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::NoCache),
            ..patch
        };
        assert!(
            !config
                .cache_policy_for_path("/na/v1/messages")
                .policy
                .cold_request_guard
                .enabled
        );
        // 序列化往返：未配置时不输出多余字段以外的变化。
        let empty = CacheRoutePolicyPatch::default();
        assert!(empty.is_empty());
        let round_trip: CacheRoutePolicyPatch = serde_json::from_value(
            serde_json::to_value(&config.routes.route_mut("/dfcache/minea").cache_policy).unwrap(),
        )
        .unwrap();
        assert_eq!(
            round_trip.cold_request_guard,
            Some(ColdRequestGuardPolicyPatch {
                enabled: Some(true)
            })
        );
    }

    #[test]
    fn cache_hit_shaping_rejects_out_of_range_values() {
        let patch = CacheRoutePolicyPatch {
            hit_shaping: Some(CacheHitShapingPolicyPatch {
                target_hit_ratio: Some(1.2),
                ..CacheHitShapingPolicyPatch::default()
            }),
            ..CacheRoutePolicyPatch::default()
        };
        let base = Config::default().legacy_cache_route_policy_default();
        assert!(patch.validate("test", base.clone()).is_err());
        let patch = CacheRoutePolicyPatch {
            hit_shaping: Some(CacheHitShapingPolicyPatch {
                min_cache_read_tokens: Some(-1),
                ..CacheHitShapingPolicyPatch::default()
            }),
            ..CacheRoutePolicyPatch::default()
        };
        assert!(patch.validate("test", base).is_err());
    }

    #[test]
    fn default_prompt_cache_target_read_ratio_is_98_percent() {
        assert_eq!(Config::default().prompt_cache_target_read_ratio, 0.98);
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
    fn request_api_keys_support_legacy_single_key() {
        let config: Config = Config::from_json_str(r#"{"apiKey":" sk-legacy "}"#).unwrap();

        assert_eq!(config.request_api_keys(), vec!["sk-legacy".to_string()]);
    }

    #[test]
    fn managed_request_key_policies_are_additive_and_keep_legacy_keys() {
        let config: Config = Config::from_json_str(
            r#"{
                "apiKey": "sk-legacy-primary",
                "apiKeys": ["sk-legacy-extra"],
                "requestAdmission": {
                    "rpm": 300,
                    "maxConcurrentRequests": 32,
                    "maxQueuedRequests": 64,
                    "queueTimeoutMs": 1000
                },
                "requestApiKeyPolicies": [{
                    "apiKey": "sk-managed",
                    "name": "client-a",
                    "enabled": true,
                    "requestAdmission": {
                        "rpm": 120,
                        "maxConcurrentRequests": 5,
                        "maxQueuedRequests": 7,
                        "queueTimeoutMs": 2500
                    }
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(
            config.request_api_keys(),
            vec![
                "sk-legacy-primary".to_string(),
                "sk-legacy-extra".to_string(),
                "sk-managed".to_string(),
            ]
        );
        assert_eq!(config.request_admission, RequestAdmissionConfig::default());
        assert_eq!(config.request_api_key_policies().len(), 1);
        assert_eq!(
            config.request_api_key_policies()[0].request_admission,
            Some(RequestAdmissionConfig {
                rpm: 120,
                max_concurrent_requests: 5,
                max_queued_requests: 7,
                queue_timeout_ms: 2_500,
            })
        );

        let round_trip: Config =
            Config::from_json_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert_eq!(round_trip.request_api_keys(), config.request_api_keys());
        assert_eq!(
            round_trip.request_api_key_policies(),
            config.request_api_key_policies()
        );
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

        let legacy: Config = Config::from_json_str(r#"{"apiKey":"sk-legacy"}"#).unwrap();
        assert_eq!(legacy.request_admission, RequestAdmissionConfig::default());

        let legacy_null: Config =
            Config::from_json_str(r#"{"apiKey":"sk-legacy","requestAdmission":null}"#).unwrap();
        assert_eq!(
            legacy_null.request_admission,
            RequestAdmissionConfig::default()
        );

        let partial: Config = Config::from_json_str(
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

        let queue_only: Config = Config::from_json_str(
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

        let partial_queue_disabled: Config = Config::from_json_str(
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

        let disabled: Config = Config::from_json_str(
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
        let config = RequestAdmissionConfig {
            rpm: MAX_REQUEST_API_KEY_RPM + 1,
            ..Default::default()
        };
        assert!(config.validate().is_err());
        assert_eq!(config.normalized().rpm, MAX_REQUEST_API_KEY_RPM);

        let config = RequestAdmissionConfig {
            max_concurrent_requests: MAX_REQUEST_API_KEY_CONCURRENT_REQUESTS + 1,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        let config = RequestAdmissionConfig {
            max_queued_requests: MAX_REQUEST_API_KEY_QUEUED_REQUESTS + 1,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        let config = RequestAdmissionConfig {
            queue_timeout_ms: MAX_REQUEST_API_KEY_QUEUE_TIMEOUT_MS + 1,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn request_api_keys_merge_and_deduplicate_primary_and_extra_keys() {
        let config: Config = Config::from_json_str(
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
        file.set_request_api_key_policies([RequestApiKeyPolicy {
            allowed_routes: Vec::new(),
            usage_limits: Vec::new(),
            api_key: "sk-extra".to_string(),
            name: "file-managed".to_string(),
            enabled: true,
            request_admission: Some(RequestAdmissionConfig {
                max_concurrent_requests: 4,
                ..RequestAdmissionConfig::default()
            }),
        }]);
        file.admin_api_key = Some("sk-admin-file".to_string());

        assert!(database.fill_missing_access_keys_from(&file));
        assert_eq!(
            database.request_api_keys(),
            vec!["sk-file".to_string(), "sk-extra".to_string()]
        );
        assert_eq!(
            database.request_api_key_policies(),
            file.request_api_key_policies()
        );
        assert_eq!(database.admin_api_key.as_deref(), Some("sk-admin-file"));
    }

    #[test]
    fn fill_missing_access_keys_does_not_resurrect_removed_managed_key_from_file() {
        let mut database = Config::default();
        database.set_request_api_keys(["sk-legacy"]);
        let mut file = Config::default();
        file.set_request_api_keys(["sk-legacy", "sk-removed"]);
        file.set_request_api_key_policies([RequestApiKeyPolicy {
            allowed_routes: Vec::new(),
            usage_limits: Vec::new(),
            api_key: "sk-removed".to_string(),
            name: "stale-file-entry".to_string(),
            enabled: true,
            request_admission: Some(RequestAdmissionConfig::default()),
        }]);

        assert!(!database.fill_missing_access_keys_from(&file));
        assert_eq!(
            database.request_api_keys(),
            vec!["sk-legacy".to_string()],
            "database key removal must not be undone by stale file policies"
        );
        assert!(database.request_api_key_policies().is_empty());
    }

    #[test]
    fn account_capacity_wait_is_always_bounded() {
        let mut config = AccountsConfig::default();
        assert_eq!(config.effective_dispatch_max_wait_secs(), 60);

        config.dispatch_max_wait_secs = 0;
        assert_eq!(config.effective_dispatch_max_wait_secs(), 60);

        config.dispatch_max_wait_secs = 7;
        assert_eq!(config.effective_dispatch_max_wait_secs(), 7);
    }

    #[test]
    fn account_route_policy_denies_matching_routes_only() {
        let mut config = TestEndpointRule {
            mode: RouteRuleMode::DenyList,
            rules: vec!["/cc".to_string(), "/dfcache/team-a".to_string()],
            ..TestEndpointRule::default()
        };

        assert!(!config.allows("/cc/v1/messages"));
        assert!(!config.allows("/dfcache/team-a/v1/messages"));
        assert!(config.allows("/v1/messages"));

        config.rules = vec!["*".to_string()];
        assert!(!config.allows("/v1/messages"));
        assert!(!config.allows("/ha/v1/messages"));
    }

    #[test]
    fn account_route_policy_allow_list_requires_a_match() {
        let config = TestEndpointRule {
            mode: RouteRuleMode::AllowList,
            rules: vec!["/ha".to_string(), "/dfcache/team-b".to_string()],
            ..TestEndpointRule::default()
        };

        assert!(config.allows("/ha/v1/messages"));
        assert!(config.allows("/dfcache/team-b/v1/messages"));
        assert!(!config.allows("/cc/v1/messages"));
        assert!(!config.allows("/v1/messages"));
    }

    #[test]
    fn account_route_policy_matches_case_insensitively() {
        let config = TestEndpointRule {
            mode: RouteRuleMode::AllowList,
            rules: vec!["/CC/V1".to_string()],
            ..TestEndpointRule::default()
        };

        assert!(config.allows("/cc/v1/messages"));
        assert!(!config.allows("/ha/v1/messages"));
    }

    #[test]
    fn account_route_policy_does_not_match_mid_path_segments() {
        let config = TestEndpointRule {
            mode: RouteRuleMode::AllowList,
            rules: vec!["/v1".to_string()],
            ..TestEndpointRule::default()
        };

        assert!(config.allows("/v1/messages"));
        assert!(!config.allows("/cc/v1/messages"));
        assert!(!config.allows("/ha/v1/messages"));
    }

    #[test]
    fn account_stream_response_mode_accepts_legacy_capture_value() {
        let config: Config = Config::from_json_str(
            r#"{
                "apiKey": "sk-test",
                "accounts": {
                    "streamResponseMode": "event_passthrough_capture"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            config.accounts.stream_response_mode,
            StreamResponseMode::EventPassthrough
        );
    }

    #[test]
    fn reported_usage_deserializes_from_camel_case_config() {
        let config: Config = Config::from_json_str(
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
        let policy = config
            .routes
            .match_endpoint("/cc/v1/messages")
            .unwrap()
            .reported_usage
            .clone()
            .unwrap();
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
        let config: Config = Config::from_json_str(
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
    fn reported_usage_default_and_prefix_overrides_match_longest_prefix() {
        let mut config = Config::default();
        config.routes.route_mut("/cc/v1").reported_usage = Some(ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(32),
            ..Default::default()
        });
        assert_eq!(
            config
                .cache_policy_for_path("/v1/messages")
                .policy
                .reported_usage
                .input
                .mode,
            ReportedUsageFieldMode::Raw
        );
        assert_eq!(
            config
                .cache_policy_for_path("/v1/messages")
                .policy
                .reported_usage
                .final_cache_read_max_tokens,
            700_000
        );
        assert_eq!(
            config
                .cache_policy_for_path("/cc/v1/messages")
                .policy
                .reported_usage
                .input
                .max_tokens,
            32
        );
        assert_eq!(
            config
                .cache_policy_for_path("/cc/other")
                .policy
                .reported_usage
                .input
                .max_tokens,
            96
        );
        assert!(config.routes.route_mut("/na").reported_usage.is_none());
        assert!(config.routes.route_mut("/na").reported_usage.is_none());
    }

    #[test]
    fn legacy_defined_cache_routes_normalize_and_reject_unsafe_paths() {
        assert_eq!(
            normalize_defined_cache_route("dfcache/CC"),
            Some("/dfcache/cc".to_string())
        );
        assert_eq!(
            normalize_legacy_defined_cache_routes(&[
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
                ..CachePolicyConfig::default()
            },
            ..Config::default()
        };

        config.routes.route_mut("/ha").reported_usage = Some(ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(500),
            cache_creation: ReportedUsageFieldPolicy::sample_target(150_000),
            ..ReportedUsagePathPolicy::default()
        });
        config.routes.route_mut("/ha").cache_policy = CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
            ..CacheRoutePolicyPatch::default()
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

        config.routes.route_mut(&"/ha".to_string()).cache_policy = CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
            reported_usage: Some(ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(42),
                ..ReportedUsagePathPolicy::default()
            }),
            ..CacheRoutePolicyPatch::default()
        };

        let explicit_ha = config.cache_policy_for_path("/ha/v1/messages");
        assert_eq!(explicit_ha.policy.reported_usage.input.max_tokens, 42);
    }

    #[test]
    fn cache_policy_preserves_cache_type_only_override() {
        let mut config = Config::default();
        config.routes.route_mut(&"/tool".to_string()).cache_policy = CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::PrefixTool),
            ..CacheRoutePolicyPatch::default()
        };

        assert!(
            config
                .routes
                .routes
                .iter()
                .any(|route| route.prefix == "/tool")
        );

        let resolved = config.cache_policy_for_path("/tool/v1/messages");
        assert_eq!(resolved.namespace.as_deref(), Some("/tool"));
        assert_eq!(
            resolved.policy.cache_type,
            PromptCacheStrategyType::PrefixTool
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
    fn prefix_tool_cache_policy_defaults_match_current_behavior() {
        let policy = PrefixToolCachePolicy::default();

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
    fn stable_segment_cache_policy_defaults_match_design() {
        let policy = StableSegmentCachePolicy::default();

        assert_eq!(policy.coverage_ratio, 0.9);
        assert_eq!(policy.max_coverage_tokens, 120_000);
        assert_eq!(policy.min_total_input_tokens, 4_096);
        assert_eq!(policy.min_segment_tokens, 1_024);
        assert!(policy.incremental_create_enabled);
        assert_eq!(policy.max_new_creation_tokens_per_request, 30_000);
        assert!(policy.include_tools);
        assert!(policy.include_system);
        assert!(policy.include_history);
        assert_eq!(policy.history_message_limit, 40);
        assert!(policy.honor_explicit_cache_control);
        assert!(policy.auto_cache_system);
        assert!(policy.auto_cache_tools);
        assert!(policy.auto_cache_history_message_ends);
        assert!(!policy.cache_current_user_stable_prefix);
        assert_eq!(policy.current_user_stable_prefix_max_tokens, 0);
        assert_eq!(policy.default_ttl_secs, 300);
        assert_eq!(policy.extended_ttl_secs, 3_600);
        assert_eq!(policy.reported_input_min_tokens, 32);
        assert_eq!(policy.reported_input_max_tokens, 256);
    }

    #[test]
    fn stable_segment_cache_amplification_defaults_are_disabled() {
        let policy = StableSegmentCachePolicy::default();
        assert_eq!(policy.token_scale, 1.0);
        assert!(!policy.amplifies_cache());
        assert!(!policy.uplifts_output());
        assert_eq!(policy.final_cache_read_max_tokens, 0);
        assert_eq!(policy.final_cache_creation_max_tokens, 0);
        assert_eq!(policy.final_output_max_tokens, 0);
    }

    #[test]
    fn stable_segment_cache_amplification_fields_patch_and_normalize() {
        let patch: StableSegmentCachePolicyPatch = serde_json::from_value(serde_json::json!({
            "tokenScale": 2.5,
            "maxSimulatedInputTokens": 300000,
            "capJitterMinTokens": 20000,
            "capJitterMaxTokens": 10000,
            "scaleMinInputTokens": 8000,
            "outputUpliftPercent": 150,
            "outputUpliftMinTokens": 600,
            "finalCacheReadMaxTokens": 400000,
            "finalCacheReadJitterMinTokens": 1000,
            "finalCacheReadJitterMaxTokens": 5000,
            "finalCacheCreationMaxTokens": 60000,
            "finalOutputMaxTokens": 32000
        }))
        .expect("deserialize patch");
        assert!(
            patch.validate_raw("stableSegment").is_err(),
            "capJitter 最小值大于最大值应拒绝"
        );

        let policy = patch.apply_to(StableSegmentCachePolicy::default());
        assert_eq!(policy.token_scale, 2.5);
        assert!(policy.amplifies_cache());
        assert_eq!(policy.max_simulated_input_tokens, 300_000);
        assert_eq!(
            (policy.cap_jitter_min_tokens, policy.cap_jitter_max_tokens),
            (10_000, 20_000)
        );
        assert_eq!(policy.scale_min_input_tokens, 8_000);
        assert_eq!(policy.output_uplift_percent, 150);
        assert_eq!(policy.output_uplift_min_tokens, 600);
        assert_eq!(policy.final_cache_read_max_tokens, 400_000);
        assert_eq!(policy.final_cache_creation_max_tokens, 60_000);
        assert_eq!(policy.final_output_max_tokens, 32_000);
        assert!(policy.validate("stableSegment").is_ok());
    }

    #[test]
    fn stable_segment_cache_amplification_rejects_out_of_range_values() {
        for (field, value) in [
            ("tokenScale", serde_json::json!(3.5)),
            ("tokenScale", serde_json::json!(0.5)),
            ("outputUpliftPercent", serde_json::json!(250)),
            ("finalCacheReadMaxTokens", serde_json::json!(-1)),
            ("scaleMinInputTokens", serde_json::json!(-5)),
        ] {
            let patch: StableSegmentCachePolicyPatch =
                serde_json::from_value(serde_json::json!({ field: value })).expect("deserialize");
            assert!(
                patch.validate_raw("stableSegment").is_err(),
                "{field} 越界应拒绝"
            );
        }
        let normalized = StableSegmentCachePolicy {
            token_scale: 9.0,
            output_uplift_percent: 900,
            ..StableSegmentCachePolicy::default()
        }
        .normalized();
        assert_eq!(normalized.token_scale, 3.0);
        assert_eq!(normalized.output_uplift_percent, 200);
    }

    #[test]
    fn stable_segment_cache_policy_deserializes_template_and_path_patch() {
        let mut config: Config = Config::from_json_value(serde_json::json!({
            "cachePolicy": {
                "stableSegmentCache": {
                    "reportedUsage": {
                        "skipNonStreamUsageProjection": true
                    },
                    "stableSegment": {
                        "coverageRatio": 0.8,
                        "maxCoverageTokens": 90000,
                        "minTotalInputTokens": 3000,
                        "minSegmentTokens": 700,
                        "historyMessageLimit": 20,
                        "reportedInputMaxTokens": 192
                    }
                },
                "pathOverrides": {
                    "/dfcache/stable": {
                        "cacheType": "stable_segment_cache",
                        "routeNamespace": true,
                        "stableSegment": {
                            "coverageRatio": 0.6,
                            "maxNewCreationTokensPerRequest": 12000,
                            "reportedInputMinTokens": 48
                        }
                    }
                }
            }
        }))
        .expect("deserialize config");
        config.routes = config.routes.with_builtin_path_defaults();

        let builtin = config.cache_policy_for_path("/cc/v1/messages");
        assert_eq!(
            builtin.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );

        let resolved = config.cache_policy_for_path("/dfcache/stable/v1/messages");
        assert_eq!(resolved.namespace.as_deref(), Some("/dfcache/stable"));
        assert_eq!(
            resolved.policy.cache_type,
            PromptCacheStrategyType::StableSegmentCache
        );
        assert!(!resolved.policy.simulation.enabled);
        assert!(!resolved.policy.creation_control.enabled);
        assert!(
            resolved
                .policy
                .reported_usage
                .skip_non_stream_usage_projection
        );
        assert_eq!(resolved.policy.stable_segment.coverage_ratio, 0.6);
        assert_eq!(resolved.policy.stable_segment.max_coverage_tokens, 90_000);
        assert_eq!(resolved.policy.stable_segment.min_total_input_tokens, 3_000);
        assert_eq!(resolved.policy.stable_segment.min_segment_tokens, 700);
        assert_eq!(resolved.policy.stable_segment.history_message_limit, 20);
        assert_eq!(
            resolved
                .policy
                .stable_segment
                .max_new_creation_tokens_per_request,
            12_000
        );
        assert_eq!(resolved.policy.stable_segment.reported_input_min_tokens, 48);
        assert_eq!(
            resolved.policy.stable_segment.reported_input_max_tokens,
            192
        );
    }

    #[test]
    fn prefix_tool_plus_policy_resolves_template_and_path_override() {
        let mut config: Config = Config::from_json_value(serde_json::json!({
            "cachePolicy": {
                "prefixToolPlus": {
                    "hitShaping": { "enabled": true },
                    "prefixToolPlus": {
                        "coverageRatio": 0.8,
                        "shaping": { "cacheRead": { "multiplier": 2.5 } }
                    }
                },
                "pathOverrides": {
                    "/plus": { "cacheType": "prefix_tool_plus" },
                    "/plus-custom": {
                        "cacheType": "prefix_tool_plus",
                        "prefixToolPlus": {
                            "creationFrequency": { "enabled": true, "minSuccessfulRequestsBetweenCreation": 5 },
                            "suppressedCreationTarget": "cache_read",
                            "shaping": { "cacheCreation": { "multiplier": 3, "maxTokens": 50000 } },
                            "nonStream": { "mode": "raw" },
                            "smallRequest": { "enabled": true, "maxInputTokens": 2048, "disableCache": true }
                        }
                    }
                }
            }
        }))
        .expect("deserialize config");
        config.cache_policy = config.cache_policy.normalized();
        config
            .routes
            .validate_with_config(&config)
            .expect("valid plus policy");

        let template = config.cache_policy_for_path("/plus/v1/messages");
        assert_eq!(template.namespace.as_deref(), Some("/plus"));
        assert_eq!(
            template.policy.cache_type,
            PromptCacheStrategyType::PrefixToolPlus
        );
        assert_eq!(template.policy.prefix_tool_plus.coverage_ratio, 0.8);
        assert_eq!(
            template
                .policy
                .prefix_tool_plus
                .shaping
                .cache_read
                .multiplier,
            2.5
        );
        // Plus 策略自带整形，不叠加 hitShaping / reportedUsage。
        assert!(!template.policy.hit_shaping.enabled);
        assert!(!template.policy.reported_usage.enabled);

        let custom = config.cache_policy_for_path("/plus-custom/v1/messages");
        let plus = custom.policy.prefix_tool_plus;
        assert!(plus.creation_frequency.enabled);
        assert_eq!(
            plus.creation_frequency
                .min_successful_requests_between_creation,
            5
        );
        assert_eq!(
            plus.suppressed_creation_target,
            SuppressedCreationTarget::CacheRead
        );
        assert_eq!(plus.shaping.cache_creation.multiplier, 3.0);
        assert_eq!(plus.shaping.cache_creation.max_tokens, 50_000);
        // 路径级参数整体替换模板。
        assert_eq!(plus.shaping.cache_read.multiplier, 1.0);
        assert_eq!(
            plus.non_stream.mode,
            crate::model::prefix_tool_plus_policy::UsageShapingProfileMode::Raw
        );
        assert!(plus.small_request.enabled && plus.small_request.disable_cache);
        assert_eq!(plus.small_request.max_input_tokens, 2_048);
    }

    #[test]
    fn bounded_high_cache_policy_resolves_own_template_and_path_override() {
        let mut config: Config = Config::from_json_value(serde_json::json!({
            "cachePolicy": {
                "default": { "hitShaping": { "enabled": true, "minCostRatio": 0.9 } },
                "currentHighCache": { "hitShaping": { "targetHitRatio": 0.6 } },
                "boundedHighCache": {
                    "simulation": { "enabled": true, "tokenScale": 2.0 },
                    "hitShaping": { "enabled": true, "targetHitRatio": 0.95, "minCostRatio": 0.3 }
                },
                "pathOverrides": {
                    "/bounded": { "cacheType": "bounded_high_cache" },
                    "/bounded-custom": {
                        "cacheType": "bounded_high_cache",
                        "hitShaping": { "maxReadMultiplier": 5.0 },
                        "reportedUsage": {
                            "input": { "mode": "sample-max", "maxTokens": 500 }
                        }
                    },
                    "/current": { "cacheType": "current_high_cache" }
                }
            }
        }))
        .expect("deserialize config");
        config.cache_policy = config.cache_policy.normalized();
        config
            .routes
            .validate_with_config(&config)
            .expect("valid bounded policy");

        let template = config.cache_policy_for_path("/bounded/v1/messages");
        assert_eq!(
            template.policy.cache_type,
            PromptCacheStrategyType::BoundedHighCache
        );
        assert!(template.policy.cache_type.uses_high_cache_simulation());
        // 只取自己的模板，不继承 default / currentHighCache。
        assert_eq!(template.policy.hit_shaping.target_hit_ratio, 0.95);
        assert_eq!(template.policy.hit_shaping.min_cost_ratio, 0.3);
        assert_eq!(template.policy.simulation.token_scale, 2.0);

        let custom = config.cache_policy_for_path("/bounded-custom/v1/messages");
        assert_eq!(custom.namespace.as_deref(), Some("/bounded-custom"));
        assert_eq!(custom.policy.hit_shaping.max_read_multiplier, 5.0);
        assert_eq!(custom.policy.hit_shaping.target_hit_ratio, 0.95);
        assert_eq!(custom.policy.reported_usage.input.max_tokens, 500);

        // current_high_cache 不受新模板影响。
        let current = config.cache_policy_for_path("/current/v1/messages");
        assert_eq!(
            current.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(current.policy.hit_shaping.target_hit_ratio, 0.6);
        assert_eq!(current.policy.hit_shaping.min_cost_ratio, 0.9);

        let serialized = serde_json::to_value(PromptCacheStrategyType::BoundedHighCache)
            .expect("serialize strategy");
        assert_eq!(serialized, serde_json::json!("bounded_high_cache"));
    }

    #[test]
    fn prefix_tool_plus_policy_rejects_invalid_path_override() {
        let config: Config = Config::from_json_value(serde_json::json!({
            "cachePolicy": {
                "pathOverrides": {
                    "/plus": {
                        "cacheType": "prefix_tool_plus",
                        "prefixToolPlus": {
                            "shaping": { "output": { "minTokens": 10, "maxTokens": 5 } }
                        }
                    }
                }
            }
        }))
        .expect("deserialize config");
        let err = config
            .routes
            .validate_with_config(&config)
            .expect_err("min > max should fail");
        assert!(err.contains("prefixToolPlus.shaping.output"), "{err}");
    }

    #[test]
    fn prefix_tool_cache_policy_deserializes_template_and_path_patch() {
        let mut config: Config = Config::from_json_value(serde_json::json!({
            "cachePolicy": {
                "prefixTool": {
                    "reportedUsage": {
                        "skipNonStreamUsageProjection": true
                    },
                    "prefixTool": {
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
                    "/dfcache/pool-param": {
                        "cacheType": "prefix_tool",
                        "prefixTool": {
                            "coverageRatio": 0.5,
                            "maxNewCreationTokensPerRequest": 1000,
                            "reportedInputMinTokens": 128
                        }
                    }
                }
            }
        }))
        .expect("deserialize config");
        config.routes = config.routes.with_builtin_path_defaults();

        let template = config.cache_policy_for_path("/cc/v1/messages");
        assert_eq!(
            template.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );

        let resolved = config.cache_policy_for_path("/dfcache/pool-param/v1/messages");
        assert_eq!(resolved.namespace.as_deref(), Some("/dfcache/pool-param"));
        assert_eq!(
            resolved.policy.cache_type,
            PromptCacheStrategyType::PrefixTool
        );
        assert!(
            resolved
                .policy
                .reported_usage
                .skip_non_stream_usage_projection
        );
        assert_eq!(resolved.policy.prefix_tool.coverage_ratio, 0.5);
        assert_eq!(resolved.policy.prefix_tool.max_coverage_tokens, 12_000);
        assert_eq!(
            resolved
                .policy
                .prefix_tool
                .max_new_creation_tokens_per_request,
            1_000
        );
        assert!(resolved.policy.prefix_tool.cache_current_user_stable_prefix);
        assert_eq!(
            resolved
                .policy
                .prefix_tool
                .current_user_stable_prefix_max_tokens,
            1_500
        );
        assert_eq!(resolved.policy.prefix_tool.reported_input_min_tokens, 128);
        assert_eq!(resolved.policy.prefix_tool.reported_input_max_tokens, 2_048);
    }

    #[test]
    fn prefix_tool_cache_policy_rejects_invalid_values() {
        let config: Config = Config::from_json_value(serde_json::json!({
            "cachePolicy": {
                "pathOverrides": {
                    "/dfcache/bad": {
                        "cacheType": "prefix_tool",
                        "prefixTool": {
                            "coverageRatio": 1.5
                        }
                    }
                }
            }
        }))
        .expect("deserialize config");

        let err = config
            .routes
            .validate_with_config(&config)
            .expect_err("invalid coverage ratio should fail");
        assert!(err.contains("coverageRatio"));

        let invalid_range: Config = Config::from_json_value(serde_json::json!({
            "cachePolicy": {
                "pathOverrides": {
                    "/dfcache/bad-range": {
                        "cacheType": "prefix_tool",
                        "prefixTool": {
                            "reportedInputMinTokens": 4096,
                            "reportedInputMaxTokens": 32
                        }
                    }
                }
            }
        }))
        .expect("deserialize config");

        let err = invalid_range
            .routes
            .validate_with_config(&invalid_range)
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
        config
            .routes
            .route_mut(&"/dfcache/team-a".to_string())
            .cache_policy = CacheRoutePolicyPatch {
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
        };

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
        config
            .routes
            .route_mut(&"/reports".to_string())
            .cache_policy = CacheRoutePolicyPatch {
            reported_usage: Some(ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(64),
                ..ReportedUsagePathPolicy::default()
            }),
            ..CacheRoutePolicyPatch::default()
        };

        let resolved = config.cache_policy_for_path("/reports/v1/messages");

        assert_eq!(resolved.namespace, None);
        assert_eq!(
            resolved.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(resolved.policy.reported_usage.input.max_tokens, 64);
    }

    #[test]
    fn builtin_route_cache_strategy_is_config_driven() {
        let mut config = Config::default();
        config.routes.route_mut(&"/cc".to_string()).cache_policy = CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::NoCache),
            ..CacheRoutePolicyPatch::default()
        };
        config.routes.route_mut(&"/na".to_string()).cache_policy = CacheRoutePolicyPatch {
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
        };
        config.routes.route_mut(&"/ha".to_string()).cache_policy = CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
            route_namespace: Some(true),
            ..CacheRoutePolicyPatch::default()
        };

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
        let mut routes = RoutesConfig::default();
        routes.route_mut("/cc").cache_policy.cache_type = Some(PromptCacheStrategyType::PrefixTool);
        routes.route_mut("/na").cache_policy = CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
            simulation: Some(CacheSimulationPolicyPatch {
                enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut routes = routes.with_builtin_path_defaults();
        for prefix in ["", "/cc", "/ha", "/na"] {
            assert!(routes.routes.iter().any(|route| route.prefix == prefix));
        }
        assert_eq!(
            routes.route_mut("/cc").cache_policy.cache_type,
            Some(PromptCacheStrategyType::PrefixTool)
        );
        assert_eq!(
            routes.route_mut("/na").cache_policy.cache_type,
            Some(PromptCacheStrategyType::CurrentHighCache)
        );
        assert!(routes.route_mut("/na").cache_policy.simulation.is_some());
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
                !crate::model::prompt_injection::catalog::DEFAULT_TASK_QUALITY_PROMPT
                    .contains(marker),
                "default prompt must not teach the model internal marker {marker}"
            );
        }
    }

    #[test]
    fn cache_policy_legacy_defined_cache_routes_default_to_current_strategy() {
        let config =
            Config::from_json_value(serde_json::json!({"definedCacheRoutes": ["/dfcache/team-a"]}))
                .unwrap();

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
        config.routes.route_mut(&"/bad".to_string()).cache_policy = CacheRoutePolicyPatch {
            simulation: Some(CacheSimulationPolicyPatch {
                target_read_ratio: Some(1.5),
                ..CacheSimulationPolicyPatch::default()
            }),
            ..CacheRoutePolicyPatch::default()
        };

        assert!(config.routes.validate_with_config(&config).is_err());

        let mut invalid_prefix = Config::default();
        invalid_prefix
            .routes
            .routes
            .push(crate::model::routes::RouteConfig::new(" "));
        assert!(
            invalid_prefix
                .routes
                .validate_with_config(&invalid_prefix)
                .is_err()
        );

        let mut invalid_creation_control = Config::default();
        invalid_creation_control
            .routes
            .route_mut(&"/bad-creation".to_string())
            .cache_policy = CacheRoutePolicyPatch {
            creation_control: Some(PromptCacheCreationControlConfig {
                min_creation_delta_tokens: -1,
                ..PromptCacheCreationControlConfig::default()
            }),
            ..CacheRoutePolicyPatch::default()
        };
        assert!(
            invalid_creation_control
                .routes
                .validate_with_config(&invalid_creation_control)
                .is_err()
        );

        let mut invalid_bounds = Config::default();
        invalid_bounds
            .routes
            .route_mut(&"/bad-bounds".to_string())
            .cache_policy = CacheRoutePolicyPatch {
            bounds: Some(CacheBoundsPolicyPatch {
                entry_ttl_secs: Some(0),
                ..CacheBoundsPolicyPatch::default()
            }),
            ..CacheRoutePolicyPatch::default()
        };
        assert!(
            invalid_bounds
                .routes
                .validate_with_config(&invalid_bounds)
                .is_err()
        );

        let mut invalid_stable_segment = Config::default();
        invalid_stable_segment
            .routes
            .route_mut(&"/bad-stable".to_string())
            .cache_policy = CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::StableSegmentCache),
            stable_segment: Some(Box::new(StableSegmentCachePolicyPatch {
                coverage_ratio: Some(1.5),
                default_ttl_secs: Some(300),
                extended_ttl_secs: Some(60),
                ..StableSegmentCachePolicyPatch::default()
            })),
            ..CacheRoutePolicyPatch::default()
        };
        assert!(
            invalid_stable_segment
                .routes
                .validate_with_config(&invalid_stable_segment)
                .is_err()
        );
    }

    #[test]
    fn inference_attempt_budget_uses_compatible_default_and_preserves_explicit_value() {
        let historical: Config = Config::from_json_str("{}").unwrap();
        assert_eq!(historical.inference_upstream_max_attempts, 4);

        let explicit: Config =
            Config::from_json_str(r#"{"inferenceUpstreamMaxAttempts":7}"#).unwrap();
        assert_eq!(explicit.inference_upstream_max_attempts, 7);
    }
}
