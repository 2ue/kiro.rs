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

## Baseline

- [Module map](baseline/module-map.md)
- [Runtime flows](baseline/runtime-flows.md)
- [Storage and state](baseline/storage-and-state.md)
- [Test and release gates](baseline/test-and-release-gates.md)
- [Risk hotspots](baseline/risk-hotspots.md)

## Ideas

- [Inbox](ideas/inbox.md)
