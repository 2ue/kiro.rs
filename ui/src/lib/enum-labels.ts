/**
 * 控制台里所有枚举值的中文文案与说明。
 *
 * 规则：
 * - 下拉选项永远显示 `label`，不显示英文枚举值；需要时在 `description` 里解释取值含义。
 * - 文案按“这个选项会让系统做什么”来写，不照搬字段名直译。
 * - 新增枚举先加到这里，再在页面里引用；页面不要各自维护一份。
 */

export type EnumOption<T extends string = string> = {
  value: T
  label: string
  description?: string
}

/** 按 value 取文案；没登记的值原样返回，方便发现漏项。 */
export function enumLabel(options: readonly EnumOption[], value: string | null | undefined): string {
  if (value == null) return '-'
  return options.find((option) => option.value === value)?.label ?? value
}

// ─── 账号：鉴权与请求发送方式 ───────────────────────────────────────────────────

export const ACCOUNT_AUTH_TYPE_OPTIONS: EnumOption[] = [
  { value: 'bearer', label: 'Authorization: Bearer', description: '密钥放在 Authorization 头里，大多数中转站用这种方式' },
  { value: 'x_api_key', label: 'x-api-key 头', description: '密钥放在 x-api-key 头里，与 Anthropic 官方 API 一致' },
]

export const ACCOUNT_HEADER_PROFILE_OPTIONS: EnumOption[] = [
  { value: 'generic', label: '通用', description: '只发鉴权头和必要的协议头，适合大多数中转站' },
  { value: 'anthropic_passthrough', label: '透传 Anthropic 头', description: '把下游请求里的 anthropic-* 头原样转发给上游' },
  { value: 'claude_code_mimic', label: '模拟 Claude Code', description: '补齐 Claude Code CLI 会发的 user-agent 与 beta 头，用于只接受官方客户端的上游' },
]

export const ACCOUNT_WIRE_PROFILE_OPTIONS: EnumOption[] = [
  { value: 'default', label: '默认（HTTP/2 优先）', description: '按上游协商结果选择 HTTP 版本，头名小写' },
  { value: 'http1_title_case', label: '仅 HTTP/1.1，头名首字母大写', description: '部分老式网关要求 Content-Type 这类写法，并且不支持 HTTP/2' },
]

export const ACCOUNT_TLS_PROFILE_OPTIONS: EnumOption[] = [
  { value: 'default', label: '默认（rustls）', description: '内置 TLS 实现，无系统依赖' },
  { value: 'native_tls', label: '系统 TLS', description: '使用操作系统的 TLS 库，个别上游对 TLS 指纹有要求时使用' },
]

// ─── 账号：usage 与流式行为 ─────────────────────────────────────────────────────

export const USAGE_PROJECTION_MODE_OPTIONS: EnumOption[] = [
  { value: 'current_path_policy', label: '按路由策略整形', description: '下游看到的 usage 按入口路由挂载的缓存策略整理（默认）' },
  { value: 'pass_through', label: '原样返回上游 usage', description: '不做任何整形，上游返回什么下游就看到什么' },
]

export const ACCOUNT_STREAM_RESPONSE_OPTIONS: EnumOption[] = [
  { value: 'inherit', label: '继承全局' },
  { value: 'event_passthrough', label: '按事件透传', description: '逐个 SSE 事件转发，并屏蔽上游错误事件里的内部细节' },
]

export const STREAM_RETRY_MODE_OPTIONS: EnumOption[] = [
  { value: 'inherit', label: '继承全局' },
  { value: 'enabled', label: '开启', description: '首个有效输出之前上游出错、断流或空闲超时时，换账号重试' },
  { value: 'disabled', label: '关闭', description: '首输出前出错直接按失败处理，不换账号' },
  { value: 'early_commit', label: '提前提交首包', description: '立即把 message_start 发给下游以降低首字延迟；首个内容前出错仍可换账号重放' },
]

export const AUTO_DISABLE_POLICY_OPTIONS: EnumOption[] = [
  { value: 'inherit', label: '继承全局' },
  { value: 'enabled', label: '开启', description: '连续失败达到阈值后自动停用该账号一段时间' },
  { value: 'disabled', label: '关闭', description: '只降权，不自动停用' },
]

// ─── 账号：路由与模型 ───────────────────────────────────────────────────────────

export const RULE_MODE_OPTIONS: EnumOption[] = [
  { value: 'allow_all', label: '允许全部' },
  { value: 'allow_list', label: '只允许列表内', description: '只有命中规则的项可用' },
  { value: 'deny_list', label: '拒绝列表内', description: '命中规则的项不可用，其余可用' },
]

export const MODEL_MAPPING_MODE_OPTIONS: EnumOption[] = [
  { value: 'passthrough', label: '不映射', description: '请求里的模型名原样发给上游' },
  { value: 'passthrough_mapping', label: '只按规则替换', description: '命中规则的替换，其余原样透传' },
  { value: 'direct_mapping', label: '规则映射，未命中按原名', description: '先按规则替换；没命中时用请求原名，不做别名归一' },
  { value: 'processed_mapping', label: '先解析别名再映射', description: '先把 sonnet、opus 等短名解析成完整模型，再按规则替换（默认）' },
]

export const MODEL_MAPPING_RULE_KIND_OPTIONS: EnumOption[] = [
  { value: 'alias', label: '别名' },
  { value: 'version_equivalent', label: '版本等价' },
  { value: 'fallback', label: '兜底' },
]

// ─── 运行配置 ───────────────────────────────────────────────────────────────────

export const CAPACITY_MODE_OPTIONS: EnumOption[] = [
  { value: 'wait', label: '排队等待', description: '没有空闲账号槽位时进入等待队列，直到拿到槽位或超过等待时间（默认）' },
  { value: 'fail_fast', label: '立即失败', description: '没有空闲槽位时直接返回不可用' },
]

export const MODEL_UNAVAILABLE_COOLDOWN_MODE_OPTIONS: EnumOption[] = [
  { value: 'model', label: '只冷却该模型', description: '上游说模型不可用时，只避开这个账号的这个模型（默认）' },
  { value: 'pool', label: '冷却整个账号', description: '把整个账号暂时移出调度' },
  { value: 'disabled', label: '不冷却' },
]

export const MODEL_RESOLUTION_MODE_OPTIONS: EnumOption[] = [
  { value: 'compatible', label: '兼容 Claude Code', description: '允许 sonnet、opus、default 等短名，并把同系列旧版本映射到可用模型（默认）' },
  { value: 'alias_only', label: '只认精确名和显式别名', description: '不做同系列宽松归一' },
  { value: 'exact_only', label: '只认目录里的精确模型 ID' },
]

export const COMPAT_PROFILE_OPTIONS: EnumOption[] = [
  { value: 'claude-code', label: 'Claude Code 兼容', description: '保留 Claude Code CLI 需要的协议改写（默认）' },
  { value: 'anthropic-strict', label: '严格 Anthropic', description: '尽量不做协议和提示词改写' },
  { value: 'debug', label: '调试', description: '同 Claude Code 兼容，但默认暴露代理警告头' },
]

export const TLS_BACKEND_OPTIONS: EnumOption[] = [
  { value: 'rustls', label: 'rustls（内置）' },
  { value: 'native-tls', label: '系统 TLS' },
]

export const MISSING_MAX_TOKENS_POLICY_OPTIONS: EnumOption[] = [
  { value: 'default_value', label: '补默认值', description: '请求没带 max_tokens 时补一个较小的正数（默认）' },
  { value: 'reject', label: '拒绝请求' },
]

export const THINKING_TRIGGER_OPTIONS: EnumOption[] = [
  { value: 'off', label: '不自动开启', description: '只按客户端显式的 thinking 字段处理' },
  { value: 'real_request', label: '按请求信号触发', description: '客户端带 ultrathink 等可见信号时开启 thinking' },
  { value: 'always', label: '始终开启', description: '请求未显式关闭时一律开启 thinking' },
]

export const PAYLOAD_GUARD_MODE_OPTIONS: EnumOption[] = [
  { value: 'on_too_long', label: '上游报超长后再裁剪', description: '首次只做协议修复，上游返回输入过长后裁剪并重试一次（默认）' },
  { value: 'preemptive', label: '发送前预先裁剪', description: '按字节上限在发送上游前就裁剪历史' },
]

export const TOO_LONG_HANDLING_OPTIONS: EnumOption[] = [
  { value: 'client_compaction', label: '交给客户端压缩', description: '按 Claude Code 协议返回“prompt is too long”，由客户端压缩后重试（默认）' },
  { value: 'trim_retry', label: '服务端裁剪后重试' },
]

export const OVERSIZED_IMAGE_HANDLING_OPTIONS: EnumOption[] = [
  { value: 'drop-with-placeholder', label: '移除图片并留占位' },
  { value: 'reject', label: '直接报错' },
]

export const IMAGE_PROCESSING_MODE_OPTIONS: EnumOption[] = [
  { value: 'safe', label: '安全', description: '只做无损的规范化' },
  { value: 'light', label: '轻量', description: '允许轻度压缩以满足上游限制' },
]

export const REPORTED_USAGE_FIELD_MODE_OPTIONS: EnumOption[] = [
  { value: 'preserve', label: '保持整形结果' },
  { value: 'raw', label: '原样上报上游值' },
  { value: 'sample-max', label: '随机采样，不超过上限' },
  { value: 'sample-target', label: '围绕目标值采样' },
]

export const INPUT_DELTA_TARGET_OPTIONS: EnumOption[] = [
  { value: 'input', label: '留在输入' },
  { value: 'cache_read', label: '转到缓存读取' },
  { value: 'drop', label: '直接丢弃' },
]

/**
 * 通用配置表单按字段名查选项；没登记的字段退回为文本框。
 * 字段名重复出现在不同对象里时（如 `mode`、`policy`），由调用方按上下文传更具体的表。
 */
export const ENUM_OPTIONS_BY_FIELD: Record<string, EnumOption[]> = {
  capacityMode: CAPACITY_MODE_OPTIONS,
  streamResponseMode: ACCOUNT_STREAM_RESPONSE_OPTIONS.filter((option) => option.value !== 'inherit'),
  modelUnavailableCooldownMode: MODEL_UNAVAILABLE_COOLDOWN_MODE_OPTIONS,
  modelResolutionMode: MODEL_RESOLUTION_MODE_OPTIONS,
  compatProfile: COMPAT_PROFILE_OPTIONS,
  tlsBackend: TLS_BACKEND_OPTIONS,
  policy: MISSING_MAX_TOKENS_POLICY_OPTIONS,
  thinkingTrigger: THINKING_TRIGGER_OPTIONS,
  tooLongHandling: TOO_LONG_HANDLING_OPTIONS,
  oversizedImageHandling: OVERSIZED_IMAGE_HANDLING_OPTIONS,
  payloadGuardMode: PAYLOAD_GUARD_MODE_OPTIONS,
}
