# Roadmap

Last reviewed: 2026-08-02 Asia/Shanghai

## Done

- External pool local-route hot path moved to cached/no-wait gates.
- Raw preflight route gate moved to cached/no-wait.
- Raw direct external no longer pre-checks eligible pool before entering direct external path.
- New real local PgSQL/Redis integration tests added for cached external availability:
  - cold cache + locked PgSQL + 128 concurrent local gates return fast;
  - warmed cache respects available/full/released external runtime capacity.
- Current active TODOs migrated from `feature/todo` into this plan's topics.
- Route-policy config authority focused pass: built-in `/v1`、`/cc`、`/ha`、`/na` routes remain fixed entrypoints, but cache, usage, prompt steering, external-pool route rules, and cache namespace now resolve from runtime configuration. Full Rust all-targets, UI/admin-ui build, docs contract, prompt parity/independence, and diff checks passed; live reload/browser/production gates remain post-focused follow-up.

## In Progress

- Document disposition cleanup:
  - current valid issues migrated into this plan;
  - first historical archive batch for old slow-first-token/stream-fluidity analysis.
- External pool strategy productization:
  - local capacity queue-first vs external takeover policy;
  - cooldown controls and manual recovery;
  - no-local-credential temporary external-direct and quick return to local-first.

## Next

1. Finish first archive batch and update archive indexes.
2. Refresh `docs/plantree/README.md` registered plan table to include this plan.
3. Create RoutePlanner / CapacityLedger design topic with state-machine table.
4. Implement explicit local capacity overflow policy.
5. Implement external pool cooldown policy controls and manual recovery path.
6. Run focused fake + real local PgSQL/Redis scheduler/load chaos matrix.

## Deferred

- Greenfield AI Gateway implementation.
- Full project-wide Markdown migration.
- Deletion of any legacy document.
- Real upstream high-concurrency pressure.
