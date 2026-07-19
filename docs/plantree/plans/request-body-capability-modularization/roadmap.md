# Roadmap

## Done

- Inventory current request body processing surfaces from code.
- Register plan root and baseline links.
- Added explicit request/body capability plan types for parsed Anthropic, local Kiro, and external body pipelines.
- Routed parsed Anthropic preprocessing, local Kiro body preparation, and external raw/normalized body preparation through those plans with compatibility defaults.
- Split `converter.rs` internals into schema, model, content, tools, tool-pairing, and history modules.
- Added `BodyConversionConfig` and wired it through runtime config, request config, local body planning, and both React admin surfaces.
- Extended fake upstream loadtest scenarios with random, dense, tiered 3/10/22 second slow first byte, and mixed chaos.
- Validated raw and normalized external pool paths against fake upstream normal, slow, long-context, high-concurrency, error, recovery, and mixed-chaos scenarios.
- Verified raw body passthrough with optional model rewrite and with explicit direct disabled.
- Verified usage projection and external-pool billing remain independent from raw/normalized body mode.
- Completed cleanup of the temporary validation proxy, database, Redis namespace, and owned ports.
- Audited top-level Anthropic reasoning controls through local Kiro conversion, credential selection, IDE/CLI endpoint transformation, retry, and response handling.
- Documented the schema-driven, per-attempt target for exact `high`/`xhigh`/`max` and future effort fidelity, non-deleting thinking forwarding, operator injection/force, Admin controls, and exact outbound capture.

## In Progress

None. The reasoning-fidelity implementation has not started; its target plan is ready for review.

## Next

- Implement immutable reasoning intent and exact per-credential/endpoint/region/model capability persistence without changing the conservative default policy.
- Move reasoning materialization into each concrete provider attempt; make IDE/CLI endpoint transforms semantic no-ops and propagate the effective decision to response decoding.
- Add `request_only`, `inject_if_missing`, and explicit `force` Admin policy, including both React UIs, persistence, peer invalidation, audit, and migration from overlapping legacy settings.
- Verify all schema-advertised efforts and thinking shapes across stream/non-stream, IDE/CLI, sticky routing, retry/failover, fake-upstream exact-body capture, and isolated real Claude Code CLI/Kiro A/B evidence.
- Preserve the profiling scenario as a behavioral oracle for the [Greenfield AI Gateway plan](../greenfield-ai-gateway/README.md); broader route-planner replacement belongs to the separate target system.
- Keep profiling the normalized long-context path if CPU/RSS pressure reappears under real upstream slow streams.

## Deferred

- Full plugin/trait system for every processing stage.
- Route planner that can select target before all parsed-body processing for more non-raw external cases.
