# 现网并发积压与长耗时请求因果分析

- 分析日期：2026-09-25
- 代码基线：`main` 分支，HEAD `79d8de8`，`Cargo.toml` version = 0.0.170
- 分析方式：只读静态代码分析，未修改任何代码、未启动任何服务
- 分析边界：仅回答"当前系统是否会产生该现象"及因果方向，不含修复实施

---

## 一、分析背景：现网观测到的问题

### 1.1 现网配置

| 项 | 值 |
|---|---|
| Kiro 账号数 | 10 |
| 单账号并发位 | 6 |
| 单账号 RPM 限制 | 30 |
| 请求形态 | 长连接流式请求，部分单请求耗时超过几百秒 |

### 1.2 观测到的现象序列

1. 某个时间点，系统并发量"超级多"，超过了 10 个账号理论上能容纳的并发量；
2. 同一时段请求普遍耗时很长，部分单请求耗时超过几百秒；
3. 积压并发越来越多，请求持续排队；
4. **整个过程中 RPM 一直非常低**；
5. 并发持续攀升到某个点后回落，随后维持在一个水平（推测与调用方不再发起更多调用有关）。

### 1.3 提问者明确无法判断的点（本次分析的核心问题）

> 请求耗时长，不确定是**因为请求时间太长导致并发占用积压**，还是**因为积压导致请求时间长**。

以及一个补充事实：

> 我只是看到了这个现象，没有在开始的时候观察到，或者我也没办法观察到。

即：几百秒的耗时，究竟是调度到的 Kiro 上游本身慢，还是排队等待造成的？提问者认为自己看到的只是**表现层**，不是**根因层**——这个判断是正确的。

### 1.4 本文要回答的三个问题

1. 当前系统是否会产生上述现象？
2. 因果方向是哪一个？是否存在双向反馈？
3. 为什么现网"观察不到"？是操作问题还是系统缺陷？

---

## 二、结论摘要

| # | 结论 | 性质 |
|---|---|---|
| 1 | 现象**会**产生，且核心机制是设计取舍而非 bug | 确认 |
| 2 | "RPM 低 + 并发打满"同时出现是长请求场景的算术必然，瓶颈在并发位而非 RPM | 前提纠正 |
| 3 | 主因果方向是**请求耗时长 → 并发积压**；排队在结构上贡献不了几百秒 | 确认 |
| 4 | 存在真实但次要的反向放大路径（空闲超时重试重新抢槽） | 确认 |
| 5 | 排队耗时与上游耗时在遥测中被混算，现网**在原理上无法区分**两个方向 | **埋点缺陷** |
| 6 | RPM 令牌在排队前就扣减，统计反映"尝试量"而非"上游成交量" | **计量缺陷** |
| 7 | 不存在并发槽泄漏 | 排除 |
| 8 | 不存在权重放大 | 排除 |
| 9 | "涨到某点回落后维持"是有界队列的保护机制正常工作 | 排除 |
| 10 | 若 `credential_dispatch_max_wait_secs` 被设为 0，结论 3 不成立 | **需现网确认** |

---

## 三、前提纠正：RPM 低与并发打满并不矛盾

10 账号 × 6 并发位 = **60 个上游并发槽**。

若单请求平均耗时 300s，稳态吞吐上限为：

```
60 槽 / 300s ≈ 0.2 req/s ≈ 12 req/min
```

而 RPM 天花板是 `10 × 30 = 300 req/min`。

也就是说：**在长连接场景下，RPM 限制永远碰不到，它在这个场景里根本不起作用。** 系统的唯一约束是并发槽数量。

因此"RPM 一直非常低但并发一直涨"不需要额外解释——它是长请求占位的直接算术后果。

这带来一个实际风险：**继续用 RPM 作为健康度或负载指标会持续误判**，因为 RPM 衡量的是请求进入速率，不是上游产能。

---

## 四、因果方向判定

### 4.1 排队侧：所有等待都有界，结构上贡献不了几百秒

系统有两层准入，两层的等待都有硬上限：

| 层 | 参数 | 默认值 | 代码位置 |
|---|---|---|---|
| 请求 API Key 层（Layer A） | `queue_timeout_ms` | **1000 ms** | `src/model/config.rs:3354` |
| 凭据/账号层（Layer B） | `credential_dispatch_max_wait_secs` | **5 s** | `src/model/config.rs:4191` |

```rust
// src/model/config.rs:4191
fn default_credential_dispatch_max_wait_secs() -> u64 {
    5
}
```

凭据层超时后直接放弃并返回"排队等待超时"，不会无限挂住（`src/kiro/token_manager/manager.rs:6428-6478`）。

等待过程中的单次休眠也被双重裁剪（`src/kiro/token_manager/manager.rs:4843-4870`）：

```rust
let fallback = if self.redis_store.is_some() {
    StdDuration::from_secs(1)
} else {
    StdDuration::from_secs(CONCURRENCY_WAIT_WAKEUP_SECS)   // 30
};
let mut wakeup = wait_for.unwrap_or(fallback).min(fallback);
if let Some(remaining) = max_wait_remaining {
    wakeup = wakeup.min(remaining);        // 再被剩余预算裁剪
}
```

**两层排队相加上限约 6 秒。几百秒不可能来自排队。**

### 4.2 上游侧：耗时没有上限，且慢请求可以无限期占位

这是一个明确的设计决定：

```
// src/kiro/provider.rs:152-153
/// Kiro provider 不设置 reqwest 整请求总超时：流式正文由 Anthropic SSE idle timeout 管控，
/// 请求头和非流式 body 分别由专门的 timeout helper 管控。
```

只有三类超时存在：请求头超时、非流式 body 超时（`kiro_upstream_response_timeout_secs` 默认 180s，`src/model/config.rs:4195`）、SSE **空闲**超时（`kiro_upstream_stream_idle_timeout_secs` 默认 180s，`src/model/config.rs:4199`）。**没有整请求总墙钟上限。**

关键在于流式转发循环里每收到一个上游 chunk 会同时做两件事（`src/anthropic/handlers.rs:8748-8759`）：

```rust
Some(Ok(chunk)) => {
    state.usage_guard.context().request.mark_first_upstream_chunk();
    state.idle_deadline = Instant::now()
        + Duration::from_secs(state.stream_idle_timeout_secs);   // 重置空闲超时
    state.completion.touch();                                    // 续租 in-flight lease
```

`touch()` 沿 `src/kiro/provider.rs:983-987` → `src/kiro/token_manager/concurrency.rs:721-753` 更新 lease 的 `last_seen_at`。

而 lease 回收的兜底机制判据正是 `last_seen_at`（`src/kiro/token_manager/manager.rs:4686-4700`）：

```rust
entry.in_flight_leases.retain(|lease| {
    let keep = now.saturating_duration_since(lease.last_seen_at) <= max_age;
    ...
});
```

`max_age` 即 `credential_in_flight_lease_max_secs`，默认 **900s**（`src/model/config.rs:4254`）。

**结论：一个"慢但持续有输出"的上游请求，既不会触发 180s 空闲超时，也不会被 900s lease 回收兜底命中，可以无限期持有并发槽。** 这是几百秒耗时 + 并发槽被长期占满的直接来源。

### 4.3 反向路径：真实存在，但只是放大器

`kiro_upstream_stream_retry_on_idle_timeout` 默认为 **true**（`src/model/config.rs:3713`）。空闲超时触发重试后，请求会重新进入整个凭据选择循环、再次竞争已经稀缺的并发槽，把这条请求的墙钟时间进一步拉长。

这构成"积压 → 耗时"的反馈环，但它**只能在已有长耗时的基础上叠加**，无法凭空产生几百秒。

### 4.4 判定

> **主方向：请求耗时长（上游慢）→ 占满并发槽 → 后续请求积压排队。**
> **次要方向：积压 → 重试竞争加剧 → 个别请求墙钟时间被进一步拉长。**

排队本身不是几百秒耗时的来源，它是结果而非原因。

---

## 五、为什么"观察不到"：这是系统埋点缺陷，不是操作问题

提问者说"我也没办法观察到"——这个判断准确，原因在代码里：

### 5.1 遥测中没有排队耗时字段

`UsageLatencyTrace`（`src/anthropic/usage.rs:218` 起）拥有十几个时延字段：`payload_guard_ms`、`upstream_header_ms`、`first_upstream_chunk_ms`、`first_output_delta_ms`、`first_thinking_delta_ms`、`first_visible_text_delta_ms`、`stream_gap_to_first_output_ms`、`stream_retry_attempts`、`client_dropped_ms`、`terminal_reason` 等。

**但没有任何字段记录凭据层排队等待耗时。** 全仓库检索 `queue_wait_ms` / `queued_ms` / `wait_duration` 无结果。

### 5.2 计时起点早于选号，导致排队时间被算进"上游耗时"

`UsageContext` 的 `started_at` 在构造时打点（`src/anthropic/handlers.rs:4807`）：

```rust
started_at: Instant::now(),
```

该位置在 request admission 中间件**之后**、凭据选择（Layer B 排队）**之前**。

而两个关键指标都以它为基准：

```rust
// src/anthropic/handlers.rs:2661-2669
fn mark_upstream_header(&self) {
    let elapsed = self.elapsed_ms();          // = started_at.elapsed()
    ...
}

// src/anthropic/handlers.rs:4271
let duration_ms = self.request.started_at.elapsed().as_millis() as u64;
```

于是 `upstream_header_ms` 与 `response_latency_ms` **静默包含了凭据层排队时间**。

**后果：从现有遥测数据在原理上无法区分"上游慢"与"排队久"。** 这正是本次现网问题无法定位根因的直接原因，属于真实的埋点缺陷。

---

## 六、附带发现的计量缺陷：RPM 在排队前扣减

`src/anthropic/request_admission.rs:491-518`：

```rust
if initial_config.rpm > 0 {
    self.reserve_rpm(&state, initial_config.rpm)?;      // 先扣 RPM 令牌
}
let counted_concurrency = if initial_config.max_concurrent_requests > 0 {
    self.acquire_concurrency(state.clone(), initial_config).await?   // 后排队获取并发
} else {
    false
};
```

排队超时（`QueueTimeout`）或被拒（`ConcurrencyFull` / `QueueFull`）的请求，**RPM 额度已经被消耗**。

因此 RPM 统计反映的是"尝试量"而非"上游成交量"。在本次现网场景中，这会与第三节的误判叠加：既低估了真实压力，又无法用 RPM 判断上游产能。

---

## 七、排除清单（已验证不是问题，避免重复排查）

| 项 | 验证结论 | 依据 |
|---|---|---|
| 并发槽泄漏 | 不存在。`RateLimited` 重排队前显式释放 lease；两类 guard 均实现 `Drop` | `manager.rs:6583-6584` 的 `drop(in_flight_lease)`；`concurrency.rs:776`、`:922` 的 `impl Drop` |
| 权重放大（6 个位实际不足 6 并发） | 不存在。`weighted_capacity.enabled` 默认 `false`，1 请求 = 1 槽 | `src/model/config.rs:565`；`src/anthropic/handlers.rs:2230-2239` |
| 客户端断连是观测盲区 | 不是。断连有独立埋点 `client_dropped_ms` | `handlers.rs:2917-2925`、`:4160-4163`、`:4505` |
| "并发涨到某点回落后维持"是失控信号 | 不是。有界队列打满后开始拒绝，调用方退避，系统进入稳态——保护机制正常工作 | `request_admission.rs:776-861` 的 `ConcurrencyFull` / `QueueFull` / `QueueTimeout` |
| 管理面看不到并发与排队 | 能看到。快照已分别暴露 in-flight 与 queued | `src/kiro/token_manager/admin_snapshot.rs:253-269`；`src/admin/types.rs:100-103` |

---

## 八、需要现网确认的边界条件

`dispatch_max_wait` 在配置值为 `0` 时返回 `None`，此时凭据层等待**变为无界**（`src/kiro/token_manager/manager.rs:5042-5069`）：

```rust
fn dispatch_max_wait(&self, acquire_mode: AcquireMode) -> Option<StdDuration> {
    if let Some(max_wait) = acquire_mode.max_wait_override() {
        return Some(max_wait);
    }
    let secs = self.config.lock().credential_dispatch_max_wait_secs;
    (secs > 0).then(|| StdDuration::from_secs(secs))     // 0 → None → 无界
}
```

`dispatch_wait_exceeded` 与 `dispatch_wait_remaining` 在收到 `None` 时同样返回 `None`，即不再做超时判定与预算裁剪。

**若现网将 `credential_dispatch_max_wait_secs` 配置为 0，第四节"排队有界、贡献不了几百秒"的结论不成立，因果方向需要重新判定。** 请优先核对该值。

---

## 九、建议（按性价比排序，未实施）

### 1. 补齐排队/上游分离埋点（最高优先）

- 在 `UsageLatencyTrace` 增加 `dispatch_queue_wait_ms`；
- 将上游计时起点从 `UsageContext` 构造处移到**拿到凭据之后**，或额外记录一个 `credential_acquired_at`。

不做这一步，后续所有容量调优都建立在猜测上，本次问题也无法在复现时定位。

### 2. 为超长流式请求增加总墙钟上限

当前只有空闲超时，持续吐 token 的慢上游可永久占位。建议增加可配置的整请求总时长上限，超限后主动释放并发槽，避免单个异常请求长期消耗产能。

需权衡：会截断合法的长时间推理请求，上限值应保守设置并可按模型区分。

### 3. 将 RPM 扣减移到并发获取成功之后

让 RPM 指标真实反映上游成交量，消除第六节的计量偏差。

### 4. 按长连接场景重新校准容量

长连接下 RPM=30 无实际约束力，真正该调的是并发位数与队列深度。建议按 `所需槽位 = 目标吞吐 × 平均请求耗时` 反推，例如目标 60 req/min、平均 300s，则需约 300 个槽位——这个数字会直接说明当前 60 槽的容量差距。

---

## 十、本文与既有审计文档的关系

本文聚焦单一现网现象的因果定位，与 [`service-problem-audit-v0.0.170-20260924.md`](./service-problem-audit-v0.0.170-20260924.md) 的全量问题审计互补。第五节的埋点缺陷、第六节的计量缺陷为本次分析新发现，未包含在该审计文档的 8 项问题中。
