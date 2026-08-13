# 外部池调度影响本地凭据与 fallback 矩阵缺失

Status: `implemented / third-stage-hot-path-fixed / follow-up-chaos-needed`

Severity: `P0`

Last reviewed: 2026-07-28 Asia/Shanghai

Evidence root:

- `tmp/prod-evidence/20260728-024841-external-pool-scheduler/`
- Redacted archive: `tmp/prod-evidence/20260728-024841-external-pool-scheduler/20260728-024841-external-pool-scheduler-redacted.tar.gz`

Related production machines:

- `152.53.243.159`
- `152.53.194.170`
- `152.53.194.142`
- `152.53.242.178`

## 0. 结论与影响

这是一个生产级调度耦合问题。runtime/storage 主线已先发 `v0.0.121`，随后本问题完成只读生产复核、第一阶段代码修复和 `v0.0.122` 发布。2026-07-28 追加复核时发现：第一阶段只修复了“外部池不可立即接管时不要改变本地排队语义”，但 parsed preflight 路径在外部池最终失败后仍直接返回外部错误，没有再给本地凭据一次 bounded rescue 机会。本轮先补齐 stream/non-stream parsed preflight 的本地 rescue，且保持 local-only、一次性、共享 attempt budget 限制，避免 fallback 回环。

同日继续复核 159 生产事故后确认还有第三阶段热路径问题：`local_attempt_policy`、parsed preflight、raw preflight、本地失败 fallback 的 route gate 仍会调用“立即可用外部池”或“外部池资格”检查，而旧检查会同步等待外部池权威快照、运行态快照、PgSQL/Redis 状态。这样即使请求最终走本地凭据，只要 `externalPoolsEnabled=true`，本地请求生命周期也会被外部池 PgSQL/Redis 尾延迟、坏记录刷新、冷却/失败状态写入放大。

第三阶段修复目标：

- 本地优先路径只允许读取已有缓存/本地快照来判断外部池是否可以接管。
- 冷缓存或缓存失效时，只触发后台刷新，不阻塞当前请求。
- 没有可信的“外部池当前有容量”缓存时，当前请求保持本地原始调度语义。
- 真实进入外部池转发后，仍使用权威选择、容量协调、外部池内部 failover。
- 外部池 direct 模式继续只走外部池，不因为本地账号存在而调度本地。
- raw preflight 和 parsed preflight 共享同一个“本地主路径 no-wait gate”原则。

用户侧现象：

- 159/170 开启外部池后，本地凭据调度明显异常，页面和业务接口可能卡住。
- 关闭外部池后，调度恢复正常。
- 142 在外部池关闭或未进入外部池路径时更稳定。
- 用户怀疑外部池 fallback、preflight、capacity、Redis/PgSQL snapshot 或回环调度影响本地凭据。

必须验证的核心问题：

> 外部池开启后，外部池资格判断、fallback、容量协调、坏记录刷新或 Redis/PgSQL 同步路径，是否把本地凭据调度拖入同一个故障域，导致本地凭据即使有容量也变成不可调度。

本轮补充结论：

> 外部池直连是允许的，但它是独立策略：`externalDirectPolicyEnabled=true` 时，请求直接走 `external_direct_policy`，失败后不 rescue 本地。local-first fallback 路径和 parsed preflight fallback 路径可以在外部池最终失败后 rescue 本地一次，但 rescue 调用的是 provider-local entrypoint，不再经过 external fallback context，所以不会形成本地/外部池来回调度循环。

2026-07-28 第三阶段修复后的目标策略：

| 配置/状态 | 目标行为 | 本地主路径是否允许同步等待外部池 PgSQL/Redis |
| --- | --- | --- |
| `externalPoolsEnabled=false` | 只使用本地账号；不读外部池资格、权威快照或运行态 | 否 |
| `externalPoolsEnabled=true` + `externalDirectPolicyEnabled=true` | 只使用外部池；不因为本地账号存在而调度本地账号；外部池失败不自动 rescue 本地 | 不适用，这是外部池直连路径 |
| 外部池开启、非直连、本地有账号且本地容量正常 | 本地优先，按本地账号并发/RPM/排队策略调度 | 否 |
| 外部池开启、非直连、本地有账号但本地容量不足/Redis degraded/全部冷却 | 只有已有缓存证明外部池当前可接管时，才允许 preflight/fallback 到外部池；否则保持本地原始等待/错误语义 | 否 |
| 外部池开启、非直连、本地没有账号 | 临时等价外部池直连；后续如果添加本地账号，应在本地 route state 刷新后尽快回到本地优先 | 冷缓存时不得阻塞；应后台刷新外部池快照 |
| 外部池失败后 local rescue | 只允许 bounded、local-only、最多一次发送，并共享 attempt budget；失败后直接返回，不再进入外部池 | rescue 是本地 provider 路径，不应回到 external fallback context |
| 外部池全部满/冷却/配置坏记录 | 不能把本地请求无限推入外部池队列，也不能持续污染本地 pool circuit | 否 |

仍需在后续专项中进一步产品化的策略点：

- 本地容量不足时到底“优先本地排队”还是“优先外部池接管”，需要形成显式配置，而不能由外部池可用性检查隐式决定。
- 外部池 cooldown 需要拆成可配置策略：哪些错误会冷却、冷却多久、是否允许手动恢复、是否允许模型级/池级冷却。
- 外部池内部重试策略需要独立于本地 fallback/rescue 策略，避免和本地账号调度互相放大。
- 无本地账号时的“临时外部池直连”需要有快速回切本地的刷新/事件触发机制；当前依赖本地 route state 和缓存失效。

## 1. 用户可见问题

已登记的现象包括：

- 外部池调度路径非常影响本地凭证。
- 外部池开启后，即使本地账号并发/RPM 没打满，也可能出现调度异常、页面慢、业务慢。
- 外部池关闭后，所有调度恢复正常。
- 159 分析里出现外部池坏记录或空 `api_key` 记录反复刷新、静态资格解析失败、Redis scheduler 慢、PgSQL 凭据状态写入慢等链式现象。
- 需要明确哪些场景会 fallback 到外部池，哪些场景不能 fallback，哪些场景应该本地失败，哪些场景应该直连外部池。

## 2. 根因假设

当前根因分两层。

第一层，生产证据支持外部池是 159 调度异常的强触发器/放大器：

- 159 当前 `externalPools.externalPoolsEnabled=true`。
- 170 当前 `externalPools.externalPoolsEnabled=false`，更新时间 `2026-07-27 09:13:48Z`。
- 142 当前 `externalPools.externalPoolsEnabled=false`。
- 159 近 12 小时 usage 聚合同时出现：
  - `local_credential | local_success`: `56284`
  - `local_credential | local_error_no_fallback | queue full`: `370`
  - `local_credential | local_rescue_after_external | queue full`: `31`
  - `external_pool | external_fallback_preflight | success`: `24`
  - `external_pool | external_fallback_after_local_attempts | success`: `3`
  - 多条 Redis 调度协调不可用并等待约 `300s` 的记录。
- 142 在 external pools 关闭时承接更多本地成功请求，近 12 小时 `local_success=72177`，未出现同类 external route 和 queue full 聚合。

第二层，源码根因是外部池“静态资格”被错误用于改变本地凭据排队语义：

- `ExternalFallbackContext::local_attempt_policy()` 在本地凭据选择前调用 `has_eligible_external_pool_for_model()`。
- 该函数只证明外部池静态配置/模型/body mode 合格，不证明外部池当前有容量、未冷却、Redis coordinator 可用、dispatch fence 可用。
- 一旦静态资格为 true，本地 acquire mode 会变为 `FailFastOnCapacityWaitForRedis`。
- 因此本地容量/RPM/冷却/Redis degraded 本应按本地队列或本地错误处理的请求，会被推入 external fallback/preflight。
- 如果外部池实际不可接管，又会触发 external capacity failure / local rescue，再次回到本地调度，形成“本地 → 外部池 → 本地 rescue”的放大链。
- `fallback_after_local_error` 旧逻辑还会在确认外部池可接管前记录 `local_pool_failure`，可能提前污染 local pool circuit。

所以最终定性：

> 外部池不是完全独立旁路。开启外部池后，旧逻辑用“静态存在/模型合格”改变本地凭据调度策略，导致外部池容量不足、冷却、Redis/PgSQL 慢或坏数据时，仍能把本地请求推入更重的 fallback/rescue 链路，并把压力反向打回本地调度。

仍需继续复核的假设：

1. 外部池开启后，每个请求额外进入外部池资格判断、snapshot、preflight、fallback capacity 协调路径。
2. 外部池 snapshot 可能依赖 PgSQL，scheduler/capacity 可能依赖 Redis，与本地 token manager/scheduler 共用故障域。
3. 外部池坏记录（例如空 `api_key`）可能被每轮重复解析、重复告警、重复刷新，放大 PgSQL/Redis 和日志压力。
4. fallback 决策可能存在回环或优先级混乱：本地容量/错误/Redis degraded/无本地凭据/外部池容量不足之间的状态转换不够明确。
5. 外部池不可用、容量不足或全部冷却时，可能没有快速短路，而是继续占用本地调度链路。

需要特别区分：

- “fallback 到外部池”：本地失败后走外部池。
- “直连外部池”：无本地凭据或配置指定时直接走外部池。
- “外部池 fallback 回本地”：如果存在，必须证明不会形成回环。
- “外部池 preflight 失败”：应不应影响本地凭据调度，需要按配置定义。

## 2.1 现有外部池路由状态机

当前实现需要按三类入口理解。

### 2.1.1 外部池直连

入口：

- `ExternalFallbackContext::direct_policy_response`
- `ExternalPoolManager::direct_policy_reason`
- `direct_external_policy_static_reason`

触发条件：

- `externalPoolsEnabled=true`
- `externalDirectPolicyEnabled=true`
- 可由 model/path rules 命中；如果没有 rules，当前实现会给全局 `explicit_direct`。

行为：

- 不先尝试本地凭据。
- route subtype 为 `external_direct_policy`。
- 直接调用 `ExternalPoolManager::forward_with_failover`。
- 外部池内部可以在多个外部池之间 failover。
- 外部池最终失败时直接返回外部池错误。
- 不会触发本地 rescue。`local_rescue_reason_after_external_error` 第一层显式判断 `external_direct_policy_enabled`，直接返回 `None`。

原因：

- 直连外部池是配置明确指定的“外部优先/外部直连”策略。
- 如果直连失败又自动回本地，会和 local-first fallback 策略混淆。
- 后续如果需要“直连外部池失败后可选回本地”，应该新增独立开关，例如 `directExternalLocalRescueEnabled`，不能复用当前 local-first rescue，否则可观测语义会混乱。

### 2.1.2 本地优先，再 fallback 外部池

入口：

- `ExternalFallbackContext::local_attempt_policy`
- `ExternalFallbackContext::fallback_after_local_error_outcome_with_diagnostics`

行为：

1. 默认走本地凭据。
2. 只有外部池能在短预算内证明“当前可立即接管”时，本地 acquire 才切换到 bounded fail-fast。
3. 本地失败后按错误类型分类：
   - 容量、Redis scheduler degraded、全部冷却、risk circuit、transient 等临时本地问题，必须再次证明外部池当前可立即接管。
   - 无本地凭据、全部 disabled、代理不可用、模型不兼容等“没有本地可行路线”的问题，允许进入外部池自身容量策略。
4. 外部池最终失败后，如果 rescue 开关允许且共享 attempt budget 还有余量，尝试本地一次。
5. 本地 rescue 调用 provider-local `*_max_wait` entrypoint，最大等待由 `externalPoolLocalRescueMaxWaitSecs` 控制，`max_sends=Some(1)`。
6. rescue 成功则本次请求按 `local_rescue_after_external` 记录 usage；rescue 失败则直接把本地错误映射返回，不再进入外部池。

### 2.1.3 parsed preflight fallback，再 rescue 本地

入口：

- `ExternalFallbackContext::local_pool_preflight_outcome`
- `maybe_local_pool_preflight_external_outcome`
- stream: `handle_stream_request`
- non-stream: `handle_kiro_response`

历史缺口：

- preflight 发现本地池当前不可调度后直接进入外部池。
- 如果外部池最终失败，旧逻辑直接 `err.into_response`。
- 这不满足“本地当时不够用，但外部池失败时可能已有本地资源释放，应再试本地一次”的要求。

本轮修复：

- stream/non-stream parsed preflight 的 `ExternalPoolForwardOutcome::FinalError` 现在进入同一套 `budgeted_local_rescue_reason_after_external_error`。
- 满足配置和 budget 时调用 local-only rescue。
- rescue 成功继续按本地成功 response 处理。
- rescue 失败记录本地 provider error 并返回，不再 external fallback。

仍保持不 rescue 的 preflight：

- body 解析之前的 raw passthrough preflight 仍保持外部失败即返回。
- 原因是此时没有可靠的规范化 `KiroRequest` / 本地请求体上下文，强行二次解析再本地调用会引入更大的协议风险。

### 2.1.4 外部池内部 failover

入口：

- `ExternalPoolManager::forward_with_failover_result`

行为：

- 只在外部池集合内部选择/重选/冷却/重试。
- 不直接调用本地凭据。
- 外部池最终失败以 `ExternalPoolForwardOutcome::FinalError` 交回 caller。
- 是否本地 rescue 由 `handlers.rs` 的 caller 决定。

### 2.1.5 防回环机制

当前实现有三层防回环：

1. 路径隔离：
   - 本地 rescue 不走 `call_api_stream_maybe_fail_fast` / `call_api_maybe_fail_fast`。
   - rescue 直接走 provider-local `*_max_wait` entrypoint。
   - 因此 rescue 失败不会再次进入 `ExternalFallbackContext::*fallback*`。
2. 次数限制：
   - rescue 传入 `max_sends=Some(1)`。
   - 单次 rescue 最多真实发送一次本地上游请求。
3. 预算限制：
   - 共享 `InferenceAttemptBudget` 无剩余额度时，`budgeted_local_rescue_reason_after_external_error` 返回 `None`。
   - 新增测试覆盖 external + local rescue 消耗完 2-send budget 后，不允许第二轮 external/local cycle。

## 3. 复现矩阵

后续分析必须覆盖以下配置组合，不能只测单一 happy path。

### 3.1 本地凭据维度

- 有本地凭据，全部健康。
- 有本地凭据，但容量满。
- 有本地凭据，但 RPM 满。
- 有本地凭据，部分冷却。
- 有本地凭据，全部冷却。
- 有本地凭据，Redis scheduler degraded。
- 没有本地凭据。
- 本地凭据存在但全部 disabled。

### 3.2 外部池维度

- 外部池关闭。
- 外部池开启，但没有任何外部池。
- 外部池开启，有坏记录，例如空 `api_key`。
- 外部池开启，有一个可用池。
- 外部池开启，有多个可用池。
- 外部池开启，全部池容量不足。
- 外部池开启，全部池突发冷却。
- 外部池开启，部分池 429/5xx。
- 外部池开启，外部池模型不支持当前请求模型。

### 3.3 配置维度

必须逐项验证：

- `externalPoolsEnabled`
- `fallbackOnLocalCapacityExhausted`
- `fallbackOnNoAvailableCredentials`
- `fallbackOnLocalTransientExhausted`
- `fallbackOnSchedulerRedisDegraded`
- 是否存在 direct external pool 模式
- 是否存在 strict local-first 模式
- request admission 打开/关闭
- 每账号 RPM/并发限制
- 外部池 RPM/并发限制

### 3.4 异常维度

- PgSQL external pool snapshot 慢。
- Redis scheduler 慢。
- Redis external pool capacity 慢。
- 外部池 endpoint timeout。
- 外部池返回 401/403/429/500。
- 外部池非流式/流式 usage 缺失。
- 下游客户端中断。
- 长流集中完成。

## 4. 方案方向

已选定并实现的第一阶段修复：

1. 新增 external pool 当前可用性判断：
   - `ExternalPoolManager::has_immediately_available_pool_for_model`
   - `ExternalPoolManager::has_immediately_available_pool_for_body_mode_and_model`
   - 该判断在短预算内检查 authoritative snapshot、Redis runtime、并发容量、global capacity、cooldown 和模型/body mode。
   - 超过预算或无法快速证明可接管时返回 false，保持本地原始排队语义。
2. 本地请求策略改为：
   - 只有外部池当前可立即接管时，`local_attempt_policy()` 才使用 `FailFastOnCapacityWaitForRedis`。
   - 如果外部池只是静态合格但容量满/冷却/协调不可用，本地凭据继续按 `WaitForCapacity` 运行。
3. preflight/fallback 按 route reason 分流：
   - `local_capacity_full`
   - `local_scheduler_redis_degraded`
   - `local_all_cooling_down`
   - `local_pool_risk_circuit_open`
   - `local_transient_exhausted`
   - `local_auxiliary_attempts_exhausted`
   - `local_auxiliary_concurrency_saturated`
   - `local_attempt_reserved_for_fallback`

   这些只是本地临时调度问题，只有外部池能立即接管时才 fallback。
4. 无本地可行路线的原因仍允许走外部池自身容量策略：
   - `local_no_credentials`
   - `local_all_disabled`
   - `local_proxy_blocked`
   - `local_no_model_compatible`
   - `no_available_credentials`
   - `unsupported_model`
5. `record_local_pool_failure` 移到确认外部池 ready 之后，避免“外部池不可接管但先污染 local pool circuit”。

后续设计仍必须满足：

1. 外部池资格 snapshot 单飞、缓存、有 TTL，有坏记录隔离；坏记录不能每个请求重复解析和告警。
2. 外部池调度 Redis/PgSQL 与本地凭据主调度故障域隔离；外部池慢不能拖住本地凭据选择。
3. fallback 决策必须是单向有限状态机，不允许外部池和本地池来回回环。
4. 每种失败类型必须有明确路由：
   - 本地容量耗尽；
   - 本地无可用凭据；
   - 本地 Redis scheduler degraded；
   - 本地 transient exhausted；
   - 外部池容量耗尽；
   - 外部池模型不支持；
   - 外部池认证失败；
   - 外部池全部冷却。
5. 外部池不可用时应快速降级或快速失败，并记录明确 `routeSubtype/selectionFailure`，不能表现成本地账号全部不可用。
6. 外部池坏配置应在管理面显示并禁用该池，不能污染请求热路径。

本轮第二阶段修复：

1. 新增 `maybe_local_pool_preflight_external_outcome`，让 stream/non-stream 主路径能够拿到 external preflight 的 `FinalError`，而不是只能拿到已经转换好的 HTTP response。
2. stream parsed preflight：
   - external preflight 成功：直接返回外部池 response。
   - external preflight FinalError：按 `budgeted_local_rescue_reason_after_external_error` 判断是否 rescue。
   - rescue 调用 `call_stream_local_rescue_after_external_error`，local-only，`max_sends=Some(1)`。
3. non-stream parsed preflight：
   - 同样接入 external FinalError 后 local-only rescue。
4. direct external policy 保持不 rescue：
   - 测试已覆盖 rate-limit、timeout、capacity、bad-request、server-error 等外部错误类型在 direct policy 下均不触发本地 rescue。
5. raw pre-parse passthrough preflight 保持旧行为：
   - 无可靠本地请求体上下文，不做本地 rescue。
   - 第三阶段修复后，它的外部池 route gate 也改为 cached/no-wait，不再同步等待外部池 PgSQL/Redis。

本轮第三阶段热路径修复：

1. `ExternalPoolManager` 新增 cached/no-wait route gate：
   - `has_cached_eligible_pool_for_model`
   - `has_cached_eligible_pool_for_body_mode_and_model`
   - `has_cached_immediately_available_pool_for_model`
   - `has_cached_immediately_available_pool_for_body_mode_and_model`
2. cached gate 只读取已存在的静态快照、权威快照和运行态快照：
   - 静态快照缺失/过期：只尝试后台刷新，不等待。
   - 权威快照缺失/过期：只尝试后台刷新，不等待。
   - 运行态快照缺失：只尝试后台 Redis 刷新，不等待。
   - 缓存不能证明外部池可接管时，返回 false，保持本地原始语义。
3. `ExternalFallbackContext::local_attempt_policy` 改为 cached immediate gate：
   - 外部池开启但冷缓存/外部池状态未知时，不再把本地 acquire mode 改成 fail-fast。
4. parsed preflight 和本地失败 fallback 的 `external_pool_ready_for_route_reason` 改为同步内存判断：
   - 容量类/Redis degraded/全部冷却/risk circuit 等 reason 必须已有缓存证明外部池当前有容量。
   - unsupported/no-local-route 等非容量类 reason 只使用 cached static eligibility，不同步查 PgSQL。
5. raw preflight 修复：
   - 删除 pre-parse 阶段的阻塞式 `has_eligible_pool_for_body_mode_and_model(...).await`。
   - 改用 `raw_external_pool_ready_for_route_reason`，逻辑与 parsed route gate 一致。
6. raw direct 修复：
   - 直连外部池不再先阻塞检查 raw eligible pool。
   - direct policy 命中后直接进入外部池转发；如果外部池不可用，由外部池路径返回错误，不退回本地。

## 5. 验证计划

后续修复后至少需要：

- 单元测试：fallback classifier 和 route decision 状态机。
- 集成测试：本地凭据 + 外部池矩阵，覆盖上面的配置/容量/异常组合。
- chaos 测试：外部池 PgSQL snapshot 慢、Redis 慢、外部池 timeout 时，本地凭据成功请求不被拖慢。
- 真实协议测试：Claude Code CLI 多轮、tools、stream、non-stream 在外部池开启/关闭下都正常。
- 指标验证：usage 中 routeKind/routeSubtype/selectionFailure 能准确解释每次路由，不出现“本地有容量但被外部池拖死”的不可解释状态。

当前已完成验证：

- `external_pool::tests::external_pool_immediate_availability_requires_current_capacity_and_recovers`
  - 有健康外部池时即时可用为 true。
  - 外部池并发槽被占满后即时可用为 false。
  - 外部池 lease 释放并 drain 后即时可用恢复 true。
- `external_pool::tests::external_pool_cached_immediate_availability_is_no_wait_under_pg_lock_for_five_rounds`
  - 本地 PgSQL/Redis 集成环境真实执行通过。
  - 每轮锁住 `external_upstream_pools`，模拟权威外部池快照 PgSQL 阻塞。
  - 128 并发 cached local gate 必须在 250ms 内返回 false，不等待 PgSQL。
  - 每轮最多触发一次后台权威快照刷新，防止突发请求把 PgSQL 查询 fan-out。
- `external_pool::tests::external_pool_cached_immediate_availability_uses_cached_runtime_capacity`
  - 本地 PgSQL/Redis 集成环境真实执行通过。
  - 冷缓存时 cached gate 返回 false，不同步读 Redis/PgSQL。
  - 权威检查暖缓存后 cached gate 返回 true。
  - 外部池并发槽占满后 cached gate 返回 false。
  - lease 释放并 drain 后 cached gate 恢复 true。
- `anthropic::handlers::tests::local_external_fallback_capacity_gate_reason_matrix_is_explicit`
  - 明确哪些 route reason 必须要求外部池当前有容量。
  - 明确无本地可行路线的 reason 仍可进入外部池自身容量策略。
- 旧回归：
  - `external_fallback_classifier_*`: `5/5` 通过。
  - `local_pool_preflight_*`: `2/2` 通过。
- 外部池完整 Rust 分组：
  - `cargo test --locked --bin kiro-rs external_pool -- --test-threads=1`
  - `222 passed / 0 failed`
  - 覆盖外部池 Redis、capacity、model mapping、billing、raw/normalized、SSE、fallback、release、atomic acquire、coordinator restart/fault 等。
- 本轮新增和重跑：
  - `cargo test --locked --bin kiro-rs local_rescue -- --test-threads=1`
    - `4 passed / 0 failed`
    - 覆盖 direct policy 禁止本地 rescue、rescue 分类、budget 限制、provider-local rescue 真实发送次数。
  - `cargo test --locked --bin kiro-rs preflight_external_error -- --test-threads=1`
    - `1 passed / 0 failed`
    - 覆盖 parsed preflight external error 可以 rescue 一次，且 external+local rescue 消耗完 2-send budget 后阻止第二轮 cycle。
  - `cargo test --locked --bin kiro-rs external_fallback -- --test-threads=1`
    - `9 passed / 0 failed`
    - 覆盖 fallback 分类、route-reason availability gate、thinking signature 不进入 external fallback、usage/profile 相关回归。
  - `cargo test --locked --bin kiro-rs external_pool -- --test-threads=1`
    - `222 passed / 0 failed`
    - 本轮再次通过。
- 静态质量：
  - `cargo fmt --check`: 通过。
  - `cargo check --locked --bin kiro-rs`: 通过。
  - `cargo check --all-targets --locked`: 通过，第三阶段修复后无新增 warning。
  - `rustup run 1.92.0 node scripts/ci/check-clippy-baseline.mjs`: 通过，`813 warnings <= 849 baseline`。
  - 第三阶段修复后 clippy baseline：通过，`811 warnings <= 849 baseline`。
  - `git diff --check`: 通过。
  - `node feature/tests/inventory-build-artifacts.mjs --gate`: 通过，`targets=0 reservations=0 target_processes=0 blockers=0`。
- 发布：
  - 修复 commit: `dcff076 fix: gate external fallback on current pool availability`
  - release commit: `6e4d801 chore(release): 0.0.122`
  - tag: `v0.0.122`
  - 后续发布 `v0.0.123` 前已确认 `ghcr.io/2ue/kiro-rs:0.0.122` 镜像存在。
  - 本轮 second-stage preflight rescue 修复尚未单独发布新 tag。
- 本轮第三阶段测试命令：
  - `KIRO_RS_TEST_POSTGRES_URL='postgres://<local-kiro-rs-postgres>:25432/kiro_rs' KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:26379/0' RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh external_pool_fix_tests_real -- cargo test external_pool_cached_immediate_availability -- --nocapture --test-threads=1`
  - 结果：`2 passed / 0 failed`。
  - 同一测试在 `#[cfg(test)]` 收窄旧阻塞方法后重跑：`2 passed / 0 failed`。
  - `RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh external_pool_fix_handler_external_fallback -- cargo test external_fallback -- --nocapture`
    - 结果：`9 passed / 0 failed`。
  - `RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh external_pool_fix_handler_raw -- cargo test raw_external -- --nocapture`
    - 结果：`2 passed / 0 failed`。
  - `RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh external_pool_fix_preflight_rescue -- cargo test preflight_external_error -- --nocapture`
    - 结果：`1 passed / 0 failed`。
  - `KIRO_RS_TEST_POSTGRES_URL='postgres://<local-kiro-rs-postgres>:25432/kiro_rs' KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:26379/0' RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh external_pool_fix_immediate_real -- cargo test external_pool_immediate_availability_requires_current_capacity_and_recovers -- --nocapture --test-threads=1`
    - 结果：`1 passed / 0 failed`。
  - scoped target 已清理：`removed=true reservation_released=true`。

仍需完成验证：

- 外部池开启下的真实生产级 load/chaos 长时间矩阵仍需继续补：外部池容量满、Redis coordinator 慢、local rescue enabled、外部池全部冷却、坏配置持续存在、长流集中完成等组合。
- 真实 Claude Code CLI 多轮 + tools + stream/non-stream + 外部池开启/关闭的端到端回归仍应作为后续发版前的大矩阵验证项目。

## 6. 残余风险与回滚

残余风险：

- 当前第一阶段修复降低外部池对本地调度的反向污染，但还没有完成所有外部池 fallback 组合的 load/chaos 矩阵。
- 用户提供的 159 分析结论已用只读生产证据复核一部分，但 170 事故发生时的历史 `externalPoolsEnabled` 状态仍需从更早 audit/runtime config 证据确认。
- 外部池和本地凭据调度耦合可能跨越 provider、external_pool、token_manager、storage、Redis cache 多个模块，修复需要完整矩阵验证。

临时回滚/止血：

- 如果生产再次出现外部池开启后本地调度异常，优先关闭 `externalPoolsEnabled` 或禁用坏外部池记录。
- 不应通过降低本地账号 RPM/并发作为根治手段。
- 不应把外部池 fallback 默认扩大到所有本地错误，避免掩盖本地调度故障并制造回环。
