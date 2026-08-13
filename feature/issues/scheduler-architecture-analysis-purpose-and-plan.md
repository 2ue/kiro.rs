# 整体调度架构分析：本地凭证、外部池、fallback/rescue 与容量账本

Status: `analysis-complete / target-design-proposed / implementation-not-authorized`

Severity: `P0`

Last reviewed: 2026-08-04 Asia/Shanghai

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
8. 路线轨迹和 attempts ledger 可以作为 usage 明细的独立诊断附属信息，保证生产问题可解释，但不能参与最终 usage 的 token 整形、原始成本、展示计费或下游标准字段计算。
9. 最终 usage 必须继续由 usage 配置、真实上游 usage 或明确的本地估算 fallback 独立计算；调度重试、换池、冷却和排队不能改变相同请求的最终 usage 结果。

## 8. 本轮源码核对结论

本节记录的是当前工作树源码已经核对到的真实行为，不是目标行为。源码证据集中在：

- 入口与准入：`src/anthropic/handlers/request_entry.rs`、`src/anthropic/request_admission.rs`。
- 本地凭证：`src/kiro/token_manager/manager.rs`、`route_state.rs`、`capacity.rs`、`strategy.rs`。
- 外部池：`src/external_pool.rs`、`src/external_pool/retry_pipeline.rs`。
- 失败映射与 fallback：`src/anthropic/handlers.rs`、`src/anthropic/inference_attempt_budget.rs`。
- 配置合同：`src/model/config.rs`。
- 对照实现：`../sub2api/backend/internal/service/openai_account_scheduler.go`、`openai_gateway_handler.go`、`openai_gateway_scheduling.go`、`gateway_service.go`。

### 8.1 入口不是策略

内置入口 `/v1`、`/cc`、`/ha`、`/na` 以及动态入口只是路由入口。当前代码已经通过 `external_pool_route_allowed(endpoint)`、`cache_policy_for_path(endpoint)`、提示词路由规则等配置解析策略；不应再把某个路径名称当作“本地优先”“外部直连”或“特殊模型处理”的隐含开关。

因此后续设计中的“本地优先”“外部直连”“是否允许 fallback/rescue”都必须来自运行配置和路由规则。除非协议注册、兼容性或安全边界必须识别入口，否则不能写死 `/cc`、`/v1`、`/ha`、`/na`。

### 8.2 请求准入与排队

`request_admission_middleware` 先按请求 API Key 做进程内准入：

1. 检查“请求 API Key admission”是否启用。
2. 检查单 Key 的“RPM”。
3. 检查“最大并发请求数”。
4. 并发满时进入“最大排队数量”限制的 FIFO 等待；队列满或“排队超时”直接向下游返回 429/503 类错误。
5. 本地调度错误可以给当前请求 API Key 写入短暂“本地临时退避”，后续请求在准入层快速返回，避免同一调用方在本地池明显不可用时继续放大重试。

这个准入队列是第一层等待。它不等价于本地凭证队列或外部池队列；当前系统存在多层等待可能连续叠加。

### 8.3 Raw 请求入口

`handle_messages_endpoint` 先做轻量 Raw 探测和协议校验，再按配置执行：

1. 缺少“最大输出 token”默认策略。
2. JSON/历史 transcript 探测。
3. 严格协议污染检查。
4. 仅在请求历史未被清洗时尝试 Raw 外部直连。
5. 若未直连，且“本地池预检”命中且外部池有可用容量，则可在完整解析前把 Raw 请求发送到外部池。
6. 否则进入完整 `MessagesRequest` 解析和标准本地链路。

Raw 外部路径的正文仍由选中的外部池在发送阶段处理。`请求正文模式` 不应参与候选池筛选；它只决定已经选中的池如何处理正文。当前版本已将这两层解耦，但文档和观测仍需要明确区分。

当前源码仍保留一个轻量“是否存在模型兼容外部池”的 Raw 入口检查（`raw_external_pool_has_eligible_pool`）。这不是容量租约承诺，也不是按 `请求正文模式` 筛选；真正的权威池快照、冷却、容量和租约检查仍在外部发送循环中完成。后续若要取消该轻量检查，必须单独评估“无模型候选时是否应在解析前直接返回外部错误”这一协议语义，不能把它误写成“Raw 直连完全不预检”。

### 8.4 标准请求的总链路

完整解析后的标准请求顺序为：

```text
请求 API Key 准入
  -> 请求体/图片/文档/工具协议处理
  -> 提示词策略（按配置）
  -> 建立外部 fallback 上下文
  -> 外部直连策略判断
  -> 模型解析/能力校验
  -> 原生 WebSearch MCP 分支（如命中）
  -> 本地正文处理与容量权重计算
  -> 本地池预检（如启用）
  -> 本地凭证 acquire + Token refresh + Redis 调度租约
  -> 本地上游请求
  -> 本地错误分类
  -> 外部池 fallback（如允许）
  -> 外部池内部同池重试/换池
  -> 外部最终错误
  -> 有条件的本地 rescue（仅 local-first fallback 链）
  -> usage/诊断/统计旁路写入
```

其中流式和非流式共用上面的路由骨架，但“下游是否已经提交字节”会决定是否还能安全换号：

- 首个语义 SSE 字节提交前，允许按配置执行流重试。
- 已提交 `message_start`、文本、thinking、`tool_use`、错误事件或其他语义字节后，不得重放请求，除非协议层明确判定只写出了非语义 keepalive。
- 非流式请求在响应体完整读取和协议校验前仍可按推理尝试预算重试。

### 8.5 外部直连

当“外部池是否启用”“外部池直连策略”“入口路由规则”以及模型/路径规则共同命中时，`direct_policy_response` 直接进入 `ExternalPoolManager::forward_with_failover`：

- 不执行本地凭证 acquire。
- 不执行本地容量预检。
- 不因为外部池 失败隐式回本地；当前 `local_rescue_reason_after_external_error` 也会在“外部池直连策略”开启时直接禁止 local rescue。
- 仍会执行外部池自身的候选、容量租约、模型支持、冷却、同池重试、跨池重试和最终错误映射。

这是用户要求的“显示直连外部池时，即使错误也不要 fallback 本地账号”的正确语义。若生产日志同时出现本地错误，必须先确认该请求是否来自“仅本地”入口或另一条请求，而不能只凭 `Fallback 原因` 字段推断发生了直连回本地。

### 8.6 本地池预检与本地优先

当未命中外部直连、启用了“本地池预检”且外部池允许该入口时，系统读取本地池最新路由状态：

| 本地路由状态 | 含义 | 当前是否可在发送前切外部 |
| --- | --- | --- |
| `Ready` | 至少一个账号模型兼容、代理可用、未冷却、RPM/并发可用 | 否 |
| `CapacityFull` | 账号或全局并发槽满 | 仅当“本地容量耗尽时 fallback”开启且外部有立即可用容量 |
| `AllCoolingDown` | 候选账号均处于账号冷却或 RPM 等待 | 仅当“本地临时耗尽时 fallback”开启且外部有立即可用容量 |
| `SchedulerRedisDegraded` | 本地 Redis 调度协调异常 | 仅当“Redis 调度降级时 fallback”开启且外部有立即可用容量 |
| `RiskCircuitOpen` | 本地池风险熔断 | 仅当本地熔断 fallback 配置允许且外部有立即可用容量 |
| `NoCredentials` / `AllDisabled` / `ProxyBlocked` | 没有可用本地账号 | 仅当“无可用本地凭证时 fallback”开启且外部有候选 |
| `NoModelCompatible` | 本地账号不支持请求模型 | 仅当“不支持模型时 fallback”开启且外部有候选 |

发送前预检只决定“是否绕过本地等待并直接尝试外部”，不是容量的最终承诺。外部调用前仍会重新读取权威池快照并申请租约。

当前实现的一个重要细节是：本地池预检只在“新请求选择阶段”使用；本地请求已经进入上游后，不会因为预检状态变化自动取消本地请求。上游失败后是否切外部由错误分类、最新本地状态、配置和外部池可用性共同决定。

### 8.7 本地凭证 acquire、排队与同请求换号

`acquire_context_for_session_with_mode_and_auxiliary_budget` 的真实行为：

1. 读取当前本地账号和运行配置，建立本请求的 `local_excluded_ids`。
2. 如果有会话绑定且绑定账号仍满足模型/代理/冷却/RPM 条件，优先命中 sticky。
3. 否则按“负载均衡模式”从可调度候选中选择：
   - `priority`：优先级为第一排序，同优先级内偏向低并发。
   - `balanced`：按本地实现的均衡键。
   - `weighted_least_inflight`：按加权最小并发评分。
   - `health_balanced`：综合错误率、延迟、负载、优先级、观察期和选择压力。
4. 候选过滤包含：未被本请求排除、未禁用、账户配额保护未命中、支持模型、代理资源可用、无账号冷却、无 RPM 阻塞、账号并发和全局并发可用。
5. 选中后先占用本地并发租约，再尝试 Token refresh；Token refresh 需要独立的“辅助上游最大尝试次数”和辅助并发上限。
6. 选中账号在槽位竞争中失败时，当前请求会把账号放入 `local_excluded_ids` 并换号；普通容量等待则进入本地调度等待队列。
7. 上游 refresh/协调失败不一定修改账号健康：共享失败波、Redis/PgSQL 协调失败、未提交的辅助失败只在当前请求临时排除，避免把基础设施抖动放大成账号禁用。
8. 已发送的本地推理请求失败后，上层依据 `KiroCallFailureKind`、错误文本、已尝试账号和配置决定是否进入外部 fallback；本地 provider 自身在“推理尝试预算”内也可能先换号。

本地账号的失败处理并不是“一次请求只打一个账号”，但它也不是无限换号：本请求有总“推理上游最大尝试次数”，本地 acquire 还有按账号数与失败阈值计算的上限，外层流式首输出前重试还有单独安全窗口。

### 8.8 本地错误到外部池

`fallback_after_local_error_outcome_with_diagnostics` 的判断顺序：

1. 先把本地错误分类为：
   - 请求本身错误：不 fallback，例如格式、上下文、明确的模型/协议请求错误。
   - 本地容量或排队耗尽。
   - 本地 Redis 调度降级。
   - 无可用本地账号/全部禁用/代理不可用。
   - 本地瞬态上游失败、429、网络/流错误。
   - 模型不支持。
   - 辅助 WebSearch/Token refresh 预算或并发耗尽。
2. 重新读取本地池最新状态，防止预检快照过期。
3. 如果本地状态仍为 `Ready` 且有可调度容量，则默认抑制 external fallback，避免一个偶发账号错误就把整个请求改走外部；但明确分类为“本地调度协调降级”“本次请求为外部 fallback 保留本地尝试”等类型时，允许按对应策略继续。
4. 检查外部池是否有与模型兼容的候选；容量类/冷却类/协调类原因要求“立即可用容量”，无账号/模型不兼容等终态只要求存在候选。
5. 记录已尝试本地账号、分类原因和新鲜本地状态，再把请求交给外部池 failover。

这意味着“本地优先”不是“本地任何一个账号报错就立即外部”，而是“本地尝试失败且该失败能由外部缓解，且当前本地状态不再适合继续承担请求”才切换。用户要求的“部分账号错误仍丝滑换其他账号”应优先由本地同请求换号和冷却完成，而不是过早把流量整体切外部。

### 8.9 外部池候选、容量与优先级

外部池每轮从权威 PostgreSQL 快照中筛选：

1. 入口允许进入外部池。
2. 池“启用”。
3. 未被“外部池自动禁用”。
4. 支持请求模型（支持模型匹配在候选选择阶段完成）。
5. Redis 运行态读取池级并发、全局并发、池级冷却和模型级冷却。
6. 冷却或容量满的池被暂时排除；Redis 运行态读取失败会把当前候选视为协调不可用，而不是盲目发送。
7. 在剩余可用池中按以下顺序选择：

```text
优先级数字更小
  -> 池负载比例更低
  -> 池 ID 更小
  -> 同优先级且同负载的池随机打散
```

因此当前“优先级”是强排序门槛：只要最低优先级池有可用槽位，较高数字优先级池不会分流；只有最低优先级池被冷却、容量满、自动禁用、模型不支持或本轮被排除后才会到下一层。它不是按权重持续分流。

`请求正文模式` 在当前修复后不参与上述候选筛选。选中池以后，`外部池正文模式`、`模型映射模式`、`是否要求映射命中`、`是否保留路径`、`是否补请求头`才决定实际发送正文和模型。

### 8.10 外部池容量等待

当没有可立即获取的外部池时：

- “外部池容量模式”为 `fail_fast`：直接返回外部调度不可用。
- “外部池容量模式”为 `wait`：进入外部池独立等待队列，受“外部池最大排队数量”和“外部池调度最大等待时间”限制；期间每次最多等待短时间或等待冷却/租约变化通知，然后重新扫描。
- Redis 协调不可用、模型级不可用冷却等状态不会进入普通容量等待，而是直接返回明确错误。
- 选中池后申请租约发生竞态时，当前池可被加入本请求排除集合，然后重选其他池；若没有其他池，再按容量模式决定等待或报错。

当前请求可能依次经历“请求 API Key 准入队列→本地账号等待队列→外部池等待队列→同池重试间隔→本地 rescue 等待”，这正是尾延迟和内部 RPM 放大的主要结构性风险之一。

### 8.11 外部池发送、同池重试、换池与冷却

`forward_with_failover_result` 对同一请求维护：

- `excluded`：当前请求已经失败或租约不可用的外部池 ID。
- “外部池最多尝试”：跨池/整体池级尝试预算。
- “同池重试次数”：同一个池的局部重试预算。
- 统一“推理上游最大尝试次数”：所有本地、外部、rescue 发送共享。
- payload guard retry：仅在标准处理池收到输入过长类 400 时，按配置裁剪后重试一次；Raw 透传不强行裁剪。

错误处理顺序：

1. 发送前准备/模型映射/协议构造失败：不占用发送次数，按准备错误直接结束或转为可换池错误。
2. 网络错误：默认可跨池重试；按“外部池网络错误冷却”冷却当前池。
3. 协议错误：若“协议错误跨池重试”开启则换池；按“外部池协议错误冷却”冷却当前池。
4. 配置的“同池重试状态码”命中且不是认证、配额、渠道禁用、模型不可用等终态：先在同一池按“同池重试次数”和“同池重试间隔”重试。
5. 配置的“跨池重试状态码”或分类后的认证/配额/渠道禁用/网络/协议错误命中：当前池进入 `excluded`，冷却/自动禁用后换池。
6. `Retry-After` 会成为池冷却时长输入；连续瞬态失败会按退避倍率和抖动上浮，受到最大冷却时间限制。
7. “外部池自动禁用”只对配置允许的认证、安全锁、配额、端点配置或渠道禁用类别生效；普通 5xx 目前不会因为一次错误永久自动禁用。
8. 流式响应开始后若已向下游提交语义字节，不再重放；流式首输出前的安全窗口才允许换池。

当前实现已经避免“同一个明显坏池无限同池重试”，但仍需要在目标设计中把“请求内排除”“池级冷却”“自动禁用”“跨实例候选快照”统一成一个可解释状态。

### 8.12 外部错误后的本地 rescue

`local_rescue_reason_after_external_error` 当前有三道门：

1. 开启“外部池本地 rescue”。
2. 未开启“外部池直连策略”。
3. 原始请求必须是本地优先 fallback 到外部的路线，且当前新鲜本地状态允许恢复：
   - 外部直连、无本地凭证、全部禁用、模型不兼容、Redis 降级、风险熔断等状态禁止回本地。
   - 本地容量类路线只有在新鲜状态重新出现 `Ready` 且有 dispatchable 容量时才允许。
4. 外部错误类型还要命中：
   - “外部池限流冷却”对应的救援开关。
   - 超时对应的救援开关。
   - 容量错误对应的救援开关。
   - 400/公共无效请求和其他外部错误的固定分类。
5. 统一“推理上游最大尝试次数”仍有余额；rescue 最多使用有界的本地等待和一次本地发送预算。

这已经实现了用户要求的关键边界：**外部池直连失败不回本地；只有本地优先切到外部且本地在外部失败后重新恢复可调度时才允许一次本地救援。**

### 8.13 WebSearch/MCP 辅助调用

原生 WebSearch 命中后会先经过本地池预检；若本地不适合且外部可用，整个请求可以直接走外部，不再执行本地 MCP。若已进入本地 MCP：

- Token refresh、enterprise profile discovery、MCP/WebSearch completion 受“辅助上游最大尝试次数”和“辅助上游最大并发请求数”约束。
- 辅助错误会生成选择失败归因；只有可分类为本地容量/无账号/Redis 降级/辅助预算耗尽等类型时，才可能复用本地→外部 fallback。
- 普通 MCP 上游错误不应直接修改主模型账号健康或触发永久禁用。

这条链路仍是“推理请求”和“辅助请求”两套预算，目标设计需要明确二者的总 deadline 和是否允许跨域 fallback。

## 9. 当前设计问题清单

以下问题分为“源码已确认的结构问题”和“需要后续测试才能定性的风险”，不把风险直接当成已证实 bug。

### 9.1 已确认的结构问题

1. **没有单一不可变路由计划。** Handler、TokenManager、ExternalPoolManager、错误分类器和 WebSearch/MCP 各自可以改变路线，usage 只能事后拼接轨迹。
2. **本地池与外部池的等待模型不统一。** 本地有请求 API Key 准入、本地账号等待、Redis selection admission；外部有外部池等待队列、Redis lease、池内 retry delay；rescue 又有本地最大等待。
3. **优先级在外部池是硬门槛。** 健康但优先级数字较大的池无法在低优先级坏池“仍可选但频繁报错”时自动分流，除非坏池被冷却、排除或自动禁用。
4. **候选排除集合只在单个请求内生效。** 账号/池的冷却和自动禁用是跨请求状态，但“刚失败一次的账号/池”是否立即被所有实例可靠排除，依赖 Redis 状态更新和快照刷新。
5. **预检不是容量承诺。** 本地预检和外部可用性检查与真正租约申请之间存在竞态；这是正常分布式系统现象，但目前用户可见解释不足。
6. **错误、重试、冷却和总 deadline 没有一个统一可视化账本。** 虽然已增加统一“推理上游最大尝试次数”，但等待时长、同池重试、跨池重试和 rescue 的预算仍分布在多个配置字段。
7. **usage 明细不是完整配置快照。** `Fallback 原因`、`路由`、`模型（本地解析）`只能说明本次请求轨迹，不能单独证明请求开始时全部运行配置。
8. **路径策略容易被误读。** 入口本身和策略来源没有在每个决策节点统一记录；如果未来新增路径或把内置入口绑定到别的策略，单看路径仍可能误判。

### 9.2 需要通过矩阵验证的风险

1. 高优先级坏池在冷却结束瞬间是否重新抢占全部流量。
2. 多实例中一个实例已经写入冷却，其他实例的权威快照是否在下一次选择前生效。
3. 同一请求在外部池 lease 竞态、同池重试、跨池重试和 rescue 叠加时是否仍严格受“推理上游最大尝试次数”限制。
4. 本地 Redis degraded 与外部 Redis coordinator degraded 同时发生时，是否会进入过长等待或错误地使用 emergency lease。
5. sticky 会话绑定在账号冷却/失败后是否能及时逃逸到健康账号；当前本地实现有一次请求内临时重选，但需要长会话和跨实例验证。
6. WebSearch/MCP 辅助失败是否会把正常主模型请求错误地归为本地凭证不可用。
7. usage/统计旁路写入变慢时，是否会延长租约释放或影响主请求完成。

## 10. 目标调度语义

### 10.1 顶层原则

目标不是“尽量多重试”，而是：

1. 可用账号优先，坏账号不反复打。
2. 失败尽可能在内部消化，但请求必须有总尝试、总等待和总 deadline。
3. 直连就是直连；直连失败不隐式回本地。
4. 本地优先只有在本地确实不可用或当前请求已安全失败时才切外部。
5. 本地恢复后只有原始 local-first 请求才允许一次有界 rescue。
6. 优先级是倾向，不是对坏池的绝对保护；健康、容量和冷却必须先于静态优先级。
7. 所有路径按配置解释；内置路径只代表入口注册。

### 10.2 统一 RoutePlan

每个请求在真正发送前生成不可变 `RoutePlan`（名称为设计概念，不要求本轮直接改代码），至少包含：

| 项目 | 目标含义 |
| --- | --- |
| 首选调度域 | 本地账号 / 外部池 |
| 入口策略来源 | 运行配置的路径规则、模型规则、直连规则 |
| 是否允许本地→外部 fallback | 明确列出允许原因 |
| 是否允许外部→本地 rescue | 仅 local-first fallback 链可为真 |
| 请求正文处理 | Raw 透传 / 标准处理；只影响选中域的发送处理 |
| 模型字段 | 请求模型、模型解析结果、外部发送模型分别记录 |
| 总推理尝试上限 | “推理上游最大尝试次数” |
| 辅助尝试上限 | “辅助上游最大尝试次数” |
| 总等待 deadline | 准入、本地、外部、rescue 共享的剩余时间 |
| 已失败账号/池集合 | 请求内排除，避免回打 |
| 允许的错误转换 | 原地重试 / 换账号 / 换池 / fallback / rescue |

`RoutePlan` 不应在 body pipeline、provider error mapper 或 usage writer 中被隐式改写；需要改路线时只能产生带原因的新状态转移事件。

### 10.3 目标有限状态机

```text
Admitted
  -> LocalSelect
      -> LocalWait
      -> LocalSend
          -> LocalSuccess
          -> LocalRetrySameAccount
          -> LocalRetryOtherAccount
          -> LocalFallbackExternal
      -> ExternalSelect
  -> ExternalDirectSelect
      -> ExternalSend
          -> ExternalRetrySamePool
          -> ExternalRetryOtherPool
          -> ExternalFinalError
  -> ExternalFallbackSelect
      -> ExternalSend
          -> ExternalFinalError
              -> LocalRescueSelect (仅 local-first fallback 且本地 fresh Ready)
                  -> LocalRescueSend
                  -> FinalError
```

明确禁止：

```text
ExternalDirect -> LocalRescue
ExternalDirect -> LocalFallbackLocal
LocalRescue -> External
External -> Local -> External 无限回环
已提交语义流字节后重放同一请求
冷却中的账号/池无新鲜状态依据反复发送
多个等待队列在没有总 deadline 的情况下串联
```

### 10.4 错误分类与动作

| 错误类别 | 原地重试 | 换账号/池 | fallback/rescue | 直接报错 |
| --- | --- | --- | --- | --- |
| 请求格式、工具 schema、图片格式、上下文明确超限 | 否；仅按 payload/上下文专门策略处理 | 否 | 否 | 是 |
| 外部标准池输入过长 | 仅按“失败后再处理并重试”配置裁剪一次 | 可在裁剪失败后换池 | 不因该错误回本地 | 最终 400 |
| 认证、配额、渠道禁用、端点配置错误 | 同池不重试 | 立即排除当前账号/池并换候选 | 仅按原始路线规则 | 无候选时错误 |
| 429/Retry-After | 默认不在同池盲打；读取冷却 | 换健康账号/池 | 本地优先可配置 rescue | 全部不可用时 429/503 |
| 408/5xx/网络/协议瞬态错误 | 配置允许时有限同池重试 | 预算内换账号/池 | local-first 可按错误类型 rescue | 预算耗尽时 502/503 |
| 模型不支持 | 不重试同一池 | 只换支持该模型的候选 | 不把模型缺失误救到本地 | 无支持候选时 400/503 |
| Redis/调度协调异常 | 不重复打同一坏协调路径 | 仅在有独立可用域且策略允许时切换 | 不因外部协调错误回本地直连 | 有界等待后 503 |
| 流式首语义字节前错误 | 按流式重试开关和预算 | 可换账号/池 | 仍需未提交和总预算 | 预算耗尽 |
| 流式已提交语义字节后错误 | 不重放 | 不换号重放 | 不 rescue 重放 | 结束流或错误事件 |

## 11. 配置重新分组与建议

以下名称必须使用页面中文字段；内部字段名只作为源码定位，不作为用户配置说明。

### 11.1 请求准入

- “全局最大并发”
- “最大排队数量”
- “请求 API Key RPM”
- “请求 API Key 最大并发”
- “请求 API Key 排队超时”

建议把“请求 API Key admission”和“本地账号等待”“外部池等待”在 UI 上分成三个独立卡片，并显示每层剩余 deadline，避免用户误以为一个“最大并发”控制了全部资源。

### 11.2 本地账号

- “账号最大并发”
- “账号 RPM”
- “账号错误冷却”
- “账号冷却退避倍数”
- “账号冷却抖动”
- “账号最大冷却时间”
- “账号调度最大等待时间”
- “本地账号负载均衡模式”
- “本地账号优先级”
- “本地池风险熔断”

建议：

1. “优先级”只在健康、容量和冷却过滤后作为倾向；新增“优先级是否允许健康溢出”或采用统一评分，避免坏账号因静态优先级长期抢占。
2. 将“临时不可调度”作为明确运行态，支持查看剩余时间、原因、失败次数和手动清除；不要把“禁用”和“冷却”混成一个状态。
3. 对 sticky 绑定增加“失败逃逸阈值”和“冷却期间强制逃逸”可见配置。

### 11.3 外部池全局

- “外部池是否启用”
- “外部池直连策略”
- “外部池入口路由模式”
- “外部池容量模式”
- “外部池全局最大并发”
- “外部池最大排队数量”
- “外部池调度最大等待时间”
- “外部池最多尝试”
- “同池重试次数”
- “同池重试状态码”
- “同池重试间隔”
- “跨池重试状态码”
- “网络错误跨池重试”
- “协议错误跨池重试”
- “外部池自动禁用”
- “外部池连续失败阈值”
- “外部池连续失败窗口”
- “外部池自动禁用时长”

建议默认值的产品语义：

- 普通瞬态 5xx/网络错误：有限同池重试 + 立即把失败池降权/短冷却，再换池。
- 认证、配额、渠道禁用、端点错误：不做同池重试，立即进入“临时不可调度”或自动禁用。
- 自动禁用必须有阈值、窗口、时长和手动恢复，不建议仅凭一次 5xx 永久禁用。
- “外部池最多尝试”必须是整个请求的池级上限，不与“同池重试次数”相乘成无界发送。

### 11.4 本地→外部与外部→本地

- “本地池预检”
- “本地容量耗尽时 fallback”
- “本地临时耗尽时 fallback”
- “Redis 调度降级时 fallback”
- “无可用本地凭证时 fallback”
- “不支持模型时 fallback”
- “本地瞬态错误时 fallback”
- “外部池本地 rescue”
- “外部池本地 rescue 最大等待时间”
- “外部池限流时本地 rescue”
- “外部池超时时本地 rescue”
- “外部池容量不足时本地 rescue”

必须额外显示一条只读解释：

> 开启“外部池直连策略”后，外部池失败不会隐式回到本地账号；只有原始请求是本地优先并且外部失败后本地重新出现可调度容量，才可能执行一次有界本地 rescue。

### 11.5 外部池账号级配置

每个外部池卡片应明确分开：

- “启用”
- “优先级”
- “账号最大并发”
- “外部池正文模式”
- “模型映射模式”
- “模型映射规则”
- “模型映射必须命中”
- “支持模型”
- “外部池自动禁用策略”
- “保留请求路径”
- “补齐 Anthropic 请求头”
- “当前临时不可调度原因/剩余时间”
- “清除冷却”

“外部池正文模式”只决定正文处理，不决定该池是否有资格成为候选；“模型映射”先参与模型支持候选判断，选中池后再执行一次发送模型转换，不能把“模型参与筛选”和“正文是否透传”混为一谈。

## 12. 与 `sub2api` 的可借鉴点

可借鉴：

1. `Select` 返回“选择结果 + 等待计划 + 调度决策”，而不是只返回一个账号。
2. 请求携带 `ExcludedIDs`，failover 后把失败账号从当前请求候选中排除。
3. sticky 不是绝对绑定：账号不可调度、错误率/首字延迟异常、并发满时允许 sticky escape。
4. scheduler 使用 Top-K、负载、错误率、首字延迟和等待数形成候选顺序，避免单一优先级账号垄断。
5. handler 的 failover loop 明确区分：
   - 同账号重试；
   - 换账号；
   - 已提交流后是否安全切换；
   - 最大切换次数；
   - 账号健康上报。
6. scheduler 对快照过期进行 fresh DB recheck，避免“已被限流/暂停账号仍被旧快照选中”。
7. 观测字段记录候选数、Top-K、sticky 命中、负载偏斜、切换次数，能够解释为什么选中某账号。

不能直接照抄：

1. `sub2api` 的账号/平台模型与 kiro.rs 的本地 Kiro 凭证、外部 Anthropic 池不是同一资源域。
2. kiro.rs 必须保留“直连外部不回本地”和“本地优先才允许 rescue”的业务边界。
3. Kiro Token refresh、WebSearch/MCP 辅助请求、缓存/usage 整形和模型 alias 解析需要独立预算和协议处理。
4. `sub2api` 的优先级/权重不能替代 kiro.rs 的外部池冷却、Redis 租约和模型级冷却。

## 13. 当前缺陷与目标的差距

### P0：统一路由状态机和总 deadline

目前已有多个局部修复，但还没有统一结构能保证所有 handler、provider、外部池和 rescue 都遵守相同转换规则。实现前必须先把 `RoutePlan`、`RouteState`、`AttemptLedger` 和 `Deadline` 的字段合同定下来。

### P0：健康优先于静态优先级

当前外部池严格优先级会在“低数字池反复失败但尚未自动禁用/冷却时间过短”时造成重复命中。目标是：

1. 先排除禁用、自动禁用、冷却、模型不支持、容量满、Redis 运行态无效的池。
2. 对近期连续失败池增加健康惩罚和短期熔断。
3. 在低优先级池连续失败达到阈值后，允许同一请求或后续请求选择健康的高数字优先级池。
4. 保留优先级作为健康候选之间的倾向，而不是绝对门槛。

### P1：统一排队

当前多个队列可串联。目标是由 `RoutePlanner` 计算“剩余等待预算”，每次只允许一个主要等待点；进入另一资源域前必须扣减剩余 deadline，不能重新获得一套完整等待时间。

### P1：候选/失败可观测

每条 usage 需要至少记录：

- “模型（请求）”
- “模型（本地解析）”
- “模型（上游）”
- “路由”
- “路由原因”
- “候选总数”
- “候选排除原因”
- “已失败本地账号”
- “已失败外部池”
- “同账号/同池重试次数”
- “跨账号/跨池切换次数”
- “是否进入 fallback/rescue”
- “最终错误来源”

当前 usage 明细中的 `Fallback 原因` 不能替代这些完整字段。

## 14. 验证矩阵（实施前必须冻结）

### 14.1 正常路径

1. 只有本地账号、容量足够：只发本地，外部池零发送。
2. 本地优先、外部启用、本地 Ready：只发本地，外部池零发送。
3. 外部直连开启：只发外部，外部失败不发本地。
4. 多个健康外部池：验证优先级、负载和健康评分的实际分布。
5. 多账号本地：验证 sticky 命中、容量满时逃逸、成功后重新绑定。
6. Raw 透传：选池后只按该池配置处理正文和模型，不改变候选域。
7. 标准处理：模型映射、Anthropic 请求头、usage 处理与路径配置一致。

### 14.2 本地故障路径

1. 单个本地账号 429：当前请求换其他本地账号，失败账号进入冷却，不切外部。
2. 单个本地账号 5xx/网络错误：首输出前按预算换号；达到本地失败阈值后切外部。
3. 本地全部账号冷却：若“本地临时耗尽时 fallback”开启且外部有容量，直接外部；否则按等待/429/503 配置。
4. 本地 Redis degraded：验证是否按配置切外部，而不是在本地队列无界等待。
5. 本地全部禁用/无账号：只在“无可用本地凭证时 fallback”开启时外部。
6. 本地不支持模型：只在“不支持模型时 fallback”开启时选择支持该模型的外部池。
7. WebSearch/MCP 辅助失败：验证不会错误污染主模型账号健康。

### 14.3 外部故障路径

1. 外部池 429 + `Retry-After`：当前池冷却并换其他池，不回打。
2. 外部池 5xx：按“同池重试次数”有限重试，再换池；连续失败上浮冷却。
3. 外部池网络错误：按“网络错误跨池重试”换池。
4. 外部池协议错误：按“协议错误跨池重试”换池。
5. 外部池认证/配额/渠道禁用：禁止同池重试，临时不可调度或自动禁用。
6. 外部池模型不可用：验证模型级冷却只影响该模型，不无故冷却整个池。
7. 外部池容量满：`fail_fast` 直接错误；`wait` 只在总 deadline 内等待。
8. 外部池 Raw 发送失败：验证是否允许重新选择标准处理池，但不把 Raw body 强行裁剪成标准正文。
9. 外部池直连失败：验证本地发送计数为 0。
10. 本地优先 fallback 到外部后外部失败：
    - 本地恢复 Ready 且有容量：最多一次 local rescue。
    - 本地仍无容量/无账号/Redis degraded：不回本地。

### 14.4 组合与混沌

1. 三个外部池优先级 `1/10/20`，池 1 偶发错误、池 2 健康、池 3 限流：验证池 2 能在池 1 冷却/健康惩罚期间承接流量。
2. 池 1 完全不可用但未手动禁用：验证自动临时不可调度和其他池承接。
3. 两个实例同时选择同一池：验证 Redis 租约不会超卖。
4. 一个实例写冷却、另一个实例立即选池：验证权威运行态生效时间。
5. 本地账号和外部池同时瞬态失败：验证总“推理上游最大尝试次数”不被乘法放大。
6. 请求 API Key 准入、本地等待、外部等待全部接近上限：验证总 deadline、队列租约和最终错误清晰。
7. 流式首字前、首字后、客户端断开三种时点：验证只有安全窗口允许重试。
8. usage/PgSQL/Redis/dashboard 延迟或失败：验证主请求仍可完成，旁路只记录延迟或丢弃。

### 14.5 配置和路径矩阵

对每个内置入口和一个自定义入口重复以下组合：

- 本地优先 + 外部备用。
- 外部直连。
- 禁止外部池。
- 外部池 allow-list/deny-list。
- Raw 透传 + 模型透传。
- Raw 透传 + 模型优先映射。
- 标准处理 + 模型优先映射。
- 修改路径策略后热加载。

验收条件是行为只随配置改变，不随入口名称改变；同一配置复制到不同入口，调度决策、缓存、usage 配置、提示词和 fallback 结果一致。调度路线可以改变诊断字段，但不能改变相同请求、相同最终上游 usage 和相同 usage 配置下的最终 usage 结果。

## 15. 实施分阶段方案

本轮不授权改运行代码。后续实施必须按以下顺序，每一步都更新问题文档、状态索引和 plan-tree：

1. **阶段 A：观测合同**
   - 固化 `RoutePlan` 摘要、候选排除原因、失败账号/池集合、尝试账本和总 deadline 字段。
   - 将这些字段作为独立调度诊断，不改变最终 usage 计算和调度行为，先让生产问题可解释。
2. **阶段 B：统一预算与状态机**
   - 把本地、外部、同池、跨池、rescue 和辅助请求接入统一 `AttemptLedger`。
   - 把多层等待改成共享 deadline。
3. **阶段 C：健康感知选择**
   - 外部池在冷却/禁用过滤后增加连续失败惩罚和临时溢出策略。
   - 本地 sticky 和优先级同样允许健康逃逸。
4. **阶段 D：故障域和队列隔离**
   - 观测写入、Redis/PgSQL 慢路径和主业务租约释放完全旁路化。
   - 验证多实例和 chaos。
5. **阶段 E：配置与 UI**
   - 按本节配置分组重命名/补充解释。
   - 明确“直连不回本地”“local-first 才可 rescue”的产品合同。
6. **阶段 F：发布门禁**
   - Rust/UI/admin-ui、fake upstream、真实 Claude Code CLI、真实本地 mock、两实例、负载/混沌、生产观察全部通过后才能发版。

## 16. 未决问题

以下问题必须在实施前由产品/运维确认，当前文档不代替决策：

1. “优先级不是绝对指标”是否允许健康的高数字优先级池在低数字池仍可用但健康惩罚很高时承接流量？建议允许，并设置最小健康阈值和最大溢出比例。
2. 外部池连续瞬态失败达到何阈值后进入临时不可调度？建议按池级滑动窗口 + 指数冷却，而不是一次 5xx 自动禁用。
3. `external_pool_retry_max_attempts=0` 的产品语义是否继续表示按候选数动态尝试，还是改成明确的“只尝试一次”？当前源码把 0 解释为根据候选可用数计算默认尝试上限，需要在 UI 中说明。
4. 同一个请求的本地→外部→local rescue 是否允许总共两次以上本地发送？建议只允许一次 bounded rescue，并由统一预算硬限制。
5. WebSearch/MCP 辅助失败是否允许跨到外部池执行“整个请求”，还是只允许重试辅助调用？建议默认只在明确配置时切换整个请求。
6. 自动禁用后的手动恢复、冷却清除、运行态审计是否需要统一成一个“临时不可调度”操作，而不是分别操作账号和池。
7. 是否要求 usage 记录完整运行时配置版本/快照 ID，以便解释“当时为何直连/fallback”；当前只有请求轨迹，建议增加配置版本和规则命中摘要。

## 17. 本轮结论

用户提出的目标是合理且可实现的，但不能靠继续增加 handler 局部条件完成。当前实现已经具备不少必要零件：本地候选过滤、请求内临时排除、外部池冷却、同池/跨池重试、直连不回本地、容量感知 local rescue、统一推理尝试预算。真正缺失的是统一的路由计划、健康感知的优先级溢出、共享 deadline/队列语义和完整决策可观测性。

本轮只完成源码级分析和文档记录：

- 未修改运行代码。
- 未改变任何配置默认值。
- 未执行生产请求。
- 未发版。

后续任何实现必须以本文件的状态机、配置边界和验证矩阵为准；若实现改变了其中任一语义，必须先更新本文件、`feature/issues/current-issue-status-index-20260731.md` 和对应 plan-tree 状态，再进入代码修改。

## 18. 风险与回滚边界

1. 调度改造同时触及请求入口、TokenManager、外部池租约、流式重试、usage 和运行时配置，不能一次性替换；必须按“观测字段→预算/状态机→选择策略→故障域→默认开关”分阶段上线。
2. 任何新的健康评分、优先级溢出或自动临时不可调度策略都必须可由运行配置关闭，并保留现有“本地优先 / 外部直连 / 本地 rescue”边界作为回退路径。
3. 出现上游发送次数增加、尾延迟明显上升、外部池直连误回本地、流式重复提交、账号/池冷却状态不收敛时，立即关闭新增策略开关，回退到当前已验证的有限重试和容量感知 rescue。
4. 新增 `RoutePlan`、尝试账本和候选排除字段只允许向后兼容追加；不得删除现有 usage 字段或改变下游错误状态码，避免 dashboard/usage 解析回归。
5. 发布前必须保留可复现的 fake upstream、本地 PostgreSQL/Redis、双实例和真实 Claude Code CLI 验证报告；没有这些证据不能把“focused pass”标成“生产已修复”。

## 19. 2026-08-04 目标确认与 focused 复核结果

本轮按照用户要求先记录目标，再确认当前实现是否符合目标；没有修改运行时代码。

目标契约已记录为提议决策：

- [Decision 001：本地账号与外部池统一调度目标契约](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/decisions/001-local-external-scheduler-target-contract.md)
- [当前目标符合度矩阵](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/scheduler-target-compliance-matrix.md)
- [持续调度验证方案](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/sustained-scheduling-validation.md)
- [统一调度目标契约、状态机与验证方案](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/scheduler-target-state-machine-and-test-contract.md)

当前确认结论：

1. 已有 focused 证据支持：外部直连不调度本地、不隐式 local rescue；本地 fallback 和一次 bounded local rescue 有条件生效；外部同池/跨池重试、Retry-After 冷却、Raw/标准正文边界和模型解析边界存在。
2. 已确认与目标不一致：外部池选择仍按“优先级数字”硬排序；健康高数字池不能在低数字池持续失败但尚未被排除时稳定溢出承接。
3. 已确认结构缺口：本地/外部等待虽然各自有上限，但还没有贯穿请求准入、资源等待、重试间隔和 rescue 的统一剩余 deadline；调度诊断也还不是完整配置快照、候选拒绝和统一尝试账本。最终 usage 计算不属于调度域，必须继续按 usage 配置独立执行。
4. 尚未定性的部分：三池故障波、双实例冷却传播与租约竞态、sticky 长会话逃逸、旁路 usage/dashboard 延迟和 15–30 分钟资源回落。

验证证据：[调度目标契约 focused 验证（2026-08-04）](../evidence/scheduler-target-contract-focused-validation-20260804.md)：

- 外部池 focused：`10 passed / 0 failed`；
- handler/fallback/rescue focused：`9 passed / 0 failed`；
- Node 隔离与 runner 合同：`104 total / 92 passed / 12 skipped / 0 failed`；
- 文档合同：`74 issue documents / 330 links / 0 failure`；
- 没有设置真实 PostgreSQL 外部池集成环境，因此相关集成测试按设计跳过。

因此当前状态仍为：

`analysis-complete / target-design-proposed / decision-proposed / compliance-matrix-recorded / implementation-not-authorized`

在用户确认 Decision 001 的开放参数、并完成持续调度验证前，不进入统一调度器代码重构。
