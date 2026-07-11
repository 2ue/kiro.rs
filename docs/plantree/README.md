# Plan Tree

This directory is the durable planning entrypoint for active architecture and validation work in this repository.

## Authority Order

1. User requirements in the current thread.
2. Active plan roots listed below.
3. Baseline project maps in `baseline/`.
4. Older analysis documents in `docs/`.

## Active Plans

| Plan | Status | Current Phase | Last Landed | Next Target |
| --- | --- | --- | --- | --- |
| [Request body capability modularization](plans/request-body-capability-modularization/README.md) | In Progress | Inventory and execution planning | 2026-07-06: plan root created from current code inventory | Extract capability plans without changing caller-visible behavior |
| [Admin observability, routing model support, and config IA](plans/admin-observability-routing-config/README.md) | In Progress | Analysis and execution planning | 2026-07-07: code-path inventory for usage query, model eligibility, retry, and config UI | Implement exact log search, model support eligibility, prompt-logic retry, and clearer config grouping |
| [Runtime correctness and release gates](plans/runtime-correctness-and-release-gates/README.md) | In Progress | Final validation and evidence closure | 2026-07-10/11: [static, storage, release, protocol, load, and shutdown gates passed](plans/runtime-correctness-and-release-gates/history/evidence-index.md) | Complete the end-to-end Docker gate after the crates.io fetch timeout |

## Baseline

- [Module map](baseline/module-map.md)
- [Runtime flows](baseline/runtime-flows.md)
- [Storage and state](baseline/storage-and-state.md)
- [Test and release gates](baseline/test-and-release-gates.md)
- [Risk hotspots](baseline/risk-hotspots.md)

## Ideas

- [Inbox](ideas/inbox.md)
