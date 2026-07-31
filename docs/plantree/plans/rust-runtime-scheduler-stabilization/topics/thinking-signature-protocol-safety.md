# Thinking / reasoning signature 优化

Last reviewed: 2026-07-28 Asia/Shanghai

Related:

- [Thinking Signature 官方机制与改造方案](../../../../../docs/analysis/thinking-signature-remediation-plan-20260728.md)
- [Kiro 上游“签名”与设备指纹全链路分析](../../../../../docs/analysis/kiro-upstream-signature-and-fingerprint-analysis-20260727.md)

## 当前状态

已有深入只读分析，但尚未实现。该问题可能影响真实 Claude Code CLI 长会话、tools、thinking、MCP、payload guard、历史压缩和重试。

核心官方事实：

- thinking `signature` 和 `redacted_thinking.data` 是不透明块。
- 客户端唯一正确行为是逐字回传。
- 工具调用回合内必须保留最新 assistant 的 thinking + signature。
- signature 绑定模型，不绑定 session。
- interleaved thinking 下，一条 assistant 消息可能有多个 thinking/redacted 块。

## P0-1：payload guard 保护最新工具回合 thinking/signature

问题：

- 历史裁剪逻辑曾按 `messages.len() - 1` 判断当前窗口。
- 工具回合里最后一个通常是 `user(tool_result)`，最新 assistant 会被当成历史，thinking/signature 可能被剥离。

建议：

- 使用结构化 protected boundary：
  - 找到最后一个 `user` tool_result。
  - 向前定位其对应的最新 assistant tool_use 消息。
  - 保护该 assistant 中的 signed thinking/redacted blocks。
- 不能简单沿角色连续回退，否则可能把更老、不同模型的签名钉住。

验收：

- 工具续写场景中最新 assistant 的 signed thinking 不被 payload guard 删除。
- 历史更老 thinking 可以按配置丢弃。
- 多轮长会话 + tools + payload guard 超限时不会产生 `THINKING_SIGNATURE_INVALID`。

## P0-2：多块 / 交错 thinking 的模型表示

问题：

- 当前 `ReasoningContent` 是单值，`AssistantMessage` 只有一个 `reasoning_content`。
- 多个 thinking/redacted blocks 会被拒绝或无法保序。
- 即使改成数组，如果 `tool_use` 与 thinking 的相对位置丢失，也可能仍不符合上游要求。

待决策：

- Branch A：如果 Kiro 接受数组/位置化 reasoning，则实现 Vec 或位置化内容模型。
- Branch B：如果 Kiro 只接受单值，则对受保护最新 assistant 的多块 thinking fail-closed，不伪造、不合并、不丢弃。

阻塞证据：

- 需要真实抓包确认 Kiro 是否接受：
  - 1 元素数组。
  - 2 元素数组。
  - thinking → tool_use → thinking → tool_use 交错顺序。
  - 最新工具回合完全省略 thinking 是否仍 400。

验收：

- 不再因为合法多块 thinking 在本地转换阶段直接错误。
- 如果上游不支持多块，本地返回明确错误，不 silently corrupt。

## P0-3：redacted_thinking.data 不能做内容/长度/base64 规范化校验

问题：

- 现有分析指出 `validate_redacted_thinking_data` 对不透明数据做了空检查、长度上限、base64 解码和规范再编码。
- 这违反“逐字保留不透明块”的原则。

建议：

- 校验只保留 JSON 语法层：
  - `type == "redacted_thinking"`
  - `data` 是 JSON string
- 不解码、不重编码、不做内容长度断言。
- 资源防护交给全局 body size / payload guard。

验收：

- redacted data 原字节 round-trip。
- malformed JSON 仍拒绝。
- 大 body 按全局 body 限制处理，不由 redacted 专项校验破坏不透明性。

## P0-4：transcript sanitizer 不扫描 signature/redacted data

问题：

- 分析指出 sanitizer 曾把 signature/redacted data 当普通文本扫描，可能改动不透明块。

建议：

- signature 和 redacted data 永不进入 sanitizer。
- signed thinking/redacted 作为协议原子处理。
- 只允许清洗未签名 thinking 或普通文本，且不得跨保护窗口。

验收：

- 含敏感模式字符串的 signature/redacted data 不被改写。
- 普通 tool transcript sanitizer 行为不回退。

## P0-5：thinking signature retry 不能无脑 strip all

问题：

- 当前 retry 可能剥离全部历史 reasoningContent。
- signature 与模型绑定，不同模型来源的块即使原样保留也会失败。
- 无模型溯源时无法正确判断哪些可以保留、哪些应该删除、哪些必须 fail closed。

建议：

- 为 reasoning block 记录模型溯源：真实上游模型，不是 alias。
- retry 分类：
  - 受保护/当前模型：必须保留。
  - 受保护/模型不匹配：fail closed，除非抓包证明无 thinking 降级合法。
  - 历史/当前模型：可保留。
  - 历史/模型不匹配：可省略。
  - 未知：保守处理。

验收：

- 模型不匹配签名不再通过 strip-all 伪装成成功路径。
- 工具续写必需 signature 不被 retry 删除。
- usage/error 中能区分 thinking_signature_invalid、retry stripped、fail closed。

## P0-6：真实抓包矩阵

必须完成的真实测试：

1. 单 object `reasoningContent` 基线。
2. 1 元素数组。
3. 2 元素数组。
4. thinking → tool_use → thinking → tool_use 交错。
5. 最新工具回合 thinking 完全省略。
6. 最新 thinking 重排。
7. 模型不匹配。
8. 更老工具回合 thinking 省略、最新保留。
9. `display:"omitted"` + 空 thinking + signature。
10. 捕获 Kiro 响应 shape，确认 object/array 反序列化需求。

验收：

- 抓包请求和响应必须脱敏。
- 不把抓包结论写成官方事实；只标注 Kiro 当前行为。
- Branch A/B 选择必须引用抓包证据。
