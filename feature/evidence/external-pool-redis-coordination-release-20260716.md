# External Pool Redis Coordination And Release Evidence

Date: 2026-07-16 19:50 CST

Status: `focused isolated PASS / non-frozen dirty candidate / final release gates open`

## Build And Safety Identity

- Git HEAD: `401473ca1649`; the worktree contained concurrent uncommitted changes, so this is not a frozen candidate SHA.
- Toolchain: `cargo 1.92.0`, `rustc 1.92.0`.
- Final static checkpoint for this batch: `cargo fmt --all` and `cargo +1.92.0 check --all-targets`, exit 0, zero warnings at that checkpoint.
- No new release binary SHA was produced. Test results came from Cargo debug test binaries and must be repeated on the final release candidate.
- PostgreSQL: isolated container on `127.0.0.1:47432`; password was read only through shell substitution and was not printed or written.
- Redis: isolated direct instance on `127.0.0.1:47379`; restart-only Redis on `127.0.0.1:47380`.
- Toxiproxy: API `127.0.0.1:48474`, Redis proxy `127.0.0.1:46380`, proxy name `scheduler`.
- End state: Toxiproxy enabled with no toxics. No test targeted `127.0.0.1:9022`, production, or real credentials.

## Commands

Secrets are intentionally represented as placeholders:

```bash
export KIRO_RS_TEST_POSTGRES_URL='postgresql://<isolated-user>:<redacted>@127.0.0.1:47432/<isolated-db>'
export KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:47379/'
cargo test external_pool_one_malformed_runtime_isolated_from_fifty_nine_healthy_for_five_rounds -- --nocapture --test-threads=1

export KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:47380/'
export KIRO_RS_TEST_REDIS_RESTART_CONTAINER='kiro-rs-validation-redis-restart-20260716'
cargo test external_pool_redis_restart_fails_closed_and_recovers_five_of_five -- --nocapture --test-threads=1
cargo test external_pool_redis_data_loss_fences_active_confirmed_lease_before_reacquire -- --nocapture --test-threads=1
cargo test external_pool_redis_restart_fences_multiple_active_leases_across_managers_for_five_rounds -- --nocapture --test-threads=1

export KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:46380/'
export KIRO_RS_TEST_REDIS_DIRECT_URL='redis://127.0.0.1:47379/'
export KIRO_RS_TEST_TOXIPROXY_API='http://127.0.0.1:48474'
export KIRO_RS_TEST_TOXIPROXY_NAME='scheduler'
cargo test external_pool_release_dispatcher_drains_10k_real_leases_after_commit_unknown_for_five_rounds -- --nocapture --test-threads=1

export KIRO_RS_RUN_EXTERNAL_REDIS_RTT_MATRIX=1
cargo test external_pool_redis_rtt_and_concurrency_matrix_five_outer_rounds -- --nocapture --test-threads=1
```

Additional focused commands:

```bash
cargo test redis_external_pool_ -- --nocapture --test-threads=1
cargo test external_pool_coordinator_clean_startup_has_no_recovery_barrier_for_five_rounds -- --nocapture --test-threads=1
cargo test external_pool_redis_disconnect_fails_closed_and_recovers -- --nocapture --test-threads=1
cargo test external_pool_lease_touch_and_drop_release_are_accepted_and_drained -- --nocapture --test-threads=1
```

## Preserved Red Evidence

| Red case | Observed result | Root cause and disposition |
| --- | --- | --- |
| One malformed pool, Redis list | `list round 0: healthy pools must remain selectable` | Snapshot Lua used hard `GET`; fixed with pool-level `pcall` sentinel |
| Redis data loss with old confirmed lease | `Redis restart/data loss admitted a second lease before the old lease was fenced` | Redis lease sets had no process generation; fixed with PG/run-id epoch and barrier |
| First active restart test timing | `reacquire must be attempted before old heartbeat fencing` | Test used 9s max age, leaving about 4s safety cutoff; Docker reconnect consumed the race window. Test changed to 15s max age/8s barrier while retaining a 5s heartbeat probe requirement |
| No-active restart initial suite | rounds 0-3 passed, round 4 exceeded 10s recovery window | Redis container startup plus ConnectionManager retry jitter; a complete rerun and final post-dispatcher rerun were 5/5. This is not recorded as a strict 10s production SLA |
| First 21-cell fixture, 0ms/c64 | warmup failures `160/200` | Five managers shared one PG authority but used five Redis prefixes, so they rotated one PG epoch across five namespaces. Fixed the fixture to production topology: same PG authority and same Redis prefix; pool limit raised from 128 to 1024 to cover aggregate c320 |

Thresholds were not loosened to hide these reds. The RTT assertion was corrected only to separate fixed request RTTs from the already designed 5-second low-frequency run-id probe.

## Correctness Results

| Scenario | Rounds/results | Important assertions |
| --- | --- | --- |
| Malformed cooldown | invalid JSON/list/hash/set x 5 | 59 healthy pools selectable; one invalid pool isolated; status returns all 60 |
| Clean startup | 5/5 | PG row initially absent; first guard `Ready`; no 35s barrier; peer reuses epoch |
| Transport disconnect, no restart | 5/5, test body 6.38s | fail closed then recover; run-id/epoch/PG authority version unchanged; no barrier |
| Redis stop/start, no active lease | final 5/5, test body 12.28s | downtime fail closed; recovery remains 5/5 |
| Redis SIGKILL/data loss, one active lease | 5/5 after dispatcher, test body 51.02s | fresh manager denied inside barrier; old epoch lost before reacquire; epoch rotates |
| Redis SIGKILL/data loss, four active leases | 5/5, test body 50.89s | two leases on manager plus two on peer; all four heartbeats fenced; fresh manager recovers after barrier |
| Redis storage primitive group | 10/10, test body 16.09s | atomic acquire, no oversell, pending tombstone, confirmed release, queue lease and 60-pool/10k competition |
| Single Drop/drain | 1/1 | dispatcher accepts Drop, explicit drain reaches pool/global zero without TTL |

## 10k Real Lease Fault And Recovery

Each of five rounds first wrote 10,000 real confirmed leases into Redis and created one release reservation for each. A downstream `reset_peer` then allowed the Redis command to be commit-unknown: Redis might execute a batch while the client only receives a connection error. The dispatcher had to retain all intents, retry after recovery, and treat an idempotent successful response with `removed=false` as completed.

| Round | Drop registration | Recovery after toxic removal | Failed retry rounds | End state |
| --- | ---: | ---: | ---: | --- |
| 0 | 18ms | 1549ms | 1 | pending/pool/global/tombstone 0 |
| 1 | 11ms | 1406ms | 1 | pending/pool/global/tombstone 0 |
| 2 | 11ms | 1427ms | 1 | pending/pool/global/tombstone 0 |
| 3 | 11ms | 1469ms | 1 | pending/pool/global/tombstone 0 |
| 4 | 16ms | 1471ms | 1 | pending/pool/global/tombstone 0 |

The test body took 52.24s; a preceding concurrent-worktree rebuild took 30.70s and is not included as performance data. Every round started exactly one worker. A duplicate registration kept pending at 10,000 and incremented the dedup counter once. After drain, all 65,536 capacity permits could be acquired, the next permit failed as the hard bound requires, and dropping the permits restored all 65,536.

Resource samples for the 10k process:

- RSS: start 22,320 KiB; sampled peak 44,032 KiB; end 45,152 KiB.
- FD: start 20; sampled peak 21; end 21.
- Drop uses HashMap registration and did not create 10,000 tasks; worker starts were exactly 5 for five bursts.

## Complete 21-Cell RTT Matrix

Topology per cell: five managers sharing one PG authority and one Redis prefix, 60 pools, 200 aggregate warmup requests, 1,000 aggregate measured requests, and five post-cell recovery requests. `reqRTT=2000` means one snapshot plus one atomic acquire per measured request. Probe RTT is separate and bounded by `(ceil(wall/5s)+2) x 5 managers`.

| Redis delay | Per-manager c | Aggregate c | Success | Recovery | Probe/bound | p50 us | p95 us | p99 us | Wall ms |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 64 | 320 | 1000/1000 | 5/5 | 0/15 | 1145815 | 1232363 | 1239612 | 3668 |
| 0 | 16 | 80 | 1000/1000 | 5/5 | 5/15 | 260709 | 307734 | 320055 | 3365 |
| 0 | 1 | 5 | 1000/1000 | 5/5 | 5/15 | 18584 | 28256 | 50554 | 3924 |
| 50 | 64 | 320 | 1000/1000 | 5/5 | 5/15 | 1090849 | 1183790 | 1219949 | 3544 |
| 50 | 16 | 80 | 1000/1000 | 5/5 | 5/15 | 345062 | 607231 | 680791 | 4571 |
| 50 | 1 | 5 | 1000/1000 | 5/5 | 25/40 | 124368 | 201295 | 298458 | 27555 |
| 74 | 64 | 320 | 1000/1000 | 5/5 | 3/15 | 1433368 | 1614002 | 1642289 | 4550 |
| 74 | 16 | 80 | 1000/1000 | 5/5 | 3/15 | 262683 | 350423 | 388117 | 3553 |
| 74 | 1 | 5 | 1000/1000 | 5/5 | 35/50 | 171901 | 238485 | 276242 | 36252 |
| 75 | 64 | 320 | 1000/1000 | 5/5 | 4/15 | 1011813 | 1095446 | 1188384 | 3429 |
| 75 | 16 | 80 | 1000/1000 | 5/5 | 1/15 | 247807 | 279328 | 392649 | 3291 |
| 75 | 1 | 5 | 1000/1000 | 5/5 | 35/50 | 179385 | 236631 | 288686 | 37501 |
| 90 | 64 | 320 | 1000/1000 | 5/5 | 5/15 | 1189683 | 1376205 | 1506211 | 3964 |
| 90 | 16 | 80 | 1000/1000 | 5/5 | 5/15 | 290585 | 406885 | 478367 | 3915 |
| 90 | 1 | 5 | 1000/1000 | 5/5 | 41/55 | 209466 | 296996 | 347615 | 43929 |
| 150 | 64 | 320 | 1000/1000 | 5/5 | 2/15 | 1211695 | 1665021 | 1672698 | 4318 |
| 150 | 16 | 80 | 1000/1000 | 5/5 | 4/15 | 320633 | 456927 | 523870 | 4569 |
| 150 | 1 | 5 | 1000/1000 | 5/5 | 70/85 | 343262 | 486413 | 525178 | 71050 |
| 500 | 64 | 320 | 1000/1000 | 5/5 | 5/20 | 1235955 | 2033923 | 2587354 | 5531 |
| 500 | 16 | 80 | 1000/1000 | 5/5 | 10/30 | 1071057 | 1602714 | 1674652 | 15094 |
| 500 | 1 | 5 | 1000/1000 | 5/5 | 204/245 | 1057533 | 1581233 | 1622306 | 233137 |

All cells had `admission_rejections=0`. Test body time was 630.49s; compilation before the test took 53.27s. Resource samples:

- RSS: start 24,912 KiB; sampled peak 57,168 KiB; end 30,192 KiB.
- FD: start 44; sampled peak 45; end 45.

The matrix is a correctness/resource/fault-delay result, not a release latency pass. In particular, debug p95 at zero injected delay was 1.232s for aggregate c320 and 28.256ms for aggregate c5. A frozen release binary and same-host baseline are still required before claiming performance acceptance.

## Supported Topology And Residual Evidence

The passing multi-manager tests use one PostgreSQL authority and one Redis key prefix. A separate isolated runner used two PostgreSQL databases with the same Redis namespace and observed `external pool coordinator is recovering for ~34s`. This is a split-authority configuration: PostgreSQL advisory locks cannot coordinate across databases. It must not be classified as an intermittent healthy state. Current operational choices are shared PG authority, or distinct Redis prefixes.

Other explicit limits:

- standalone Redis only; Redis Cluster/CROSSSLOT is rejected and unverified;
- 35-second barrier assumes a maximum 30-second heartbeat interval, 2-second Redis operation timeout and margin;
- a runtime stop-the-world longer than the margin is outside the proof;
- one manager can have one release worker at a time; it exits when idle and is restarted by the next burst;
- graceful shutdown drains, but process crash or Redis outage beyond the shutdown budget still relies on lease TTL;
- dispatch queue lease cleanup remains a separate bounded task/TTL path;
- current worktree identity is non-frozen and must not be used as release provenance.
