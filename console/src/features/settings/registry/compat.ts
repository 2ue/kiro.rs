import type { SettingsSection } from './types'
import { num, bool, sel, off } from './helpers'

export const COMPAT_SECTIONS: SettingsSection[] = [
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
          num('missingMaxTokens.defaultValue', '补充值', '自动补全时写入的输出上限。', {
            min: 1,
            max: 200_000,
            suffix: 'tokens',
            disabledWhen: (g) => g('missingMaxTokens.policy') === 'reject',
          }),
        ],
      },
      {
        id: 'stats',
        title: '统计口径',
        fields: [
          num('highCacheThreshold', '高缓存命中阈值', '缓存读取达到多少 token 视为高缓存请求（仅影响统计）。', {
            min: 0,
            suffix: 'tokens',
          }),
        ],
      },
    ],
  },
]
