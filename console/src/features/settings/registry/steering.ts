import type { SettingsSection } from './types'
import { num, bool, sel, off } from './helpers'

export const STEERING_SECTIONS: SettingsSection[] = [
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
          sel(
            'promptSteering.scope',
            '作用范围',
            '',
            [
              ['route_rules', '按路径规则'],
              ['claude_code_profile', 'Claude Code / Debug profile'],
              ['all_routes', '全部 messages 路由'],
            ],
            { disabledWhen: off('promptSteering.enabled') },
          ),
          sel(
            'promptSteering.routeMode',
            '路径规则模式',
            '',
            [
              ['allow_list', '只对规则命中的入口生效'],
              ['deny_list', '对规则外的入口生效'],
              ['allow_all', '全部入口生效'],
            ],
            { disabledWhen: (g) => !g('promptSteering.enabled') || g('promptSteering.scope') !== 'route_rules' },
          ),
          {
            kind: 'list',
            path: 'promptSteering.routeRules',
            label: '路径规则',
            desc: '每行一个路径前缀',
            disabledWhen: (g) => !g('promptSteering.enabled') || g('promptSteering.scope') !== 'route_rules',
          },
          bool('promptSteering.applyToExternalPool', '应用到外部池', '外部池 raw passthrough 时也按同一规则处理。', {
            disabledWhen: off('promptSteering.enabled'),
          }),
          bool('promptSteering.applyToCountTokens', 'count_tokens 同步计入', '避免估算低于真实请求。', {
            disabledWhen: off('promptSteering.enabled'),
          }),
        ],
      },
      {
        id: 'blocks',
        title: '提示内容',
        fields: [
          bool('promptSteering.languageConstraint.enabled', '语言约束', '减少中英混杂、语言串台。', {
            disabledWhen: off('promptSteering.enabled'),
          }),
          {
            kind: 'text',
            multiline: true,
            path: 'promptSteering.languageConstraint.prompt',
            label: '语言约束提示词',
            advanced: true,
            disabledWhen: off('promptSteering.languageConstraint.enabled'),
          },
          bool('promptSteering.taskQuality.enabled', '任务质量', '强调最新用户消息与任务边界。', {
            disabledWhen: off('promptSteering.enabled'),
          }),
          {
            kind: 'text',
            multiline: true,
            path: 'promptSteering.taskQuality.prompt',
            label: '任务质量提示词',
            advanced: true,
            disabledWhen: off('promptSteering.taskQuality.enabled'),
          },
          bool('promptSteering.toolChoice.enabled', 'tool_choice 引导', '本地 Kiro 的 tool_choice 兼容提示。', {
            disabledWhen: off('promptSteering.enabled'),
          }),
          bool('promptSteering.thinking.enabled', 'thinking 提示控制', 'synthetic thinking 兼容提示。', {
            disabledWhen: off('promptSteering.enabled'),
          }),
          bool('promptSteering.chunkedWrite.enabled', '分块写入提示', 'Write/Edit 分块兼容提示。', {
            disabledWhen: off('promptSteering.enabled'),
          }),
          bool('promptSteering.chunkedWrite.systemPromptEnabled', '分块 system 提示', '', {
            advanced: true,
            disabledWhen: off('promptSteering.chunkedWrite.enabled'),
          }),
          bool('promptSteering.chunkedWrite.toolDescriptionEnabled', '分块工具描述', '', {
            advanced: true,
            disabledWhen: off('promptSteering.chunkedWrite.enabled'),
          }),
          bool('promptSteering.custom.enabled', '自定义追加提示词', '', { disabledWhen: off('promptSteering.enabled') }),
          {
            kind: 'text',
            multiline: true,
            path: 'promptSteering.custom.prompt',
            label: '自定义提示词',
            disabledWhen: off('promptSteering.custom.enabled'),
          },
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
          bool('kiroCachePointRecordPlan', '记录缓存点计划', '把计划写入请求诊断数据。', {
            advanced: true,
            disabledWhen: off('kiroCachePointEnabled'),
          }),
        ],
      },
      {
        id: 'simulation',
        title: '本地模拟缓存（全局默认）',
        description: '影响对外上报的缓存用量；不改变上游实际计费',
        fields: [
          num('promptCacheTargetReadRatio', '目标缓存读取比例', '希望输入里大约多少比例显示为缓存读取；最高 0.99。', {
            min: 0,
            max: 0.99,
            step: 0.01,
            suffix: '比例',
          }),
          num('promptCacheTokenScale', '输入放大倍数', '计算展示用量前先把输入放大；1 表示不放大。', {
            min: 1,
            max: 3,
            step: 0.1,
            suffix: '倍',
          }),
          num('promptCacheMaxSimulatedInputTokens', '模拟输入上限', '0 表示不设上限。', { min: 0, suffix: 'tokens' }),
          num('promptCacheScaleMinInputTokens', '放大生效门槛', '原始输入低于该值时不放大。', { min: 0, suffix: 'tokens', advanced: true }),
          num('promptCacheCapJitterMinTokens', '触顶扣减下限', '接近上限时至少少显示这么多。', {
            min: 0,
            suffix: 'tokens',
            advanced: true,
          }),
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
]
