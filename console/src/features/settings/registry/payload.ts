import type { SettingsSection } from './types'
import { num, bool, sel, off } from './helpers'

export const PAYLOAD_SECTIONS: SettingsSection[] = [
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
          sel(
            'payloadGuardMode',
            '处理时机',
            '发送前处理更稳；失败后处理保留更多上下文。',
            [
              ['preemptive', '发送前先处理'],
              ['on_too_long', '失败后再处理并重试'],
            ],
            { disabledWhen: off('payloadGuardEnabled') },
          ),
          num('payloadGuardMaxBytes', '请求大小阈值', '超过才触发处理（1048576 = 1 MB）；0 或不小于 65536。', {
            min: 0,
            suffix: '字节',
            disabledWhen: off('payloadGuardEnabled'),
          }),
          num('payloadGuardKiroMaxWeight', 'Kiro 加权阈值', 'ASCII=1、非 ASCII=8 加权计算；默认 1300000。', {
            min: 1,
            suffix: 'weight',
            advanced: true,
            disabledWhen: off('payloadGuardEnabled'),
          }),
          num('payloadGuardSafetyMarginBytes', '安全余量', '处理目标比阈值小出的缓冲，避免裁剪后仍超限。', {
            min: 0,
            suffix: '字节',
            advanced: true,
            disabledWhen: off('payloadGuardEnabled'),
          }),
          bool('payloadGuardTrimHistory', '优先裁剪旧历史', '内容太长时优先缩短较早的对话历史。', {
            disabledWhen: off('payloadGuardEnabled'),
          }),
          bool('payloadGuardExternalEnabled', '外部池也应用', '请求转发到外部池前执行同样的大小保护。', {
            disabledWhen: off('payloadGuardEnabled'),
          }),
          bool('compressionEnabled', '启用请求压缩', '发送前尽量去掉冗余内容。', { advanced: true }),
          bool('whitespaceCompression', '仅压缩空白字符', '只处理多余空格和换行，风险较低。', {
            advanced: true,
            disabledWhen: off('compressionEnabled'),
          }),
        ],
      },
      {
        id: 'history',
        title: '历史内容清理',
        description: '需要先启用大小保护',
        fields: [
          bool('payloadShaping.enabled', '启用内容清理', '请求太大时优先清理较早的历史消息。', {
            disabledWhen: off('payloadGuardEnabled'),
          }),
          bool('payloadShaping.truncateHistoricalToolResults', '截短历史工具结果', '只保留开头和结尾。', {
            disabledWhen: off('payloadShaping.enabled'),
          }),
          num('payloadShaping.historicalToolResultMaxChars', '历史工具结果保留字符', '', {
            min: 0,
            suffix: '字符',
            disabledWhen: off('payloadShaping.enabled'),
          }),
          num('payloadShaping.historicalToolResultHeadLines', '保留头部行数', '', {
            min: 0,
            suffix: '行',
            advanced: true,
            disabledWhen: off('payloadShaping.enabled'),
          }),
          num('payloadShaping.historicalToolResultTailLines', '保留尾部行数', '', {
            min: 0,
            suffix: '行',
            advanced: true,
            disabledWhen: off('payloadShaping.enabled'),
          }),
          bool('payloadShaping.discardHistoricalThinking', '移除历史思考内容', '避免旧 thinking 反复占用请求体积。', {
            disabledWhen: off('payloadShaping.enabled'),
          }),
          bool('payloadShaping.compressToolDefinitions', '压缩工具说明', '工具说明太大时精简描述和参数说明。', {
            disabledWhen: off('payloadShaping.enabled'),
          }),
          num('payloadShaping.toolDefinitionsBudgetBytes', '工具说明大小上限', '所有工具说明合计尽量控制在该大小以内。', {
            min: 0,
            suffix: '字节',
            advanced: true,
            disabledWhen: off('payloadShaping.enabled'),
          }),
          num('payloadShaping.toolDescriptionMaxChars', '单工具描述上限', '', {
            min: 0,
            suffix: '字符',
            advanced: true,
            disabledWhen: off('payloadShaping.enabled'),
          }),
          num('payloadShaping.toolSchemaAnnotationMaxChars', '工具参数说明上限', '', {
            min: 0,
            suffix: '字符',
            advanced: true,
            disabledWhen: off('payloadShaping.enabled'),
          }),
          bool('payloadShaping.webFetchTrimEnabled', '清理网页抓取历史', '历史里的网页正文太长时截短。', {
            advanced: true,
            disabledWhen: off('payloadShaping.enabled'),
          }),
          num('payloadShaping.webFetchBodyMaxChars', '网页正文保留字符', '', {
            min: 0,
            suffix: '字符',
            advanced: true,
            disabledWhen: off('payloadShaping.enabled'),
          }),
        ],
      },
      {
        id: 'current',
        title: '当前请求兜底',
        description: '历史清理后仍超限时才处理本轮内容',
        fields: [
          bool('payloadShaping.fitCurrentPayloadToBudget', '自动压缩当前内容', '', { disabledWhen: off('payloadShaping.enabled') }),
          bool('payloadShaping.truncateCurrentToolResults', '截短当前工具结果', '', {
            disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget'),
          }),
          num('payloadShaping.currentToolResultMaxChars', '当前工具结果保留字符', '', {
            min: 0,
            suffix: '字符',
            disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget'),
          }),
          bool('payloadShaping.truncateCurrentUserContent', '截短当前用户文本', '可能损失本轮请求细节。', {
            advanced: true,
            disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget'),
          }),
          num('payloadShaping.currentUserContentMaxChars', '当前用户文本保留字符', '', {
            min: 0,
            suffix: '字符',
            advanced: true,
            disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget'),
          }),
          bool('payloadShaping.truncateCurrentDocuments', '截短当前文档', '', {
            advanced: true,
            disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget'),
          }),
          num('payloadShaping.currentDocumentMaxChars', '当前文档保留字符', '', {
            min: 0,
            suffix: '字符',
            advanced: true,
            disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget'),
          }),
          bool('payloadShaping.truncateCurrentImages', '移除超限图片', '', {
            advanced: true,
            disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget'),
          }),
          num('payloadShaping.currentImagesMaxBytes', '当前图片保留大小', '', {
            min: 0,
            suffix: '字节',
            advanced: true,
            disabledWhen: off('payloadShaping.fitCurrentPayloadToBudget'),
          }),
          sel(
            'payloadShaping.oversizedImageHandling',
            '超大图片处理',
            '',
            [
              ['drop-with-placeholder', '替换为占位说明'],
              ['reject', '直接拒绝'],
            ],
            { advanced: true },
          ),
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
          bool('imageProcessing.safeMaterializeFileSources', '展开本地文件 source', '', {
            disabledWhen: (g) => g('imageProcessing.mode') !== 'safe',
          }),
          bool('imageProcessing.safeDownloadRemoteSources', '下载远程图片和文档', '', {
            disabledWhen: (g) => g('imageProcessing.mode') !== 'safe',
          }),
          bool('imageProcessing.safeNormalizeBase64MediaTypes', '修正 base64 图片类型', '', {
            disabledWhen: (g) => g('imageProcessing.mode') !== 'safe',
          }),
        ],
      },
      {
        id: 'conversion',
        title: '协议转换',
        fields: [
          bool('bodyConversion.toolSchemaNormalization', '工具 schema 规范化', '清理 OpenAPI、Zod、MCP 工具 schema 中上游容易拒绝的字段。'),
          bool('bodyConversion.toolNameMapping', '工具名映射', '清洗或缩短不符合 Kiro 约束的工具名并反向映射。'),
          sel(
            'bodyConversion.toolSchemaKeyMapping',
            '非法 schema key',
            '',
            [
              ['sanitize', '清洗并反向映射'],
              ['reject', '明确报错'],
              ['disabled', '不处理'],
            ],
            { advanced: true },
          ),
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
]
