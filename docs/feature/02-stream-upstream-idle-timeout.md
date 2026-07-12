# 流式上游空闲超时（首字前不重试）

- 状态：根因已定位，程序侧有明确健壮性改进空间，改动未实施
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
  - **22 条"完全静默、0 chunk"发生在首字之前**，此时当前请求尚未向下游发出任何内容 —— **换号重试是安全的**（不会产生重复/拼接输出）。
  - 当前实现（`src/kiro/provider.rs:298` 附近）在 idle timeout 时只让调度器**短暂避开该凭据**，但**不对当前请求发起重试**，直接把超时错误返回给客户端。这是可改进的健壮性缺口。
  - 出过 chunk 的 19 条（尤其已出可见文本的）**不可安全重试** —— 重试会导致下游收到重复内容。这类只能靠调参缓解。
  - `kiroUpstreamStreamIdleTimeoutSecs=180s` 对交互式场景偏长，可评估下调，让静默失败更早暴露、更早触发（首字前的）重试。

## 复现说明

依赖上游瞬态状态（接单后静默 180s），**无法在本地稳定复现**。可通过单测/集成测试模拟：构造一个"返回 200 header 后不产生 chunk"的假上游流，断言：
1. 首字前静默 → 触发换号重试（改进后）。
2. 已出可见文本后静默 → 不重试，返回超时错误（保持现状）。

## 改进方案（未实施，中优先级）

1. **首字前重试**：在 idle timeout 处理中区分"是否已向下游发出可见内容"。若**未发出任何输出**，对当前请求换号重试（复用现有 credential 重试链路），而非直接失败。
2. **超时调参**：评估将 `kiroUpstreamStreamIdleTimeoutSecs` 从 180s 下调（如 60–90s），平衡"给上游足够出字时间"与"尽早失败重试"。需结合大 `max_tokens` + thinking 场景的正常首字延迟分布确定阈值。
3. **诊断**：在 latencyTrace 已有 `firstVisibleTextDeltaMs` 基础上，记录"是否触发首字前重试"便于回归观测。

## 边界与风险

- 重试安全性是硬约束：**必须**严格以"是否已向下游发出可见内容"为界，误判会导致下游收到重复输出。
- 下调超时过激进会误杀正常的慢首字请求（大输出 + 深度 thinking 合法地需要较长首字时间）。

## 回归清单

- [ ] 模拟"首字前静默" → 改进后触发换号重试，最终成功或以更清晰错误返回。
- [ ] 模拟"已出可见文本后静默" → 不重试，返回超时错误（无重复输出）。
- [ ] 下调超时后，正常慢首字请求（大 maxTokens + thinking）不被误杀。
- [ ] latencyTrace 正确标记首字前/后与重试动作。

## 关联

- 同为流式瞬态：`docs/feature/06-stream-upstream-status-error.md`、`docs/feature/07-stream-internal-read-error.md`。
- 生产证据：`tmp/analysis-usage-llm-errors/root-causes/02-stream_upstream_idle_timeout/`。
- 代码：`src/kiro/provider.rs`（SSE idle timeout 处理）、`src/model/config.rs`（`kiro_upstream_stream_idle_timeout_secs` 默认 180）。
