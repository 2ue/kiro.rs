# 整体调度架构优化

Last reviewed: 2026-07-28 Asia/Shanghai

Related:

- [整体调度架构分析：本地凭证、外部池、fallback/rescue 与容量账本](../../../../../feature/issues/scheduler-architecture-analysis-purpose-and-plan.md)
- [外部池调度影响本地凭据与 fallback 矩阵缺失](../../../../../feature/issues/external-pool-scheduler-interference-and-fallback-matrix-20260727.md)
- [统一调度目标契约、状态机与验证方案](scheduler-target-state-machine-and-test-contract.md)

## 当前状态

2026-08-04 已完成源码级调度分析和目标设计记录，但尚未实现统一调度器。权威分析文档已经覆盖本地账号、外部池、请求准入、容量/排队、冷却、同池/跨池重试、fallback/rescue、WebSearch/MCP、`sub2api` 对照、配置重组和验证矩阵：

- [整体调度架构分析：本地凭证、外部池、fallback/rescue 与容量账本](../../../../../feature/issues/scheduler-architecture-analysis-purpose-and-plan.md)

当前状态：`analysis-complete / target-design-proposed / implementation-not-authorized`。

当前源码仍是多套局部机制叠加：

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

用户反馈“几种模式没啥作用”是合理风险：这些模式分别控制局部点，不是一个统一 RoutePlan。

## P0-1：完成当前调度链路图

产物：

- 从入口到本地凭证、外部池、MCP/WebSearch、usage 的完整调用链图。
- 标明每一步是否可能访问：
  - PgSQL
  - Redis
  - 上游模型
  - usage/stat/dashboard
  - MCP auxiliary call
- 标明哪些步骤在 HTTP 请求生命周期内，哪些是后台/旁路。

验收：

- 能解释生产现象：下游 RPM 不高，但内部 RPM/attempts/queue/cpu 可能放大。
- 能解释外部池开启为什么曾经影响本地凭证。
- 能解释健康检查正常但业务卡住的 runtime/storage 耦合风险。

## P0-2：设计统一 RoutePlanner / RoutePlan

目标：

- 在真正执行前生成可解释的 `RoutePlan`。
- RoutePlan 明确：
  - 首选本地还是外部池。
  - 是否允许外部池 fallback。
  - 是否允许 local rescue。
  - 每个阶段最大等待时间。
  - 每个阶段最大 sends。
  - 当前请求的 attempt budget。
  - 失败时是否可重选。

参考项目：

- `../sub2api`：AccountSelectionResult、WaitPlan、ScheduleDecision。
- `~/Desktop/project/new-api`：channel/group/model/priority/weight/retry 的用户可解释语义。
- `~/Desktop/project/CLIProxyAPI`：ModelRouter / Scheduler / Executor 分层。

验收：

- handler 不再散落大量局部 if 决策。
- usage 中能记录 RoutePlan 摘要。
- 表驱动测试可以直接验证 RoutePlan 输出。

## P0-3：设计 CapacityLedger，拆清内部 RPM 口径

需要拆分：

- downstream request RPM
- admitted request RPM
- route plan RPM
- local credential acquire attempts
- local upstream sends
- external upstream sends
- external internal failover sends
- MCP/WebSearch auxiliary sends
- usage records RPM
- dashboard aggregation read RPM

问题：

- 用户多次观察到“下游 RPM 不高，但系统内部 RPM 很高”。
- 如果不拆账本，queue full、api_rate_limit、external fallback、MCP error、client retry 会混成一个数。

验收：

- dashboard/usage 能分别展示请求数、attempts、actual upstream sends。
- usage 明细的调度诊断区域能说明这次请求产生了几个本地 send、几个外部 send、几个 MCP send；
  这些计数不参与最终 usage token、usage 整形或费用计算。
- request admission 可选择按 downstream requests 或 actual upstream sends 限制。

## P0-4：调度故障域隔离

必须保证：

- usage/dashboard/stat 失败不影响主业务。
- 外部池 snapshot 失败不影响本地 ready 请求。
- Redis/PgSQL 尾延迟不阻塞 HTTP runtime。
- 主业务 Redis 和观测 Redis/统计 Redis 的故障域继续分离。

验收：

- PgSQL 慢查询注入时，正常本地请求仍可响应。
- Redis 外部池 coordinator 慢时，本地 ready 请求不阻塞。
- usage writer 队列满只丢弃或延迟观测，不阻断请求。
- dashboard 查询超时不影响 `/v1/messages`、`/cc/v1/messages`。

## P0-5：RouteExecutor 有限状态机

需要明确允许转换：

```text
LocalOnly
ExternalDirect
LocalFirst -> ExternalFallback
LocalFirst -> ExternalFallback -> LocalRescue
TemporaryExternalDirectBecauseNoLocalCredential -> LocalFirstAfterLocalCredentialAppears
```

明确禁止：

```text
ExternalDirect -> LocalRescue
LocalRescue -> ExternalFallback
ExternalFallback -> LocalRescue -> ExternalFallback
多层队列连续等待且无总 deadline
无限 retry/failover
```

验收：

- 状态机测试覆盖所有允许/禁止转换。
- 每个转换都有 route subtype 和 reason。
- 每个请求有总 attempt budget 和总 deadline。

## P1：调度模式命名与 UI 配置整理

问题：

- 当前多种模式分散在本地账号、外部池、admission、fallback、rescue、cooldown 中。
- 用户很难判断某个开关实际控制什么。

建议：

- 用“策略卡片”解释：
  - 本地优先策略。
  - 外部池直连策略。
  - 本地容量不足策略。
  - 外部池错误策略。
  - cooldown 策略。
  - admission 策略。

验收：

- 每个开关有明确作用范围和不作用范围。
- UI 不把需求逻辑写成大段说明，只描述功能；详细策略文档链接到文档页。
