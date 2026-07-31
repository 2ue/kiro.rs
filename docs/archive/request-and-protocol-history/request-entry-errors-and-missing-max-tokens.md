# Messages 入口错误记录与 `max_tokens` 兼容策略

## 背景

`/v1/messages`、`/cc/v1/messages`、`/na/v1/messages` 等 Anthropic Messages 入口在进入本地账号调度、外部池调度、usage 计算之前，需要先解析请求体。历史实现里，如果客户端请求缺少顶层 `max_tokens`，或者 JSON 本身不合法，`serde_json::from_slice::<MessagesRequest>` 会直接返回 400：

```json
{
  "type": "invalid_request_error",
  "message": "Invalid JSON body: missing field `max_tokens` ..."
}
```

这个错误发生在正常请求日志和 usage 记录之前，因此现网会出现“下游拿到了 400，但系统 usage 页面查不到”的情况。

Anthropic Messages 的 `max_tokens` 表示本次输出上限。它不是缓存、计费或路由字段，缺失时不应该悄悄补成一个极大值；极大值会改变客户端输出预算、非流请求等待时间和成本风险。补 `0` 也不合理，因为 `0` 不是一个有实际输出预算的正常请求值，还会干扰 stop reason 推断。

## 当前实现

入口现在会先做一次轻量顶层探测，只扫描 JSON 顶层字段，不反序列化 `messages`、图片、工具结果或历史内容：

- 是否存在顶层 `max_tokens`
- 顶层 `model`
- 顶层 `stream`
- 顶层对象是否完整
- 请求体字节数

当顶层 `max_tokens` 缺失且 JSON 顶层对象完整时，按运行时配置处理：

- `default_value`：在原始 JSON 顶层补入 `"max_tokens": 20480`，后续本地解析、外部池 raw 透传、外部池预检都会看到补全后的请求体。
- `reject`：直接返回 400，并记录一条入口错误 usage。

默认策略是：

```json
{
  "missingMaxTokens": {
    "policy": "default_value",
    "defaultValue": 20480
  }
}
```

可切换为严格拒绝：

```json
{
  "missingMaxTokens": {
    "policy": "reject",
    "defaultValue": 20480
  }
}
```

`defaultValue` 限制为 `1..=200000`。默认值 20480 的取舍是：兼容长回答和 Claude Code 类调用的常见输出预算，同时不会把缺字段请求放大成 64k、128k 或 200k 输出预算。

## 错误记录内容

入口错误现在会写入 usage 记录，状态为 `error`，并带上：

- `id` / `errorId`：本系统生成的 request id
- `errorSource=request_entry`
- `errorStatusCode=400`
- `errorType=invalid_request_error`
- `publicErrorStatusCode=400`
- `publicErrorMessage`
- `errorMetadata.stage=request_entry`
- `errorMetadata.reason`
- `errorMetadata.bodyBytes`
- `errorMetadata.maxTokensPresent`
- `errorMetadata.completeTopLevelObject`
- `errorMetadata.missingMaxTokensPolicy`
- `errorMetadata.defaultedMaxTokens`

不会记录完整请求体、消息内容、工具参数、图片内容或凭证。错误详情和元数据仍会经过现有 usage 错误诊断裁剪逻辑，避免异常字符串造成存储压力。

## 主路径压力控制

这条记录路径复用现有 `UsageRecorder`：

- 内存只保留固定窗口。
- PgSQL 和 Redis 写入使用已有异步有界队列。
- 队列满时丢弃本条持久化/summary，并递增 dropped 计数，不阻塞主请求。
- 入口探测是单次线性扫描 raw body；只有缺少顶层 `max_tokens` 且策略为 `default_value` 时才复制请求体并追加一个小字段。

因此异常请求高峰不会同步压住主业务写库。代价是：极端写入队列拥塞时，入口错误可能只保留在内存窗口和 warn 日志里，持久化可能被丢弃；这是为了保护主链路。

## 前端展示

`/ui` 的运行配置页新增“缺失 max_tokens”：

- “自动补全”：按 `defaultValue` 补入顶层 `max_tokens`。
- “直接拒绝”：保持严格协议，返回 400。

usage 详情页新增入口错误字段和错误元数据展示，便于用 request id 追踪这类解析前失败。

## 仍然拒绝的情况

以下请求不会被补全：

- JSON 不完整或无效。
- 顶层不是对象。
- 顶层对象已经有 `max_tokens`，但类型或值不合法。
- `model` 为空。

这些情况仍返回 400，并记录为入口错误。这样可以兼容“字段缺失”这一类线上异常，但不会把任意坏请求改造成看似正常的请求。
