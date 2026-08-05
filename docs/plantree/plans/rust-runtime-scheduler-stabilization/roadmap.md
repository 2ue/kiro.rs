# Roadmap

Last reviewed: 2026-08-05 Asia/Shanghai

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
  regression and full Rust gates passed. See [专项证据](../../../../feature/evidence/external-pool-ha-scheduler-validation-20260805.md).
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
  - owning issue: [外部池高可用调度与冷却回归](../../../feature/issues/external-pool-ha-scheduler-cooldown-regression-20260805.md);
  - local root-cause fix and release-candidate validation are complete;
  - remaining work is production rollout/observation plus the larger RoutePlan, candidate
    rejection observability and long-window policy follow-ups. These are not blockers for the
    verified local P0 candidate.

## Next

1. Finish the final release diff/version/tag/push workflow for the verified candidate.
2. Monitor the release workflow and record the published artifact result.
3. After publication, perform read-only production observation and update the issue/evidence
   indexes without changing usage semantics.
4. Continue the independent documentation archive and scheduler observability follow-ups.

## Deferred

- Greenfield AI Gateway implementation.
- Full project-wide Markdown migration.
- Deletion of any legacy document.
- Real upstream high-concurrency pressure.
