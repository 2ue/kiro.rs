/**
 * 运行配置字段注册表：每个可编辑字段只在这里定义一次，
 * 配置页、外部池策略页、命令面板搜索都从这里读取。
 */

type Getter = (path: string) => unknown

export interface FieldBase {
  path: string
  label: string
  desc?: string
  /** 默认折叠到"高级"里 */
  advanced?: boolean
  disabledWhen?: (get: Getter) => boolean
}

export type FieldDef =
  | (FieldBase & { kind: 'number'; min?: number; max?: number; step?: number; suffix?: string })
  | (FieldBase & { kind: 'bool' })
  | (FieldBase & { kind: 'select'; options: Array<{ value: string; label: string }> })
  | (FieldBase & { kind: 'list'; placeholder?: string })
  | (FieldBase & { kind: 'codes' })
  | (FieldBase & { kind: 'text'; multiline?: boolean })

export interface FieldGroup {
  id: string
  title: string
  description?: string
  fields: FieldDef[]
}

export interface SettingsSection {
  id: string
  title: string
  description: string
  groups: FieldGroup[]
}

const num = (path: string, label: string, desc: string, opts: Partial<Extract<FieldDef, { kind: 'number' }>> = {}): FieldDef => ({ kind: 'number', path, label, desc, ...opts })
const bool = (path: string, label: string, desc = '', opts: Partial<FieldBase> = {}): FieldDef => ({ kind: 'bool', path, label, desc, ...opts })
const sel = (path: string, label: string, desc: string, options: Array<[string, string]>, opts: Partial<FieldBase> = {}): FieldDef => ({
  kind: 'select',
  path,
  label,
  desc,
  options: options.map(([value, l]) => ({ value, label: l })),
  ...opts,
})
const off = (path: string) => (get: Getter) => !get(path)

const EP = 'externalPools.'
const ROUTE_MODES: Array<[string, string]> = [
  ['allow_all', '全部入口'],
  ['allow_list', '只允许下列入口'],
  ['deny_list', '禁止下列入口'],
]

export const SETTINGS_SECTIONS: SettingsSection[] = [
  {
    id: 'capacity',
    title: '容量与限流',
    description: '全局并发、排队、单账号限速与请求 Key 准入',
    groups: [
      {
        id: 'global',
        title: '全局容量',
        fields: [
          num('dispatchGlobalMaxConcurrentRequests', '全局最大并发', '整个服务同一时间最多处理多少个请求；0 表示不限制。', { min: 0, suffix: '并发' }),
          num('dispatchMaxQueuedRequests', '最大排队请求数', '账号忙不过来时最多让多少个请求排队；0 表示不限制。', { min: 0, suffix: '请求' }),
          num('credentialDispatchMaxWaitSecs', '单请求最长排队等待', '一个请求最多等账号空闲多久；0 表示不限制。', { min: 0, suffix: '秒' }),
          num('credentialInFlightLeaseMaxSecs', '异常并发自动回收', '请求长时间没有结束时自动释放占用，避免账号并发数被卡住；0 表示关闭。', { min: 0, suffix: '秒', advanced: true }),
        ],
      },
      {
        id: 'per-account',
        title: '单账号默认值',
        description: '账号未单独设置时使用',
        fields: [
          num('credentialMaxConcurrentRequests', '单账号最大并发', '每个账号同一时间最多处理多少个请求；0 表示不限制。', { min: 0, suffix: '并发' }),
          num('credentialRpm', '单账号每分钟请求上限', '每个账号一分钟最多接多少个请求；0 表示不做本地限速。', { min: 0, suffix: 'RPM' }),
          num('credentialInfoRefreshConcurrency', '额度查询上游并发', '强制查询额度时同时向 Kiro 请求的数量；建议 1 到 3，避免触发上游限流。', { min: 1, max: 16, suffix: '并发', advanced: true }),
        ],
      },
      {
        id: 'admission',
        title: '请求 Key 准入（每实例 · 每 Key）',
        description: '多实例部署时各实例分别计数，总量近似为实例数倍',
        fields: [
          num('requestAdmission.rpm', '每 Key RPM', '每个实例分别限制每个已认证请求 API Key；0 表示关闭。', { min: 0, max: 1_000_000, suffix: 'RPM' }),
          num('requestAdmission.maxConcurrentRequests', '每 Key 并发', '每个实例分别统计同一请求 Key 的并发；0 表示关闭。', { min: 0, max: 10_000, suffix: '并发' }),
          num('requestAdmission.maxQueuedRequests', '每 Key 队列', '同一 Key 并发占满时最多等待的请求数；0 表示立即返回 429。', { min: 0, max: 100_000, suffix: '请求', advanced: true }),
          num('requestAdmission.queueTimeoutMs', '每 Key 等待', '同一 Key 等待并发名额的最长时间；0 表示立即返回 429。', { min: 0, max: 300_000, suffix: '毫秒', advanced: true }),
        ],
      },
      {
        id: 'weighted',
        title: '按 token 重量计算容量',
        fields: [
          bool('weightedCapacity.enabled', '启用加权容量', '开启后长上下文请求按输入 token 分档占用多个并发/RPM 单位；分档在"高级 JSON"中编辑。'),
          num('weightedCapacity.maxUnitsPerRequest', '单请求最大容量单位', '限制超长上下文最多占用多少本地并发/RPM 单位。', { min: 1, max: 64, suffix: '单位', disabledWhen: off('weightedCapacity.enabled') }),
        ],
      },
    ],
  },
  {
    id: 'scheduling',
    title: '调度与预热',
    description: '账号选择权重、候选数量与新账号预热',
    groups: [
      {
        id: 'weights',
        title: '账号选择权重',
        description: '仅在"健康均衡 / 低负载优先"模式下生效',
        fields: [
          num('schedulerTopK', '候选账号数量', '每次从前几个合适账号里挑选；越大越分散。', { min: 1, max: 100, suffix: '个' }),
          num('schedulerErrorWeight', '近期错误权重', '越大，最近出错多的账号越不容易被选中。', { min: 0, step: 1, suffix: '权重' }),
          num('schedulerLoadWeight', '当前负载权重', '越大，当前更空闲的账号越容易被选中。', { min: 0, step: 1, suffix: '权重' }),
          num('schedulerLatencyWeight', '响应耗时权重', '越大，响应慢的账号越少被选中；通常设较小值。', { min: 0, step: 0.001, suffix: '权重' }),
          num('schedulerPriorityWeight', '优先级权重', '越大，账号优先级越能影响选择结果。', { min: 0, step: 0.1, suffix: '权重', advanced: true }),
          num('schedulerProbationWeight', '恢复期降权', '账号刚从错误中恢复时，越大越少被选中。', { min: 0, step: 1, suffix: '权重', advanced: true }),
          num('schedulerSelectionPressureWeight', '短时集中降权', '短时间内同一账号被选太多时，越大越快分散。', { min: 0, step: 1, suffix: '权重', advanced: true }),
          num('schedulerTotalSelectionWeight', '长期使用次数权重', '越大，历史调度次数多的账号越少被选中。', { min: 0, step: 0.001, suffix: '权重', advanced: true }),
          num('schedulerErrorEwmaAlpha', '近期错误敏感度', '越高，刚发生的错误越快影响账号选择。', { min: 0.01, max: 1, step: 0.01, suffix: '系数', advanced: true }),
        ],
      },
      {
        id: 'warmup',
        title: '新账号预热',
        fields: [
          num('credentialWarmupRequests', '预热请求数', '新账号先以低比例参与调度的请求数；0 表示不预热。', { min: 0, suffix: '次' }),
          num('credentialWarmupSelectionPercent', '单个预热账号参与比例', '每次调度时预热账号被选中的概率上限。', { min: 0, max: 100, suffix: '%' }),
          num('credentialWarmupMaxSelectionPercent', '预热账号总占比上限', '所有预热中账号合计流量不超过此比例。', { min: 0, max: 100, suffix: '%' }),
        ],
      },
      {
        id: 'diag',
        title: '调度诊断',
        fields: [
          bool('selectionFailureRecordEnabled', '记录失败样本', '关闭后只保留失败原因统计，不记录具体账号样本。'),
          num('selectionFailureSampleLimit', '失败诊断样本数', '调度失败时最多记录多少个账号样本；0 表示不记录。', { min: 0, max: 1000, suffix: '个', disabledWhen: off('selectionFailureRecordEnabled') }),
        ],
      },
    ],
  },
  {
    id: 'cooldown',
    title: '冷却与恢复',
    description: '不同错误类型的暂停时间、退避与恢复观察',
    groups: [
      {
        id: 'by-error',
        title: '按错误类型的初始冷却',
        description: '均不能超过最大冷却时长',
        fields: [
          num('credentialRateLimitCooldownSecs', '429 限流', '收到上游限流后的初始冷却。', { min: 1, suffix: '秒' }),
          num('credentialServerErrorCooldownSecs', '5xx 服务繁忙', '收到上游服务端错误后的初始冷却。', { min: 1, suffix: '秒' }),
          num('credentialNetworkErrorCooldownSecs', '网络错误', '连接超时、DNS 失败等网络层错误后的冷却。', { min: 1, suffix: '秒' }),
          num('credentialStreamErrorCooldownSecs', '流式中断', '流式响应中途中断后的冷却。', { min: 1, suffix: '秒' }),
          num('credentialProtocolErrorCooldownSecs', '格式异常', '响应格式/协议异常后的冷却。', { min: 1, suffix: '秒' }),
          num('credentialAuthErrorCooldownSecs', '401/403 授权异常', '授权失败后的冷却。', { min: 1, suffix: '秒' }),
          num('credentialTransientCooldownSecs', '其他临时错误', '未归类临时错误的默认暂停时间。', { min: 1, suffix: '秒' }),
        ],
      },
      {
        id: 'backoff',
        title: '退避与恢复',
        fields: [
          num('credentialMaxCooldownSecs', '最大冷却时长', '连续出错时最多暂停多久。', { min: 1, suffix: '秒' }),
          num('credentialCooldownBackoffMultiplier', '退避倍率', '连续出错时冷却时间逐次乘以该倍数。', { min: 1, max: 10, step: 0.1, suffix: '倍' }),
          num('credentialCooldownJitterPercent', '恢复时间错开比例', '给恢复时间加随机错开，避免多个账号同时恢复又同时出错。', { min: 0, max: 100, suffix: '%' }),
          num('credentialProbationSecs', '恢复观察时间', '账号刚恢复时先少量使用，稳定后恢复正常调度。', { min: 0, suffix: '秒' }),
        ],
      },
    ],
  },
  {
    id: 'retry',
    title: '重试与超时',
    description: '上游超时、首输出保护、换号重试与端点轮换',
    groups: [
      {
        id: 'timeout',
        title: '超时',
        fields: [
          num('kiroUpstreamResponseTimeoutSecs', '开始响应等待时间', '发给上游后多久还没开始返回就认为超时；0 表示使用默认值。', { min: 0, suffix: '秒' }),
          num('kiroUpstreamStreamIdleTimeoutSecs', '流式静默超时', '流式响应长时间没有新内容时结束请求。', { min: 0, suffix: '秒' }),
          num('streamFirstOutputTimeoutSecs', '首输出等待上限', '响应头后最多等待多少秒出现首个内容；0 表示关闭。建议不低于 300 秒。', { min: 0, max: 3600, suffix: '秒' }),
          num('streamKeepaliveIntervalSecs', '流式保活间隔', '上游暂时没有内容时，每隔多久向客户端发一次保活。', { min: 1, max: 300, suffix: '秒' }),
          num('streamPreOutputHoldSecs', '首输出前静默保留', '首个输出前最多多少秒不向客户端写入，以便失败时无感换号；0 表示一直保留。', { min: 0, max: 60, suffix: '秒' }),
        ],
      },
      {
        id: 'attempts',
        title: '尝试次数上限',
        fields: [
          num('inferenceUpstreamMaxAttempts', '单请求推理发送硬上限', '本地换号、首输出前重试、外部池故障转移和本地救援共享；与账号数量无关。', { min: 1, max: 10, suffix: '次' }),
          num('credentialRetryMaxAttempts', '本地 provider 尝试上限', '单次本地调用最多尝试多少个凭据；0 表示默认 3。', { min: 0, suffix: '次' }),
          num('auxiliaryUpstreamMaxAttempts', '单请求辅助发送硬上限', 'Token 刷新与 Profile 探测共享，不计入推理次数。', { min: 1, max: 10, suffix: '次', advanced: true }),
        ],
      },
      {
        id: 'stream-retry',
        title: '首输出前流式换号',
        description: '只在还没向客户端发送任何 SSE 事件时生效',
        fields: [
          bool('kiroUpstreamStreamRetryEnabled', '启用首输出前换号', '已输出 message_start、文本或工具调用后不会重试。'),
          num('kiroUpstreamStreamRetryMaxAttempts', '最多尝试', '包含第一次调用。', { min: 1, max: 100, suffix: '次', disabledWhen: off('kiroUpstreamStreamRetryEnabled') }),
          bool('kiroUpstreamStreamRetryOnIdleTimeout', '静默超时可换号', '首输出前长时间无内容时允许换号。', { disabledWhen: off('kiroUpstreamStreamRetryEnabled') }),
          bool('kiroUpstreamStreamRetryOnReadError', '读取错误可换号', '首输出前连接中断、流读取失败时允许换号。', { disabledWhen: off('kiroUpstreamStreamRetryEnabled') }),
          bool('kiroUpstreamStreamRetryOnStatusError', '状态错误可换号', '首输出前收到 2xx JSON 错误体时允许换号。', { disabledWhen: off('kiroUpstreamStreamRetryEnabled') }),
        ],
      },
      {
        id: 'logic',
        title: '其他重试策略',
        fields: [
          bool('credentialPromptLogicRetryEnabled', '提示逻辑错误换号', '上游返回提示/工具协议 400 时换未尝试的账号重试。'),
          num('credentialPromptLogicRetryMaxAttempts', '提示逻辑最多换号', '0 表示默认 1 次。', { min: 0, suffix: '次', disabledWhen: off('credentialPromptLogicRetryEnabled') }),
          bool('kiroUpstreamRegionRotationEnabled', '上游端点轮换', '瞬态错误时在 us-east-1 / eu-central-1 之间轮换重试。'),
          bool('localBerserkModeEnabled', '狂暴模式（429 暴力轮换）', '普通 429 时把所有账号与端点轮一遍再放弃；会显著放大上游请求量。', { advanced: true }),
          num('localBerserkMaxRounds', '狂暴模式最多轮数', '一轮 = 所有账号 × 所有端点各试一遍。', { min: 1, max: 10, suffix: '轮', advanced: true, disabledWhen: off('localBerserkModeEnabled') }),
          num('localBerserkRoundDelayMs', '狂暴模式轮次间隔', '进入下一轮前的退避时间。', { min: 0, max: 60_000, suffix: '毫秒', advanced: true, disabledWhen: off('localBerserkModeEnabled') }),
        ],
      },
    ],
  },
  {
    id: 'token',
    title: 'Token 刷新',
    description: '刷新速率、后台主动刷新与辅助请求并发',
    groups: [
      {
        id: 'refresh',
        title: '刷新速率',
        fields: [
          num('tokenRefreshMaxRpm', 'Token 刷新 RPM 上限', 'Redis 可用时为跨实例共享上限，否则为单进程上限。', { min: 1, max: 6000, suffix: 'RPM' }),
          num('tokenRefreshBurst', 'Token 刷新突发容量', '允许立即发送的刷新数量，之后按 RPM 补充。', { min: 1, max: 256, suffix: '次' }),
          num('auxiliaryUpstreamMaxConcurrentRequests', '单实例辅助并发上限', '同时进行的 Token 刷新、Profile 探测和模型目录请求；饱和时立即拒绝。', { min: 1, max: 256, suffix: '路' }),
        ],
      },
      {
        id: 'background',
        title: '后台主动刷新',
        fields: [
          bool('tokenRefreshBackgroundEnabled', '闲置账号后台刷新', '定期扫描长期未调用的 OAuth 账号，在过期前主动轮换 Token。'),
          num('tokenRefreshBackgroundIntervalSecs', '扫描间隔', '后台扫描账号的间隔。', { min: 10, max: 3600, suffix: '秒', disabledWhen: off('tokenRefreshBackgroundEnabled') }),
          num('tokenRefreshBackgroundLeadSecs', '刷新提前量', 'Token 剩余多少秒时进入主动刷新窗口。', { min: 60, max: 3600, suffix: '秒', disabledWhen: off('tokenRefreshBackgroundEnabled') }),
        ],
      },
    ],
  },
  {
    id: 'payload',
    title: '请求体处理',
    description: '大小保护、历史清理、当前请求兜底、图片与协议转换',
    groups: [
      {
        id: 'guard',
        title: '大小保护',
        fields: [
          bool('payloadGuardEnabled', '启用大小保护', '发送前检查请求大小，超过限制时按规则清理内容。'),
          sel('payloadGuardMode', '处理时机', '发送前处理更稳；失败后处理保留更多上下文。', [
            ['preemptive', '发送前先处理'],
            ['on_too_long', '失败后再处理并重试'],
          ], { disabledWhen: off('payloadGuardEnabled') }),
          num('payloadGuardMaxBytes', '请求大小阈值', '超过才触发处理（1048576 = 1 MB）；0 或不小于 65536。', { min: 0, suffix: '字节', disabledWhen: off('payloadGuardEnabled') }),
          num('payloadGuardKiroMaxWeight', 'Kiro 加权阈值', 'ASCII=1、非 ASCII=8 加权计算；默认 1300000。', { min: 1, suffix: 'weight', advanced: true, disabledWhen: off('payloadGuardEnabled') }),
          num('payloadGuardSafetyMarginBytes', '安全余量', '处理目标比阈值小出的缓冲，避免裁剪后仍超限。', { min: 0, suffix: '字节', advanced: true, disabledWhen: off('payloadGuardEnabled') }),
          bool('payloadGuardTrimHistory', '优先裁剪旧历史', '内容太长时优先缩短较早的对话历史。', { disabledWhen: off('payloadGuardEnabled') }),
          bool('payloadGuardExternalEnabled', '外部池也应用', '请求转发到外部池前执行同样的大小保护。', { disabledWhen: off('payloadGuardEnabled') }),
          bool('compressionEnabled', '启用请求压缩', '发送前尽量去掉冗余内容。', { advanced: true }),
          bool('whitespaceCompression', '仅压缩空白字符', '只处理多余空格和换行，风险较低。', { advanced: true, disabledWhen: off('compressionEnabled') }),
        ],
      },
      {
        id: 'history',
        title: '历史内容清理',
        description: '需要先启用大小保护',
        fields: [
          bool('payloadShaping.enabled', '启用内容清理', '请求太大时优先清理较早的历史消息。', { disabledWhen: off('payloadGuardEnabled') }),
          bool('payloadShaping.truncateHistoricalToolResults', '截短历史工具结果', '只保留开头和结尾。', { disabledWhen: off('payloadShaping.enabled') }),
          num('payloadShaping.historicalToolResultMaxChars', '历史工具结果保留字符', '', { min: 0, suffix: '字符', disabledWhen: off('payloadShaping.enabled') }),
          num('payloadShaping.historicalToolResultHeadLines', '保留头部行数', '', { min: 0, suffix: '行', advanced: true, disabledWhen: off('payloadShaping.enabled') }),
          num('payloadShaping.historicalToolResultTailLines', '保留尾部行数', '', { min: 0, suffix: '行', advanced: true, disabledWhen: off('payloadShaping.enabled') }),
          bool('payloadShaping.discardHistoricalThinking', '移除历史思考内容', '避免旧 thinking 反复占用请求体积。', { disabledWhen: off('payloadShaping.enabled') }),
          bool('payloadShaping.compressToolDefinitions', '压缩工具说明', '工具说明太大时精简描述和参数说明。', { disabledWhen: off('payloadShaping.enabled') }),
          num('payloadShaping.toolDefinitionsBudgetBytes', '工具说明大小上限', '所有工具说明合计尽量控制在该大小以内。', { min: 0, suffix: '字节', advanced: true, disabledWhen: off('payloadShaping.enabled') }),
          num('payloadShaping.toolDescriptionMaxChars', '单工具描述上限', '', { min: 0, suffix: '字符', advanced: true, disabledWhen: off('payloadShaping.enabled') }),
          num('payloadShaping.toolSchemaAnnotationMaxChars', '工具参数说明上限', '', { min: 0, suffix: '字符', advanced: true, disabledWhen: off('payloadShaping.enabled') }),
          bool('payloadShaping.webFetchTrimEnabled', '清理网页抓取历史', '历史里的网页正文太长时截短。', { advanced: true, disabledWhen: off('payloadShaping.enabled') }),
          num('payloadShaping.webFetchBodyMaxChars', '网页正文保留字符', '', { min: 0, suffix: '字符', advanced: true, disabledWhen: off('payloadShaping.enabled') }),
        ],
      },
      {
        id: 'current',
        title: '当前请求兜底',
        description: '历史清理后仍超限时才处理本轮内容',
        fields: [
          bool('payloadShaping.fitCurrentPayloadToBudget', '自动压缩当前内容', '', { disabledWhen: off('payloadShaping.enabled') }),
          bool('payloadShaping.truncateCurrentToolResults', '截短当前工具结果', '', { disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget') }),
          num('payloadShaping.currentToolResultMaxChars', '当前工具结果保留字符', '', { min: 0, suffix: '字符', disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget') }),
          bool('payloadShaping.truncateCurrentUserContent', '截短当前用户文本', '可能损失本轮请求细节。', { advanced: true, disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget') }),
          num('payloadShaping.currentUserContentMaxChars', '当前用户文本保留字符', '', { min: 0, suffix: '字符', advanced: true, disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget') }),
          bool('payloadShaping.truncateCurrentDocuments', '截短当前文档', '', { advanced: true, disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget') }),
          num('payloadShaping.currentDocumentMaxChars', '当前文档保留字符', '', { min: 0, suffix: '字符', advanced: true, disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget') }),
          bool('payloadShaping.truncateCurrentImages', '移除超限图片', '', { advanced: true, disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget') }),
          num('payloadShaping.currentImagesMaxBytes', '当前图片保留大小', '', { min: 0, suffix: '字节', advanced: true, disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget') }),
          sel('payloadShaping.oversizedImageHandling', '超大图片处理', '', [
            ['drop-with-placeholder', '替换为占位说明'],
            ['reject', '直接拒绝'],
          ], { advanced: true }),
        ],
      },
      {
        id: 'image',
        title: '图片与文件',
        fields: [
          sel('imageProcessing.mode', '图片处理模式', 'Safe 会做兼容修复；Light 轻量透传。', [
            ['safe', 'Safe：兼容修复'],
            ['light', 'Light：轻量透传'],
          ]),
          bool('imageProcessing.safeMaterializeFileSources', '展开本地文件 source', '', { disabledWhen: (g) => g('imageProcessing.mode') !== 'safe' }),
          bool('imageProcessing.safeDownloadRemoteSources', '下载远程图片和文档', '', { disabledWhen: (g) => g('imageProcessing.mode') !== 'safe' }),
          bool('imageProcessing.safeNormalizeBase64MediaTypes', '修正 base64 图片类型', '', { disabledWhen: (g) => g('imageProcessing.mode') !== 'safe' }),
        ],
      },
      {
        id: 'conversion',
        title: '协议转换',
        fields: [
          bool('bodyConversion.toolSchemaNormalization', '工具 schema 规范化', '清理 OpenAPI、Zod、MCP 工具 schema 中上游容易拒绝的字段。'),
          bool('bodyConversion.toolNameMapping', '工具名映射', '清洗或缩短不符合 Kiro 约束的工具名并反向映射。'),
          sel('bodyConversion.toolSchemaKeyMapping', '非法 schema key', '', [
            ['sanitize', '清洗并反向映射'],
            ['reject', '明确报错'],
            ['disabled', '不处理'],
          ], { advanced: true }),
          { kind: 'text', path: 'bodyConversion.toolSchemaKeyValidationRegex', label: 'schema key 校验正则', advanced: true },
          bool('bodyConversion.toolChoiceSteering', '结构化 tool_choice', '按 none=0、any=N、named=1 过滤工具。'),
          bool('bodyConversion.thinkingPromptControls', 'thinking 转换能力', '允许本地 Kiro 生成原生 thinking 字段。'),
          bool('bodyConversion.nativeReasoningFields', '原生 reasoning 字段', '对支持的模型上报 additionalModelRequestFields。'),
          bool('bodyConversion.chunkedToolPolicy', '分块工具策略', 'Write/Edit 分块协议能力。', { advanced: true }),
          bool('bodyConversion.toolPairingRepair', '工具配对修复', '清理不配对、重复或孤立的 tool_use / tool_result。'),
          bool('bodyConversion.historyPlaceholderTools', '历史工具占位', '历史里出现但当前缺失的工具补充占位定义。', { advanced: true }),
        ],
      },
    ],
  },
  {
    id: 'steering',
    title: '提示词引导',
    description: '注入语言约束、任务质量与兼容提示',
    groups: [
      {
        id: 'scope',
        title: '作用范围',
        fields: [
          bool('promptSteering.enabled', '启用提示词引导', '总开关；关闭后不注入任何引导提示。'),
          sel('promptSteering.scope', '作用范围', '', [
            ['route_rules', '按路径规则'],
            ['claude_code_profile', 'Claude Code / Debug profile'],
            ['all_routes', '全部 messages 路由'],
          ], { disabledWhen: off('promptSteering.enabled') }),
          sel('promptSteering.routeMode', '路径规则模式', '', [
            ['allow_list', '只对规则命中的入口生效'],
            ['deny_list', '对规则外的入口生效'],
            ['allow_all', '全部入口生效'],
          ], { disabledWhen: (g) => !g('promptSteering.enabled') || g('promptSteering.scope') !== 'route_rules' }),
          { kind: 'list', path: 'promptSteering.routeRules', label: '路径规则', desc: '每行一个路径前缀', disabledWhen: (g) => !g('promptSteering.enabled') || g('promptSteering.scope') !== 'route_rules' },
          bool('promptSteering.applyToExternalPool', '应用到外部池', '外部池 raw passthrough 时也按同一规则处理。', { disabledWhen: off('promptSteering.enabled') }),
          bool('promptSteering.applyToCountTokens', 'count_tokens 同步计入', '避免估算低于真实请求。', { disabledWhen: off('promptSteering.enabled') }),
        ],
      },
      {
        id: 'blocks',
        title: '提示内容',
        fields: [
          bool('promptSteering.languageConstraint.enabled', '语言约束', '减少中英混杂、语言串台。', { disabledWhen: off('promptSteering.enabled') }),
          { kind: 'text', multiline: true, path: 'promptSteering.languageConstraint.prompt', label: '语言约束提示词', advanced: true, disabledWhen: off('promptSteering.languageConstraint.enabled') },
          bool('promptSteering.taskQuality.enabled', '任务质量', '强调最新用户消息与任务边界。', { disabledWhen: off('promptSteering.enabled') }),
          { kind: 'text', multiline: true, path: 'promptSteering.taskQuality.prompt', label: '任务质量提示词', advanced: true, disabledWhen: off('promptSteering.taskQuality.enabled') },
          bool('promptSteering.toolChoice.enabled', 'tool_choice 引导', '本地 Kiro 的 tool_choice 兼容提示。', { disabledWhen: off('promptSteering.enabled') }),
          bool('promptSteering.thinking.enabled', 'thinking 提示控制', 'synthetic thinking 兼容提示。', { disabledWhen: off('promptSteering.enabled') }),
          bool('promptSteering.chunkedWrite.enabled', '分块写入提示', 'Write/Edit 分块兼容提示。', { disabledWhen: off('promptSteering.enabled') }),
          bool('promptSteering.chunkedWrite.systemPromptEnabled', '分块 system 提示', '', { advanced: true, disabledWhen: off('promptSteering.chunkedWrite.enabled') }),
          bool('promptSteering.chunkedWrite.toolDescriptionEnabled', '分块工具描述', '', { advanced: true, disabledWhen: off('promptSteering.chunkedWrite.enabled') }),
          bool('promptSteering.custom.enabled', '自定义追加提示词', '', { disabledWhen: off('promptSteering.enabled') }),
          { kind: 'text', multiline: true, path: 'promptSteering.custom.prompt', label: '自定义提示词', disabledWhen: off('promptSteering.custom.enabled') },
        ],
      },
    ],
  },
  {
    id: 'cache',
    title: 'Prompt 缓存',
    description: '本地模拟缓存全局参数；按路径的策略在"高级 JSON"中编辑',
    groups: [
      {
        id: 'kiro-cache-point',
        title: 'Kiro 原生缓存点',
        fields: [
          bool('kiroCachePointEnabled', '启用 Kiro cachePoint', '向 Kiro 上游发送缓存断点。'),
          bool('kiroCachePointToolsOnly', '仅在工具定义处打点', '', { disabledWhen: off('kiroCachePointEnabled') }),
          bool('kiroCachePointRecordPlan', '记录缓存点计划', '把计划写入请求诊断数据。', { advanced: true, disabledWhen: off('kiroCachePointEnabled') }),
        ],
      },
      {
        id: 'simulation',
        title: '本地模拟缓存（全局默认）',
        description: '影响对外上报的缓存用量；不改变上游实际计费',
        fields: [
          num('promptCacheTargetReadRatio', '目标缓存读取比例', '希望输入里大约多少比例显示为缓存读取；最高 0.99。', { min: 0, max: 0.99, step: 0.01, suffix: '比例' }),
          num('promptCacheTokenScale', '输入放大倍数', '计算展示用量前先把输入放大；1 表示不放大。', { min: 1, max: 3, step: 0.1, suffix: '倍' }),
          num('promptCacheMaxSimulatedInputTokens', '模拟输入上限', '0 表示不设上限。', { min: 0, suffix: 'tokens' }),
          num('promptCacheScaleMinInputTokens', '放大生效门槛', '原始输入低于该值时不放大。', { min: 0, suffix: 'tokens', advanced: true }),
          num('promptCacheCapJitterMinTokens', '触顶扣减下限', '接近上限时至少少显示这么多。', { min: 0, suffix: 'tokens', advanced: true }),
          num('promptCacheCapJitterMaxTokens', '触顶扣减上限', '必须大于等于扣减下限。', { min: 0, suffix: 'tokens', advanced: true }),
        ],
      },
      {
        id: 'bounds',
        title: '缓存条目容量',
        fields: [
          num('promptCacheMaxEntriesPerAccount', '每账号最大条目', '', { min: 0, suffix: '条' }),
          num('promptCacheMaxEntriesGlobal', '全局最大条目', '', { min: 0, suffix: '条' }),
          num('promptCacheEntryTtlSecs', '条目有效期', '', { min: 1, suffix: '秒' }),
          num('promptCacheEstimatedBytesLimit', '估算内存上限', '0 表示不限制。', { min: 0, suffix: '字节', advanced: true }),
        ],
      },
    ],
  },
  {
    id: 'compat',
    title: '协议兼容',
    description: '兼容模式、模型解析、thinking 与 max_tokens',
    groups: [
      {
        id: 'mode',
        title: '兼容模式',
        fields: [
          sel('compatProfile', '兼容模式', '', [
            ['claude-code', 'Claude Code 兼容'],
            ['anthropic-strict', 'Anthropic 严格模式'],
            ['debug', '调试模式'],
          ]),
          sel('kiroAgentModeStrategy', 'Kiro 工作模式', '', [
            ['vibe', 'Vibe'],
            ['spec', 'Spec'],
            ['auto', '自动'],
          ]),
          sel('thinkingTriggerMode', '思考触发', '', [
            ['real_request', '按请求触发'],
            ['always', '总是触发'],
          ]),
          bool('extractThinking', '整理思考内容', '把响应里的思考内容单独整理，便于客户端展示。'),
          bool('exposeProxyWarnings', '显示处理告警', '把代理处理中的提醒返回给客户端。', { advanced: true }),
        ],
      },
      {
        id: 'model',
        title: '模型解析',
        fields: [
          sel('modelResolutionMode', '模型解析策略', '', [
            ['compatible', '默认兼容解析'],
            ['alias_only', '仅精确与显式别名'],
            ['exact_only', '仅完整模型名'],
          ]),
          bool('modelMapping.enabled', '启用模型映射', '映射规则在"高级 JSON"中编辑。'),
          bool('modelMapping.autoGenerateRules', '自动生成映射规则', '', { disabledWhen: off('modelMapping.enabled') }),
        ],
      },
      {
        id: 'max-tokens',
        title: '缺失 max_tokens',
        fields: [
          sel('missingMaxTokens.policy', '处理方式', '', [
            ['default_value', '自动补全'],
            ['reject', '直接拒绝'],
          ]),
          num('missingMaxTokens.defaultValue', '补充值', '自动补全时写入的输出上限。', { min: 1, max: 200_000, suffix: 'tokens', disabledWhen: (g) => g('missingMaxTokens.policy') === 'reject' }),
        ],
      },
      {
        id: 'stats',
        title: '统计口径',
        fields: [num('highCacheThreshold', '高缓存命中阈值', '缓存读取达到多少 token 视为高缓存请求（仅影响统计）。', { min: 0, suffix: 'tokens' })],
      },
    ],
  },
]

/** 外部池路由策略：唯一编辑入口在外部池页 → 策略 */
export const POOL_POLICY_GROUPS: FieldGroup[] = [
  {
    id: 'switch',
    title: '启用与直连',
    fields: [
      bool(`${EP}externalPoolsEnabled`, '启用外部池', '允许请求在本地不可调度或策略命中时进入外部池。'),
      bool(`${EP}externalDirectPolicyEnabled`, '外部直连策略', '命中直连模型或路径规则的请求直接走外部池。', { disabledWhen: off(`${EP}externalPoolsEnabled`) }),
      { kind: 'list', path: `${EP}directExternalModelRules`, label: '直连模型规则', desc: '每行一个模型名或通配', disabledWhen: (g) => !g(`${EP}externalPoolsEnabled`) || !g(`${EP}externalDirectPolicyEnabled`) },
      { kind: 'list', path: `${EP}directExternalPathRules`, label: '直连路径规则', desc: '每行一个路径前缀', disabledWhen: (g) => !g(`${EP}externalPoolsEnabled`) || !g(`${EP}externalDirectPolicyEnabled`) },
      bool(`${EP}directExternalOnLocalMaintenance`, '本地维护时直连', '本地保护熔断打开时请求直接走外部池。', { advanced: true }),
    ],
  },
  {
    id: 'routing',
    title: '入口路由',
    fields: [
      sel(`${EP}localPoolRouteMode`, '本地账号路由', '决定哪些入口允许进入本地账号。', ROUTE_MODES),
      { kind: 'list', path: `${EP}localPoolRouteRules`, label: '本地路由规则', desc: '每行一个路径前缀', disabledWhen: (g) => g(`${EP}localPoolRouteMode`) === 'allow_all' },
      sel(`${EP}externalPoolRouteMode`, '外部池路由', '决定哪些入口允许进入外部池。', ROUTE_MODES),
      { kind: 'list', path: `${EP}externalPoolRouteRules`, label: '外部池路由规则', desc: '每行一个路径前缀', disabledWhen: (g) => g(`${EP}externalPoolRouteMode`) === 'allow_all' },
    ],
  },
  {
    id: 'fallback',
    title: '何时兜底到外部池',
    fields: [
      bool(`${EP}localPoolPreflightEnabled`, '本地不可调度时预检外部池', '本地容量不足、无可用账号或调度 Redis 退化时优先尝试外部池。'),
      bool(`${EP}fallbackOnLocalCapacityExhausted`, '本地容量不足'),
      bool(`${EP}fallbackOnNoAvailableCredentials`, '没有可用账号'),
      bool(`${EP}fallbackOnLocalTransientExhausted`, '本地临时错误过多'),
      bool(`${EP}fallbackOnUnsupportedModel`, '本地不支持该模型'),
      bool(`${EP}fallbackOnSchedulerRedisDegraded`, '调度 Redis 降级'),
    ],
  },
  {
    id: 'circuit',
    title: '本地保护熔断',
    fields: [
      bool(`${EP}localPoolCircuitEnabled`, '启用本地保护统计', '短时间内多个账号连续失败时暂停本地调度。'),
      num(`${EP}localPoolCircuitWindowSecs`, '统计窗口', '', { min: 1, suffix: '秒', disabledWhen: off(`${EP}localPoolCircuitEnabled`) }),
      num(`${EP}localPoolCircuitOpenAfterFailures`, '失败阈值', '', { min: 1, suffix: '次', disabledWhen: off(`${EP}localPoolCircuitEnabled`) }),
      num(`${EP}localPoolCircuitRequireDistinctCredentials`, '至少涉及账号数', '', { min: 0, suffix: '个', disabledWhen: off(`${EP}localPoolCircuitEnabled`) }),
      num(`${EP}localPoolCircuitOpenSecs`, '暂停时长', '', { min: 1, suffix: '秒', disabledWhen: off(`${EP}localPoolCircuitEnabled`) }),
    ],
  },
  {
    id: 'rescue',
    title: '外部池失败后回本地',
    fields: [
      bool(`${EP}externalPoolLocalRescueEnabled`, '启用本地救援', '外部池作为兜底失败后，最后再尝试一次本地账号。'),
      bool(`${EP}externalPoolLocalRescueOnRateLimit`, '429 时回本地', '', { disabledWhen: off(`${EP}externalPoolLocalRescueEnabled`) }),
      bool(`${EP}externalPoolLocalRescueOnTimeout`, '超时时回本地', '', { disabledWhen: off(`${EP}externalPoolLocalRescueEnabled`) }),
      bool(`${EP}externalPoolLocalRescueOnCapacity`, '容量不足时回本地', '', { disabledWhen: off(`${EP}externalPoolLocalRescueEnabled`) }),
      num(`${EP}externalPoolLocalRescueMaxWaitSecs`, '最多等待本地空闲', '', { min: 0, max: 300, suffix: '秒', disabledWhen: off(`${EP}externalPoolLocalRescueEnabled`) }),
    ],
  },
  {
    id: 'capacity',
    title: '容量与重试',
    fields: [
      sel(`${EP}externalPoolCapacityMode`, '满并发处理', '', [
        ['fail_fast', '立即失败'],
        ['wait', '排队等待'],
      ]),
      num(`${EP}externalPoolGlobalMaxConcurrentRequests`, '全局最大并发', '所有外部池合计；0 表示不限制。', { min: 0, max: 100_000, suffix: '并发' }),
      num(`${EP}externalPoolMaxQueuedRequests`, '最大排队请求', '0 表示不排队。', { min: 0, max: 100_000, suffix: '请求' }),
      num(`${EP}externalPoolDispatchMaxWaitSecs`, '最长排队等待', '', { min: 1, max: 86_400, suffix: '秒' }),
      num(`${EP}externalPoolRetryMaxAttempts`, '跨池最多故障转移', '0 表示不重试其它外部池。', { min: 0, max: 10_000, suffix: '次' }),
      { kind: 'codes', path: `${EP}externalPoolRetryStatusCodes`, label: '跨池重试状态码', desc: '逗号分隔，例如 429, 500, 502' },
      bool(`${EP}externalPoolRetryOnNetworkError`, '网络错误跨池重试'),
      bool(`${EP}externalPoolRetryOnProtocolError`, '协议错误跨池重试'),
      num(`${EP}externalPoolSamePoolRetryCount`, '同池重试次数', '命中同池状态码时先在同一池重试。', { min: 0, max: 10, suffix: '次' }),
      { kind: 'codes', path: `${EP}externalPoolSamePoolRetryStatusCodes`, label: '同池重试状态码', disabledWhen: (g) => !g(`${EP}externalPoolSamePoolRetryCount`) },
      num(`${EP}externalPoolSamePoolRetryDelayMs`, '同池重试间隔', '', { min: 0, max: 60_000, suffix: '毫秒', disabledWhen: (g) => !g(`${EP}externalPoolSamePoolRetryCount`) }),
      num(`${EP}externalPoolTransientFailurePriorityPenalty`, '失败池临时降权', '瞬态失败后在窗口内临时增加有效优先级；0 表示关闭。', { min: 0, max: 10_000, suffix: '优先级', advanced: true }),
      num(`${EP}externalPoolTransientFailureCooldownThreshold`, '连续失败计数（兼容）', '保留旧字段，不再触发池级冷却。', { min: 0, max: 1000, suffix: '次', advanced: true }),
      num(`${EP}externalPoolMaxInputTokens`, '估算输入上限（兼容）', '保留历史配置，当前不用于拒绝。', { min: 0, suffix: 'tokens', advanced: true }),
    ],
  },
  {
    id: 'timeout',
    title: '冷却与超时',
    fields: [
      num(`${EP}externalPoolRateLimitCooldownSecs`, '429 冷却', '', { min: 1, suffix: '秒' }),
      num(`${EP}externalPoolServerErrorCooldownSecs`, '5xx 冷却', '', { min: 1, suffix: '秒' }),
      num(`${EP}externalPoolNetworkErrorCooldownSecs`, '网络错误冷却', '', { min: 1, suffix: '秒' }),
      num(`${EP}externalPoolProtocolErrorCooldownSecs`, '协议/认证冷却', '', { min: 1, suffix: '秒' }),
      sel(`${EP}externalPoolModelUnavailableCooldownMode`, '模型不可用冷却范围', '', [
        ['disabled', '不冷却'],
        ['model', '仅该模型'],
        ['pool', '整个池'],
      ]),
      num(`${EP}externalPoolModelUnavailableCooldownSecs`, '模型不可用冷却', '', { min: 1, suffix: '秒', disabledWhen: (g) => g(`${EP}externalPoolModelUnavailableCooldownMode`) === 'disabled' }),
      num(`${EP}externalPoolRequestTimeoutSecs`, '非流式总超时', '', { min: 0, suffix: '秒' }),
      num(`${EP}externalPoolStreamRequestTimeoutSecs`, '流式总超时', '', { min: 0, suffix: '秒' }),
      num(`${EP}externalPoolStreamIdleTimeoutSecs`, '流式空闲超时', '', { min: 0, suffix: '秒' }),
      bool(`${EP}externalPoolStreamPreOutputRetryEnabled`, '流式首输出前错误换池', '未提交内容前遇到错误、断流时换其他外部池。'),
      sel(`${EP}externalPoolStreamResponseMode`, '默认 SSE 转发', '', [['event_passthrough', 'SSE 事件级透传']], { advanced: true }),
    ],
  },
  {
    id: 'auto-disable',
    title: '自动禁用',
    fields: [
      bool(`${EP}externalPoolAutoDisableEnabled`, '启用自动禁用', '外部池出现明确的不可用错误时自动停用一段时间。'),
      bool(`${EP}externalPoolAutoDisableOnAuthError`, '认证错误', '', { disabledWhen: off(`${EP}externalPoolAutoDisableEnabled`) }),
      bool(`${EP}externalPoolAutoDisableOnSecurityLock`, '安全锁定', '', { disabledWhen: off(`${EP}externalPoolAutoDisableEnabled`) }),
      bool(`${EP}externalPoolAutoDisableOnQuotaExhausted`, '额度耗尽', '', { disabledWhen: off(`${EP}externalPoolAutoDisableEnabled`) }),
      bool(`${EP}externalPoolAutoDisableOnMisconfiguredEndpoint`, '端点配置错误', '', { disabledWhen: off(`${EP}externalPoolAutoDisableEnabled`) }),
      bool(`${EP}externalPoolAutoDisableOnChannelDisabled`, '上游通道禁用', '', { disabledWhen: off(`${EP}externalPoolAutoDisableEnabled`) }),
      num(`${EP}externalPoolAutoDisableFailureThreshold`, '触发阈值', '', { min: 1, suffix: '次', disabledWhen: off(`${EP}externalPoolAutoDisableEnabled`) }),
      num(`${EP}externalPoolAutoDisableWindowSecs`, '统计窗口', '', { min: 1, suffix: '秒', disabledWhen: off(`${EP}externalPoolAutoDisableEnabled`) }),
      num(`${EP}externalPoolAutoDisableDurationSecs`, '禁用时长', '0 表示直到手动解除。', { min: 0, suffix: '秒', disabledWhen: off(`${EP}externalPoolAutoDisableEnabled`) }),
    ],
  },
  {
    id: 'quality',
    title: '质量感知调度',
    fields: [
      bool(`${EP}externalPoolQualityAwareSchedulingEnabled`, '启用质量感知调度', '关闭后退回"优先级 → 负载"的排序，可作为快速回退手段。'),
      num(`${EP}externalPoolQualityTopK`, '候选池数量 K', '在评分最优的 K 个池里加权随机挑选。', { min: 1, max: 100, suffix: '个', disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolDegradeErrorRateThreshold`, '降级失败率阈值', '失败率达到该比例即临时降级。', { min: 0, max: 1, step: 0.05, suffix: '比例', disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolDegradeWindowSecs`, '劣化判定窗口', '', { min: 1, max: 86_400, suffix: '秒', disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolDegradeProbationSecs`, '单次降级时长', '连续触发按 2 倍延长。', { min: 1, max: 86_400, suffix: '秒', disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolMaxProbationSecs`, '最长降级时长', '不得低于单次降级时长。', { min: 1, max: 86_400, suffix: '秒', disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolProbeSharePercent`, '降级期探测流量', '被降级池仍保留的流量比例；0 会变成硬避让。', { min: 0, max: 100, suffix: '%', disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolRecoveryRampSecs`, '恢复爬坡时长', '降级到期后流量线性回升的时长。', { min: 0, max: 86_400, suffix: '秒', disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolQualityErrorWeight`, '失败率权重', '', { min: 0, max: 1_000_000, step: 1, suffix: '权重', advanced: true, disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolQualityLatencyWeight`, '延迟权重', '', { min: 0, max: 1_000_000, step: 1, suffix: '权重', advanced: true, disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolQualityLoadWeight`, '负载权重', '', { min: 0, max: 1_000_000, step: 1, suffix: '权重', advanced: true, disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolQualityProbationWeight`, '降级权重', '', { min: 0, max: 1_000_000, step: 1, suffix: '权重', advanced: true, disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolQualityPriorityWeight`, '优先级权重', '仅用于展示分数构成。', { min: 0, max: 1_000_000, step: 1, suffix: '权重', advanced: true, disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolQualityEwmaAlpha`, '质量平滑系数', '', { min: 0.01, max: 1, step: 0.01, suffix: '系数', advanced: true, disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolQualityMinSamples`, '最少样本数', '', { min: 1, max: 10_000, suffix: '次', advanced: true, disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
      num(`${EP}externalPoolQualitySampleTtlSecs`, '质量数据有效期', '', { min: 1, max: 86_400, suffix: '秒', advanced: true, disabledWhen: off(`${EP}externalPoolQualityAwareSchedulingEnabled`) }),
    ],
  },
  {
    id: 'billing',
    title: '用量投影（按路径整理的外部池）',
    description: '只作用于"下游 usage 口径 = 按入口路径整理"的外部池',
    fields: [
      bool(`${EP}externalPoolUsageProjectionCostFloorEnabled`, '启用成本底线', '上报成本低于上游原始成本时补齐。'),
      num(`${EP}externalPoolUsageProjectionCostFloorMarginPercent`, '成本补齐余量', '', { min: 0, suffix: '%', disabledWhen: off(`${EP}externalPoolUsageProjectionCostFloorEnabled`) }),
      num(`${EP}externalPoolUsageProjectionUpliftPercent`, '缓存读写放大', '0 表示关闭。', { min: 0, suffix: '%' }),
      num(`${EP}externalPoolUsageProjectionOutputUpliftMinTokens`, '输出补偿阈值', '输出超过该值后开始放大；0 表示关闭。', { min: 0, suffix: 'tokens' }),
      num(`${EP}externalPoolUsageProjectionOutputUpliftPercent`, '输出放大', '', { min: 0, suffix: '%' }),
    ],
  },
  {
    id: 'debug',
    title: '诊断记录',
    fields: [
      bool(`${EP}externalPoolUsageDebugEnabled`, '记录 usage 原始数据', '临时记录外部池上游原始响应 usage 样本；默认关闭。'),
      { kind: 'text', path: `${EP}externalPoolUsageDebugDir`, label: '诊断目录', disabledWhen: off(`${EP}externalPoolUsageDebugEnabled`) },
      num(`${EP}externalPoolUsageDebugMaxBodyBytes`, '单条片段上限', '', { min: 0, max: 1024 * 1024, suffix: 'Bytes', disabledWhen: off(`${EP}externalPoolUsageDebugEnabled`) }),
      num(`${EP}externalPoolUsageDebugMaxFiles`, '最多诊断文件', '', { min: 0, max: 100_000, suffix: '个', disabledWhen: off(`${EP}externalPoolUsageDebugEnabled`) }),
    ],
  },
]

export const ALL_FIELDS: Array<FieldDef & { section: string; sectionTitle: string; group: string }> = [
  ...SETTINGS_SECTIONS.flatMap((s) => s.groups.flatMap((g) => g.fields.map((f) => ({ ...f, section: s.id, sectionTitle: s.title, group: g.title })))),
  ...POOL_POLICY_GROUPS.flatMap((g) => g.fields.map((f) => ({ ...f, section: 'pools', sectionTitle: '外部池策略', group: g.title }))),
]

export const FIELD_BY_PATH = new Map(ALL_FIELDS.map((f) => [f.path, f]))
