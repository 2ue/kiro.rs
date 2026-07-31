# 整体调度架构分析：本地凭证、外部池、fallback/rescue 与容量账本

Status: `analysis-planned / read-only-source-inventory-started`

Severity: `P0`

Last reviewed: 2026-07-28 Asia/Shanghai

Owner intent:

> 仅分析整体调度，包括但不限于外部池和本地凭证。用户认为调度存在较大结构性问题，几种模式体感作用不明显。必须充分分析当前调度逻辑，说明如何设计更科学；可参考 `../sub2api`、`~/Desktop/project/new-api`、`~/Desktop/project/CLIProxyAPI`。本阶段不改代码。

## 1. 分析目的

本分析不是为了修一个单点 fallback 条件，而是为了回答以下架构问题：

1. 当前 kiro.rs 的“调度”到底由哪些组件共同决定，而不是只看外部池或本地凭证单点。
2. 本地凭证、外部池、request admission、Redis 分布式调度、sticky session、MCP/WebSearch、retry、fallback、rescue、usage/stat/dashboard 写入之间是否存在隐式耦合。
3. 当前几种模式为什么用户体感“不起作用”或“不符合预期”：是配置命名问题、作用范围问题、实现错误，还是缺少统一调度模型。
4. 外部池开启后为什么可能影响本地凭证，即使外部池理论上只是备用池。
5. 哪些场景应该 fallback 到外部池，哪些场景不能 fallback，哪些场景可以外部池直连，哪些场景允许外部失败后 rescue 回本地，并且如何保证不出现本地/外部池回环。
6. 当前下游 RPM、内部 RPM、selection RPM、上游真实 send RPM、MCP auxiliary RPM、usage 记录 RPM 的口径是否混乱，是否解释了“下游不高但内部飙升”的现象。
7. 如何设计一个更科学、可解释、可测试、可观测、不会因统计/dashboard/Redis/PgSQL 尾延迟拖垮主业务的调度架构。

最终目标是形成可执行的调度重构设计，而不是继续在 handler 里堆叠更多局部 if。

## 2. 本阶段范围

本阶段只做只读分析和文档记录，不改实现代码、不跑压测、不触碰生产流量。

纳入分析：

- 请求入口 admission。
- 本地凭证调度、账号过滤、并发/RPM、sticky session。
- Redis scheduler state sync、lease、breaker、degraded 行为。
- 外部池启用、direct policy、static/authoritative snapshot、pool selection、external capacity mode、external failover。
- 本地错误后 external fallback。
- 外部池错误后 local rescue。
- MCP/WebSearch 辅助调用对本地凭证、attempt budget、usage/error 记录的影响。
- usage、stats、dashboard、runtime mutation 对主业务调度资源的影响边界。
- 多实例部署下入口 admission、本地 Redis scheduler、外部池全局容量之间的关系。
- 参考项目中可借鉴的路由规划、候选选择、等待计划、决策 trace、插件/执行器边界。

不纳入本阶段：

- 不直接修复代码。
- 不提交 tag 或发版。
- 不以当前文档代替后续真实测试结论。
- 不把参考项目实现直接视为正确答案；只提炼可借鉴设计边界。

## 3. 当前初步判断

当前源码显示，kiro.rs 的调度不是一个单一调度器，而是多套局部机制叠加：

```text
HTTP request
  -> request admission
  -> body/model/cache/prompt/payload 处理
  -> external direct policy
  -> local pool preflight
  -> local credential acquire
  -> local upstream call
  -> local error classifier
  -> external fallback
  -> external pool failover
  -> external final error classifier
  -> local rescue
  -> usage/stat/runtime mutation
```

这导致现有“模式”多数只是局部策略：

- 本地 load balancing mode 只影响本地候选账号已经 dispatchable 之后的排序/抽样。
- `AcquireMode` 只影响本地凭证 acquire 的等待/快速失败语义。
- `externalPoolCapacityMode` 只影响外部池内部容量满时是否等待。
- `fallbackOn*` 只影响本地错误原因是否允许切外部池。
- direct policy 可以直接绕过本地。
- local rescue 又是外部池失败后的另一条局部补救链路。

因此，用户体感“几种模式没啥作用”是合理风险：它们不是无效，而是没有被统一成一个用户可理解、工程可证明的全局 RoutePlan。

## 3.1 根因假设 / Root Cause 待验证

当前阶段的根因假设是：

1. 调度职责分散在 request admission、handler、本地 token manager、external pool、provider error classifier、MCP/WebSearch auxiliary、usage/runtime mutation 多处，缺少单一 RoutePlan。
2. 本地凭证和外部池不是两个互不干扰的 pool。外部池 immediate availability、snapshot、fallback/rescue 判断、bad config、capacity wait 和 PgSQL/Redis 触点会在部分 local-first 路径进入主请求链路。
3. attempts、fallback、rescue、MCP auxiliary 与 usage 记录缺少统一容量账本，导致“下游 RPM 不高，但内部 selection/send/error RPM 放大”的生产现象难以解释和限制。
4. Redis/PgSQL/usage/dashboard 等观测或持久化路径仍可能通过同步桥、热路径等待或高基数查询影响主业务调度。

这些是架构级待验证根因，不是最终断言。后续必须通过源码链路图、状态机矩阵和配置组合测试确认或修正。

## 4. 分析方法

### 4.1 当前 kiro.rs 源码梳理

按以下顺序只读梳理：

1. 入口准入：确认 request API key admission 的作用范围、是否跨实例、是否限制 attempts。
2. 本地凭证调度：确认候选过滤、并发/RPM、sticky、load balancing mode、Redis scheduler 参与点。
3. 外部池调度：确认 direct、preflight、snapshot、capacity mode、pool failover、bad config 隔离、Redis/PgSQL 触点。
4. fallback/rescue 状态机：确认 local -> external、external -> local rescue、direct external 禁 rescue、防回环和 attempt budget。
5. usage/observability：确认 routeKind/routeSubtype、selectionFailure、attempt budget、内部 RPM 口径是否能解释生产现象。
6. 故障域：确认 Redis/PgSQL/dashboard/usage 是否可能影响主业务调度。

### 4.2 参考项目对照

只提炼设计模式，不直接照抄实现：

- `new-api`：关注 channel distributor、group/model/priority/weight/retry 语义。
- `sub2api`：关注账号 scheduler、sticky/load-aware、AccountSelectionResult、WaitPlan、scheduleDecision、failover loop。
- `CLIProxyAPI`：关注 ModelRouter / Scheduler / Executor 分层、候选 auth 调度、插件异常 fuse、路由与执行边界。

### 4.3 设计评价标准

每个方案必须回答：

- 能否解释每条请求为什么走本地、外部池、direct、fallback 或 rescue。
- 能否限制总 upstream sends，防止内部 RPM 因 retry/fallback/rescue 放大。
- 能否避免外部池坏配置或外部池 Redis/PgSQL 抖动影响本地 ready 请求。
- 能否保证一条请求不会在多个队列连续等待导致尾延迟放大。
- 能否在多实例部署下区分下游 RPM、内部 attempts、local sends、external sends、MCP sends。
- 能否把 usage/dashboard/stat 失败降级为观测延迟，而不是主业务阻断。
- 能否用表驱动状态机测试覆盖所有关键组合。

## 5. 重点参考位置

### 5.1 kiro.rs 当前调度相关位置

- Request admission 配置：`src/model/config.rs:3003`
- Request admission middleware：`src/anthropic/request_admission.rs:1049`
- `/v1/messages`、`/cc/v1/messages`、`/ha/v1/messages`、`/dfcache/*/v1/messages` admission 挂载：`src/anthropic/router.rs`
- 本地 `AcquireMode`：`src/kiro/token_manager/types.rs:122`
- 本地 route state：`src/kiro/token_manager/route_state.rs:5`
- 本地账号 dispatchable 过滤：`src/kiro/token_manager/capacity.rs:29`
- 本地 RPM selection window：`src/kiro/token_manager/rpm.rs`
- 本地 health/weighted/balanced 策略：`src/kiro/token_manager/strategy.rs`
- 本地候选选择：`src/kiro/token_manager/manager.rs:5089`
- 本地 acquire 主循环：`src/kiro/token_manager/manager.rs:5612`
- Redis scheduler affinity/hot/snapshot block/await 桥：`src/kiro/token_manager/manager.rs:3410`、`src/kiro/token_manager/manager.rs:3455`、`src/kiro/token_manager/manager.rs:3551`
- Redis scheduler state sync：`src/kiro/token_manager/manager.rs:9332`
- PgSQL credential runtime mutation：`src/kiro/token_manager/manager.rs:2010`、`src/kiro/token_manager/manager.rs:8946`
- 外部池配置：`src/model/config.rs:2677`
- 外部池 direct policy：`src/external_pool.rs:3124`
- 外部池 immediate availability：`src/external_pool.rs:4210`
- 外部池 forward/failover 主循环：`src/external_pool.rs:4394`
- 外部池 capacity unavailable / wait：`src/external_pool.rs:5776`
- handler 中 local attempt policy：`src/anthropic/handlers.rs:1569`
- handler 中 local preflight：`src/anthropic/handlers.rs:1664`
- handler 中 local error -> external fallback：`src/anthropic/handlers.rs:1725`
- fallback reason matrix：`src/anthropic/handlers.rs:2005`
- external -> local rescue：`src/anthropic/handlers.rs:6259`
- shared inference attempt budget：`src/anthropic/inference_attempt_budget.rs:175`
- 既有问题文档：`feature/issues/external-pool-scheduler-interference-and-fallback-matrix-20260727.md`
- 既有问题文档：`feature/issues/retry-budget-admission-and-rpm-amplification.md`
- 既有问题文档：`feature/issues/redis-scheduler-degraded-and-fallback.md`
- 既有问题文档：`feature/issues/business-observability-redis-fault-domain.md`
- 既有问题文档：`feature/issues/runtime-completion-storage-bridge-starvation.md`
- 既有问题文档：`feature/issues/dashboard-observability-redesign.md`

### 5.2 new-api 参考位置

- 路由注册和 relay middleware：`~/Desktop/project/new-api/router/relay-router.go`
- Channel distributor：`~/Desktop/project/new-api/middleware/distributor.go`
- Channel 选择与 retry/group/priority：`~/Desktop/project/new-api/service/channel_select.go`
- Channel cache 与 priority/weight 选择：`~/Desktop/project/new-api/model/channel_cache.go`
- 模型请求限流：`~/Desktop/project/new-api/middleware/model-rate-limit.go`
- 通用 Redis rate limit：`~/Desktop/project/new-api/middleware/rate-limit.go`

关注点：

- group/model/path -> channel 的路由边界。
- priority/weight/retry 的用户可解释语义。
- middleware 先选 channel，relay adaptor 后执行的职责分离。

### 5.3 sub2api 参考位置

- OpenAI 账号 scheduler：`../sub2api/backend/internal/service/openai_account_scheduler.go`
- AccountSelectionResult / AccountWaitPlan：`../sub2api/backend/internal/service/gateway_service.go`
- OpenAI gateway failover loop：`../sub2api/backend/internal/handler/openai_gateway_handler.go`
- 并发槽获取与等待：`../sub2api/backend/internal/handler/gateway_helper.go`
- Concurrency service：`../sub2api/backend/internal/service/concurrency_service.go`
- scheduler snapshot / outbox：`../sub2api/backend/internal/repository/scheduler_cache.go`、`../sub2api/backend/internal/repository/scheduler_outbox_repo.go`

关注点：

- scheduler 返回 `SelectionResult + WaitPlan + ScheduleDecision`。
- sticky / previous response / load balance 分层记录。
- handler failover 使用 failedAccountIDs 排除已失败账号。
- 并发等待有明确 timeout、max waiting 和 streaming ping 行为。

### 5.4 CLIProxyAPI 参考位置

- PluginHost ModelRouter：`~/Desktop/project/CLIProxyAPI/internal/pluginhost/model_router.go`
- PluginHost Scheduler：`~/Desktop/project/CLIProxyAPI/internal/pluginhost/scheduler.go`
- Executor route readiness：`~/Desktop/project/CLIProxyAPI/internal/pluginhost/executor_route.go`
- Scheduler / ModelRouter API 类型：`~/Desktop/project/CLIProxyAPI/sdk/pluginapi/types.go`
- Built-in auth scheduler：`~/Desktop/project/CLIProxyAPI/sdk/cliproxy/auth/scheduler.go`
- Auth conductor scheduler bridge：`~/Desktop/project/CLIProxyAPI/sdk/cliproxy/auth/conductor.go`
- Usage queue 旁路参考：`~/Desktop/project/CLIProxyAPI/internal/redisqueue/queue.go`

关注点：

- ModelRouter、Scheduler、Executor 的职责边界。
- scheduler 只能从候选 auth 中选择或委托内置策略，不能任意重入整条路由。
- 插件异常 fuse，不让坏路由器持续影响主链路。
- usage queue 是旁路队列，不参与执行路径。

## 5.5 复现 / 观测方法

本问题不是单个固定错误码，复现应覆盖多组可观测现象：

1. local-only：关闭外部池，验证本地凭证容量/RPM/冷却/Redis degraded 下的 route decision、attempts、usage 口径。
2. local-first + external enabled：本地 ready、本地容量满、本地 RPM 满、本地无凭证、本地全禁用、本地 Redis degraded 分别验证是否触发外部池，以及触发前是否等待外部池权威 snapshot。
3. external direct：开启外部池直连后验证请求不调度本地凭证；外部池不可用时只按 external direct 策略失败，不 rescue 本地。
4. external fallback/rescue：本地失败切外部池、外部池失败 rescue 本地时，记录 failed local credential IDs、failed pool IDs、attempt budget 和 route trace，确认不会 local -> external -> local -> external 回环。
5. 故障注入：外部池 bad config、外部池全部冷却、外部池容量满、外部池 429/5xx/timeout、Redis 延迟/断连、PgSQL pool 等待、usage/dashboard 查询慢，确认本地 ready 请求不被拖慢。
6. 口径核对：同一压测窗口同时采集 downstream accepted RPM、admission reject RPM、local selection RPM、local upstream send RPM、external send RPM、MCP auxiliary RPM、usage success/error RPM。

验收复现不能只看页面 RPM 卡片；必须同时看 route decision trace、attempt ledger、上游 send 计数和资源采样。

## 6. 后续分析产物

后续应至少输出以下产物：

1. 当前调度链路图：从入口到本地、外部池、MCP、usage 的完整调用链。
2. 当前配置语义表：每个模式/开关实际控制什么、不能控制什么。
3. fallback/rescue 状态机矩阵：列出 local/external/direct/rescue 的所有允许和禁止转换。
4. 容量账本分析：downstream RPM、local selection RPM、local upstream send、external send、MCP auxiliary send、usage RPM 的口径拆分。
5. 故障域分析：Redis/PgSQL/usage/dashboard/external snapshot 对主业务调度的影响边界。
6. 与 `new-api`、`sub2api`、`CLIProxyAPI` 的对照表：可借鉴点、不可照抄点、适配 kiro.rs 的原因。
7. 新调度方案草案：RoutePlanner、RoutePlan、CapacityLedger、PoolScheduler trait、RouteExecutor finite-state-machine。
8. 测试矩阵草案：不用生产流量即可验证各配置组合是否不会回环、不会多层排队、不会内部 RPM 无界放大。

## 7. 拟议设计方向

初步方向如下，后续需要继续源码级论证：

```text
RequestAdmission
  -> RoutePlanner
  -> CapacityLedger
  -> RouteExecutor finite-state-machine
      -> LocalCredentialPoolScheduler
      -> ExternalPoolScheduler
      -> Optional LocalRescue
  -> Async ResultRecorder / UsageWriter
```

核心原则：

1. 一条请求先生成不可变 `RoutePlan`，再执行；不要在 handler/provider/external manager 多处现场改道。
2. 本地池和外部池实现同一类 scheduler 接口：`snapshot`、`try_acquire`、`release`、`report_outcome`。
3. 所有真实上游发送都从同一个 `CapacityLedger / AttemptBudget` 扣账。
4. local-first 模式下，如果本地池 Ready 且有 dispatchable credential，外部池 bad config、snapshot、Redis coordinator 不得进入主路径。
5. 一条请求最多只允许一个主要等待点，避免 admission queue、本地 queue、外部池 queue、rescue wait 连续叠加。
6. direct external 是明确策略；local-first fallback 是另一种策略；external-first local rescue 也应是独立策略。
7. usage、dashboard、统计、runtime rollup 必须作为旁路结果消费方，不能阻断 dispatch、forward、stream、release。
8. 每条 usage 需要记录 route decision trace 和 attempts ledger，保证生产问题可解释。

## 7.1 方案 / Selected Direction 草案

当前建议方案是把“路由规划”和“执行”拆开：

- `RoutePlanner`：基于配置、请求事实、本地/外部池 cached snapshot 生成不可变 RoutePlan。
- `CapacityLedger`：统一限制 request、local send、external send、MCP auxiliary send 和 rescue attempt，防止内部 RPM 放大。
- `PoolScheduler` trait：本地凭证池和外部池都暴露 `snapshot / try_acquire / release / report_outcome`，避免 handler 针对两类 pool 写分叉状态机。
- `RouteExecutor` finite-state-machine：只按 RoutePlan 执行有限转换，禁止隐式重入整条路由。
- `Async ResultRecorder / UsageWriter`：记录 route trace、usage、dashboard rollup，但失败只能影响观测延迟，不能阻塞主请求 release/stream/return。

实现前还需要把配置语义表和状态机矩阵补全，避免把当前局部 if 迁移成新的局部 if。

## 8. 初步验收标准

调度重构设计只有满足以下条件，才算可以进入实现：

- 能用一张状态机表解释 local-only、local-first、overflow、external-direct、external-first-rescue 的所有转换。
- 能明确证明不会出现 local -> external -> local -> external 回环。
- 能解释为什么某个模式开关会生效，以及在哪些路径不会生效。
- 能证明外部池关闭、外部池全坏、外部池容量满、外部池 Redis degraded 时，本地 ready 请求不被拖慢或改道。
- 能区分并记录 downstream accepted RPM、local send RPM、external send RPM、MCP send RPM、internal amplification ratio。
- 能保证 usage/dashboard 失败不影响主业务请求。
- 能给出覆盖本地凭证健康/容量满/RPM 满/Redis degraded/无凭证/全禁用、外部池关闭/无池/可用/满/冷却/坏配置/Redis degraded 的测试矩阵。

## 8.1 风险 / 回滚边界

主要风险：

- 调度重构会影响请求入口、provider、token manager、external pool、usage、dashboard 多个边界，必须分阶段落地并保留旧策略开关或兼容路径。
- 如果只实现 RoutePlanner 但没有 CapacityLedger，可能仍然出现 attempts 放大。
- 如果只缓存外部池 snapshot 但没有明确 stale/authoritative 语义，可能把坏外部池状态传播到本地 ready 路径。
- 如果只把 writes 异步化但 release 顺序错误，可能造成 lease 泄漏或凭据并发槽假满。
- 如果观测字段不稳定，生产排查会再次依赖页面 RPM 猜测。

回滚边界：

- 产品行为改变必须 behind runtime config 或按小步兼容发布；出现异常时能回退到当前 local-only/local-first/direct external 的既有路径。
- 新的 route trace/ledger 字段应先只增不删，避免破坏现有 usage/dashboard 查询。
- 外部池策略变更应先覆盖 local-only、external disabled、direct external、local-first overflow、external failure rescue 的表驱动测试，再允许默认启用。

## 9. 当前未关闭问题

- 当前只完成分析目的、范围、方法和参考位置登记。
- 还没有输出完整源码级调用链图。
- 还没有输出完整 fallback/rescue 状态机矩阵。
- 还没有完成与三个参考项目的逐项对照表。
- 还没有形成最终 RoutePlan/CapacityLedger 详细接口设计。
- 还没有进行实现或验证。
