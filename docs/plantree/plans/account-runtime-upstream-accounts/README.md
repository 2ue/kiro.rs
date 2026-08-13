# Account Runtime Upstream Accounts

Role: Current Rust refactor plan for removing Kiro concepts and making upstream accounts the scheduling unit

Status: In Progress; implementation branch created

Authority: Defines the target boundary for the current-repository refactor requested after `feature/usage-correction-cost-floor`

As of: 2026-08-13

Related: [Plan Tree](../../README.md), [current module map](../../baseline/module-map.md), [protocol contracts](../../baseline/protocol-and-api-contracts.md), [runtime flows](../../baseline/runtime-flows.md), [target architecture](topics/final-target-plan.md)

## Purpose

Turn the current Rust service into a Kiro-independent account scheduling and usage shaping runtime.

The target system has no Kiro account, Kiro provider, Kiro upstream protocol, Kiro endpoint, Kiro credential refresh, Kiro model/quota/subscription, Kiro EventStream, or Kiro body-envelope concept. Current Kiro-named code may be mined only for generic scheduler, lease, usage, body, stream, and observability behavior. It must not remain as product terminology or runtime dependency in the target implementation.

## Final Target

The final codebase should operate around upstream accounts:

```text
client Anthropic/Claude-compatible request
  -> protocol entry
  -> body, model and capability processing
  -> account runtime and scheduler
  -> selected upstream account HTTP execution
  -> stream/non-stream response shaping
  -> raw, effective and reported usage
  -> statistics, Admin and observability
```

The former external pool concept becomes the account concept. There is no local-versus-external pool split in the target architecture. If accounts later need grouping, groups are configuration filters and UI organization only; they are not a scheduler boundary.

## Required Git Start Sequence

Implementation must not start on the current feature branch.

1. Commit the current `feature/usage-correction-cost-floor` work, including this plan.
2. Fast-forward the default mainline branch. `origin/HEAD` currently points to `origin/master`, so `master` is the mainline unless the remote default changes before execution.
3. Merge `feature/usage-correction-cost-floor` into `master`.
4. Create a new implementation branch from the updated `master`, preferably `feature/account-runtime-upstream-accounts`.
5. Implement the target refactor only on that new branch.

## Plan Documents

- [Final target plan](topics/final-target-plan.md): scope, deletion boundary, retained generic behavior, module shape, implementation order, and acceptance checks.

## Current State

The implementation branch `feature/account-runtime-upstream-accounts` has been created from the updated `master`.

Initial neutral `account_runtime` domain types have landed as the first code boundary:

- upstream account identity, auth policy, proxy and limits;
- account attempt trace, delivery evidence and normalized upstream error classes;
- raw usage facts with explicit confidence;
- a minimal dispatch candidate/selection primitive for later scheduler extraction.
- a migration bridge from the old `ExternalPool` record into the new `UpstreamAccount` boundary, proving the former external pool can be treated as an upstream account without exposing Kiro concepts.
- `/api/admin/accounts` aliases for the existing external-pool Admin handlers and matching frontend account API aliases, giving new callers an account-named boundary while the old UI is migrated.
- the Admin UI resource navigation now exposes the upstream account page at `/accounts`, with the old `/external-pools` route redirecting to it.
- the upstream account page now calls the account-named Admin API aliases for list, status, create, update, enable, cooldown, supported-model discovery, delete and test actions.

Last verified on 2026-08-13:

- `feature/tests/run-cargo-scoped.sh account-runtime-initial-test2 -- cargo test account_runtime`
- `feature/tests/run-cargo-scoped.sh account-runtime-initial-check -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-bridge-test2 -- cargo test external_pool_projects_to_upstream_account_boundary`
- `feature/tests/run-cargo-scoped.sh account-bridge-check2 -- cargo check`
