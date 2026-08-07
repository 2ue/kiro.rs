# Roadmap

Last reviewed: 2026-08-07 Asia/Shanghai

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

## Next

1. Perform read-only `v0.0.134` production observation and update the issue/evidence
   indexes without changing usage semantics.
2. Continue the independent documentation archive and scheduler observability follow-ups.

## Deferred

- Greenfield AI Gateway implementation.
- Full project-wide Markdown migration.
- Deletion of any legacy document.
- Real upstream high-concurrency pressure.
