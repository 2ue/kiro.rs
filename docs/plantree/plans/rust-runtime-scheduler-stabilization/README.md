# Rust Runtime Scheduler Stabilization

Role: 当前 Rust 项目的运行时、调度、外部池、thinking signature 和验证门禁稳定化计划

Status: `In Progress`

Current phase: `external-pool hot-path fixed / scheduler architecture and signature blockers open`

Last reviewed: 2026-08-07 Asia/Shanghai

Authority:

- 本 plan 是当前 Rust 仓库继续修复、测试、发版的短期执行入口。
- 它不取代长期 [Greenfield AI Gateway](../greenfield-ai-gateway/README.md) 目标架构；greenfield 是长期参考，本 plan 处理现有 Rust 服务的生产稳定性。
- 具体问题证据仍保留在 `feature/issues` 和 `feature/evidence`；本 plan 只维护当前路线、未完成项、决策和验收。

## Reading path

1. [Roadmap](roadmap.md)
2. [Implementation status](implementation-status.md)
3. Topics:
   - [外部池与本地凭证调度](topics/external-pool-local-first-scheduler.md)
   - [整体调度架构优化](topics/route-planner-capacity-ledger.md)
   - [统一调度目标契约、状态机与验证方案](topics/scheduler-target-state-machine-and-test-contract.md)
   - [当前目标符合度矩阵](topics/scheduler-target-compliance-matrix.md)
   - [持续调度验证方案](topics/sustained-scheduling-validation.md)
   - [外部池流式首语义输出前错误恢复](topics/external-pool-stream-pre-output-retry-20260806.md)
   - [Thinking signature 协议安全](topics/thinking-signature-protocol-safety.md)
   - [验证与发版门禁](topics/validation-and-release-gates.md)
4. Decisions:
   - [Decision 001：本地账号与外部池统一调度目标契约](decisions/001-local-external-scheduler-target-contract.md)
5. Indexes:
   - [项目文档处置索引](indexes/document-disposition.md)
   - [Issue status governance](indexes/issue-status-governance.md)
6. History:
   - [Evidence index](history/evidence-index.md)

## Scope

In scope:

- 本地凭证调度、并发、RPM、Redis scheduler degraded、capacity semantics。
- 外部池 direct、local-first fallback、capacity wait、cooldown、internal failover、local rescue。
- RoutePlanner / RoutePlan / CapacityLedger / RouteExecutor 状态机设计。
- Thinking/reasoning signature、redacted thinking、payload guard、sanitizer、retry。
- 真实 Claude Code CLI、load/chaos、PgSQL/Redis 故障域、usage/dashboard 非阻塞验证。
- 项目文档按当前事实重新分类、迁移和归档。

Out of scope:

- 不在本 plan 中启动 greenfield 重写。
- 不删除旧文档；删除需要单独 disposition 审计。
- 不把旧 evidence 改写成当前事实。
- 不对生产做压测。

## Current active problems

| Problem | Status | Owning topic |
| --- | --- | --- |
| 外部池开启影响本地凭证热路径 | Hot-path fixed; strategy/chaos follow-up open | [External pool local-first scheduler](topics/external-pool-local-first-scheduler.md) |
| 外部池流式首语义输出前 error/空回不会换池 | Focused implementation, normal routing/output rerun, frozen Claude CLI and L3-L5 load/chaos gates passed; production rollout observation pending | [External pool stream pre-output retry](topics/external-pool-stream-pre-output-retry-20260806.md), [Stream terminal errors and precommit retry](../../../../feature/issues/stream-terminal-errors-and-precommit-retry.md) |
| 调度模式分散、无法统一解释 route/fallback/rescue | Open design | [Route planner and capacity ledger](topics/route-planner-capacity-ledger.md) |
| thinking signature / redacted thinking / payload guard 风险 | Open blocker | [Thinking signature protocol safety](topics/thinking-signature-protocol-safety.md) |
| 真实 CLI / load chaos / release gates 未全量闭环 | Open validation | [Validation and release gates](topics/validation-and-release-gates.md) |
| 项目文档状态漂移、旧分析未归档 | In progress | [Document disposition](indexes/document-disposition.md) |
| 本地账号 WebSearch/tools/image Wave 1 | WebSearch and tool parsing focused verified; image/model/multi-tool-history follow-up open | [Current issue status index](../../../../feature/issues/current-issue-status-index-20260731.md), [analysis priority queue](../../../../feature/issues/issue-analysis-priority-queue-20260731.md) |
| issue 状态、索引和 plan-tree 状态容易漂移 | In progress | [Issue status governance](indexes/issue-status-governance.md), [current issue status index](../../../../feature/issues/current-issue-status-index-20260731.md), [analysis priority queue](../../../../feature/issues/issue-analysis-priority-queue-20260731.md) |
