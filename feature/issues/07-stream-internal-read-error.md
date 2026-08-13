# 流内部读取错误（上游流 body 解码失败）

Status: `historical-production-classified / transport-fault-gate-pending`

Severity: P1

- 状态：已定性；上游/网络瞬态问题
- 严重级别：低 —— 生产近 12 小时 2 条（占非成功请求 0.7%）
- 分类来源：`tmp/analysis-usage-llm-errors` root-cause `07-stream_internal_read_error`

## 现象

流式读取上游 SSE body 时中途解码失败：

```
api_error: upstream stream read error: error decoding response body
terminalReason: internal_error
```

全量特征（2 条）：
- 均 `stream=true`、`errorStatusCode:200`（头已返回，读流中途失败）。
- `credentialAttemptCount:1`、`routeSubtype: local_error_no_fallback` —— 不重试。
- `requestedMaxTokens:8192`，模型 opus-4-6 / opus-4-7 各 1。
- `durationMs` 129s / 228s —— 流已持续一段时间后连接/解码中断。

## 根因与性质判定：上游 / 网络瞬态

`error decoding response body` 是 reqwest 在读取上游流时遇到连接中断或分块解码失败，属传输层瞬态，不由请求内容决定。

## 程序可规避性：有限

- 与 [[06-stream-upstream-status-error]] 同理：错误发生在流已开始之后，**换号重试不安全**（重复输出风险）。
- 仅当能确认错误在任何可见输出之前发生时，才可安全重试。
- 量极小（2 条），**建议暂不单独改造**，可并入统一的"首字前流式重试"设计一起评估（见 [[02-stream-upstream-idle-timeout]]）。

## 复现说明

真实网络抖动不可稳定触发，但 fake upstream 可在 header 后、首字节前、thinking/text/tool_use 后分别关闭 socket、发送截断 chunk 或产生 body decode error。每个提交点至少 5 轮；同时执行 client drop 对照，避免混淆 upstream read error 与 downstream cancellation。

## 处理方案

- 下游未提交时允许受共享 attempt budget 约束的 transport retry。
- 下游已提交后 0 retry，返回规范 stream error，usage 为 `stream_error`/`internal_error` 而非 success。
- 所有 body/decoder/task 在 EOF/error/drop 后释放，错误 burst 后可恢复，不保留孤儿连接。

## 验证与残余风险

历史 2 条样本不足以证明当前实现。C02-C04、D05、F01 和 L4 必须覆盖 reset、truncated frame、malformed SSE、idle 与 recovery。未知 reqwest/HTTP2 错误变体可能改变 classifier；默认不应在已提交后重试。回滚不得恢复伪 terminal 或无界 body drain。

## 关联

- 生产证据：`tmp/analysis-usage-llm-errors/root-causes/07-stream_internal_read_error/`。
- 相关：[[06-stream-upstream-status-error]]、[[02-stream-upstream-idle-timeout]]。
