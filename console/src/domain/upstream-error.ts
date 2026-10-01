/**
 * 上游/后端错误翻译：把原始错误文本映射为"中文标题 + 建议动作"，原文保留供排查。
 * 只做展示层翻译，不改变任何错误判定。
 */

export interface TranslatedError {
  title: string
  hint?: string
  raw: string
}

const REFRESH_KIND: Record<string, { title: string; hint: string }> = {
  invalid_grant: { title: 'Refresh Token 已失效', hint: '账号需要重新登录获取凭据，或直接删除' },
  credential_auth: { title: '认证信息被拒绝', hint: '检查 clientId / clientSecret 或重新导入账号' },
  rate_limited: { title: 'Token 刷新被限流', hint: '稍后重试；可在"Token 刷新"配置中调整 RPM' },
  upstream_unavailable: { title: '认证服务暂不可用', hint: '上游临时故障，稍后重试' },
  network: { title: '无法连接认证服务', hint: '检查网络或账号绑定的代理' },
  timeout: { title: '认证服务请求超时', hint: '检查网络或代理延迟后重试' },
  protocol: { title: '认证服务返回异常', hint: '稍后重试；持续出现请检查代理是否篡改响应' },
  oversize: { title: '认证响应过大', hint: '多为代理返回了错误页面，检查代理配置' },
  malformed_response: { title: '认证响应格式异常', hint: '多为代理返回了错误页面，检查代理配置' },
  missing_token: { title: '认证响应缺少 Token', hint: '凭据可能已失效，尝试重新导入' },
  invalid_configuration: { title: '凭据配置不完整', hint: '缺少必要字段（如 refreshToken、IdC 的 clientId/clientSecret 或 region）' },
  coordination: { title: 'Token 刷新协调失败', hint: '多实例刷新锁暂不可用，检查 Redis 后重试' },
  coordination_unavailable: { title: 'Token 刷新协调失败', hint: '多实例刷新锁暂不可用，检查 Redis 后重试' },
  persistence: { title: 'Token 保存失败', hint: '检查数据库连接' },
  internal: { title: 'Token 刷新内部错误', hint: '查看服务日志' },
}

const PATTERNS: Array<{ test: RegExp; title: string; hint?: string }> = [
  { test: /invalid_grant/i, title: 'Refresh Token 已失效', hint: '账号需要重新登录获取凭据，或直接删除' },
  {
    test: /MONTHLY_REQUEST_COUNT|OVERAGE_REQUEST_LIMIT_EXCEEDED|quota.*exceed/i,
    title: '账号额度已用尽',
    hint: '等待额度重置，或为该账号开启超额',
  },
  { test: /TEMPORARILY_SUSPENDED/i, title: '账号被上游临时暂停', hint: '稍后体检；持续出现建议停用' },
  { test: /ACCOUNT_SUSPENDED|account.*(suspend|banned)/i, title: '账号已被上游封禁', hint: '删除该账号' },
  { test: /INSUFFICIENT_MODEL_CAPACITY/i, title: '上游模型容量不足', hint: '稍后重试或换用其他模型' },
  { test: /INVALID_MODEL_ID|model.*not.*(found|support)/i, title: '模型不可用', hint: '检查模型名称或账号的支持模型列表' },
  {
    test: /CONTENT_LENGTH_EXCEEDS_THRESHOLD|input is too long|prompt is too long/i,
    title: '请求内容过长',
    hint: '调整"请求体处理"中的大小保护与历史清理',
  },
  {
    test: /ThrottlingException|\b429\b|too many requests|rate limit/i,
    title: '上游限流',
    hint: '账号会自动冷却；频繁出现可降低并发或 RPM',
  },
  { test: /AccessDeniedException|\b403\b|forbidden/i, title: '上游拒绝访问', hint: '检查账号权限、Profile ARN 或代理出口' },
  { test: /\b401\b|unauthorized/i, title: '认证失败', hint: '检查 Key 或重新导入账号' },
  { test: /timed? ?out|timeout|deadline/i, title: '请求超时', hint: '检查网络、代理或调整超时配置' },
  { test: /connection (refused|reset)|dns|connect error|tcp connect|proxy/i, title: '网络连接失败', hint: '检查网络或代理配置' },
  { test: /\b5\d\d\b|internal server error|bad gateway|service unavailable/i, title: '上游服务异常', hint: '稍后重试' },
]

export function translateError(raw: string | null | undefined): TranslatedError | null {
  const text = (raw ?? '').trim()
  if (!text) return null
  const kind = text.match(/token refresh failed:.*?kind=([a-z_]+)/i)?.[1]
  if (kind && REFRESH_KIND[kind]) return { ...REFRESH_KIND[kind]!, raw: text }
  for (const p of PATTERNS) if (p.test.test(text)) return { title: p.title, hint: p.hint, raw: text }
  return { title: text.length > 80 ? `${text.slice(0, 80)}…` : text, raw: text }
}
