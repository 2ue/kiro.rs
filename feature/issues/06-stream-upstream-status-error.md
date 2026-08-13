# 流式上游状态错误（流中途上游报错事件）

Status: `historical-production-classified / stream-state-machine-fault-gate-pending`

Severity: P1

- 状态：已定性；上游瞬态问题，当前保守策略正确
- 严重级别：低 —— 生产近 12 小时 5 条（占非成功请求 1.8%）
- 分类来源：`tmp/analysis-usage-llm-errors` root-cause `06-stream_upstream_status_error`

## 现象

流式响应**已经开始（HTTP 200、header 已返回）**，但上游在 eventstream 内返回错误事件后中断：

```
api_error: {"message":"Encountered an unexpected error when processing the request, please try again."}
terminalReason: upstream_status_error
```

全量特征：
- 5 条全部 `stream=true`、`errorStatusCode:200`（头已 200，错误在流中途）。
- `credentialAttempts` 里 `status=200` —— 上游已接受请求并开始响应。
- `credentialAttemptCount:1`、`routeSubtype: local_error_no_fallback` —— **不重试、不换号**。
- 模型 opus-4-8 占 4 条；有一条 `durationMs=428632`（约 7 分钟，长时间 thinking 后上游才报错）。

## 根因与性质判定：上游瞬态

上游文案 `please try again` 是明确的可重试信号，属于 Bedrock/Kiro 侧的瞬态内部错误，不由请求内容决定。

## 程序可规避性：有限，当前策略正确

- 错误发生在**首字之后 / 流已开始**（`status=200` 已返回给客户端），此时**换号重试不安全** —— 会造成重复或拼接输出，破坏 SSE 语义。
- 因此当前"不重试、直接终止"是**正确的保守策略**。
- ⚠️ 唯一可优化点：若能精确判定错误发生在**任何可见输出之前**（`firstOutputDeltaMs` 为空），则该子集可安全重试。但本类样本头已 200、且量极小（5 条），改造收益低、风险相对高，**建议暂不改**。

## 复现说明

真实上游随机错误不可稳定触发，但 fake EventStream 可稳定复现：在 header 后、首输出前、thinking 后、text 后和 tool_use 后分别发送 upstream status error event。每个提交点至少 5 轮，记录下游 terminal、usage status 和 upstream hit。

## 处理方案

- 首个下游字节前的明确 transient status error 可受共享 attempt budget 约束重试。
- 任意下游事件提交后不重试，发送规范 SSE error，不伪造 `message_stop` 或 success usage。
- 公开错误只保留 request/error ID 和稳定类别，原始 upstream body 仅进入脱敏内部诊断。

## 验证与残余风险

历史 5 条样本只证明现象。最终 C02/C03/C04、D05 和 L4 必须各 5 轮并验证错误后 5/5 normal 恢复。未知上游 error event 类型仍可能绕过 classifier；默认应 fail closed。回滚不得恢复已提交后的自动 retry 或 success terminal。

## 关联

- 生产证据：`tmp/analysis-usage-llm-errors/root-causes/06-stream_upstream_status_error/`。
- 相关：[[07-stream-internal-read-error]]（同为流式链路瞬态）、[[02-stream-upstream-idle-timeout]]。
