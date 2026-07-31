# Stream idle 与上游异常处理实施方案

## 适用范围

本方案处理流式请求中的首字延迟、thinking 首包、可见文本首包、上游 idle、HTTP 200 JSON exception、malformed SSE、客户端断开、lease 释放和 usage 记录。

## 来源项目与学习点

- `kirocc-prox/internal/kiroclient/idle_reader.go`：stream idle timeout 处理清晰。
- `kirocc-prox/internal/kiroclient/client.go`：识别 HTTP 200 JSON exception，避免误判为 eventstream parse 错误。
- `kiroxy/internal/messages/gate_writer.go`：状态机思想可学习，但不得隐藏真实 thinking 输出。

## 当前项目现状

当前项目已经支持：

- Anthropic SSE。
- thinking 输出。
- request id。
- usage latency trace。
- 上游错误归一化。
- 外部账号 stream usage capture。

需要加强：

- 区分 first upstream byte、first thinking delta、first visible text。
- 对 200 JSON exception 做明确分类。
- 确保所有异常路径释放 lease。
- 确保客户端断开不会继续占用账号。

## 目标

- 建立统一 stream phase trace。
- 所有 stream 结束路径都必须释放资源。
- thinking 模式必须真实输出思考流。
- 非 thinking 模式可以使用轻量 gate 防止空响应误导，但不得延迟正常首包。
- 上游异常必须内部记录原始状态，对下游返回统一英文错误。

## 非目标

- 不重写整个 SSE parser。
- 不隐藏 thinking delta。
- 不默认 retry 已经开始向下游输出的 stream。
- 不把上游原始错误直接透传。

## 涉及文件

- `src/anthropic/stream.rs`
- `src/kiro/provider.rs`
- `src/kiro/endpoint/ide.rs`
- `src/anthropic/usage.rs`
- `src/anthropic/envelope.rs`
- `src/external_pool.rs`
- `src/model/config.rs`

## 新增数据结构

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamPhaseTrace {
    pub request_started_at_ms: i64,
    pub upstream_request_sent_at_ms: Option<i64>,
    pub upstream_headers_at_ms: Option<i64>,
    pub first_upstream_byte_at_ms: Option<i64>,
    pub first_thinking_delta_at_ms: Option<i64>,
    pub first_visible_delta_at_ms: Option<i64>,
    pub final_event_at_ms: Option<i64>,
    pub client_dropped_at_ms: Option<i64>,
    pub lease_released_at_ms: Option<i64>,
    pub terminal_reason: Option<StreamTerminalReason>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamTerminalReason {
    Completed,
    UpstreamStatusError,
    UpstreamJsonException,
    UpstreamIdleTimeout,
    MalformedSse,
    ClientDropped,
    InternalError,
}
```

## Timeout 规则

必须区分三个 timeout：

1. 上游建连和响应头 timeout：使用 `kiro_upstream_response_timeout_secs`。
2. 响应头之后 event idle timeout：新增或沿用 `kiro_upstream_stream_idle_timeout_secs`。
3. 下游写入 timeout：如果当前没有配置，第一阶段只记录 client drop，不新增复杂写超时。

建议默认：

```text
kiro_upstream_stream_idle_timeout_secs = 90
```

规则：

- 已收到 headers 但没有任何 event，超过 idle timeout 必须中止。
- 已收到 thinking delta 后，后续长时间没有 event，也必须按 idle timeout 中止。
- 收到 terminal event 后必须释放 lease。
- 客户端断开后必须释放 lease，并记录 `ClientDropped`。

## 200 JSON exception 识别

识别条件：

- HTTP status 是 200。
- `content-type` 是 JSON，或响应体前 512 bytes 可以解析为 JSON object。
- JSON 包含 `__type`、`code`、`message`、`error` 等异常字段。
- body 不是 eventstream frame。

处理：

- 归类为 `UpstreamJsonException`。
- 原始 JSON 摘要写入内部 diagnostics。
- 对下游返回统一错误。
- 如果 stream 还没有向下游发送任何 chunk，可以返回普通 JSON error。
- 如果已经开始 SSE，则发送 SSE error event，带 request id 和 error id。

## 对外错误映射

不得暴露 upstream 原始 message。

| 内部原因 | 对外 message |
| --- | --- |
| 上游 idle | `The upstream account did not produce data before the stream timeout.` |
| 200 JSON exception | `The upstream account could not complete this request.` |
| malformed SSE | `The upstream account returned an invalid stream.` |
| client dropped | 不需要返回，内部记录 |
| 内部 parser error | `The stream could not be completed.` |

所有返回都必须带 error id。

## 实施步骤

1. 在 stream 入口创建 `StreamPhaseTrace`。
2. 在上游请求发送、headers、first byte、thinking delta、visible delta、terminal event 处更新时间。
3. 引入 idle reader 包装，不改变现有 parser 输出语义。
4. 在解析 eventstream 前做 200 JSON exception sniff。
5. 所有返回路径使用 guard/drop 释放 lease。
6. 将 trace 写入 usage latency trace。
7. fake Kiro server 增加对应场景。

## 测试方案

新增测试：

- `stream_records_first_thinking_before_first_visible_text`
- `stream_idle_timeout_releases_lease`
- `stream_200_json_exception_is_not_parsed_as_sse`
- `stream_malformed_sse_records_terminal_reason`
- `stream_client_drop_releases_lease`
- `stream_error_event_contains_request_id_and_error_id`
- `stream_thinking_delta_is_not_gated`

真实测试：

- Claude Code CLI thinking 长会话。
- 上游慢首字模拟。
- 客户端中断连接。
- 大并发下 stream idle 释放验证。

## 验收标准

- usage 中能区分 TTFB、first thinking、first visible text。
- idle timeout 后账号 in-flight 下降。
- 200 JSON exception 不再显示为迷惑性 SSE parse 错误。
- thinking 模式真实输出 thinking delta。
- 对下游错误统一英文且带 error id。

## 风险与回滚

风险：

- sniff body 可能消费 stream 首包。
- idle timeout 设置过短会误杀慢请求。

规避：

- sniff 必须使用可回放 buffer。
- timeout 默认保守。
- 配置可调。

回滚：

- 关闭 JSON exception sniff。
- 调大 stream idle timeout。
- 回滚 idle reader 包装，保留 trace 字段不影响行为。

## 不得做的事项

- 不得隐藏 thinking 输出。
- 不得在已向下游输出正常内容后重试另一个账号。
- 不得把上游原始 exception body 直接返回给下游。
- 不得只依赖 Drop 释放关键 lease，必须在显式 terminal 路径释放。

## 后续可选扩展

可以增加 per-route stream timeout，但必须先完成基础 trace 和 fake server 测试。

