# Retry Budget, Admission, And RPM Amplification

Status: `shared-budget-400-profile-and-process-local-refresh-focused-pass / token-refresh-cluster-open / external-eligibility-load-and-attribution-gates-pending / NO-GO`

Severity: P0

Token refresh 的短 TTL、失败波、invalid-bearer 自动恢复、Redis/PgSQL 集群 fence、60/8 RPM admission 与 health-owner 取消语义已拆到当前权威专题 [Token Refresh Failure Wave And Cluster RPM](token-refresh-failure-wave-and-cluster-rpm.md)。本文件继续负责跨 local/external/stream/payload/MCP/auxiliary 的总预算和渠道放大；下文 2026-07-17 的 local negative-result 章节是阶段 A 历史检查点，不能覆盖新专题中的集群和自动恢复阻断项。

## 现象

下游 RPM 很低时，单个请求仍可在 provider credentials、stream rectification、payload retry、external failover 和 rescue 多层产生大量上游调用。已有动态证据：HTTP 500/429 的真实 Claude CLI 场景最高 30x，partial disconnect 9x；错误波后虽能恢复，错误期间峰值仍不可接受。

## 根因类

- 各层拥有独立 retry 计数；没有 request-scoped 总预算。
- provider 默认重试可随账号池大小增长，model-unavailable 分支也受 pool-sized 上限且缺专属 cap。
- 请求 API Key 不是完整的并发/RPM/队列实体，usage 也缺 channel 归因。
- OAuth refresh、模型目录同步和 inference 前 `ListAvailableProfiles` 等辅助调用历史上未与 inference attempts 完整分账；旧 profile discovery 对同账号没有 singleflight/失败 backoff，403/500 时并发量会直接变成 auxiliary HTTP 数。当前 dirty tree 已补 request-scoped auxiliary budget、process concurrency 和局部 channel snapshot，但持久 usage attribution、跨实例 aggregate 与最终 handler/CLI 证据仍未关闭。
- 旧账号选择会在 provider 预留 inference attempt 之前调用 `try_ensure_token`，且没有把 request-scoped auxiliary budget 传入跨账号选择，因而单请求可按账号池 fan-out。当前实现已在每个真实 refresh send 前使用与账号数无关的 auxiliary hard budget；20-account 独立 manager 和 128-account shared manager 的 12 类 fake OAuth 矩阵已通过。该证据不替代 1/20/60 handler/provider、live Redis、usage attribution 或冻结候选负载。
- 当前 400 分类器把任何包含通用 `REQUEST_BODY_INVALID` 的响应归成 `tool_use_format_bad_request`；源码测试甚至把 `Image data cannot be empty` 固化成该分类。默认 prompt-logic retry 虽关闭，但开启后会把确定性的坏图/body 错误误当成可换账号恢复的工具协议错误，造成错误归因和无意义 attempt。

## 稳定复现

### 单请求放大

隔离启动 fake Kiro、fake external、PostgreSQL 与 Redis，配置 1/20/60 个可选择账号。对同一个下游 request 依次让上游返回 400 model-unavailable、invalid model、invalid tool、空图/body invalid、普通 malformed、429、500、首字节前断流和首字节后 partial。记录 inference/refresh/catalog/external hit、credential 数、每层 reason/action 与最终 usage。

修复前红灯判定：任一请求的 inference hit 随账号数线性增长；确定性 400 换号；首字节后仍发新请求；或 local/payload/stream/external 各自重置计数。已有历史真实 CLI 证据中 500/429 最高 30x、partial disconnect 9x；这些数字只是修复前基线，必须由最终候选重跑。

当前 400 聚焦复现：

```bash
cargo test bad_request_retry_matrix_bounds_real_provider_http_hits -- --nocapture
cargo test classifies_bad_request_protocol_reasons -- --nocapture
cargo test prompt_logic_retry_only_applies_to_enabled_protocol_reasons -- --nocapture
```

### 下游 burst 与恢复

单 key、多 key分别执行 1、5、10 concurrency 后突增到目标档，混入客户端立即重试与长流；每档 3 轮。队列满/超时/RPM 拒绝必须在 handler 前发生且 upstream=0。错误 burst 后再发 5 个 concurrency 1 的 normal 请求，要求 5/5 恢复。

### 长会话与辅助通道

以真实 Claude Code CLI 执行 20/100 tool cycle、120k history/resume、MCP search 和 agents。分别统计 downstream request、inference attempt、OAuth refresh、model catalog/discovery、scheduler selection 与 external attempt，不能把辅助调用混进“模型 RPM”或用 inference budget 掩盖辅助风暴。

### ListAvailableProfiles 并发放大

配置一个缺少真实 `profileArn` 的 external IdP credential，用本地 HTTP fake endpoint 分别返回成功、403 和 500。先同时发 16 个会选中该 credential 的请求，再在失败后立即追发 32 个请求；另用 1/20/60 个 credential 每账号 2 caller、每档 5 轮验证隔离性。修复前同 credential 的 16 caller 稳定产生 16 次 discovery HTTP；修复后每个 credential 每轮最多 1 次，403/500 backoff 窗口内追发为 0。完整复现和请求数见 [profile ARN auxiliary RPM 证据](../evidence/profile-arn-auxiliary-rpm-bound-20260716.md)。

### OAuth refresh 在 inference reserve 前跨账号放大

这个场景已完成 process-local manager/fake-HTTP 红绿合同，但 provider/handler、持久 usage 与多实例仍未完成。当前证据见 [OAuth auxiliary budget and cancellation](../evidence/oauth-auxiliary-budget-and-cancellation-20260718.md)；发布前仍需补齐以下端到端合同：

1. 建立 1/20/60 个已过期 external-IdP credential，全部指向隔离 fake OAuth token endpoint；endpoint 分别返回 500、429、超时、断连和 malformed success。
2. 每个账号使用不同认证身份，避免 per-credential singleflight 把跨账号 fan-out 错误合并；单个下游 Messages 请求和 1/8/32 并发 burst 各跑 5 轮。
3. 同时记录 downstream、refresh HTTP、profile discovery、local inference、external、credential selection、活跃 refresh 峰值、RSS、FD、错误分类和恢复请求。
4. 红灯判定：单请求 refresh HTTP 随账号数增长；refresh send 未出现在 request-scoped channel ledger；或 auxiliary budget 拒绝被错误记成账号健康失败/冷却。
5. 绿灯判定：每请求实际 outbound send 受与账号数无关的小常数约束；refresh/profile/inference/external 分通道可核对；预算耗尽不污染账号健康；错误后正常请求 5/5 恢复；并发 refresh 峰值有硬上限且等待/拒绝不形成无界任务。

现有 `test_all_bad_refresh_tokens_are_bounded_by_auth_cooldown` 使用长度不足的 refresh token，在发 HTTP 前就被本地校验拒绝。它只能证明本地坏 token 会快速进入 auth cooldown，不能作为上述真实 OAuth fan-out 的反证。

## 方案

- 一个请求一个 attempt ledger/budget，所有本地/外部/stream/payload/rescue 消耗同一预算。
- 默认硬上限为小常数，与账号数量无关；已下游提交后服务端不得自动重试。
- 精确分类 400 model unavailable；invalid model/body 不换号。
- `tool_use_format_bad_request` 必须要求明确的 tool-use/tool-schema 语义，不能仅凭通用 reason；空图、坏图、普通 malformed body 必须本地拒绝或首个 400 后直接失败。
- API key 级 concurrency/RPM/queue admission，公开 `Retry-After`；attempt、OAuth、catalog、scheduler selection 分开计数。
- OAuth refresh 使用 per-credential singleflight，不允许跨账号全局串行，也不允许同账号并发风暴。
- `ListAvailableProfiles` 使用认证身份指纹隔离的 per-credential async singleflight、5-60 秒 bounded negative backoff 和固定上限 LRU 状态；已有真实 ARN 必须在任何 hash/lock 前返回，状态饱和时抑制 auxiliary HTTP 并保留 fallback。

方案取舍：只降低 provider retry 次数不能覆盖 stream/payload/external 层；只做 API-key admission 不能限制一个已准入请求内部的 attempt；把所有通道塞进同一计数又会让 catalog/refresh 无法解释。因此选定“下游 key admission + request-scoped inference 硬预算 + 辅助通道独立 single-flight/归因”三层模型。预算检查只在真实 HTTP send 前做，正常 body 不被复制或重写，热路径增加固定大小的原子计数和 ledger。

## 分批状态

- 请求 API Key 的本地 RPM/并发/有限队列已实现并通过每类 5 轮聚焦测试，详见 [Request API Key Admission](request-api-key-admission.md)。
- request-scoped shared inference budget 已接入 local credential、payload/cache retry、stream precommit retry、external direct/fallback/failover 和 local rescue；默认硬上限 4，配置范围 1..=10，真实 HTTP send 前才 reserve，首个下游字节后禁止新 send。
- 400 分类已按明确 model/tool/image/body/malformed 语义拆分。10 类 x 1/20/60 账号 x 5 轮的真实本地 HTTP provider 矩阵共 240 次 inference hit；所有确定性 400 均为每请求 1 hit，可恢复类不超过共享上限。证据见 [Provider 400 分类](../evidence/provider-400-retry-classification-20260716.md)。
- startup/manual model discovery 已增加独立的 4-credential 硬上限、健康/cooldown/rate-limit 过滤和 provider 级 single-flight。60 账号、连续 5 轮时每轮严格 4 个 auxiliary hit，并发第二次同步 0 hit；E05 的 60 账号隔离服务也在 3 个启动轮次中各记录 4 hit。该上限不消耗 inference attempt budget，辅助与生成分账。
- inference 前 enterprise profile discovery 已增加 per-credential singleflight、negative backoff、认证身份 ID-reuse 隔离和 2048-entry 硬上限。红测中 16 个同账号 caller 为 16 hit；修复后成功/403/500 每轮严格 1 hit、失败窗口的 32 个追发为 0。1/20/60 账号 x 5 轮矩阵共 810 caller、405 auxiliary HTTP，严格每账号每轮 1 次且不同账号并行；已有 ARN 5000 次检查为 0 state/0 HTTP。auxiliary 原子计数和日志不消耗 inference budget。证据见 [profile ARN auxiliary RPM bound](../evidence/profile-arn-auxiliary-rpm-bound-20260716.md)。
- legacy external capacity wait 的 `0` 已解释为 30 秒有界等待；最终 `external_pool_wait_timeout` 现为 non-retryable，避免客户端在已经耗尽服务端等待后立即形成反馈回路。真实 PG/Redis 聚焦用例连续 3 轮通过；仍需统一候选上的 burst/recovery 证明。
- OAuth refresh 的跨账号 request-scoped hard budget 已接入真实 send admission。20-account 独立 manager 的 12 类失败 x c1/c8/c32 x 5 轮严格保持每请求最多 2 hits；128-account shared manager 同矩阵保持 process peak <=16、无持久禁用并完成每格 16/16 恢复；32-waiter 同 credential 四类失败每轮严格 1 hit。取消 permit 泄漏在修复前 21/22 红、结构化取消修复后 23/23 绿。详见 [2026-07-18 evidence](../evidence/oauth-auxiliary-budget-and-cancellation-20260718.md)。
- 2026-07-18 又关闭一类 provider/MCP 瞬态失败后的无意义等待：full `cargo test --all-targets` 曾在 `provider_transport_and_body_fault_matrix_is_private_typed_and_bounded` 的 `provider_header_timeout` 场景触发 30 秒测试超时。根因不是 private marker 泄漏，而是瞬态失败写入冷却后，下一轮在真正 HTTP send 前还会进入 scheduler acquire；当没有可立即调度的备选凭据时，这会把 request-scoped send budget 保护变成容量/冷却等待。当前 `maybe_exclude_after_transient_failure` 返回实际 retry target 是否存在，API 与 MCP 的 transport/body/non-eventstream/429/402/408/5xx/protocol fallback 分支只有存在备选凭据才继续重试，否则立即返回 typed error 并把 attempt action 改为 `fail`。复核结果：focused provider fault matrix `1/1` 通过，`cargo fmt --check + cargo check --all-targets` 通过，完整 all-target development run 为 `1724 passed / 0 failed / 6 ignored` 加 `kiro_loadtest 27/27`。证据见 [provider transient retry target guard](../evidence/provider-transient-no-retry-target-20260718.md)。这些运行因本机磁盘不足使用 7-10 GiB development reservation，不替代最终 release gate。
- 这些批次封住服务端多层 attempt、确定性 400 换号、模型目录账号数 fan-out、同进程 profile discovery 风暴和 process-local OAuth 跨账号/同账号放大，不等价于内部 RPM 已完全根治。持久 usage channel attribution、live Redis/PG、跨实例 aggregate admission、真实 handler/CLI、客户端重试、429/500/partial 与 L3-L5 recovery 仍未完成。

### 2026-07-17：refresh peer coordination deadline 边界修复与复核

隔离 PostgreSQL `127.0.0.1:47432`、Redis `127.0.0.1:47379` 上，原样执行
`ordinary_refresh_peer_wait_respects_coordination_deadline` 可稳定复现失败：期望
`RefreshFailureKind::Timeout`，实际为 `RefreshFailureKind::Coordination`，失败断言位于
`src/kiro/token_manager/manager_tests.rs:1485`（修复前行号）。这不是 Redis
`SET NX` 竞争失败，也不是应放宽测试期望。

根因在 peer 已持有 Redis refresh lock 的分支：本实例先执行
`sleep_until(min(now + 500ms, coordination_deadline))`，短预算场景会正好睡到 deadline；
醒来后旧实现仍无条件调用 PgSQL reload。此时 PgSQL deadline helper 因剩余时间为零返回错误，
而 `reload_credential_for_refresh_until` 又把所有 PgSQL 错误统一折叠为普通
`Coordination`，从而覆盖了外层应返回的 `Timeout`。这会让观测、retry classifier 和健康语义
无法区分“协调基础设施错误”与“协调等待按预算结束”。

窄修包含三点：

- peer sleep 醒来后在发起 PgSQL reload 前再次检查 coordination deadline，耗尽时直接返回
  `stage=Coordination / kind=Timeout / send_committed=false`，不再制造一次注定失败的 PgSQL 调用；
- PgSQL reload 若在 deadline 到达时失败，保留 `Timeout`；deadline 前的真实 PgSQL 错误仍为
  `Coordination`，没有把基础设施错误伪装成超时；
- 集成测试使用 PostgreSQL 实际分配的 credential id，不再硬编码 `1`，并在结果断言前释放
  Redis peer lock、删除测试 schema，避免断言失败再次遗留该测试的协调资源。

修复后在同一个 disposable scoped target 内连续执行 5 轮 `cargo +1.92.0 test
ordinary_refresh_ -- --nocapture --test-threads=1`。过滤器每轮实际覆盖 3 项：peer deadline、
PgSQL refresh failure 健康中立、取消 ordinary refresh 后的 critical Redis lock cleanup；结果为
`15/15 passed, 0 failed, 0 ignored`。五轮均确认 timeout 不增加
`refresh_failure_count`、不禁用 credential、不进入 cooldown，PgSQL 真实失败仍保持 typed 且健康中立。

构建生命周期证据：最终批次 admission 时可用 `49,364,808 KiB`，scoped target 峰值
`1,634,796 KiB`；退出记录为 `removed=true / reservation_released=true`。随后按 scope 检查
`target/.validation-build-refresh-coordination-*` 与对应 reservation 均为零。全局检查当时可见另一个
PID 明确归属的 `external-usage-attribution-2` 活跃 target；它不是本批产物，未删除。容器内 Redis
扫描 `kiro_rs:test:*:scheduler:refresh_lock:*` 为空，两个改动文件的 `rustfmt --check` 和
`git diff --check` 均通过。基线失败批次 `1,634,120 KiB`、一次被其他支线中间态阻断的批次
`1,126,220 KiB` 也分别由 wrapper 当轮删除并释放 reservation，没有跨批次累计。

这组证据只关闭 refresh peer deadline 的错误分类、健康中立和 cleanup 回归，不替代多实例 Redis
慢/断、完整 request-scoped auxiliary attribution、1/20/60 credential fan-out、L3 burst/recovery
及 L5 soak 门禁；这些仍按本专题其他条目保留为发布阻断，不能由本次 15/15 冒充完成。

### 2026-07-17：短 TTL 成功刷新绕过 singleflight 的 RPM 放大

全量测试暴露 `oauth_refresh_shared_manager_burst_has_process_concurrency_cap` 的 recovery 阶段在
16 个 caller 下出现 30 次 refresh HTTP。静态调用链确认这不是测试顺序污染：fake OAuth 的
成功响应与真实合法短期 token 场景一样只给 360 秒有效期；旧请求热路径同时使用“到期前 5 分钟”
和“到期前 10 分钟预警”两个窗口。刷新成功后 token 虽仍有约 6 分钟可用，却立即再次满足
`is_token_expiring_soon`。per-credential mutex 只能把并发等待者串行化，无法让等待者复用第一份
成功结果，因此突发调用会被变成逐个 OAuth refresh，解释了 sends 大于 caller 以及错误恢复期的
内部 RPM 放大。

修复将请求刷新、同进程锁后复检、跨实例 peer 接受、字段 CAS 冲突接受和辅助预算耗尽时的
refresh-required 排除统一到 5 分钟安全边界。10 分钟函数保留为状态预警/后台维护语义，禁止直接
驱动每请求刷新；没有改 credential JSON 或 PgSQL schema。测试夹具也不再依赖上一轮 6 分钟 token
触发下一轮重复刷新，而是每轮显式建立过期凭据。

新增确定性合同
`oauth_refresh_six_minute_token_singleflights_concurrent_waiters_for_five_rounds`：每轮 16 个 caller
同时进入同一 credential refresh mutex，5 轮均要求 refresh HTTP 严格为 1，立即后续请求仍为 1，
且 auxiliary/global in-flight 归零。连同以下过滤器必须由统一 scoped Cargo runner 执行；本次静态
支线未运行 Cargo，因此这里仍是待验证而不是通过证据：

```text
oauth_refresh_six_minute_token_singleflights_concurrent_waiters_for_five_rounds
oauth_refresh_shared_manager_burst_has_process_concurrency_cap
oauth_refresh_shared_success_sends_at_most_once_per_caller
oauth_refresh_failures_recover_naturally_in_same_manager_for_five_rounds
test_refresh_token_rejects_api_key_credential
```

### 2026-07-17：OAuth 失败波被 mutex 串行放大（阶段 A 历史检查点）

短 TTL 成功路径收敛后，同一实现仍有一个独立的失败路径缺口。旧的 per-credential mutex 只保证同一
时刻一个 refresh sender；leader 收到 500、网络错误、timeout 或 malformed response 后不会改变
过期 token，释放锁后每个原 waiter 都会再次通过“token 仍过期”的复检，并依次各发一次 OAuth。
因此它把并发 HTTP 从并行 N 次变成串行 N 次，但没有形成 singleflight。请求级 auxiliary budget
只能限制每个 caller，不能让 N 个独立 caller 共享 leader 的失败，正好解释了“下游 RPM 不高、
上游模型 RPM 也不高，但进程内 refresh RPM 很高”的一类现象。

阶段 A 实现把 `credential id -> TokioMutex` 收敛为固定大小的
`credential id -> CredentialRefreshState`：每个状态只有一个 async gate 和一个 typed negative
result。失败身份是认证方式、refresh token、client id/secret、token endpoint、region、scope、
machine id、storage revision、TLS backend 与实际代理的 SHA-256 版本摘要；摘要不进入日志、错误或持久化。不同 ID 使用
不同状态槽，同一 ID 更新任一认证字段后摘要立即失配；API Key 在状态分配前直接返回，因而不会和
OAuth 或其他 API Key 共用失败结果。删除/reload 会按当前 credential ID 集合回收状态，状态数不会
随请求、waiter 或失败波增长。

共享边界是 typed 且保守的：已提交 send 的错误可以共享；`RequestSend` 阶段的 network/timeout
即使无法证明 server 收到请求也可以短期共享，因为每个 caller 已消耗一次真实辅助 send admission。
本地 validation、client construction 与 budget/concurrency admission 不进入 negative result；typed
Redis refresh coordination timeout/error 作为 health-neutral 内部故障进入同一个短窗口，避免 waiter
逐个重复 acquire/poll。leader 保留原有 invalid-grant disable、auth cooldown 和 429 Retry-After 决策；
followers 带 `shared_failure_wave=true`，只在自己的请求内排除该 credential，不重复增加 health streak、
延长 cooldown、禁用或消费 auxiliary send budget。这样同时封住 HTTP 放大和内部健康状态放大，
但不改变不同 credential 间的合法 failover。Admin/direct refresh 调用不拥有 scheduler health action，
其 auth/invalid-grant/429 失败可关闭重复 send，但保留一个原子 `health_action_pending`；首个正常
scheduler caller 领取并执行一次健康动作，其他 direct/normal followers 都只共享结果。

失败窗口首次为 500ms，并按相同认证版本指数增长，使用认证摘要生成 80%-100% 稳定错峰，最大
30 秒；429 的 `Retry-After` 可把窗口延长到最多 60 秒，较长的 credential cooldown 仍由现有
scheduler authority 负责。过窗后允许一个新 leader 探测，成功立即清除失败结果，认证版本变化也
立即绕过，因此长期故障不会永久黏住。正常未过期 token 和 API Key 路径不做 hash、不获取这张 map；
过期 OAuth 路径每次仅做一次固定 SHA-256 和 O(1) map/state 检查，不创建 timer、background task、
waiter list 或每请求缓存对象。

新增/更新的确定性合同如下。本静态支线按任务约束没有运行 Cargo、服务或网络，因此只能记录为
`implemented / pending scoped execution`，不能写成通过：

```text
refresh_negative_result_is_typed_versioned_bounded_and_expires_for_five_rounds
api_key_token_path_does_not_allocate_oauth_refresh_state
oauth_refresh_transient_failure_wave_singleflights_32_waiters_for_five_rounds
oauth_refresh_auth_and_retry_after_wave_mutate_health_once_for_five_rounds
oauth_refresh_failures_recover_naturally_in_same_manager_for_five_rounds
oauth_refresh_shared_manager_burst_has_process_concurrency_cap
oauth_refresh_six_minute_token_singleflights_concurrent_waiters_for_five_rounds
```

关键绿灯断言：500/timeout/disconnect/malformed success 每类 32 caller x 5 轮均应严格 1 个 OAuth
HTTP、1 次合计 auxiliary token-refresh consumption、31 个 typed follower、0 cooldown/disable；
invalid-client/429 每类 32 caller x 5 轮也应严格 1 HTTP 且 transient health streak 严格为 1。
失败窗口到期后同一 manager 必须自然恢复，全部 in-flight/auxiliary permits 归零。
Redis coordination 已接入同一状态，但本静态支线没有现成的可注入 Redis hit-count fixture；
`32 waiter -> 1 acquire/poll wave` 的动态合同仍是 blocking follow-up，不能由 OAuth fake 测试代替。

该阶段未关闭的边界必须保留为发布阻断：negative result 是 process-local，多实例在同一故障窗口
理论上仍可各有一个 leader；process-local 跨账号 hard budget 和同账号 32-waiter 已由 2026-07-18
fake HTTP 多轮矩阵关闭，但 1/60 精确 provider cells、Redis aggregate 和持久 usage ledger 仍未关闭。
Admin 外部凭据 probe 目前只有全局 auxiliary concurrency gate，没有纳入持久 credential ID 状态。
caller 在 refresh 产生 typed result 前被取消时没有可共享的 negative result；同步 permit/task cleanup
合同已通过，但 client-drop burst 仍必须在 L4 验证不会形成重复发送反馈环。
另外，5 分钟是当前请求可用性的权威边界，合法但剩余时间小于等于 5 分钟的上游 token 仍会被判为
不可用；6 分钟成功合同不能作为该边界的反证。跨实例成功收敛仍依赖 PgSQL CAS 与 peer reload，
必须由 Redis 慢/断、CAS conflict 和小于等于 5 分钟 token 的单独测试验证。后续集群状态机、refresh RPM admission、conditional 401 recovery 和本轮实际执行结果统一由 [独立专题](token-refresh-failure-wave-and-cluster-rpm.md) 与 [partial evidence](../evidence/token-refresh-failure-wave-and-cluster-rpm-20260717.md) 维护。post-correction process-local 60/8、limit/config/revision 与 API/MCP final-attempt zero-refresh 已各五轮通过；live Redis、cluster/PG CAS/cancellation/load/frozen candidate 仍未关闭，因此不得把阶段 A 的 focused pass 写成整体通过。

## 验收、回滚与残余风险

2026-07-17 对 external eligibility 的复核补充了另一类内部放大：健康本地请求此前会先执行 external live availability，包含 PostgreSQL pool list 和 Redis runtime snapshot；不同 model 还会互相驱逐单 entry cache。完整静态列表 + process-local singleflight 是正确方向，但第一版在 TTL miss 上让全部并发等待 PostgreSQL，并把错误负缓存成空池。修复门禁必须同时限制三种量：健康 local 的 external Redis RTT 为 0、每 generation PostgreSQL list 为 1、TTL/故障边界的所有请求延迟有上限。只证明“PG 查询数为 1”不能证明没有 HOL。

真实 local failure 后的 authoritative external selection 仍会每请求读取 PostgreSQL 并执行 Redis snapshot/acquire。它必须有独立 timeout、请求/attempt 归因和 burst 上限；静态 stale snapshot 不得替代真实 dispatch authority。cross-instance pool revision 与 selection-to-send TOCTOU 另列 external coordinator 发布阻断。

429/500/400/partial/malformed、1/20/60 账号、单/多 key 和客户端重试每类至少 5 轮；400 必须拆成明确 model-unavailable、invalid model、invalid tool use、`REQUEST_BODY_INVALID` 空图和普通 malformed body。只有策略明确允许的可恢复分支可换号；空图/普通 body 在 provider 层最多 1 hit，本地可判定时为 0 hit。fake upstream hit 数不得超过硬预算，恢复请求 100% 成功，usage 中 downstream=1 与所有 attempt/action 可核对。

性能与资源门禁还要求：admission 正常命中只做 O(1) 本地工作；错误期不存在随账号数增长的扫描/HTTP fan-out；队列、key state、RSS、FD 和任务数有界；L3 burst 后恢复，L5 三轮 idle 后资源回落。多实例 aggregate admission 尚无完成方案，当前每实例限制会随实例数放大，是明确的发布阻断/残余风险，不能用单实例测试关闭。

profile discovery 的正常已有 ARN 路径已用 5 x 1000 次聚焦调用证明 0 HTTP、0 state entry，代码也在 fingerprint/lock 前返回；缺 ARN 路径为固定 SHA-256 + O(1) bounded map。该证明不替代统一 release binary 的 RSS/FD/TTFB 与 soak。singleflight/backoff 目前是进程内的，多实例理论上每实例每窗口各 1 hit；usage 也尚未持久化 auxiliary channel，因此仍是发布前明确待补证据。

回滚必须按层进行：可以关闭新 request-key admission 以恢复旧兼容行为，但 shared inference hard budget 和“下游已提交后禁止 retry”属于正确性保护，不应通过 prompt master 或 UI 总开关回滚。若新 classifier 遇到未知 400，默认保守失败而非轮换全部账号。
