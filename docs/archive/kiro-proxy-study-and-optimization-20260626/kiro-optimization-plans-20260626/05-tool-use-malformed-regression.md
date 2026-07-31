# Tool-use 异常格式回归矩阵实施方案

## 适用范围

本方案处理 Anthropic 消息、tool_use、tool_result、thinking、cache_control 转 Kiro 请求体时的格式保护和回归测试。

目标是减少 `Invalid tool use format`、`REQUEST_BODY_INVALID` 等上游错误，并在无法修复时返回统一对外错误，同时内部保留足够诊断。

## 来源项目与学习点

- `9router/open-sse/translator/request/claude-to-kiro.js`：请求转换场景多，适合补充异常输入样例。
- `9router/open-sse/translator/concerns/thinkingUnified.js`：thinking 相关块和普通内容分离处理。
- 本地 `kiro2api/internal/reqconv/cache_points.go`：cache_control 到 cachePoint 的处理值得参考。
- `Kiro-Go/proxy/kiro_api.go`：轻量实现中有 tool 格式处理样例。
- 当前项目 `src/anthropic/payload_guard.rs`：已经有修复基础，应补齐矩阵和边界。

## 当前项目现状

当前项目已经支持：

- Anthropic 请求转换。
- thinking 模型和 thinking 输出。
- payload guard。
- tool-use 修复。
- prompt cache / high-cache usage。

仍需要加强：

- 大并发下某些异常请求的原始输入必须能归类，不得只看到上游 400。
- 修复规则必须可预测，不得因为“自动修复”改变用户语义。
- tool_use 和 tool_result 顺序必须有明确不变量。

## 目标

- 建立 tool-use 回归矩阵。
- 在发送 Kiro 前执行轻量 audit。
- 对可安全修复的格式进行确定性修复。
- 对不可安全修复的请求提前返回统一错误。
- 内部记录修复动作和失败原因，但不记录敏感完整内容。

## 非目标

- 不根据用户内容猜测工具结果。
- 不伪造 tool_result。
- 不把 invalid 请求强行转成普通文本。
- 不在本方案中改变模型 alias 逻辑。
- 不隐藏真实 thinking 输出。

## 涉及文件

- `src/anthropic/converter.rs`
- `src/anthropic/payload_guard.rs`
- `src/anthropic/stream.rs`
- `src/model/config.rs`
- `src/kiro/provider.rs`
- `tests/anthropic_tool_use_regression.rs`

## Kiro 请求体不变量

发送上游前必须满足：

1. 每个 assistant `tool_use` 必须有稳定 `id`。
2. 每个 user `tool_result` 必须引用已出现且未消费的 `tool_use_id`。
3. 同一 `tool_use_id` 不得被多个结果重复消费。
4. 缺少 `tool_use_id` 的 `tool_result` 不得发送给 Kiro。
5. 空 `tool_use.input` 必须规范成 `{}`，不得发送 `null`。
6. `tool_result.content` 必须规范成 Kiro 可接受的内容类型。
7. thinking block 不得被混入 tool input。
8. cache_control 只能在 Kiro 支持的位置转换；不支持的位置不得原样塞入未知字段。
9. 历史 thinking 是否丢弃必须遵守 `discard_historical_thinking`。
10. 转换后 body 不得包含 Anthropic-only 的未知字段，除非上游已验证支持。

## 新增数据结构

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadAuditIssue {
    MissingToolUseId,
    DuplicateToolResult,
    ToolResultWithoutToolUse,
    EmptyToolUseInput,
    InvalidToolResultContent,
    ThinkingInsideToolPayload,
    UnsupportedCacheControlLocation,
    UnknownContentBlockType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadRepairAction {
    NormalizeEmptyToolInput,
    DropUnsupportedCacheControl,
    NormalizeToolResultContent,
    DropHistoricalThinking,
    RejectRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadAuditReport {
    pub request_id: String,
    pub model: String,
    pub issues: Vec<PayloadAuditIssue>,
    pub actions: Vec<PayloadRepairAction>,
    pub body_sha256: String,
    pub message_count: usize,
    pub tool_use_count: usize,
    pub tool_result_count: usize,
}
```

`body_sha256` 是规范化后请求体的哈希，用于排查同类问题，不得记录完整 body。

## 安全修复规则

允许自动修复：

- `tool_use.input == null` 改为 `{}`。
- `tool_result.content` 是纯字符串时包装为上游可接受格式。
- 不支持位置的 `cache_control` 删除，并记录 action。
- 历史 thinking 按配置丢弃。

必须拒绝请求：

- `tool_result` 引用了不存在的 `tool_use_id`。
- 同一个 `tool_use_id` 被多个 `tool_result` 消费。
- assistant 声明 tool_use 但后续消息结构无法确定配对关系。
- content block 类型完全未知且不能转成文本。

拒绝时对下游返回：

```text
The request body has an invalid tool-use sequence. If this continues, contact the administrator with error ID: {error_id}
```

## 配置与兼容策略

新增配置：

```rust
pub payload_audit_enabled: bool, // 默认 true
pub payload_audit_record_enabled: bool, // 默认 true
pub payload_audit_reject_invalid_tool_sequence: bool, // 默认 true
```

兼容要求：

- `payload_audit_enabled=false` 时保持历史行为。
- 默认开启 audit，但只执行安全修复和明确拒绝。
- 记录必须异步或随 usage 记录，不得同步写数据库。

## 实施步骤

1. 从 `converter.rs` 生成 Kiro body 后调用 audit。
2. audit 返回修复后的 body 或 reject。
3. 对 reject 创建 error id，写 usage/error metadata。
4. 对修复动作写 `PayloadAuditReport`。
5. 在上游返回 `Invalid tool use format` 时，也记录 audit report，便于判断漏检。
6. 补齐回归测试。

## 测试方案

新增测试：

- `tool_use_empty_input_is_normalized_to_empty_object`
- `tool_result_without_prior_tool_use_is_rejected`
- `duplicate_tool_result_is_rejected`
- `tool_result_string_content_is_normalized`
- `historical_thinking_is_removed_when_configured`
- `thinking_block_is_not_moved_into_tool_input`
- `unsupported_cache_control_is_dropped_and_recorded`
- `invalid_tool_use_public_error_is_normalized`
- `payload_audit_report_does_not_store_full_body`
- `upstream_invalid_tool_use_records_body_hash`

真实测试：

- 使用 Claude Code CLI 产生 tool_use。
- 使用长会话包含多轮 tool_use/tool_result。
- 使用 thinking + tool_use 混合请求。
- 检查 Kiro 不再返回 `Invalid tool use format`。

## 验收标准

- 已知异常 tool sequence 在本地测试中能提前拒绝或安全修复。
- 上游 `Invalid tool use format` 发生率下降。
- 对下游错误不暴露上游原始 body。
- 内部 usage 能通过 request id 查到 audit issue 和 action。
- 高并发下 audit 不造成明显延迟。

## 风险与回滚

风险：

- 自动修复过度，改变用户真实意图。
- audit 序列化 body 产生性能开销。

规避：

- 只做确定性安全修复。
- 哈希使用规范化摘要，不持久化完整 body。
- 可通过配置关闭 audit。

回滚：

- 设置 `payload_audit_enabled=false`。
- 保留测试，继续定位具体失败请求。

## 不得做的事项

- 不得伪造用户没有提供的 tool_result。
- 不得把 tool 错误包装成普通文本继续请求。
- 不得记录完整请求体。
- 不得把上游原始 400 body 直接返回给下游。

## 后续可选扩展

后续可以为管理端增加“最近 payload audit 问题”列表，但必须只展示摘要、哈希、issue 和 request id。

