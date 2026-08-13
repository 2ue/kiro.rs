# P5 - 并发低、RPM 高、页面卡与调度观测口径

日期：2026-07-24

## 状态

已完成源码链路复核和第一批观测/保护修复；已补入口 per-key 本地临时调度失败 backoff；仍需隔离假上游执行更大并发与异常恢复矩阵。

## 用户补充现象

- 有时没有中途导入/批量调参也会出现异常。
- 正常时单账号并发打满 10/20 没问题。
- 异常时账号本身没有认证问题，但页面看到账号并发快速下降，本地池不可用或全部转外部池。
- dashboard/usage 页面卡住，RPM 一段时间不显示，之后突然显示较大值，例如 1k+ RPM。
- 接入端看到的 RPM 不高；接入端并发可能较大并大于 RPM，但不是极端高。
- 全局并发已配置限制，但仍发生“并发低、RPM 高、页面慢、账号不可调度”的组合现象。

## 当前源码复核结论

这组现象不能用单一指标解释，至少有四个不同口径：

1. request API key admission 并发
   - 入口层是进程内限流/并发/排队，不依赖 Redis。
   - 流式响应会持有 permit 到 body 结束；立即失败响应会很快释放 permit。
   - 因此快速失败可以产生较高 completed-error RPM，但不会长期占用入口并发。

2. 本地账号调度并发
   - 账号页的 `in_flight_requests` 来自 token manager 运行态，一般是本地 lease 与 Redis 分布式 lease 合并后的结果。
   - Redis degraded、risk circuit、冷却、warmup、模型不兼容、代理不可用都会使 `dispatchable` 下降。
   - `dispatchable=0` 或 `local_all_disabled` 不等于数据库 `credentials.disabled=true`。

3. Usage realtime RPM
   - 现有 realtime RPM 是近 60 秒内 usage record 的完成/记录窗口，不是“真实打到上游的发送 RPM”。
   - 快速本地失败、admission 拒绝采样、流式长请求集中结束、writer 延迟批量落库，都可能让页面 RPM 与接入端上游发送 RPM 不一致。

4. Dashboard/Usage 页面响应
   - summary/dashboard 会同步等待 Redis/PgSQL 聚合结果；当前已使用本地 admin shadow cache 和可选 observability Redis。
   - 如果没有独立 observability Redis，则不会回落业务 Redis，而是用 PgSQL/进程内。
   - PgSQL 高基数聚合慢仍可能导致管理页慢，但不应直接占用本地账号 lease。

## 已修复

1. Usage realtime 新增成功/错误细分：
   - `successRequests`
   - `errorRequests`
   - `successRpm`
   - `errorRpm`

   两套 usage 页面已显示实时成功/错误请求数，便于区分高 RPM 是成功完成还是错误快失败。

2. request admission 采样诊断文案从内部实现口径改为功能口径：
   - 旧：`sampled request rejection`
   - 新：`request rejected before upstream dispatch`

3. local-pool risk circuit 打开后，route preflight 和 acquire 都先走进程内 circuit，不再先碰 Redis scheduler 热路径。

4. 全局调度容量配置变化会重置活跃凭据 warmup：
   - `credential_rpm`
   - `credential_max_concurrent_requests`
   - `dispatch_global_max_concurrent_requests`
   - `weighted_capacity`
   - `load_balancing_mode`
   - warmup 参数

5. 单凭据 RPM/并发修改继续重置该凭据 warmup。

6. request API key admission 增加本地临时调度失败 backoff：
   - 只在最终要向下游返回本地调度临时失败/风险保护失败时触发。
   - 触发信号限定为 `local_pool_risk_circuit_open`、`local_scheduler_redis_degraded`、`Redis 调度协调状态不可用`、本地账号调度容量/队列/并发槽位不可用等明确本地调度失败。
   - 普通上游 `429 Too Many Requests` 不触发该 backoff，避免误伤真实上游/外部池限流。
   - backoff 为进程内、按 request API key 生效，不依赖 Redis；默认按错误中的 `retry_after_secs`，并限制在 1-8 秒，避免一次故障长期压制恢复探测。
   - backoff 命中时，在入口层直接返回 429 + `retry-after`，不会进入 provider、本地账号调度或 Redis scheduler 热路径。

## 复现/验证方法

### 单元/聚焦测试

- `summary_counts_high_cache_and_sources`
- `redis_usage_summary_and_dashboard_are_materialized`
- `runtime_capacity_updates_reset_active_credential_warmup`
- `priority_mode_respects_warmup_candidate_share`
- `local_pool_risk_circuit_stops_burning_remaining_credentials`
- `auxiliary_focus_attempt_limits_map_to_public_temporary_failure_without_internal_terms`
- `test_sonnet_high_concurrency_dispatch_respects_limits_and_spreads_load`
  - 600 请求，24 账号，4 个预冷却，global limit 48，per credential limit 3；
  - 最终 20 个非冷却账号全部被使用，单账号选择分布 29-31 次，`global_in_flight` 和单账号 in-flight 均未越界。
- `test_model_scoped_429_high_concurrency_disabled_and_model_filters`
  - 混合 sonnet/opus 240 请求，验证模型级 429 冷却、禁用账号、模型过滤和 global limit 不互相污染。
- request API key admission：
  - `single_key_rpm_limit_is_stable_for_five_rounds`
  - `concurrency_is_isolated_between_keys_for_five_rounds`
  - `queue_is_bounded_and_wakes_after_release_for_five_rounds`
  - `queued_requests_are_fifo_and_newcomers_cannot_bypass_for_five_rounds`
  - `local_temporary_backoff_rejects_before_rpm_for_five_rounds`
  - `local_temporary_backoff_retry_after_is_bounded`
  - `local_temporary_backoff_wakes_and_rejects_queued_waiters`
  - `local_temporary_backoff_is_disabled_when_admission_is_disabled`
  - `local_scheduler_error_enables_admission_backoff_classification`
  - `upstream_rate_limit_does_not_enable_local_admission_backoff`

### 后续隔离负载测试

必须使用 fake upstream/temp port，不打生产、不探测已有 `9022`：

1. 正常 stream/non-stream 基线。
2. 突发 429/500/invalid tool 后恢复。
3. 突发慢首字节/慢 thinking 后恢复。
4. 本地 scheduler Redis latency/disconnect 注入。
5. 多账号 `TEMPORARILY_SUSPENDED` mock，验证 risk circuit 不继续探测剩余账号。
6. 大并发快速失败，验证：
   - admission active/queued 释放；
   - local `globalInFlight` 不虚高；
   - realtime `errorRpm` 显示错误快失败；
   - 后续正常请求恢复。
7. PgSQL/usage 聚合压力下，管理页 summary/dashboard 不影响本地账号 lease。

## 仍需关注

- `created_at` 目前仍是 usage record 生成/完成时间。没有直接改为请求开始时间，避免影响历史清理、排序、rollup 口径。
- 如果生产继续出现跨实例“同一 key 大量快速失败”，下一步可以做跨实例 collapse/backoff。但不能依赖业务 Redis 热路径，避免把 observability/调度竞态重新引入核心请求链路。
- 跨实例 risk circuit 还没有做成全局共享。若做，不能依赖业务 Redis 热路径，否则会重新引入 Redis 竞态。
