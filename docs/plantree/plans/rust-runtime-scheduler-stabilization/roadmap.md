# Roadmap

Last reviewed: 2026-08-04 Asia/Shanghai

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
- External-pool retry/cooldown stability phase: “外部池最多尝试” is independent from “同池重试次数”; “跨池重试状态码”, “网络错误跨池重试”, “协议错误跨池重试”, “同池重试状态码” and “同池重试间隔” are configurable; terminal classified errors skip same-pool retry; consecutive transient failures escalate pool cooldown with jitter; Admin and both UIs expose “清除冷却”, which clears pool/model cooldowns and invalidates the runtime snapshot immediately. Local PostgreSQL/Redis + fake upstream focused tests passed.
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
  - accept or revise the RoutePlan/finite-state-machine, shared deadline/attempt ledger,
    health-aware priority overflow and configuration regrouping recorded in the architecture
    analysis and [Decision 001](decisions/001-local-external-scheduler-target-contract.md);
  - keep implementation blocked until the open questions about priority overflow, automatic
    temporary unscheduling and auxiliary WebSearch/MCP fallback are explicitly decided.
  - execute the [current compliance matrix](topics/scheduler-target-compliance-matrix.md)
    through the [sustained scheduling validation](topics/sustained-scheduling-validation.md)
    before changing runtime behavior.

## Next

1. Finish first archive batch and update archive indexes.
2. Refresh `docs/plantree/README.md` registered plan table to include this plan.
3. Accept the RoutePlanner / CapacityLedger target semantics and decide the open questions.
4. Implement explicit local capacity overflow policy only after the target decision is accepted.
5. Run focused fake + real local PgSQL/Redis scheduler/load chaos matrix, including multi-instance cooldown clear races.

## Deferred

- Greenfield AI Gateway implementation.
- Full project-wide Markdown migration.
- Deletion of any legacy document.
- Real upstream high-concurrency pressure.
