# 流式上游空闲超时（响应已提交导致首字前重试不能安全小改）

- 状态：根因已定位；当前仅保持可观测错误，不做 unsafe retry；首输出前重试需要响应提交重构
- 严重级别：中 —— 生产近 12 小时 41 条（占非成功请求 14.6%，非工具类里最大）
- 影响端点：全部流式端点（本次样本覆盖 `/cc`、`/ha`、`/dfcache/ka`、`/v1`）
- 分类来源：`tmp/analysis-usage-llm-errors` root-cause `02-stream_upstream_idle_timeout`

## 现象

流式请求上游返回响应头后，长时间不产生数据，被本地按空闲超时掐断：

```
api_error: upstream stream idle timeout
terminalReason: upstream_idle_timeout
errorStatusCode: 200   （响应头已 200，随后静默）
```

- `durationMs` ≈ 182000–185000ms，即 `kiroUpstreamStreamIdleTimeoutSecs` 默认 **180s** + header 时间后掐断。
- `credentialAttemptCount: 1`、`routeSubtype: local_error_no_fallback` —— **不重试、不换号**。

## 全量特征（41 条）

- **两种形态**：
  - **完全静默 22 条**：收到 header 后 **0 个 chunk**（`firstUpstreamChunkMs` 为 None）。
  - **出过 chunk 19 条**：其中 **13 条无可见文本**（`firstVisibleTextDeltaMs` 全为 None）—— 首个 chunk 是 thinking 或空事件，之后卡死。
- **模型**：`claude-sonnet-5` 21、`claude-opus-4-8` 10、`claude-opus-4-7` 10。
- **maxTokens**：多为大输出（64000 占 24 条，32000 占 7 条）。
- **header 延迟**：`upstreamHeaderMs` 中位数 ~2700ms（上游接单正常，卡在出字阶段）。

## 性质判定：上游为主，程序可部分规避

- 主体是**上游行为**：Bedrock 侧接单后长时间不吐字，常见于大 `max_tokens` + 深度 thinking 或上游排队。程序无法让上游更快出字。
- **程序可规避空间（关键改进点）**：
  - 从业务语义看，**22 条"完全静默、0 chunk"发生在首字之前**，如果下游还没收到任何 SSE 内容，换号重试是安全的。
  - 但当前实现会先生成并向下游发送 `message_start` / 初始 usage，然后才读取上游流。也就是说，即使上游还没吐 chunk，下游响应也已经提交；此时在同一个 HTTP/SSE 响应里换号重试，会产生“已提交旧请求初始事件 + 新请求内容”的协议风险。
  - 因此本轮不做“小改硬重试”。正确改法应先重构 stream 响应提交时机：延迟 initial events，或引入等价的协议安全缓冲，确保在真正向下游提交前才允许换号。
  - 出过 chunk 的 19 条（尤其已出可见文本的）**不可安全重试** —— 重试会导致下游收到重复内容。这类只能靠调参缓解。
  - `kiroUpstreamStreamIdleTimeoutSecs=180s` 对交互式场景偏长，可评估下调，让静默失败更早暴露、更早触发（首字前的）重试。

## 复现说明

依赖上游瞬态状态（接单后静默 180s），**无法在本地稳定复现**。可通过单测/集成测试模拟：构造一个"返回 200 header 后不产生 chunk"的假上游流，断言：
1. 首字前静默 → 触发换号重试（改进后）。
2. 已出可见文本后静默 → 不重试，返回超时错误（保持现状）。

## 改进方案（需独立重构，中优先级）

1. **响应提交重构**：把 `message_start` 等初始事件延迟到“已选定最终上游尝试且可继续输出”之后，或至少在首 chunk 前保持服务端缓冲。只有下游尚未收到任何 SSE bytes 时，才允许对当前请求换号。
2. **首输出前重试**：重构完成后，在 idle timeout / 上游 2xx JSON 错误体 / 首输出前读流错误等分支里统一判定“是否未向下游提交”，满足条件才换号重试。
3. **首输出后保守失败**：一旦已经向下游发送任何可见文本、tool_use、thinking 或 keepalive，不重试，继续返回 SSE error，避免重复输出。
4. **超时调参**：评估将 `kiroUpstreamStreamIdleTimeoutSecs` 从 180s 下调（如 60–90s），平衡"给上游足够出字时间"与"尽早失败重试"。需结合大 `max_tokens` + thinking 场景的正常首字延迟分布确定阈值。
5. **诊断**：在 latencyTrace 记录“是否触发首输出前重试、重试前是否已提交 initial events”，便于回归观测。

## 边界与风险

- 重试安全性是硬约束：**必须**严格以"是否已向下游发出任何 SSE bytes"为界，而不仅是“是否有可见文本”。当前实现已先发 `message_start`，所以不满足安全重试条件。
- 下调超时过激进会误杀正常的慢首字请求（大输出 + 深度 thinking 合法地需要较长首字时间）。

## 回归清单

- [ ] 响应提交重构后：模拟"首字前静默且未提交任何 SSE bytes" → 触发换号重试，最终成功或以更清晰错误返回。
- [ ] 模拟"已提交 message_start 但未出可见文本后静默" → 不重试，返回超时错误（无混合响应）。
- [ ] 模拟"已出可见文本后静默" → 不重试，返回超时错误（无重复输出）。
- [ ] 下调超时后，正常慢首字请求（大 maxTokens + thinking）不被误杀。
- [ ] latencyTrace 正确标记首字前/后与重试动作。

## 关联

- 同为流式瞬态：`docs/feature/06-stream-upstream-status-error.md`、`docs/feature/07-stream-internal-read-error.md`。
- 生产证据：`tmp/analysis-usage-llm-errors/root-causes/02-stream_upstream_idle_timeout/`。
- 代码：`src/kiro/provider.rs`（SSE idle timeout 处理）、`src/model/config.rs`（`kiro_upstream_stream_idle_timeout_secs` 默认 180）。
