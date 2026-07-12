# 流内部读取错误（上游流 body 解码失败）

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

## 性质判定：上游 / 网络瞬态

`error decoding response body` 是 reqwest 在读取上游流时遇到连接中断或分块解码失败，属传输层瞬态，不由请求内容决定。

## 程序可规避性：有限

- 与 [[06-stream-upstream-status-error]] 同理：错误发生在流已开始之后，**换号重试不安全**（重复输出风险）。
- 仅当能确认错误在任何可见输出之前发生时，才可安全重试。
- 量极小（2 条），**建议暂不单独改造**，可并入统一的"首字前流式重试"设计一起评估（见 [[02-stream-upstream-idle-timeout]]）。

## 复现说明

依赖上游连接/传输层瞬态抖动，**无法在本地稳定复现**。

## 关联

- 生产证据：`tmp/analysis-usage-llm-errors/root-causes/07-stream_internal_read_error/`。
- 相关：[[06-stream-upstream-status-error]]、[[02-stream-upstream-idle-timeout]]。
