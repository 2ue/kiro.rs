# 凭据调度热路径性能分析与最终改造记录

本文档记录 `src/kiro/token_manager.rs` 的请求时凭据调度路径，以及外部备用号池在本次优化后的验证范围。

目标不是只修某一个禁用清理按钮，也不是只优化某个单测，而是把“凭据列表展示”和“请求调度”两类数据面分开后，继续把调度热路径里会随凭据数量和请求量放大的成本收敛掉。

## 调度路径

本地凭据池主调度路径：

```text
acquire_context_for_session_with_mode
  -> refresh_scheduler_state_from_redis()
  -> cleanup_expired_in_flight_leases_throttled()
  -> get_bound_credential() 或 select_next_credential_excluding()
  -> acquire_in_flight_slot()
  -> try_ensure_token()
  -> mark_rate_limited_at()
  -> record_scheduler_selection()
```

外部备用号池路径：

```text
ExternalPoolManager::forward_with_failover_result
  -> select_pool()
  -> acquire_pool()
  -> forward_once()
  -> record_external_success() 或 record_external_failure()
```

本地调度仍以本地内存为读模型，Redis 用于跨实例共享状态、冷却、lease、会话绑定和外部池容量。外部备用池仍依赖 PgSQL 池列表和 Redis 并发 lease，不把这些状态塞进本地凭据列表接口。

## 原始性能风险

### 1. Admin 凭据列表和运行态混在一个响应里

凭据列表页以前会一次性拉取完整 `CredentialStatusItem`，其中既有基础信息，也有运行态、账号信息、用量汇总、调度评分、冷却、并发 lease 等字段。200 多个凭据时，即使只是打开二次确认弹窗，也可能先触发大对象列表刷新、运行态同步和 React 大量对象 diff。

这不是“200 条基础信息很多”的问题，而是列表设计把基础信息和高频变化信息绑定在一起，导致任何轻量交互都可能付出全量运行态成本。

### 2. 本地调度每次选择都扫描全量凭据

`select_next_credential_excluding()` 必须过滤禁用、模型不兼容、代理不可用、冷却、限速、并发占满等状态。O(n) 扫描本身对几百个凭据还能接受，但在 500-1000 RPM、并发等待、候选反复竞争 lease 时，会成为稳定成本。

### 3. `health_balanced` 存在重复计算

旧逻辑会在每个候选评分时重新统计候选集合的近期选中次数，并对所有候选做全量排序。候选数变大时，这会把健康调度从可控的 O(n) 推向不必要的 O(n²) 与 O(n log n)。

### 4. 全局容量判断太晚

旧选择逻辑只看单凭据并发容量。全局并发满时仍可能选中一个凭据，然后在 `acquire_in_flight_slot()` 才发现全局 lease 不可占用。这会多走一次选择、排队和重试路径，也会让诊断分支更容易误判。

### 5. `snapshot()` 存在本地重入锁死风险

`snapshot()` 持有 `entries` 锁后调用 `global_capacity_state()`。本地无 Redis 模式下，`global_capacity_state()` 又会锁 `entries`，造成自死锁。之前两个看似“调度等待超时”的单测实际卡在 `snapshot()`：

- `test_balanced_mode_rotates_all_warming_credentials_by_recent_selection`
- `test_global_capacity_limits_dispatch_and_bounds_wait_queue`

这解释了为什么单次 acquire/release 已经完成，测试仍能卡 120 秒。

### 6. 外部备用号池也必须验证

只验证本地凭据池不够。真实请求路径还可能在本地容量满、错误、熔断或策略直连时进入外部备用号池。必须覆盖：

- 备用池总开关关闭；
- 单个池禁用；
- 多个备用池按优先级和负载选择；
- 单池容量满后选择下一个池；
- 全局外部池容量满时不是“无备用池”，而是“有备用池但临时容量不可用”。

## 已完成实现

### 凭据列表数据面分离

后端新增轻量列表与分片运行态接口：

- `GET /api/admin/credentials-list`
- `GET /api/admin/credentials/list`
- `GET /api/admin/credentials/summary`
- `GET /api/admin/credentials/runtime?ids=...`
- `GET /api/admin/credentials/account-info?ids=...`
- `GET /api/admin/credentials/usage-summary?ids=...`
- `DELETE /api/admin/credentials/disabled`

前端首屏使用基础列表和 summary；当前页再按 id 拉取 runtime/account/usage。清除已禁用的二次确认不再先拉完整凭据列表，而是直接用 summary 的 disabled count 展示确认。

### 本地调度热路径

`select_next_credential_excluding()` 现在在一次选择 pass 内完成：

- 克隆一次 `Config` 用于限制和评分参数；
- 读取一次当前 `load_balancing_mode`；
- 预先计算本地全局 in-flight；
- 在全局容量已满时直接返回无候选，让上层进入等待/队列分支；
- 一次扫描中收集 `available`、`ready`、`warming`、`total_recent`、`warming_recent`；
- 预热选择用 `should_select_warming_from_totals()`，不再重复扫描候选集合；
- `balanced` 仍按原有 `balanced_selection_key()` 选择；
- `priority` 仍按原有 `priority_selection_key()` 选择；
- `health_balanced` 仍在 top-k 中加权抽样，但 top-k 通过有界有序插入维护，不再全量排序所有候选。

`health_balanced` 的压力计算现在使用：

```text
selection_pressure_from_totals(entry, total_recent, candidate_count)
```

避免每个候选重新 sum 整个候选集合。

`scheduler_score_with_config(..., &Config)` 复用同一个配置快照，避免每个候选评分都锁 `config`。

### 等待和容量诊断

当无候选时，诊断分支现在会在同一个 entries 锁内计算：

- `available`
- `usable`
- `dispatchable`
- `global_in_flight`
- `global_has_capacity`
- `dispatch_candidate_count`
- `cooldown_blocked`
- `concurrency_blocked`
- `wait_for`

全局容量满会被归类为并发容量阻塞，进入等待队列或 fail-fast 错误，不再反复“重检恢复可用”。

### `snapshot()` 死锁修复

本地无 Redis 模式下，`snapshot()` 不再持有 `entries` 锁后重新调用会锁 `entries` 的 `global_capacity_state()`。它直接使用当前锁内已经计算出的本地 `global_in_flight` 和 `queued_requests`。

Redis 模式仍走 Redis 全局容量读取，用于跨实例运行态。

### 外部备用号池验证支撑

`ExternalPoolManager::skip_reason()` 改成不依赖 `self` 的关联函数，便于纯函数测试开关、禁用、冷却、单池容量和全局容量。

`select_external_pool_candidate()` 抽出为可测试的选择函数，`select_pool()` 和测试共用同一套多池选择逻辑：

- priority 越小越优先；
- 同 priority 下选择负载分数更低的池；
- 仍保留同优先级同负载时随机选一个最佳候选，避免长期固定命中同一个池。

补充了外部池 manager 集成测试。该组测试需要同时配置：

- `KIRO_RS_TEST_POSTGRES_URL`
- `KIRO_RS_TEST_REDIS_URL`

未配置时会跳过，不影响无外部依赖的单元测试。

## 行为保持不变

以下语义保持稳定：

- 禁用凭据不参与调度；
- 模型不兼容凭据不参与调度；
- 代理资源缺失或禁用的凭据不参与调度；
- 冷却和限速仍按原规则阻塞；
- 单凭据并发限制仍在本地/Redis lease 层强制；
- 全局并发限制仍在本地/Redis lease 层强制；
- sticky session 不会绕过硬过滤；
- 成功不会清掉并发失败写入的有效冷却；
- 外部备用池关闭时不路由；
- 外部备用池全局容量满时返回容量语义，不伪装成“没有备用池”。

## 调度参数作用点

本轮把调度相关参数逐项核对到实际代码路径，并补了能证明参数生效的测试。

本地凭据池参数：

- `load_balancing_mode`：运行时锁内读取，影响 `priority`、`balanced`、`health_balanced` 三种候选选择策略；`set_load_balancing_mode()` 和 `update_runtime_config()` 会更新内存配置。
- `credential_rpm`：转成单凭据最小请求间隔；成功拿到上下文后写入 `rate_limit_available_at`，下次选择会优先换其他凭据或等待。关闭该参数会清掉现有限速状态。
- `credential_max_concurrent_requests`：单凭据并发上限；凭据自己的 `max_concurrent_requests` 可覆盖全局值，`0` 表示不限。
- `dispatch_global_max_concurrent_requests`：本地凭据池全局并发上限；选择前先判断全局容量，满了就进入容量等待或 fail-fast，不再先选中凭据再发现全局满。
- `dispatch_max_queued_requests`：等待本地调度容量的全局队列上限；超过后直接返回“等待队列已满”。
- `credential_dispatch_max_wait_secs`：等待本地 RPM/并发容量的最大时间；到期返回“凭据调度排队等待超时”。
- `credential_in_flight_lease_max_secs`：泄漏并发 lease 的最大存活时间；开启后调度前会清理异常占用并唤醒等待请求。
- `credential_retry_max_attempts`：provider 层单次上游请求的凭据重试上限；`0` 为自动模式，小账号池至少 9 次，大账号池至少覆盖一轮凭据。`token_manager` 自身获取 token 还有 `凭据数 * MAX_FAILURES_PER_CREDENTIAL` 的硬上限，两层都不是无限循环。
- `credential_warmup_requests`、`credential_warmup_selection_percent`、`credential_warmup_max_selection_percent`：新增/预热凭据参与真实请求的次数和目标份额；不会伪造 `success_count`。
- `credential_*_cooldown_secs`、`credential_cooldown_backoff_multiplier`、`credential_cooldown_jitter_percent`、`credential_max_cooldown_secs`：不同错误类型写入对应基础冷却，连续失败按倍率退避并受最大冷却限制。
- `credential_probation_secs`：冷却/错误后的观察期，`health_balanced` 会在该窗口内加惩罚。
- `scheduler_error_ewma_alpha`：影响近期错误率和延迟 EWMA 的更新幅度。
- `scheduler_priority_weight`、`scheduler_load_weight`、`scheduler_error_weight`、`scheduler_latency_weight`、`scheduler_probation_weight`、`scheduler_selection_pressure_weight`、`scheduler_total_selection_weight`：只在 `health_balanced` 评分中生效，分数越低越优先。
- `scheduler_top_k`：`health_balanced` 在得分最好的前 N 个候选中加权抽样，避免并发下集中打同一个账号。

外部备用号池参数：

- `external_pools_enabled`：总开关，关闭时不进入外部池。
- `external_pool_global_max_concurrent_requests`、单池 `max_concurrent_requests`：分别限制外部池全局和单池并发。
- `external_pool_capacity_mode`、`external_pool_dispatch_max_wait_secs`、`external_pool_max_queued_requests`：控制外部池容量满时 fail-fast 还是进入独立等待队列，以及等待上限和队列上限。
- `external_pool_retry_max_attempts`：外部池转发失败后的池级重试上限；`0` 自动按可用池数量覆盖一轮。
- `fallback_on_local_capacity_exhausted`、`fallback_on_no_available_credentials`、`fallback_on_local_transient_exhausted`、`fallback_on_unsupported_model`：本地失败是否允许进入外部池的分类开关。
- `external_pool_local_rescue_*`：外部池失败后是否允许回救本地池，以及 rate limit、timeout、capacity 三类回救开关。
- `local_pool_circuit_*`：本地池连续失败后打开本地熔断，可配合直连外部池策略使用。

## 特殊场景行为

本轮重点复查了“全部不可用”一类边界，结论如下：

- 全部手动禁用、额度耗尽、refresh 失败禁用、风控禁用：直接返回“所有凭据均已禁用”，不进入容量队列，不会持续打上游。
- 全部因 `TooManyFailures` 自动禁用：会执行一次自愈，重置失败计数并重新启用，语义等价于进程重启后的恢复；这只针对连续 API 失败导致的自动禁用，不适用于额度、refresh、手动禁用或风控禁用。
- 全部 refreshToken 当前无效但还未达到禁用阈值：每个凭据只尝试一次后进入认证冷却，本次请求快速返回“所有可用凭据均处于上游临时冷却”并带 `retry_after_secs`，不会在同一请求里持续打到全部禁用。
- 全部处于上游瞬态冷却：直接返回“所有可用凭据均处于上游临时冷却”，带 `retry_after_secs`，不等待冷却结束。
- 全部本地 RPM 限制：等待最早的本地限速恢复；受 `credential_dispatch_max_wait_secs` 限制。
- 全部单凭据并发或全局并发占满：默认进入等待队列；队列上限由 `dispatch_max_queued_requests` 控制；外部池预检的 fail-fast 模式直接返回“本地凭据调度容量暂不可用”。
- 全部模型不兼容：直接返回“没有支持当前模型的可用凭据”，不进入队列。
- 全部绑定代理资源缺失或禁用：直接返回“所有可用凭据均因代理资源不可用而不可调度”，不回退到全局代理/直连，也不误报为全部禁用。
- MCP/WebSearch 路径本地拿不到凭据：现在和普通 API 路径一致，立即结束本轮重试；不会跑满 `credentialRetryMaxAttempts` 的本地空循环。
- 本地池失败是否进入备用号池：只由外部池开关和 fallback 分类开关决定；关闭对应开关时保留本地错误，不路由到外部池。

## 新增测试覆盖

### 本地凭据调度

新增：

- `test_scheduler_handles_500_daily_credentials_1000_rpm_simulation`
- `test_all_bad_refresh_tokens_are_bounded_by_auth_cooldown`
- `test_all_model_incompatible_credentials_fail_fast_without_queueing`
- `test_all_proxy_blocked_credentials_fail_fast_with_proxy_error`
- `test_fail_fast_global_capacity_full_returns_without_queueing`
- `test_error_specific_cooldown_parameters_are_effective`
- `test_scheduler_error_ewma_alpha_changes_error_rate_update`
- `test_health_balanced_score_parameters_are_effective`
- `test_runtime_config_disabling_credential_rpm_clears_rate_limit_state`

测试内容：

- 500 个 API key 日抛形态凭据；
- 1000 次调度请求，等价至少 1000 RPM 的调度量；
- 不 sleep 一分钟，直接验证调度路径吞吐；
- 断言 1000 次调度覆盖全部 500 个凭据；
- 断言 balanced 分布最大值和最小值差不超过 1；
- 断言最终 `global_in_flight_requests == 0`；
- 断言最终 `queued_requests == 0`。
- 验证全部坏 refreshToken 不会在单请求里持续打到禁用，而是进入 auth 冷却并快速返回退避错误。
- 验证全部模型不兼容、全部代理资源不可用时直接失败，不排队，不误报禁用。
- 验证 fail-fast 模式下全局并发满直接返回容量错误且不进入等待队列。
- 验证各类 cooldown 参数、`scheduler_error_ewma_alpha`、健康调度权重、`credential_rpm` 关闭清理状态都实际改变调度结果或运行态。

已有并继续验证：

- `test_balanced_mode_rotates_all_warming_credentials_by_recent_selection`
- `test_balanced_mode_gives_warming_group_scaled_target_share`
- `test_simulation_balanced_mode_spreads_new_warming_batch`
- `test_health_balanced_mode_prefers_best_scored_candidate`
- `test_health_balanced_mode_penalizes_recent_selection_pressure`
- `test_global_capacity_limits_dispatch_and_bounds_wait_queue`
- `test_sonnet_high_concurrency_dispatch_respects_limits_and_spreads_load`
- `test_model_scoped_429_high_concurrency_disabled_and_model_filters`

### 外部备用号池

新增：

- `external_fallback_classifier_respects_scheduler_fallback_toggles`
- `external_pool_skip_reason_respects_enabled_switches_and_capacity`
- `external_pool_candidate_selection_handles_multiple_backup_pools`
- `external_pool_manager_respects_disabled_switch_and_disabled_pools`
- `external_pool_manager_selects_multiple_pools_by_priority_and_capacity`
- `external_pool_manager_distinguishes_global_capacity_from_no_pool`

覆盖内容：

- `external_pools_enabled = false` 时不可调度；
- 池 `enabled = false` 时不可调度；
- 池冷却时不可调度；
- 单池并发满时不可调度；
- 全局外部池并发满时不可调度；
- 多个备用池按 priority 选择；
- 主池满后选择第二备用池；
- 第二池满后选择第三备用池；
- release 后优先级更高的池重新可选；
- 全局外部池容量满时 `eligible_pools > 0`、`available_pools == 0`、`temporary_unavailable_pools > 0`。
- 本地容量、瞬态失败、无可用凭据进入备用池都受对应 fallback 开关控制。

### Provider 路径

新增：

- `mcp_local_acquire_failure_stops_retry_loop`

覆盖内容：

- MCP/WebSearch 本地拿不到凭据时立即返回本地调度错误；即使 `credentialRetryMaxAttempts` 配到很大，也不会跑满空转重试。

## 验证结果

本地执行命令：

```bash
cargo fmt -- --check
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo check
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test -- --test-threads=1
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test scheduler -- --nocapture --test-threads=1
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test external_pool -- --nocapture --test-threads=1
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test balanced_mode -- --nocapture --test-threads=1
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test capacity -- --nocapture --test-threads=1
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test high_concurrency -- --nocapture --test-threads=1
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test credential -- --nocapture --test-threads=1
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test kiro::token_manager::tests -- --nocapture --test-threads=1
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test kiro::provider::tests -- --nocapture --test-threads=1
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test reported_usage -- --nocapture --test-threads=1
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test sub2api -- --nocapture --test-threads=1
(cd admin-ui && npm run build)
(cd ui && npm run build)
```

结果：

- `cargo fmt -- --check` 通过。
- `cargo check` 通过。
- `cargo test -- --test-threads=1` 通过：619 个测试。
- `cargo test scheduler` 通过：8 个测试。
- `cargo test external_pool` 通过：44 个测试。
- `cargo test balanced_mode` 通过：6 个测试。
- `cargo test capacity` 通过：8 个测试。
- `cargo test high_concurrency` 通过：2 个测试。
- `cargo test credential` 通过：111 个测试。
- `cargo test kiro::token_manager::tests` 通过：106 个测试。
- `cargo test kiro::provider::tests` 通过：16 个测试。
- `cargo test reported_usage` 通过：23 个测试。
- `cargo test sub2api` 通过：2 个测试。
- `admin-ui` production build 通过。
- `ui` production build 通过。

压测观测：

- `test_scheduler_handles_500_daily_credentials_1000_rpm_simulation` 断言 500 个日抛形态凭据、1000 次调度必须在 5 秒内完成，并覆盖全部 500 个凭据；本次 `scheduler` 过滤组测试阶段整体 `finished in 0.55s`。
- `test_sonnet_high_concurrency_dispatch_respects_limits_and_spreads_load` 在 24 个凭据、4 个冷却、600 并发请求下通过；最近一次本机输出 `elapsed_ms=361`，使用 20 个可用凭据，单凭据选择分布 29-31 次。

环境说明：

- 当前本机未设置 `KIRO_RS_TEST_POSTGRES_URL`，外部备用池真实 PgSQL+Redis manager 集成测试以跳过方式执行。
- 当前本机未设置 `KIRO_RS_TEST_REDIS_URL`，Redis 集成测试以跳过方式执行。
- 无外部依赖的备用池开关、多池选择和容量分类测试已实际执行并通过。
- `reported_usage` 与 `sub2api` 过滤测试已验证本地模拟缓存最终按 Claude 标准四字段向下游输出；流式 `message_start` 不提前写入非零 input/cache，最终 `message_delta.usage` 承担权威上报。

## 最终结论

本次改造完成后：

- Admin 凭据列表不再把基础信息、运行态、账号信息和用量汇总绑成一个全量响应。
- 本地调度在一次 pass 内完成候选分类和预热统计。
- `health_balanced` 去掉候选压力 O(n²) 和全量排序。
- 全局容量满会进入明确等待/队列语义。
- `snapshot()` 本地模式重入锁死已修复。
- 500 日抛、1000 RPM 等价调度压测已加入自动化测试。
- 外部备用号池开关、多池选择、池级容量、全局容量和“有备用池但临时不可用”语义已加入测试覆盖。
