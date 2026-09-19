# Roadmap

Last reviewed: 2026-09-18 Asia/Shanghai

## Done

- External pool local-route hot path moved to cached/no-wait gates.
- Raw preflight route gate moved to cached/no-wait.
- Raw direct external no longer filters candidates by “请求正文模式”; it still performs a
  lightweight model-compatible external-pool availability check before entering the direct
  path, and the authoritative pool/lease check remains in the send loop.
- New real local PgSQL/Redis integration tests added for cached external availability:
  - cold cache + locked PgSQL + 128 concurrent local gates return fast;
  - warmed cache respects available/full/released external runtime capacity.
- Current active TODOs migrated from `feature/todo` into this plan's topics.
- Route-policy config authority focused pass: built-in `/v1`、`/cc`、`/ha`、`/na` routes remain fixed entrypoints, but cache, usage, prompt steering, external-pool route rules, and cache namespace now resolve from runtime configuration. Full Rust all-targets, UI/admin-ui build, docs contract, prompt parity/independence, and diff checks passed; live reload/browser/production gates remain post-focused follow-up.
- External-pool body-mode/model routing P0 and protocol compatibility fix: candidate selection no longer filters by “请求正文模式”; Raw routes can reselect a standard-processing pool when the body is parseable, and missing `anthropic-version` defaults to `2023-06-01`.
- External-pool retry mechanics phase: “外部池最多尝试” is independent from “同池重试次数”; “跨池重试状态码”, “网络错误跨池重试”, “协议错误跨池重试”, “同池重试状态码” and “同池重试间隔” are configurable; Admin and both UIs expose “清除冷却”. The old conclusion that ordinary consecutive failures should escalate into pool-level long cooldown is superseded by the 2026-08-05 HA target.
- External-pool local-rescue boundary refinement: only a local-first request that entered the
  external pool because of a local capacity/attempt-preservation condition may perform one
  bounded local rescue, and only after a fresh local `Ready` state with remaining dispatchable
  capacity is observed. Direct external and terminal local-unavailable states never silently
  return to local credentials. Capacity recovery, zero-capacity and attempt-budget matrices
  passed through scoped Cargo.
- Redis scheduler/usage joint-fault and external-pool priority recovery validation: the initial
  75ms boundary failure was diagnosed as non-deterministic local test scheduling pressure, not a
  reproducible production hot-path amplification. After test diagnostics were strengthened,
  three complete outer rounds passed (`24/24` exact); no deadline relaxation was made.
- External-pool HA scheduler P0 root cause fix: self-originated Redis mutation events no longer
  clear the current process's freshly merged authoritative snapshot. Three real HTTP baseline
  rounds, 256-concurrency/1800-RPM sustained traffic, external-direct boundary, isolated storage
  regression and full Rust gates passed; released as `v0.0.133` through GitHub Actions
  `Publish Docker Images #164`. See [专项证据](../../../../feature/evidence/external-pool-ha-scheduler-validation-20260805.md).
- External-pool stream pre-output retry focused implementation: external stream 2xx now buffers
  protocol-only SSE before downstream commit and can retry another external pool on pre-output
  error event, read error, idle timeout or EOF. Global/per-pool config, PostgreSQL/admin/UI wiring,
  fake-upstream HTTP recovery, normal stream/non-stream output, external direct stream/non-stream,
  local-first fallback/rescue classifier and route config authority regressions passed; the
  user-requested 2026-08-07 rerun repeated the core scheduler/output matrix with
  `cargo +1.92.0` and reran Rust/UI/docs/artifact gates. Final frozen candidate gates also passed:
  Claude CLI `2.1.221` bare `20/20`, long-session `110 turns`, thinking-wire rerun `60/60`, L3
  `9/9`, L4 `12/12`, and L5 `900s` soak `6820/6820` with `300s` idle RSS/FD recovery. Production
  observation remains open. See
  [focused validation](../../../../feature/evidence/external-pool-stream-pre-output-retry-validation-20260806.md).
- External-pool passive quality-aware scheduling: real request success/failure samples are
  recorded in Redis with atomic EWMA updates, same-priority quality scoring uses Top-K weighted
  selection, probation/probe/recovery state is bounded and observable, and the master switch
  defaults **off** with an exact legacy-selection fallback. Ordinary transient failures do not
  enter pool hard cooldown; quality writes use an isolated Redis manager with a bounded task cap;
  probation uses the current request's full path/model-eligible cohort; and no-sample cold starts
  preserve legacy load selection. Selector invariant failure now returns bounded 503 instead of
  spinning. Focused quality tests pass; current production coordinator/Redis contention remains
  an evidence/observation follow-up.
- Source-verified scheduler architecture analysis: the current local-account/external-pool
  request chain, normal and exceptional transitions, queue/capacity/cooldown/retry semantics,
  fallback/rescue boundaries, `sub2api` comparison, configuration regrouping and target
  validation matrix are recorded in the owning issue document. This is a planning artifact;
  no runtime implementation is implied.
- Target scheduler contract and compliance records are now durable planning artifacts:
  - [Decision 001](decisions/001-local-external-scheduler-target-contract.md) separates
    user-confirmed hard boundaries from implementation parameters that still require
    confirmation.
  - [Unified target state machine and test contract](topics/scheduler-target-state-machine-and-test-contract.md)
    records the source-verified route modes, allowed/forbidden transitions, error actions,
    shared deadline/attempt requirements, health-aware priority behavior, page-field semantics
    and real sustained fake-upstream validation matrix.
  - [Current compliance matrix](topics/scheduler-target-compliance-matrix.md) explicitly
    marks focused evidence, structural non-conformance and sustained-validation gaps.
  - [Sustained scheduling validation](topics/sustained-scheduling-validation.md) defines
    isolated L0-L5 testing, fake upstream behavior, multi-instance races, soak metrics and
    no-go conditions.

## In Progress

- Document disposition cleanup:
  - current valid issues migrated into this plan;
  - first historical archive batch for old slow-first-token/stream-fluidity analysis.
- External pool strategy productization:
  - local capacity queue-first vs external takeover policy;
  - keep local rescue capacity-aware and prevent direct/external-only requests from returning
  to local credentials;
  - candidate rejection observability and clearer model-stage display;
  - no-local-credential temporary external-direct and quick return to local-first.
  - optional long-window hard-disable policy for fully unavailable external pools, after production recurrence evidence proves it will not disable merely overloaded providers.
- Claude Code/Kiro 协议互转优化计划：
  - 已完成 2026-09-15 `sub2api-kiro` 协议互操作对比和 2026-09-16 优化路线；
  - P0 fixture/contract test 与已知 event shape/tool input/tool-name/SSE lifecycle 的窄范围实现已 `focused-validated`；
  - 2026-09-16/17 已完成隔离真实本地凭据的 normal/alias/usage 闭环和真实 Claude Code CLI
    `2.1.273` 验证；2026-09-18 又在批量刷新全部账号余额后完成当前 HEAD 的
    Sonnet 4.5/Haiku 4.5 成功、thinking 和 tool-use 验证；
  - fake 外部池 non-stream/stream 协议链路已通过；`supportedModels` 的 Claude Code 模型命令
    canonical normalization 已由当前代码和 focused contract 覆盖，但真实第三方池
    discovery/runtime 组合尚未重跑；
  - semantic fallback 仍仅限已知 event key；trim 后 tool pair 的安全配对 invariant 已由
    metadata fixture 和当前 HEAD Rust contract 覆盖，minimal tool-use reconstruction 不实现；
    schema profile、thinking 多块、WebSearch index 和错误分类消费仍不得在真实证据前扩大默认行为；
  - 实现、进度、测试结果和复现记录见 [sub2api-kiro 协议互转优化执行计划](topics/sub2api-kiro-protocol-interop-optimization.md)，
    本批次证据见 [P0 evidence](../../../../feature/evidence/sub2api-kiro-protocol-interop-p0-20260916.md)。
- Scheduler target decision and implementation readiness:
  - core target semantics are accepted in [Decision 001](decisions/001-local-external-scheduler-target-contract.md):
    all upstream errors default to temporary turbulence, priority cannot block healthy-pool
    takeover, cooldown must be strict and auto-recovering, and external direct never falls
    back to local credentials.
  - execute the [current compliance matrix](topics/scheduler-target-compliance-matrix.md)
    through the [sustained scheduling validation](topics/sustained-scheduling-validation.md)
    before changing runtime behavior.
- External-pool HA follow-up after the verified P0:
  - owning issue: [外部池高可用调度与冷却回归](../../../../feature/issues/external-pool-ha-scheduler-cooldown-regression-20260805.md);
  - local root-cause fix and release-candidate validation are complete;
  - remaining work is production rollout/observation plus the larger RoutePlan, candidate
    rejection observability and long-window policy follow-ups. These are not blockers for the
    verified local P0 candidate.
- External-pool stream pre-output retry follow-up:
  - owning issue: [Stream terminal errors and precommit retry](../../../../feature/issues/stream-terminal-errors-and-precommit-retry.md);
  - handoff: [外部池流式首语义输出前错误恢复](topics/external-pool-stream-pre-output-retry-20260806.md);
  - current evidence: `yuenan` / `yuenan-1` stream sampling shows `message_start -> error`
    before content/thinking/tool output, while non-stream succeeds;
  - current code state: focused implementation, 2026-08-07 normal-routing/output rerun, frozen
    Claude CLI and L3-L5 load/chaos gates have passed; final pre-release static/UI/docs/artifact
    gates also passed; `v0.0.134` was published by GitHub Actions `Publish Docker Images #166`;
    remaining work is production rollout observation and renewed `yuenan` / `yuenan-1`
    recurrence checks.
- Claude Code/Kiro 互转运行时验证 follow-up：
  - CLI runner 已补最小脱敏 stdout hash/行数/字节数/可疑行摘要合同；完整长会话动态门
    尚未用当前工作树重跑；
  - external pool `supportedModels` 已增加 Claude Code 模型命令 canonical normalization
    contract，并保留 exact allowlist/unknown-model 本地拒绝作为独立决策；
  - 2026-09-17 已补外部池错误消费边界合同：`model_mapping_miss` 和 `model_unavailable`
    跳过当前池并允许其它合格池，不在同池重放；普通请求 400 仍 fail-closed，协议错误仍
    遵循显式协议重试开关。scoped Cargo 纯策略合同为 `2/2` 通过，HTTP 400 模型不可用
    failover loop 合同为 `1/1` 通过（本地未配置 PG/Redis 时按 helper 安全返回），第三方
    池和本地 provider 完整错误矩阵仍未重跑；
  - 2026-09-17 已补本地 provider 400/404 消费合同：`MODEL_UNAVAILABLE` 的 400/404
    只在有其它可用凭据时跨账号切换，普通 malformed/schema/tool/image/body-invalid
    400 保持单次 fail-closed；scoped fake-upstream 矩阵 `1/1` 通过。该合同不替代真实
    Kiro 404 body 验证，且不改变 thinking signature 的同凭据受控 retry。
  - 当前 HEAD 已补 payload trim/pairing metadata fixture、最终 body 无孤儿断言，以及
    Kiro dispatch 前的 fail-closed tool pairing invariant；local/external public error
    不暴露内部 pool/scheduler 术语。当前树 C0 已通过（主 Rust `2129/0/6 ignored`、
    `kiro_loadtest 31/31`、release build、diff 和 artifact inventory）。
  - 当前 HEAD 动态验证已实际运行：先批量导入 235 条凭据（15 success、220 skipped、
    0 failed），再刷新数据库 237 条余额（233 success、4 failed），筛出 299/302/303
    三个正额度目标账号。冻结 binary SHA-256
    `3133106baa178ea343e3f5542d0b6ffacc26326970f6a19e58d95a5e19558008` 在唯一
    `19023` 实例上完成 health/models、真实 upstream discovery、direct C1 和真实
    Claude CLI `2.1.273` C2；Sonnet thinking、Haiku text、Sonnet Bash
    `tool_use/tool_result`、usage、SSE lifecycle 和无内部术语泄漏均通过，状态为
    `runtime-validated / cli-pass`。服务已停止、`19023` 无残留。

## Next

1. Perform read-only `v0.0.134` production observation and update the issue/evidence
   indexes without changing usage semantics.
2. Close the Claude Code/Kiro interop follow-ups above before widening fallback behavior.
3. Continue the independent documentation archive and scheduler observability follow-ups.

## Deferred

- Greenfield AI Gateway implementation.
- Full project-wide Markdown migration.
- Deletion of any legacy document.
- Real upstream high-concurrency pressure.
