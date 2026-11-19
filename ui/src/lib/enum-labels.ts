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

// ─── 账号：连接与 Header（文案与 main 外部池表单一致）────────────────────────────

export const ACCOUNT_AUTH_TYPE_OPTIONS: EnumOption[] = [
  { value: 'bearer', label: 'Authorization: Bearer <key>' },
  { value: 'x_api_key', label: 'x-api-key: <key>' },
]

export const ACCOUNT_HEADER_PROFILE_OPTIONS: EnumOption[] = [
  { value: 'generic', label: '默认泛用转发', description: '保持泛用转发策略，仅过滤认证、连接、cookie、x-goog-api-key 等敏感请求头；是否追加 beta=true 由开关控制。' },
  { value: 'anthropic_passthrough', label: 'Anthropic allowlist', description: '只透传 Anthropic/Claude Code 相关请求头，过滤 cookie、x-goog-api-key 和非必要客户端头；URL 自动追加 beta=true。' },
  { value: 'claude_code_mimic', label: 'Claude Code mimic', description: '忽略客户端指纹，使用 Claude Code 风格的 x-stainless、User-Agent、x-client-request-id 和 anthropic-beta；URL 自动追加 beta=true。' },
]

export const ACCOUNT_WIRE_PROFILE_OPTIONS: EnumOption[] = [
  { value: 'default', label: '默认传输', description: '使用默认 reqwest 传输行为，允许 HTTP/2 协商，header 大小写由 HTTP 客户端决定。' },
  { value: 'http1_title_case', label: 'HTTP/1 Title-Case', description: '强制 HTTP/1，并启用 Title-Case header 写出；这是兼容增强，不是完整 Claude Code 复刻。' },
]

export const ACCOUNT_TLS_PROFILE_OPTIONS: EnumOption[] = [
  { value: 'default', label: '默认 TLS', description: '使用默认 TLS 栈（rustls）。' },
  { value: 'native_tls', label: 'Native TLS', description: '使用系统 native-tls 栈；这是 TLS 栈切换，不是完整 ClientHello 指纹。' },
]

// ─── 账号：usage 与流式 ─────────────────────────────────────────────────────────

export const USAGE_PROJECTION_MODE_OPTIONS: EnumOption[] = [
  { value: 'current_path_policy', label: '按当前入口路径整理 usage', description: '返回给下游的 usage 按当前入口路径的缓存策略整理。' },
  { value: 'pass_through', label: '透传上游 usage', description: '返回给下游的 usage 保持上游原始值，不按入口路径整理。' },
]

export const ACCOUNT_STREAM_RESPONSE_OPTIONS: EnumOption[] = [
  { value: 'inherit', label: '继承全局默认' },
  { value: 'event_passthrough', label: 'SSE 事件级透传', description: '文本、thinking、tool 等事件按上游事件级转发；usage 是否透传由上面的下游 usage 决定。' },
]

export const STREAM_RETRY_MODE_OPTIONS: EnumOption[] = [
  { value: 'inherit', label: '继承全局' },
  { value: 'enabled', label: '启用', description: 'stream 在 message_start、ping 等协议前缀后遇到 error、断流、EOF 或空闲超时时，可在未提交内容前换其他账号。' },
  { value: 'disabled', label: '禁用', description: 'stream body 内错误保持现有流错误行为，不重放到其他账号。' },
  { value: 'early_commit', label: '先提交首包再换池', description: '收到上游 message_start 后立即转发并持续发送 ping；首个内容前出错仍可换账号重试，下游只收到一次 message_start。' },
]

export const AUTO_DISABLE_POLICY_OPTIONS: EnumOption[] = [
  { value: 'inherit', label: '继承全局自动禁用' },
  { value: 'enabled', label: '单独启用自动禁用' },
  { value: 'disabled', label: '关闭自动禁用' },
]

// ─── 账号：路由与模型 ───────────────────────────────────────────────────────────

export const RULE_MODE_OPTIONS: EnumOption[] = [
  { value: 'allow_all', label: '全部入口允许' },
  { value: 'allow_list', label: '只允许下列入口' },
  { value: 'deny_list', label: '禁止下列入口' },
]

export const MODEL_MAPPING_MODE_OPTIONS: EnumOption[] = [
  { value: 'passthrough', label: '直接使用请求模型', description: '直接使用客户端请求里的模型，不应用映射规则和兜底转换。' },
  { value: 'passthrough_mapping', label: '请求模型优先映射', description: '先用客户端请求模型匹配规则；未命中时仍使用原请求模型。' },
  { value: 'direct_mapping', label: '映射后内部处理', description: '用客户端请求模型匹配规则；未命中后使用内部处理模型。' },
  { value: 'processed_mapping', label: '内部处理后映射', description: '先使用本系统解析后的模型匹配规则；未命中后使用内部处理模型。' },
]

export const MODEL_MAPPING_RULE_KIND_OPTIONS: EnumOption[] = [
  { value: 'alias', label: '别名' },
  { value: 'version_equivalent', label: '版本等价' },
  { value: 'fallback', label: '兜底' },
]

// ─── 运行配置（文案与 main 运行配置页一致）───────────────────────────────────────

export const CAPACITY_MODE_OPTIONS: EnumOption[] = [
  { value: 'wait', label: '等待容量' },
  { value: 'fail_fast', label: '立即失败' },
]

export const MODEL_UNAVAILABLE_COOLDOWN_MODE_OPTIONS: EnumOption[] = [
  { value: 'model', label: '仅当前模型' },
  { value: 'pool', label: '整个账号' },
  { value: 'disabled', label: '不写冷却' },
]

export const MODEL_RESOLUTION_MODE_OPTIONS: EnumOption[] = [
  { value: 'compatible', label: '默认兼容解析' },
  { value: 'alias_only', label: '仅精确与显式别名' },
  { value: 'exact_only', label: '仅完整模型名' },
]

export const COMPAT_PROFILE_OPTIONS: EnumOption[] = [
  { value: 'claude-code', label: 'Claude Code 兼容' },
  { value: 'anthropic-strict', label: 'Anthropic 严格模式' },
  { value: 'debug', label: '调试模式' },
]

export const TLS_BACKEND_OPTIONS: EnumOption[] = [
  { value: 'rustls', label: 'rustls' },
  { value: 'native-tls', label: 'native-tls' },
]

export const MISSING_MAX_TOKENS_POLICY_OPTIONS: EnumOption[] = [
  { value: 'default_value', label: '自动补全' },
  { value: 'reject', label: '直接拒绝' },
]

export const THINKING_TRIGGER_OPTIONS: EnumOption[] = [
  { value: 'off', label: '不自动开启' },
  { value: 'real_request', label: '按请求触发' },
  { value: 'always', label: '总是触发' },
]

export const PAYLOAD_GUARD_MODE_OPTIONS: EnumOption[] = [
  { value: 'on_too_long', label: '失败后再处理并重试' },
  { value: 'preemptive', label: '发送前先处理' },
]

export const TOO_LONG_HANDLING_OPTIONS: EnumOption[] = [
  { value: 'client_compaction', label: '返回 prompt is too long，交给客户端压缩' },
  { value: 'trim_retry', label: '服务端裁剪历史后重试' },
]

export const OVERSIZED_IMAGE_HANDLING_OPTIONS: EnumOption[] = [
  { value: 'drop-with-placeholder', label: '移除图片并占位' },
  { value: 'reject', label: '直接报错' },
]

export const IMAGE_PROCESSING_MODE_OPTIONS: EnumOption[] = [
  { value: 'safe', label: 'Safe：兼容修复' },
  { value: 'light', label: 'Light：轻量透传' },
]

/** 用量上报字段策略：叫法和顺序与 main 运行配置页的字段策略下拉一致。 */
export const REPORTED_USAGE_FIELD_MODE_OPTIONS: EnumOption[] = [
  { value: 'raw', label: '使用原始值' },
  { value: 'preserve', label: '使用当前值' },
  { value: 'sample-max', label: '压到上限内' },
  { value: 'sample-target', label: '压到目标附近' },
]

export const INPUT_DELTA_TARGET_OPTIONS: EnumOption[] = [
  { value: 'input', label: '输入 input' },
  { value: 'cache_read', label: '缓存读取 cache_read' },
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
