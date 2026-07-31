# Endpoint failover 策略实施方案

## 适用范围

本方案处理 Kiro endpoint 多地址尝试、失败分类、重试边界、健康状态和回滚开关。

Endpoint failover 是高风险能力，必须默认关闭。只有在测试证明上游 endpoint 存在可替代地址且协议一致时才能启用。

## 来源项目与学习点

- `kiroxy/internal/pool/pool.go`：有 endpoint failover 思路。
- `kiroxy/internal/kiroclient/headers.go`：endpoint 切换时 header 构造需要保持一致。
- 当前项目 `src/kiro/endpoint/ide.rs`：已有 native Kiro endpoint 处理，应在现有结构上加策略。

## 当前项目现状

当前项目已有：

- 默认 endpoint 配置。
- Kiro provider 请求封装。
- 请求超时。
- 错误分类。
- stream/non-stream 路径。

当前不足：

- endpoint 异常时没有可控 failover 策略。
- 如果盲目重试，可能造成重复请求或协议不一致。

## 目标

- 增加默认关闭的 endpoint failover。
- 只在安全条件下重试。
- 所有 failover 尝试写入内部 trace。
- 对下游保持统一 account/request 概念，不暴露 endpoint 切换。

## 非目标

- 不默认启用。
- 不在已经向下游输出 stream 内容后 failover。
- 不对 invalid request、tool 格式错误、认证错误做 failover。
- 不把 endpoint 地址返回给下游。

## 涉及文件

- `src/kiro/provider.rs`
- `src/kiro/endpoint/ide.rs`
- `src/model/config.rs`
- `src/kiro/call_trace.rs`
- `src/anthropic/usage.rs`

## 配置设计

新增配置：

```rust
pub kiro_endpoint_failover_enabled: bool, // 默认 false
pub kiro_endpoint_failover_max_attempts: u32, // 默认 1
pub kiro_endpoint_failover_cooldown_secs: u64, // 默认 60
pub kiro_endpoint_failover_on_429: bool, // 默认 true
pub kiro_endpoint_failover_on_5xx: bool, // 默认 true
pub kiro_endpoint_failover_on_network_error: bool, // 默认 true
pub kiro_endpoint_failover_on_200_json_exception: bool, // 默认 true
```

endpoint 列表复用现有 endpoint 配置或新增：

```rust
pub kiro_endpoint_candidates: Vec<KiroEndpointCandidate>
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroEndpointCandidate {
    pub name: String,
    pub base_url: String,
    pub enabled: bool,
    pub priority: i32,
}
```

优先级仍遵守数值越小越优先。

## Retryable 条件

允许 failover：

- 网络连接失败。
- 上游响应超时，且没有向下游输出任何内容。
- HTTP 429。
- HTTP 5xx。
- HTTP 200 JSON exception 且分类为 throttle/internal。

禁止 failover：

- HTTP 400 invalid request。
- `Invalid tool use format`。
- 认证失败。
- 权限不足。
- 模型不存在。
- 已经向下游输出 stream 内容。
- 非流式请求已经收到完整上游响应。

## 新增数据结构

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointAttemptTrace {
    pub endpoint_name: String,
    pub attempt_index: u32,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub status_code: Option<u16>,
    pub error_kind: Option<String>,
    pub retryable: bool,
    pub selected_for_failover: bool,
}
```

记录规则：

- 每次请求最多记录 `kiro_endpoint_failover_max_attempts + 1` 条。
- 不记录完整 URL query 中的敏感信息。
- 不返回给下游。

## 实施步骤

1. 增加 endpoint candidate 类型和配置解析。
2. 在 provider 内部封装 endpoint attempt loop。
3. 默认关闭 failover，保持现有单 endpoint 行为。
4. 增加 retryable 分类函数。
5. stream 路径必须在 first downstream chunk 之前才能 failover。
6. 增加 endpoint cooldown，避免持续请求异常地址。
7. 将 attempt trace 写入 usage metadata。

## 测试方案

新增测试：

- `endpoint_failover_disabled_uses_primary_only`
- `endpoint_failover_retries_network_error_before_first_chunk`
- `endpoint_failover_does_not_retry_invalid_tool_use`
- `endpoint_failover_does_not_retry_auth_error`
- `endpoint_failover_does_not_retry_after_stream_started`
- `endpoint_failover_records_attempt_trace`
- `endpoint_failover_respects_endpoint_cooldown`

fake server 场景：

- primary 500，secondary 200。
- primary 200 JSON exception，secondary 200。
- primary stream 输出一半断开，不得 failover。
- primary 400 invalid tool use，不得 failover。

## 验收标准

- 默认配置下行为完全不变。
- 开启后只对允许条件 failover。
- failover 不导致重复 stream。
- 内部 trace 能看到尝试顺序和失败原因。
- 对下游不暴露 endpoint 名称或切换过程。

## 风险与回滚

风险：

- 重试导致重复上游请求。
- endpoint 协议不一致导致新错误。

规避：

- 默认关闭。
- 只在未向下游输出前重试。
- 最大重试次数默认 1。

回滚：

- 设置 `kiro_endpoint_failover_enabled=false`。
- 清空 endpoint candidates。

## 不得做的事项

- 不得默认启用。
- 不得对 invalid request failover。
- 不得在 stream 已输出后 failover。
- 不得把 endpoint 细节暴露给下游。

## 后续可选扩展

可以引入 endpoint 健康探测，但必须独立实现，避免请求热路径主动探测。

