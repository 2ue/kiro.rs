# External Pool Redis Coordination Matrix

Date: 2026-07-16

Status: `focused matrix complete / release-candidate rerun pending`

Authority: [issue and selected design](../issues/external-pool-redis-coordination-and-release.md)

Evidence: [commands, reds, timings and full 21-cell table](../evidence/external-pool-redis-coordination-release-20260716.md)

## Test Topology

- Isolated PostgreSQL `127.0.0.1:47432`.
- Isolated Redis `127.0.0.1:47379`; restart-only Redis `127.0.0.1:47380`.
- Toxiproxy API `127.0.0.1:48474`, proxy `127.0.0.1:46380`.
- No production, `9022`, real credentials or real upstream requests.
- Multi-manager supported topology means same PG database/schema authority and same Redis key prefix.

## Focused Matrix

| ID | Scenario | Rounds/load | Result | Release-gate meaning |
| --- | --- | --- | --- | --- |
| ER01 | cooldown invalid JSON | 5 | PASS | one invalid pool isolated |
| ER02 | cooldown Redis list | 5 | PASS after preserved red | WRONGTYPE no longer aborts 60-pool batch |
| ER03 | cooldown Redis hash | 5 | PASS | one invalid pool isolated |
| ER04 | cooldown Redis set | 5 | PASS | one invalid pool isolated |
| ER05 | clean startup | 5 independent PG schemas/prefixes | PASS | no 35s cold-start barrier |
| ER06 | peer clean startup | 5 | PASS | same authority/prefix reuses epoch |
| ER07 | reset_peer, no restart | 5 | PASS | fail closed/recover; no epoch/run-id/version change |
| ER08 | stop/start, no active lease | final 5 | PASS; earlier 4/5 red preserved | not a strict 10s SLA |
| ER09 | SIGKILL/data loss, one active confirmed lease | 5 | PASS | fresh manager fenced until old heartbeat lost |
| ER10 | SIGKILL/data loss, four active leases | 5 | PASS | same-manager and cross-manager heartbeats all fenced |
| ER11 | 10k true lease Drop under commit-unknown | 10,000 x 5 | PASS | one worker/round; no lost intent; end counts zero |
| ER12 | duplicate intent/idempotent removed=false | embedded in ER11 x 5 | PASS | dedup and commit-unknown retry complete |
| ER13 | release hard capacity | 65,536 + 1 | PASS | 65,536 reserve; next fail closed; all permits restored |
| ER14 | graceful drain | ER11 plus single Drop | PASS | pending reaches zero and worker is idle |
| ER15 | storage external Redis primitive group | 10 tests | PASS | atomicity, tombstone, queue, cross-manager no oversell |
| ER16 | latency/concurrency matrix | 7 delays x 3 concurrency, 1,000/cell | PASS for correctness/resources | release-mode latency acceptance still open |
| ER17 | standalone/cluster check | source plus run-id probes | standalone PASS | Cluster is explicitly unsupported, not a pass |
| ER18 | split PG authority with same Redis prefix | separate isolated runner | FAIL/unsupported | startup config validation or Redis CAS design required |

## RTT Cell Contract

ER16 uses injected Redis delays `0/50/74/75/90/150/500ms` and per-manager concurrency `64/16/1`; five managers produce aggregate concurrency `320/80/5`. Every cell requires:

- 1,000/1,000 measured request operations succeed;
- five post-cell operations recover;
- zero selection admission rejection;
- exactly 2,000 request RTTs plus only the bounded five-second run-id probes;
- no persistent FD growth and bounded RSS peak.

All 21 cells satisfied that contract. Exact p50/p95/p99 and probe counts are in the evidence file. This does not close the performance gate because the binary was debug/non-frozen and zero-delay aggregate-c320 p95 was 1.232s.

## Final Candidate Rerun

Before release, repeat on one immutable release candidate and save binary SHA-256:

1. `cargo fmt --all -- --check`, `git diff --check`, `cargo +1.92.0 check --all-targets`, and complete `cargo test --all-targets`.
2. ER01-ER18 with isolated namespaces; ER18 must be rejected at startup/config time or remain documented unsupported.
3. The 21-cell matrix in release mode with an unchanged-host baseline and explicit regression thresholds.
4. Shared-PG/shared-prefix two-instance L3/L4 traffic while Redis restart/reset and usage cleanup run concurrently.
5. Graceful shutdown with a populated release backlog and forced process crash as a TTL-control comparison.
6. A 15-30 minute fake-upstream soak measuring RSS, FD, worker count, Redis RTT, TTFB and recovery.

Any capacity error while an eligible external pool has verified distributed capacity, epoch rotation without Redis restart, lost release intent, post-barrier resurrection of an old heartbeat, or resource growth after idle is a release blocker.
