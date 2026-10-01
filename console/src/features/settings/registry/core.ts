import type { SettingsSection } from './types'
import { num, bool, off } from './helpers'

export const CORE_SECTIONS: SettingsSection[] = [
  {
    id: 'capacity',
    title: '容量与限流',
    description: '全局并发、排队、单账号限速与请求 Key 准入',
    groups: [
      {
        id: 'global',
        title: '全局容量',
        fields: [
          num('dispatchGlobalMaxConcurrentRequests', '全局最大并发', '整个服务同一时间最多处理多少个请求；0 表示不限制。', {
            min: 0,
            suffix: '并发',
          }),
          num('dispatchMaxQueuedRequests', '最大排队请求数', '账号忙不过来时最多让多少个请求排队；0 表示不限制。', {
            min: 0,
            suffix: '请求',
          }),
          num('credentialDispatchMaxWaitSecs', '单请求最长排队等待', '一个请求最多等账号空闲多久；0 表示不限制。', {
            min: 0,
            suffix: '秒',
          }),
          num(
            'credentialInFlightLeaseMaxSecs',
            '异常并发自动回收',
            '请求长时间没有结束时自动释放占用，避免账号并发数被卡住；0 表示关闭。',
            { min: 0, suffix: '秒', advanced: true },
          ),
        ],
      },
      {
        id: 'per-account',
        title: '单账号默认值',
        description: '账号未单独设置时使用',
        fields: [
          num('credentialMaxConcurrentRequests', '单账号最大并发', '每个账号同一时间最多处理多少个请求；0 表示不限制。', {
            min: 0,
            suffix: '并发',
          }),
          num('credentialRpm', '单账号每分钟请求上限', '每个账号一分钟最多接多少个请求；0 表示不做本地限速。', { min: 0, suffix: 'RPM' }),
          num(
            'credentialInfoRefreshConcurrency',
            '额度查询上游并发',
            '强制查询额度时同时向 Kiro 请求的数量；建议 1 到 3，避免触发上游限流。',
            { min: 1, max: 16, suffix: '并发', advanced: true },
          ),
        ],
      },
      {
        id: 'admission',
        title: '请求 Key 准入（每实例 · 每 Key）',
        description: '多实例部署时各实例分别计数，总量近似为实例数倍',
        fields: [
          num('requestAdmission.rpm', '每 Key RPM', '每个实例分别限制每个已认证请求 API Key；0 表示关闭。', {
            min: 0,
            max: 1_000_000,
            suffix: 'RPM',
          }),
          num('requestAdmission.maxConcurrentRequests', '每 Key 并发', '每个实例分别统计同一请求 Key 的并发；0 表示关闭。', {
            min: 0,
            max: 10_000,
            suffix: '并发',
          }),
          num('requestAdmission.maxQueuedRequests', '每 Key 队列', '同一 Key 并发占满时最多等待的请求数；0 表示立即返回 429。', {
            min: 0,
            max: 100_000,
            suffix: '请求',
            advanced: true,
          }),
          num('requestAdmission.queueTimeoutMs', '每 Key 等待', '同一 Key 等待并发名额的最长时间；0 表示立即返回 429。', {
            min: 0,
            max: 300_000,
            suffix: '毫秒',
            advanced: true,
          }),
        ],
      },
      {
        id: 'weighted',
        title: '按 token 重量计算容量',
        fields: [
          bool(
            'weightedCapacity.enabled',
            '启用加权容量',
            '开启后长上下文请求按输入 token 分档占用多个并发/RPM 单位；分档在"高级 JSON"中编辑。',
          ),
          num('weightedCapacity.maxUnitsPerRequest', '单请求最大容量单位', '限制超长上下文最多占用多少本地并发/RPM 单位。', {
            min: 1,
            max: 64,
            suffix: '单位',
            disabledWhen: off('weightedCapacity.enabled'),
          }),
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
          num('schedulerLatencyWeight', '响应耗时权重', '越大，响应慢的账号越少被选中；通常设较小值。', {
            min: 0,
            step: 0.001,
            suffix: '权重',
          }),
          num('schedulerPriorityWeight', '优先级权重', '越大，账号优先级越能影响选择结果。', {
            min: 0,
            step: 0.1,
            suffix: '权重',
            advanced: true,
          }),
          num('schedulerProbationWeight', '恢复期降权', '账号刚从错误中恢复时，越大越少被选中。', {
            min: 0,
            step: 1,
            suffix: '权重',
            advanced: true,
          }),
          num('schedulerSelectionPressureWeight', '短时集中降权', '短时间内同一账号被选太多时，越大越快分散。', {
            min: 0,
            step: 1,
            suffix: '权重',
            advanced: true,
          }),
          num('schedulerTotalSelectionWeight', '长期使用次数权重', '越大，历史调度次数多的账号越少被选中。', {
            min: 0,
            step: 0.001,
            suffix: '权重',
            advanced: true,
          }),
          num('schedulerErrorEwmaAlpha', '近期错误敏感度', '越高，刚发生的错误越快影响账号选择。', {
            min: 0.01,
            max: 1,
            step: 0.01,
            suffix: '系数',
            advanced: true,
          }),
        ],
      },
      {
        id: 'warmup',
        title: '新账号预热',
        fields: [
          num('credentialWarmupRequests', '预热请求数', '新账号先以低比例参与调度的请求数；0 表示不预热。', { min: 0, suffix: '次' }),
          num('credentialWarmupSelectionPercent', '单个预热账号参与比例', '每次调度时预热账号被选中的概率上限。', {
            min: 0,
            max: 100,
            suffix: '%',
          }),
          num('credentialWarmupMaxSelectionPercent', '预热账号总占比上限', '所有预热中账号合计流量不超过此比例。', {
            min: 0,
            max: 100,
            suffix: '%',
          }),
        ],
      },
      {
        id: 'diag',
        title: '调度诊断',
        fields: [
          bool('selectionFailureRecordEnabled', '记录失败样本', '关闭后只保留失败原因统计，不记录具体账号样本。'),
          num('selectionFailureSampleLimit', '失败诊断样本数', '调度失败时最多记录多少个账号样本；0 表示不记录。', {
            min: 0,
            max: 1000,
            suffix: '个',
            disabledWhen: off('selectionFailureRecordEnabled'),
          }),
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
          num('credentialCooldownBackoffMultiplier', '退避倍率', '连续出错时冷却时间逐次乘以该倍数。', {
            min: 1,
            max: 10,
            step: 0.1,
            suffix: '倍',
          }),
          num('credentialCooldownJitterPercent', '恢复时间错开比例', '给恢复时间加随机错开，避免多个账号同时恢复又同时出错。', {
            min: 0,
            max: 100,
            suffix: '%',
          }),
          num('credentialProbationSecs', '恢复观察时间', '账号刚恢复时先少量使用，稳定后恢复正常调度。', { min: 0, suffix: '秒' }),
        ],
      },
    ],
  },
]
