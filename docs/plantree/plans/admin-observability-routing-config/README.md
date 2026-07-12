# Admin Observability, Routing Model Support, and Config IA

Status: Implemented And Locally Verified

## Scope

This plan covers four related but separable surfaces:

- Usage/log query performance and correctness for request id, model, route, and general search.
- Per-local-credential and per-external-pool supported-model eligibility before dispatch.
- Configurable retry to switch to an untried credential for selected prompt/protocol logic failures after model resolution succeeds.
- Backend capability boundary audit and admin UI information architecture cleanup for body, model, usage, retry, and routing settings.

## Non-Negotiable Requirements

- Existing successful caller-visible API behavior must remain compatible.
- Empty supported-model lists must mean no model restriction.
- Model restrictions must affect dispatch selection before a credential or external pool is called.
- Raw body passthrough must not enter body processing unless an explicitly enabled stage requires it.
- Request id log lookup must be exact and fast; it must not depend on scanning JSON text.
- Model filtering must have the same meaning across memory, Redis, and PgSQL paths.
- Retry-on-prompt-logic-error must be opt-in, bounded, and must not retry an already-tried credential.
- Observability changes must not block the request hot path.
- Full regression must include normal calls, error calls, scheduler selection, external pool raw/normalized behavior, usage records, and UI builds.

## Relationship And Authority

This plan remains authoritative for the exact request-id usage query, supported-model dispatch eligibility, bounded prompt/protocol retry, configuration grouping, and local verification landed in its scope. The [system architecture modernization plan](../system-architecture-modernization/README.md) owns later control-plane boundaries, generated frontend contracts, cross-replica invalidation, storage ports, and system-wide migration sequencing.

Modernization work must preserve the compatibility and dispatch semantics above unless an accepted decision explicitly supersedes them and provides migration and regression coverage.

## Reading Path

1. [Usage query performance](topics/usage-query-performance.md)
2. [Model support routing](topics/model-support-routing.md)
3. [Prompt logic retry](topics/prompt-logic-retry.md)
4. [Config and capability information architecture](topics/config-capability-information-architecture.md)
5. [Regression plan](topics/regression-plan.md)
6. [Roadmap](roadmap.md)
7. [Historical implementation snapshot](history/implementation-snapshot-2026-07-07.md)
