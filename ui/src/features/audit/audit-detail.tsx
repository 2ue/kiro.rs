import type { AdminAuditLogRow } from '@/types/api'
import type { ReactNode } from 'react'
import { accountEventView } from './account-events'

type JsonObject = Record<string, unknown>
type RuntimeChange = { path?: unknown; before?: unknown; after?: unknown; redacted?: unknown }

const FIELD_LABELS: Record<string, string> = {
  requestAdmission: '请求准入',
  'requestAdmission.rpm': '每实例 · 每 Key RPM',
  'requestAdmission.maxConcurrentRequests': '每实例 · 每 Key 并发',
  'requestAdmission.maxQueuedRequests': '每实例 · 每 Key 队列',
  'requestAdmission.queueTimeoutMs': '每实例 · 每 Key 等待',
  emptyTurnErrorStatusCode: '空回对外状态码',
  emptyTurnRetryBackoffMode: '空回重试等待方式',
  emptyTurnRetryBaseDelayMs: '空回重试首次等待',
  emptyTurnRetryMaxDelayMs: '空回重试最长等待',
  emptyTurnRetryJitterPercent: '空回重试随机抖动',
  dispatchGlobalMaxConcurrentRequests: '全局最大并发',
  dispatchMaxQueuedRequests: '最大排队请求数',
  streamKeepaliveIntervalSecs: '流式保活间隔',
  streamPreOutputHoldSecs: '首输出前静默保留',
  streamFirstOutputTimeoutSecs: '首输出等待上限',
  inferenceUpstreamMaxAttempts: '单请求推理发送硬上限',
  auxiliaryUpstreamMaxAttempts: '单请求辅助发送硬上限',
  auxiliaryUpstreamMaxConcurrentRequests: '单实例辅助并发上限',
  localBerserkModeEnabled: '狂暴模式（429 暴力轮换）',
  localBerserkMaxRounds: '狂暴模式最多轮换几轮',
  localBerserkRoundDelayMs: '狂暴模式轮次间隔',
  weightedCapacity: '加权容量',
  'weightedCapacity.enabled': '按 token 重量计算本地容量',
  'weightedCapacity.maxUnitsPerRequest': '单请求最大容量单位',
  'weightedCapacity.tiers': '容量分档',
  tokenRefreshMaxRpm: 'Token 刷新 RPM 上限',
  tokenRefreshBurst: 'Token 刷新突发容量',
  tokenRefreshBackgroundEnabled: '闲置账号后台刷新',
  tokenRefreshBackgroundIntervalSecs: '后台刷新扫描间隔',
  tokenRefreshBackgroundLeadSecs: '后台刷新提前量',
  schedulerErrorEwmaAlpha: '近期错误敏感度',
  schedulerPriorityWeight: '优先级权重',
  schedulerLoadWeight: '当前负载权重',
  schedulerErrorWeight: '近期错误权重',
  schedulerLatencyWeight: '响应耗时权重',
  schedulerProbationWeight: '恢复期降权',
  schedulerSelectionPressureWeight: '短时集中降权',
  schedulerTotalSelectionWeight: '长期使用次数权重',
  schedulerTopK: '候选账号数量',
  selectionFailureSampleLimit: '失败诊断样本数',
  selectionFailureRecordEnabled: '记录失败样本',
  compressionEnabled: '启用请求压缩',
  whitespaceCompression: '仅压缩空白字符',
  payloadGuardEnabled: '启用大小保护',
  payloadGuardExternalEnabled: '外部账号也应用大小保护',
  payloadGuardTrimHistory: '优先裁剪旧历史',
  payloadGuardMaxBytes: '请求大小阈值',
  payloadGuardSafetyMarginBytes: '安全余量',
  payloadGuardMode: '过大请求处理方式',
  'imageProcessing.resizeEnabled': '压缩过大的图片',
  'imageProcessing.resizeMaxLongSide': '压缩后长边上限',
  'imageProcessing.resizeMaxBytes': '压缩后体积上限',
  'imageProcessing.resizeJpegQuality': '压缩画质',
  'bodyConversion.toolSchemaNormalization': '工具 schema 规范化',
  'bodyConversion.toolNameMapping': '工具名映射',
  'bodyConversion.nativeReasoningFields': '原生 reasoning 字段',
  'bodyConversion.syntheticThinkingSignatures': 'Thinking 兼容签名',
  'bodyConversion.toolChoiceSteering': '结构化 tool_choice',
  'bodyConversion.toolPairingRepair': '工具配对修复',
  'bodyConversion.historyPlaceholderTools': '历史工具占位',
  'bodyConversion.historyImageDedup': '历史图片去重',
  extractThinking: '整理思考内容',
  exposeProxyWarnings: '显示处理告警',
  highCacheThreshold: '高缓存命中阈值',
  compatProfile: '兼容配置档',
  modelResolutionMode: '模型解析模式',
  modelMapping: '模型映射',
  definedCacheRoutes: '自定义缓存路径',
  auditLogRetentionEnabled: '自动清理过期日志',
  auditLogRetentionDays: '审计日志保留天数',
  proxyUrl: '代理地址',
  proxyUsername: '代理用户名',
  proxyPassword: '代理密码',
  apiKey: '请求 API Key',
  apiKeys: '请求 API Keys',
  countTokensApiUrl: 'Count Tokens 地址',
  countTokensApiKey: 'Count Tokens API Key',
}

const SEGMENT_LABELS: Record<string, string> = {
  requestAdmission: '请求准入',
  imageProcessing: '图片处理',
  bodyConversion: '请求体转换',
  promptInjection: '提示词注入',
  routeFeatures: '路由特性',
  missingMaxTokens: '缺失 max_tokens',
  localRequestCompat: '本地请求兼容',
  requestCapture: '请求采集',
  payloadShaping: '请求体整形',
  promptCacheCreationControl: '提示缓存创建控制',
  reportedUsage: '用量上报',
  cachePolicy: '缓存策略',
  definedCacheRoutes: '自定义缓存路径',
  accounts: '外部账号池',
  weightedCapacity: '加权容量',
  modelMapping: '模型映射',
  enabled: '启用',
  mode: '模式',
  rules: '规则',
  routes: '路径',
  defaults: '默认值',
  timeoutSecs: '超时时间',
  maxAttempts: '最多尝试',
  intervalSecs: '间隔时间',
  leadSecs: '提前量',
  maxConcurrentRequests: '最大并发',
  maxQueuedRequests: '最大排队请求数',
  retry: '重试',
  cache: '缓存',
}

const DETAIL_LABELS: Record<string, string> = {
  kind: '类型',
  changedCount: '变化项',
  truncated: '是否截断',
  changes: '字段变化',
  path: '字段',
  before: '原值',
  after: '新值',
  redacted: '已脱敏',
  count: '数量',
  items: '项目',
  reason: '原因',
  reasonLabel: '原因',
  accountEvent: '账号异动',
  accountSource: '账号来源',
  accountLabel: '账号',
  label: '密钥名称',
  persisted: '已保存到账号',
  region: '区域',
  total: '总数',
  success: '成功',
  skipped: '跳过',
  failed: '失败',
  message: '说明',
}

function isObject(value: unknown): value is JsonObject {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value)
}

function fieldLabel(path: string): string {
  if (FIELD_LABELS[path]) return FIELD_LABELS[path]
  const segments = path.split('.').map((segment) => segment.replace(/\[\d+\]$/, ''))
  const tail = segments[segments.length - 1] ?? path
  if (FIELD_LABELS[tail]) return FIELD_LABELS[tail]
  return segments.map((segment) => SEGMENT_LABELS[segment] ?? segment).join(' / ')
}

function valueText(value: unknown): string {
  if (value === null || value === undefined) return '未设置'
  if (typeof value === 'boolean') return value ? '已开启' : '已关闭'
  if (typeof value === 'string') return value || '空'
  if (typeof value === 'number') return Number.isFinite(value) ? String(value) : '无效数值'
  if (Array.isArray(value)) return `共 ${value.length} 项`
  if (isObject(value)) {
    if (typeof value.count === 'number') {
      const items = Array.isArray(value.items) ? `：${value.items.map(valueText).join('、')}` : ''
      return `共 ${value.count} 项${items}`
    }
    if (value.type === 'object' && typeof value.keys === 'number') return `对象，${value.keys} 个字段`
    return '对象'
  }
  return String(value)
}

function detailObject(detail: unknown): JsonObject {
  return isObject(detail) ? detail : { value: detail }
}

function jsonText(detail: unknown): string {
  if (detail === undefined) return '{}'
  try {
    return JSON.stringify(detail, null, 2)
  } catch {
    return String(detail)
  }
}

function RuntimeChangeRows({ changes }: { changes: RuntimeChange[] }): ReactNode {
  if (changes.length === 0) return <div className="text-sm text-muted-foreground">未检测到配置变化</div>
  return (
    <div className="divide-y rounded-lg border">
      {changes.map((change, index) => {
        const path = typeof change.path === 'string' ? change.path : '未知字段'
        return (
          <div key={`${path}-${index}`} className="grid gap-1 px-3 py-2 text-sm sm:grid-cols-[minmax(12rem,1fr)_minmax(0,1fr)] sm:gap-3">
            <div className="font-medium text-foreground">{fieldLabel(path)}</div>
            <div className="min-w-0 break-words text-muted-foreground">
              {change.redacted ? '值已脱敏' : `${valueText(change.before)} → ${valueText(change.after)}`}
            </div>
          </div>
        )
      })}
    </div>
  )
}

export function AuditChangeSummary({ record }: { record: AdminAuditLogRow }): ReactNode {
  const detail = detailObject(record.detail)
  if (detail.kind === 'runtime_config_change') {
    const count = typeof detail.changedCount === 'number' ? detail.changedCount : 0
    return <span className="text-xs text-muted-foreground">{count > 0 ? `修改 ${count} 项配置` : '未修改配置'}</span>
  }
  const event = accountEventView(record)
  if (event?.reason) return <span className="line-clamp-2 text-xs text-muted-foreground">{event.reason}</span>
  const entries = Object.entries(detail).filter(([key]) => !['kind', 'changes'].includes(key)).slice(0, 2)
  if (entries.length === 0) return <span className="text-xs text-muted-foreground">—</span>
  return <span className="line-clamp-2 text-xs text-muted-foreground">{entries.map(([key, value]) => `${DETAIL_LABELS[key] ?? key}：${valueText(value)}`).join('；')}</span>
}

export function AuditDetailSummary({ record }: { record: AdminAuditLogRow }): ReactNode {
  const detail = detailObject(record.detail)
  const runtimeChanges = detail.kind === 'runtime_config_change' && Array.isArray(detail.changes)
    ? detail.changes.filter(isObject) as RuntimeChange[]
    : null
  const entries = Object.entries(detail).filter(([key]) => !['kind', 'changes'].includes(key))
  return (
    <div className="space-y-3">
      {runtimeChanges ? (
        <>
          <div className="text-xs text-muted-foreground">
            {typeof detail.changedCount === 'number' ? `共 ${detail.changedCount} 项配置变化` : '配置变化'}
            {detail.truncated === true ? '（仅显示前 200 项）' : ''}
          </div>
          <RuntimeChangeRows changes={runtimeChanges} />
        </>
      ) : entries.length > 0 ? (
        <div className="grid gap-2 sm:grid-cols-2">
          {entries.map(([key, value]) => (
            <div key={key} className="min-w-0 rounded-lg border px-3 py-2">
              <div className="text-xs text-muted-foreground">{DETAIL_LABELS[key] ?? key}</div>
              <div className="break-words text-sm">{valueText(value)}</div>
            </div>
          ))}
        </div>
      ) : (
        <div className="text-sm text-muted-foreground">没有附加变化信息</div>
      )}
      <details className="rounded-lg border bg-muted/30 px-3 py-2">
        <summary className="cursor-pointer text-xs font-medium text-muted-foreground">查看原始 JSON</summary>
        <pre className="scrollbar-thin mt-2 max-h-80 overflow-auto whitespace-pre-wrap break-words font-mono text-xs">{jsonText(record.detail)}</pre>
      </details>
    </div>
  )
}
