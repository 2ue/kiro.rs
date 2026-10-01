import type { SettingsSection } from './types'
import { num, bool, off } from './helpers'

export const RETRY_SECTIONS: SettingsSection[] = [
  {
    id: 'retry',
    title: '重试与超时',
    description: '上游超时、首输出保护、换号重试与端点轮换',
    groups: [
      {
        id: 'timeout',
        title: '超时',
        fields: [
          num('kiroUpstreamResponseTimeoutSecs', '开始响应等待时间', '发给上游后多久还没开始返回就认为超时；0 表示使用默认值。', {
            min: 0,
            suffix: '秒',
          }),
          num('kiroUpstreamStreamIdleTimeoutSecs', '流式静默超时', '流式响应长时间没有新内容时结束请求。', { min: 0, suffix: '秒' }),
          num('streamFirstOutputTimeoutSecs', '首输出等待上限', '响应头后最多等待多少秒出现首个内容；0 表示关闭。建议不低于 300 秒。', {
            min: 0,
            max: 3600,
            suffix: '秒',
          }),
          num('streamKeepaliveIntervalSecs', '流式保活间隔', '上游暂时没有内容时，每隔多久向客户端发一次保活。', {
            min: 1,
            max: 300,
            suffix: '秒',
          }),
          num('streamPreOutputHoldSecs', '首输出前静默保留', '首个输出前最多多少秒不向客户端写入，以便失败时无感换号；0 表示一直保留。', {
            min: 0,
            max: 60,
            suffix: '秒',
          }),
        ],
      },
      {
        id: 'attempts',
        title: '尝试次数上限',
        fields: [
          num(
            'inferenceUpstreamMaxAttempts',
            '单请求推理发送硬上限',
            '本地换号、首输出前重试、外部池故障转移和本地救援共享；与账号数量无关。',
            { min: 1, max: 10, suffix: '次' },
          ),
          num('credentialRetryMaxAttempts', '本地 provider 尝试上限', '单次本地调用最多尝试多少个凭据；0 表示默认 3。', {
            min: 0,
            suffix: '次',
          }),
          num('auxiliaryUpstreamMaxAttempts', '单请求辅助发送硬上限', 'Token 刷新与 Profile 探测共享，不计入推理次数。', {
            min: 1,
            max: 10,
            suffix: '次',
            advanced: true,
          }),
        ],
      },
      {
        id: 'stream-retry',
        title: '首输出前流式换号',
        description: '只在还没向客户端发送任何 SSE 事件时生效',
        fields: [
          bool('kiroUpstreamStreamRetryEnabled', '启用首输出前换号', '已输出 message_start、文本或工具调用后不会重试。'),
          num('kiroUpstreamStreamRetryMaxAttempts', '最多尝试', '包含第一次调用。', {
            min: 1,
            max: 100,
            suffix: '次',
            disabledWhen: off('kiroUpstreamStreamRetryEnabled'),
          }),
          bool('kiroUpstreamStreamRetryOnIdleTimeout', '静默超时可换号', '首输出前长时间无内容时允许换号。', {
            disabledWhen: off('kiroUpstreamStreamRetryEnabled'),
          }),
          bool('kiroUpstreamStreamRetryOnReadError', '读取错误可换号', '首输出前连接中断、流读取失败时允许换号。', {
            disabledWhen: off('kiroUpstreamStreamRetryEnabled'),
          }),
          bool('kiroUpstreamStreamRetryOnStatusError', '状态错误可换号', '首输出前收到 2xx JSON 错误体时允许换号。', {
            disabledWhen: off('kiroUpstreamStreamRetryEnabled'),
          }),
        ],
      },
      {
        id: 'logic',
        title: '其他重试策略',
        fields: [
          bool('credentialPromptLogicRetryEnabled', '提示逻辑错误换号', '上游返回提示/工具协议 400 时换未尝试的账号重试。'),
          num('credentialPromptLogicRetryMaxAttempts', '提示逻辑最多换号', '0 表示默认 1 次。', {
            min: 0,
            suffix: '次',
            disabledWhen: off('credentialPromptLogicRetryEnabled'),
          }),
          bool('kiroUpstreamRegionRotationEnabled', '上游端点轮换', '瞬态错误时在 us-east-1 / eu-central-1 之间轮换重试。'),
          bool('localBerserkModeEnabled', '狂暴模式（429 暴力轮换）', '普通 429 时把所有账号与端点轮一遍再放弃；会显著放大上游请求量。', {
            advanced: true,
          }),
          num('localBerserkMaxRounds', '狂暴模式最多轮数', '一轮 = 所有账号 × 所有端点各试一遍。', {
            min: 1,
            max: 10,
            suffix: '轮',
            advanced: true,
            disabledWhen: off('localBerserkModeEnabled'),
          }),
          num('localBerserkRoundDelayMs', '狂暴模式轮次间隔', '进入下一轮前的退避时间。', {
            min: 0,
            max: 60_000,
            suffix: '毫秒',
            advanced: true,
            disabledWhen: off('localBerserkModeEnabled'),
          }),
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
          num('tokenRefreshMaxRpm', 'Token 刷新 RPM 上限', 'Redis 可用时为跨实例共享上限，否则为单进程上限。', {
            min: 1,
            max: 6000,
            suffix: 'RPM',
          }),
          num('tokenRefreshBurst', 'Token 刷新突发容量', '允许立即发送的刷新数量，之后按 RPM 补充。', { min: 1, max: 256, suffix: '次' }),
          num(
            'auxiliaryUpstreamMaxConcurrentRequests',
            '单实例辅助并发上限',
            '同时进行的 Token 刷新、Profile 探测和模型目录请求；饱和时立即拒绝。',
            { min: 1, max: 256, suffix: '路' },
          ),
        ],
      },
      {
        id: 'background',
        title: '后台主动刷新',
        fields: [
          bool('tokenRefreshBackgroundEnabled', '闲置账号后台刷新', '定期扫描长期未调用的 OAuth 账号，在过期前主动轮换 Token。'),
          num('tokenRefreshBackgroundIntervalSecs', '扫描间隔', '后台扫描账号的间隔。', {
            min: 10,
            max: 3600,
            suffix: '秒',
            disabledWhen: off('tokenRefreshBackgroundEnabled'),
          }),
          num('tokenRefreshBackgroundLeadSecs', '刷新提前量', 'Token 剩余多少秒时进入主动刷新窗口。', {
            min: 60,
            max: 3600,
            suffix: '秒',
            disabledWhen: off('tokenRefreshBackgroundEnabled'),
          }),
        ],
      },
    ],
  },
]
