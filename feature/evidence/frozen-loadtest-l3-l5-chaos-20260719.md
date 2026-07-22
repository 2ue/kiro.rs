# Frozen loadtest L3-L5 load/chaos matrix

Date: 2026-07-19

Status: `historical L5 reproduced Redis scheduler blocker / r8 L3+L4+L5 pass / remaining real-upstream/CLI/UI/upgrade/inventory gates / NO-GO`

## Scope

This evidence extends the frozen fake-upstream validation from L1 into burst, restart/failure chaos, and sustained long-stream soak. It used the current-project isolated PostgreSQL/Redis pair only, random temporary loopback ports, and fake Kiro upstreams. It did not touch the protected local `9022` service and did not run broad Docker validation.

The raw load/proxy directories were retained only long enough to extract the L5 root-cause log evidence. After extraction, the owned raw roots were deleted:

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-l5-load-chaos-aI9rvO
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-l5-load-chaos-LCCVpq
```

Only the redacted summaries and hashes remain.

## Frozen binaries and isolated services

Product binary:

```text
/tmp/kiro-frozen-20260719-r2/kiro-rs
sha256 e16df13a0ded4d53ac255f26ddc24056c4d385dde418a63944a2e00d122c642a
```

Current scheduler-wait candidate binary under rerun:

```text
/tmp/kiro-frozen-20260719-r6/kiro-rs
sha256 d75c102191828032b3a8a5b9b7a5e05cb3807307aa6b463a8992eeee8164501c
```

Current scheduler-timeout candidate binary under rerun:

```text
/tmp/kiro-frozen-20260719-r8/kiro-rs
sha256 131696bd81e1cdaeceaac6a45f9c76bf698eb559785b379a82fd77e2f742e631
```

Loadtest binary:

```text
/tmp/kiro-frozen-20260719-r5/kiro_loadtest
sha256 23c04221deb72dde601d491452d8cc9a99211df99b2cd39a386272141f2db8e3
```

Isolated development services:

```text
PostgreSQL container: kiro-final-20260718-pg, loopback 127.0.0.1:50891
Redis container:      kiro-final-20260718-redis, loopback 127.0.0.1:50892
```

The runner created one temporary PostgreSQL database and one Redis prefix per scenario and cleaned them in `finally`. Summary `cleanupError` was `null` for L3, L4, L5 900s, and L5 300s diagnostic.

## Runner hardening performed before final L3/L4

The orchestration runner is:

```text
feature/tests/frozen-load-chaos-runner.mjs
```

The runner was added so frozen `kiro-rs` and frozen `kiro_loadtest` can be validated without discovering or rebuilding from repository `target/`. It accepts explicit binary paths, uses random non-`9022` ports, creates isolated storage namespaces, hashes logs/reports, and deletes raw roots by default.

The runner itself needed three fixture corrections before the final L3/L4/L5 evidence was considered valid:

1. `docker exec env ...` was invalid for the local service checks. It was fixed to `docker exec <container> env ...`.
2. The initial load configuration used very high synthetic RPM values and polluted burst tests with artificial local 429s. For L3-L5 load/chaos, request and credential RPM are now set to `0` so the suite isolates scheduler/load behavior from RPM gates.
3. L3 recovery-after-error-burst and L4 restart/mixed-chaos recovery expectations were corrected to match the fake upstream’s actual state transition. Recovery cases now restart the fake upstream to `normal-stream` and use a bounded cooldown before checking healthy traffic.

## L3 burst/recovery result

Summary:

```text
/tmp/kiro-l3-load-chaos-summary-20260719-r5.json
runId l3_mrrc3gc0_52862
passed true
resultCount 9
```

| Case | Expect | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | Pass |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | --- |
| `l3_normal_c1_r5` | success | 5 | `{"200":5}` | 5 / 0 | 11 | 11 | 12 | yes |
| `l3_normal_c5_r20` | success | 20 | `{"200":20}` | 20 / 0 | 14 | 14 | 16 | yes |
| `l3_normal_c10_r50` | success | 50 | `{"200":50}` | 50 / 0 | 9 | 9 | 12 | yes |
| `l3_spike_c40_r100` | success | 100 | `{"200":100}` | 100 / 0 | 34 | 34 | 39 | yes |
| `l3_recovery_after_spike_c3_r10` | success | 10 | `{"200":10}` | 10 / 0 | 7 | 7 | 9 | yes |
| `l3_recovery_after_error_burst_c12_r40` | recovered | 40 | `{"200":5,"429":28,"502":7}` | 5 / 35 | 21 | 21 | 22 | yes |
| `l3_post_error_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 6 | 6 | 7 | yes |
| `l3_invalid_tool_burst_c20_r40` | error | 40 | `{"502":40}` | 0 / 40 | 68 | 0 | 68 | yes |
| `l3_invalid_tool_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 142 | 142 | 215 | yes |

L3 conclusion: short burst and immediate recovery scenarios pass for this frozen binary under fake upstream. This does not close sustained soak or real upstream validation.

## L4 restart/failure chaos result

Summary:

```text
/tmp/kiro-l4-load-chaos-summary-20260719-r2.json
runId l4_mrrc75lm_81889
passed true
resultCount 12
```

| Case | Expect | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | FD start→peak→end | Pass |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- |
| `l4_proxy_restart_during_long_stream` | any | 80 | `{"200":8,"transport_error":72}` | 8 / 72 | 530 | 530 | 4711 | 30→45→26 | yes |
| `l4_proxy_restart_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 7 | 7 | 8 | 29→32→32 | yes |
| `l4_rate_limit_burst_c20_r40` | error | 40 | `{"429":40}` | 0 / 40 | 40 | 0 | 40 | 30→49→49 | yes |
| `l4_rate_limit_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 7 | 7 | 9 | 29→32→32 | yes |
| `l4_server_error_burst_c20_r40` | error | 40 | `{"429":20,"502":20}` | 0 / 40 | 53 | 0 | 53 | 31→50→50 | yes |
| `l4_server_error_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 7 | 7 | 9 | 30→33→33 | yes |
| `l4_invalid_tool_burst_c20_r40` | error | 40 | `{"502":40}` | 0 / 40 | 55 | 0 | 55 | 30→49→49 | yes |
| `l4_invalid_tool_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 26 | 26 | 28 | 29→32→32 | yes |
| `l4_client_drop_c20_r40` | error | 40 | `{"200":40}` | 0 / 40 | 55 | 0 | 55 | 30→30→29 | yes |
| `l4_client_drop_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 6 | 6 | 7 | 29→32→32 | yes |
| `l4_mixed_chaos_c24_r96` | mixed | 96 | `{"200":27,"429":66,"502":3}` | 25 / 71 | 359 | 8330 | 1590 | 30→62→52 | yes |
| `l4_mixed_chaos_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 36 | 36 | 37 | 28→31→31 | yes |

L4 conclusion: bounded restart/error/client-drop scenarios recover on the frozen binary. `l4_mixed_chaos_c24_r96` intentionally contains errors and shows high first-text p95 because the scenario mixes long/error behavior; recovery normal traffic passes afterward.

## Historical L5 sustained soak red result

The first L5 attempt used a fast mixed-chaos scenario and exhausted 100,000 requests too quickly. That run is not counted as a valid 15-minute soak. The L5 suite was changed to use a long-stream fake upstream so concurrency remains sustained for the full duration.

Final 900-second summary:

```text
/tmp/kiro-l5-load-chaos-summary-20260719-r2.json
runId l5_mrrch93p_21610
passed false
```

| Case | Expect | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | FD start→peak→end | Pass |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- |
| `l5_long_stream_soak_900s_c20` | success | 100000 | `{"200":5949,"429":94051}` | 5949 / 94051 | 260 | 389 | 2607 | 30→71→49 | no |
| `l5_post_soak_recovery_normal_c3_r12` | success | 12 | `{"200":10,"429":2}` | 10 / 2 | 63 | 63 | 68 | 29→32→32 | no |

Resource cleanup after the 900-second L5 did pass the resource-return criteria:

```text
durationSecs=900
idleCooldownSecs=60
start RSS=30,785,536 bytes, fd=31
idle RSS=17,825,792 bytes, fd=30
rssReturnedWithin32MiB=true
fdReturnedWithin5=true
```

Diagnostic 300-second summary:

```text
/tmp/kiro-l5-300s-diagnostic-summary-20260719.json
runId l5_mrrd8tmu_16612
passed false
```

| Case | Expect | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | FD start→peak→end | Pass |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- |
| `l5_long_stream_soak_300s_c20` | success | 14564 | `{"200":2203,"429":12361}` | 2203 / 12361 | 298 | 439 | 2680 | 30→69→49 | no |
| `l5_post_soak_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 43 | 43 | 59 | 29→33→32 | yes |

Resource cleanup after the 300-second diagnostic also passed the resource-return criteria:

```text
durationSecs=300
idleCooldownSecs=30
start RSS=30,539,776 bytes, fd=31
idle RSS=19,267,584 bytes, fd=30
rssReturnedWithin32MiB=true
fdReturnedWithin5=true
```

## L5 root cause extracted from the diagnostic raw proxy log

The red condition is not a real credential disable, not configured RPM saturation, and not upstream model failure. The proxy log from the 300-second diagnostic showed the same production-class failure:

```text
本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=2）
```

Extracted counts from the raw proxy log before deletion:

```text
redis_degraded=24722
dispatch_unavailable=24722
concurrency_slot=4
session_binding=6
```

The first observed hot-path breaker was:

```text
占用 Redis 凭据并发槽超过共享总期限 75ms
breaker=capacity
timeout_ms=75
backoff_ms≈1991
```

Additional affinity failures were also present:

```text
读取 Redis 会话绑定超过共享总期限 75ms
原子写入 Redis 会话绑定超过共享总期限 75ms
breaker=affinity
```

This is important because the run used only fake upstream, isolated local Redis/PostgreSQL, disabled RPM gates, 4 fake credentials, and concurrency 20. The sustained long-stream shape alone is enough to reproduce the scheduler hot-path breaker and mass local 429s.

## Interpretation

L3 and L4 passing does not prove the scheduler is stable under sustained stream occupancy. L5 proves the opposite for this frozen binary:

- short bursts and recovery complete;
- process RSS/FD return after idle;
- but sustained long-stream load opens the Redis scheduler hot-path breaker;
- once the breaker is open, the local pool fail-closes and returns a large number of 429s;
- post-soak recovery may still see residual 429s if the breaker window overlaps the recovery check.

This directly maps to the user’s production symptoms: high concurrency with lower downstream RPM, healthy credentials in persistent storage, but request admission reports local scheduler/Redis degraded and behaves like no local account is ready.

## Release impact

Release remains `NO-GO`.

This historical evidence upgraded SCH-001/SCH-003/SCH-005/OPS-001 from “pending frozen load” to “reproduced on frozen L5”. The next candidate had to fix the sustained-load Redis scheduler degraded behavior and rerun the matrix.

## Candidate r6 scheduler-wait rerun

Candidate r6 contains the scheduler wait change where normal local requests that encounter `SchedulerRedisUnavailableError` enter a bounded local-only dispatch wait instead of immediately returning a 429. `FailFastOnCapacity` still returns the degraded condition for external fallback preflight, preserving the routing contract. r6 also includes the Redis weighted in-flight cleanup key-order fix.

### 120-second diagnostic

Summary:

```text
/tmp/kiro-l5-120s-after-scheduler-wait-keepraw-20260719-r1.json
passed false
```

| Case | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | RSS start→idle | FD start→idle | Pass |
| --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- | --- |
| `l5_long_stream_soak_120s_c20` | 921 | `{"200":921}` | 921 / 0 | 379 | 379 | 2674 | 29,851,648→68,894,720 | 31→29 | request pass; resource gate failed |
| `l5_post_soak_recovery_normal_c3_r12` | 12 | `{"200":12}` | 12 / 0 | 19 | 19 | 21 | 68,894,720→73,269,248 | 29→31 | yes |

Raw log inspection before deletion showed no Redis degraded, no 429, and breaker cumulative failures all zero. The only failing gate was the strict RSS return criterion after a short 20-second idle cooldown; FD returned within the configured bound. Raw root deleted after extraction:

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-l5-load-chaos-2LSuOK
```

### First 300-second diagnostic

Summary:

```text
/tmp/kiro-l5-300s-after-scheduler-wait-20260719-r1.json
passed false
```

| Case | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | RSS start→idle | FD start→idle | Pass |
| --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- | --- |
| `l5_long_stream_soak_300s_c20` | 1988 | `{"200":1830,"429":158}` | 1830 / 158 | 5014 | 348 | 5015 | 29,818,880→73,465,856 | 31→29 | no |
| `l5_post_soak_recovery_normal_c3_r12` | 12 | `{"200":12}` | 12 / 0 | 7 | 7 | 8 | 73,416,704→77,856,768 | 28→31 | yes |

This run proved the quick 429 storm was reduced dramatically compared with the historical 300-second red (`12,361` 429 → `158` 429), but did not close L5. Raw was not retained, so it could not distinguish residual Redis degraded from normal bounded wait timeout.

### Second 300-second keep-raw diagnostic

Summary:

```text
/tmp/kiro-l5-300s-r6-keepraw-20260719-r2.json
passed true
rawRoot /var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-l5-load-chaos-fhPioD
```

| Case | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | RSS start→idle | FD start→idle | Pass |
| --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- | --- |
| `l5_long_stream_soak_300s_c20` | 2281 | `{"200":2281}` | 2281 / 0 | 361 | 361 | 2733 | 29,753,344→48,218,112 | 31→30 | yes |
| `l5_post_soak_recovery_normal_c3_r12` | 12 | `{"200":12}` | 12 / 0 | 30 | 30 | 33 | 48,234,496→64,110,592 | 29→32 | yes |

Raw log grep before deletion found no `Redis 调度协调状态不可用`, no `占用 Redis 凭据并发槽`, no `排队等待超时`, no `429`, no `local_all_disabled`, and no scheduler breaker failures. Breaker cumulative counters at shutdown were:

```text
capacity admitted=2293 failures=0 suppressed=0
snapshot admitted=116 failures=0 suppressed=0
affinity admitted=6879 failures=0 suppressed=0
```

The raw root was deleted after extraction:

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-l5-load-chaos-fhPioD
```

### Current interpretation

r6 closes the deterministic 300-second fake-upstream scheduler-degraded failure in one keep-raw rerun and substantially reduces the failure mode in the earlier diagnostic. It did not close the final soak: a later 900-second keep-raw rerun was stopped early after the red condition was already reproduced.

### r6 900-second rerun stopped after early red evidence

Run command used candidate r6, the same frozen loadtest binary, and the same isolated PostgreSQL/Redis pair:

```text
/tmp/kiro-l5-900s-r6-final-keepraw-20260719-r1.json
rawRoot /var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-l5-load-chaos-uJrbSK
```

The run was manually stopped after roughly 4.5 minutes because the release-blocking condition had already appeared and continuing would only generate avoidable local load. Extracted redacted proxy-log counts before deletion:

```text
redis_degraded=46
capacity_timeout=3
affinity_read_timeout=1
queue_timeout=46
returned_429=23
local_all_disabled=0
pg_usage_slow=6
pg_sql_slow=3
usage_retry=3
breaker_recovery=8
proxy_log_sha256=751250bd1a21590a182c9eb2d1273ef56331a76fa4ac4670a116919825d68cc7
```

The first failure sequence was:

```text
PgSQL usage batch slow: 432 ms, then 1569 ms, then 1707 ms
slow rollup SQL: usage_rollup_totals upsert 4.970967625 s
Redis affinity breaker: 读取 Redis 会话绑定超过共享总期限 75ms
Redis capacity breaker: 占用 Redis 凭据并发槽超过共享总期限 75ms
429: 账号调度排队等待超时（Redis 调度协调状态不可用，waited_secs=5, max_wait_secs=5, retry_after_secs=1）
capacity breaker recovery: suppressed_requests=901086
```

This identifies a second-stage defect in r6: the bounded degraded wait still listened to ordinary capacity wakeups, so a Redis-breaker-open window could be woken repeatedly by unrelated capacity changes and produce a large internal retry loop without corresponding downstream RPM. The owned raw root, temporary PostgreSQL database `kiro_l5_rhltd6_9003_0`, and Redis prefix `kiro_l5_mrrhltd6_9003:kiro_l5_rhltd6_9003_0:` were deleted after extraction.

### Candidate r7

Candidate r7 adds a dedicated Redis-degraded recovery sleep for normal local requests. The degraded branch no longer waits on ordinary capacity signals, so capacity/stream/usage wakeups cannot spin the request loop while the Redis capacity breaker is open. A focused real-Redis test was updated with a 1ms noisy notifier and passed, asserting the suppressed count stays bounded.

```text
/tmp/kiro-frozen-20260719-r7/kiro-rs
sha256 58f465f57b4c5d183338aa823a90828cecf1eb74289bd9bb8153dc801238bba5

KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:50892/0 \
RUSTUP_TOOLCHAIN=1.92.0 \
feature/tests/run-cargo-scoped.sh scheduler-degraded-sleep-redis-20260719-r1 -- \
  cargo test redis_backed_in_flight_limit_does_not_fail_open_while_degraded -- --nocapture

result: 1 passed / 0 failed
scope cleanup: size_kib=1697416 removed=true reservation_released=true

RUSTUP_TOOLCHAIN=1.92.0 \
feature/tests/run-cargo-scoped.sh scheduler-degraded-sleep-fmt-check-20260719-r1 -- \
  bash -lc 'cargo fmt --check && cargo check -q --all-targets'

result: pass
scope cleanup: size_kib=446948 removed=true reservation_released=true
```

r7 L5 300-second keep-raw passed:

```text
/tmp/kiro-l5-300s-r7-keepraw-20260719-r1.json
passed true
rawRoot /var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-l5-load-chaos-4ho8H5
```

| Case | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | RSS start→idle | FD start→idle | Pass |
| --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- | --- |
| `l5_long_stream_soak_300s_c20` | 2121 | `{"200":2121}` | 2121 / 0 | 961 | 961 | 3281 | 29,573,120→60,063,744 | 31→29 | yes |
| `l5_post_soak_recovery_normal_c3_r12` | 12 | `{"200":12}` | 12 / 0 | 24 | 24 | 41 | 60,063,744→68,157,440 | 28→31 | yes |

Raw proxy-log counts before deletion:

```text
redis_degraded=0
capacity_timeout=0
affinity_read_timeout=0
queue_timeout=0
returned_429=0
local_all_disabled=0
pg_usage_slow=4
pg_sql_slow=0
usage_retry=0
suppressed=0
capacity admitted=2133 failures=0 suppressed=0
snapshot admitted=109 failures=0 suppressed=0
affinity admitted=6399 failures=0 suppressed=0
proxy_log_sha256=a55ac3fb8ddb3df69e2d5f21accca58d4eea51f89db5b6bfa76f3e16fc1eb56c
long_report_sha256=e4fbddaf4674bc2b52b6a31b43dadfa3d2125cf24608051bc05ee4f1b640883b
recovery_report_sha256=e8c00f58fd85ba601799df2d5e76261ee9e3aac9e1dcc241a9740a41de78bc26
```

The run still showed PgSQL usage slow writes up to 1688 ms, so usage writer latency remains an observability/performance item. It no longer propagated into Redis scheduler breaker failure in this 300-second run. The owned raw root was deleted after extraction.

r7 later failed the 900-second gate and was replaced by r8.

### r7 900-second rerun stopped after early red evidence

Run command used candidate r7, the same frozen loadtest binary, and the same isolated PostgreSQL/Redis pair:

```text
/tmp/kiro-l5-900s-r7-final-keepraw-20260719-r1.json
rawRoot /var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-l5-load-chaos-skwUO0
```

The run was manually stopped after roughly 2-3 minutes because the release-blocking condition appeared again. Extracted redacted proxy-log counts before deletion:

```text
redis_degraded=22
capacity_slot_timeout=6
affinity_read_timeout=1
affinity_write_timeout=3
affinity_cleanup_timeout=3
queue_timeout=22
returned_429=11
local_all_disabled=0
pg_usage_slow=16
pg_slow_statement=0
usage_record_timeout=0
breaker_recovery=12
proxy_log_sha256=a0040f7350b9f59fc5017c0891550f20ad41ed161d8fa7d83cb2216d43b4f073
```

The important difference from r6 is that r7 removed the internal retry spin but still let a single capacity hot-path timeout open the capacity breaker. Once the breaker opened, normal requests could still wait until the 5-second dispatch deadline and return 429. This kept the production-class failure alive, only with much lower internal amplification.

The owned raw root was deleted after extraction. The runner had already cleaned the temporary database and Redis prefix:

```text
PostgreSQL database kiro_l5_riz8b0_2739_0 absent
Redis prefix kiro_l5_mrriz8b0_2739:kiro_l5_riz8b0_2739_0: remaining keys 0
```

### Candidate r8

Candidate r8 changes the scheduler Redis breaker policy:

- capacity/queue hot-path timeout is raised from 75 ms to 250 ms;
- affinity/sticky operations keep a separate 75 ms timeout and a separate breaker;
- capacity timeout failures must be consecutive before opening the capacity breaker;
- a successful capacity operation resets the timeout streak;
- failure logs now report the actual operation budget instead of always printing the old 75 ms value.

This is intentionally narrower than fail-open: Redis capacity acquire still fails the current request closed when its own bounded deadline is exceeded, but one transient 75-250 ms scheduling/Redis jitter no longer freezes the whole local pool.

Focused validation:

```text
RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh scheduler-timeout-r8-fmt-check-20260719-r2 -- bash -lc 'cargo fmt --check && cargo check -q --all-targets'
result: pass
scope cleanup: size_kib=447616 removed=true reservation_released=true

KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:50892/0 RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh scheduler-timeout-r8-focused-redis-backedinflight-20260719-r1 -- cargo test redis_backed_in_flight_limit_does_not_fail_open_while_degraded -- --nocapture
result: 1 passed / 0 failed
scope cleanup: size_kib=1695752 removed=true reservation_released=true

KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:50892/0 RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh scheduler-timeout-r8-focused-scheduler-redis-20260719-r1 -- cargo test scheduler_redis_ -- --nocapture
result: 5 passed / 0 failed
scope cleanup: size_kib=1693952 removed=true reservation_released=true

RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh scheduler-timeout-r8-capacity-threshold-20260719-r1 -- cargo test scheduler_redis_capacity_ -- --nocapture
result: 2 passed / 0 failed
scope cleanup: size_kib=1698140 removed=true reservation_released=true

RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh scheduler-timeout-r8-fmt-check-20260719-r3 -- bash -lc 'cargo fmt --check && cargo check -q --all-targets'
result: pass
scope cleanup: size_kib=446948 removed=true reservation_released=true

RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh frozen-product-20260719-r8 -- bash -lc 'cargo build --release --locked --bin kiro-rs && cp "$CARGO_TARGET_DIR/release/kiro-rs" /tmp/kiro-frozen-20260719-r8/kiro-rs && shasum -a 256 /tmp/kiro-frozen-20260719-r8/kiro-rs'
result: sha256 131696bd81e1cdaeceaac6a45f9c76bf698eb559785b379a82fd77e2f742e631
scope cleanup: size_kib=788936 removed=true reservation_released=true
```

The Toxiproxy latency-injection tests were compiled but skipped because this local environment did not provide `KIRO_RS_TEST_TOXIPROXY_API` / `KIRO_RS_TEST_TOXIPROXY_NAME`. They are not counted as dynamic pass evidence.

#### r8 300-second L5 diagnostic

Summary:

```text
/tmp/kiro-l5-300s-r8-keepraw-20260719-r1.json
passed true
rawRoot /var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-l5-load-chaos-r8nKcJ
```

| Case | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | RSS start→idle | FD start→idle | Pass |
| --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- | --- |
| `l5_long_stream_soak_300s_c20` | 2281 | `{"200":2281}` | 2281 / 0 | 388 | 388 | 2727 | 30,720,000→19,103,744 | 30→28 | yes |
| `l5_post_soak_recovery_normal_c3_r12` | 12 | `{"200":12}` | 12 / 0 | 11 | 11 | 14 | 19,103,744→40,157,184 | 28→31 | yes |

Raw proxy-log counts before deletion:

```text
redis_degraded=0
capacity_timeout=0
queue_timeout=0
returned_429=0
local_all_disabled=0
pg_usage_slow=2
pg_sql_slow=0
usage_retry=0
suppressed=0
proxy_log_sha256=176dc8da46f2a9252d6c8fd9b1b8864ad250febcdc600c753b9a4f6d2f6f2685
long_report_sha256=2d53eaaad2364d27bc5f8fcf03b1fd7db7593ab7bd75752c1a69b430612ef9d2
recovery_report_sha256=e6c4882a2afdc3f8e3b2f8d3da7953fa19bcc7d2f96c5811e04ae997bdc35957
```

The owned raw root was deleted after extraction.

#### r8 900-second final L5 soak

Summary:

```text
/tmp/kiro-l5-900s-r8-final-keepraw-20260719-r1.json
passed true
rawRoot /var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-l5-load-chaos-4nV05E
```

| Case | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | RSS start→idle | FD start→idle | Pass |
| --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- | --- |
| `l5_long_stream_soak_900s_c20` | 6821 | `{"200":6821}` | 6821 / 0 | 354 | 355 | 2724 | 28,606,464→21,200,896 | 31→28 | yes |
| `l5_post_soak_recovery_normal_c3_r12` | 12 | `{"200":12}` | 12 / 0 | 11 | 11 | 15 | 21,200,896→48,971,776 | 28→31 | yes |

Raw proxy-log counts before deletion:

```text
redis_degraded=0
capacity_timeout=0
capacity_timeout_not_opened=0
queue_timeout=0
returned_429=0
local_all_disabled=0
pg_usage_slow=0
pg_sql_slow=0
usage_retry=0
capacity admitted=6833 failures=0 suppressed=0
snapshot admitted=343 failures=0 suppressed=0
affinity admitted=20451 failures=2 suppressed=0
proxy_log_sha256=f254f17e1e5bcf10f85fbd063b97762f956da135856965984fb3bbdabb825774
long_report_sha256=6a16c13d6a992a0cfe4af1f1c35d02759ee755061aed490d0eb25c49406a2537
recovery_report_sha256=1084302f2422cd42cea5b08dcb7ff33d7000dfcbc18f57115750082a7fb5d602
```

The affinity breaker failures were sticky/session cleanup operations at the retained 75 ms budget and recovered without affecting local capacity admission. Capacity breaker failures stayed at zero for the whole 900-second soak. The owned raw root was deleted after extraction.

#### r8 L3 burst/recovery regression

Summary:

```text
/tmp/kiro-l3-r8-load-chaos-summary-20260719-r1.json
passed true
resultCount 9
product sha256 131696bd81e1cdaeceaac6a45f9c76bf698eb559785b379a82fd77e2f742e631
loadtest sha256 23c04221deb72dde601d491452d8cc9a99211df99b2cd39a386272141f2db8e3
```

| Case | Expect | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | FD start→peak→end | Pass |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- |
| `l3_normal_c1_r5` | success | 5 | `{"200":5}` | 5 / 0 | 24 | 24 | 29 | 30→30→30 | yes |
| `l3_normal_c5_r20` | success | 20 | `{"200":20}` | 20 / 0 | 14 | 14 | 18 | 29→34→34 | yes |
| `l3_normal_c10_r50` | success | 50 | `{"200":50}` | 50 / 0 | 22 | 22 | 25 | 29→39→39 | yes |
| `l3_spike_c40_r100` | success | 100 | `{"200":100}` | 100 / 0 | 34 | 34 | 40 | 29→69→69 | yes |
| `l3_recovery_after_spike_c3_r10` | success | 10 | `{"200":10}` | 10 / 0 | 5 | 5 | 7 | 29→32→32 | yes |
| `l3_recovery_after_error_burst_c12_r40` | recovered | 40 | `{"200":5,"429":28,"502":7}` | 5 / 35 | 18 | 18 | 25 | 30→41→41 | yes |
| `l3_post_error_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 49 | 49 | 50 | 29→32→32 | yes |
| `l3_invalid_tool_burst_c20_r40` | error | 40 | `{"502":40}` | 0 / 40 | 27 | 0 | 27 | 30→49→49 | yes |
| `l3_invalid_tool_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 22 | 22 | 23 | 29→32→32 | yes |

The r8 L3 pass keeps the same fake-upstream burst/error/recovery coverage after the scheduler timeout change. It closes the pending r8 L3 regression gate for this frozen candidate.

#### r8 L4 restart/failure chaos regression

Summary:

```text
/tmp/kiro-l4-r8-load-chaos-summary-20260719-r1.json
passed true
resultCount 12
product sha256 131696bd81e1cdaeceaac6a45f9c76bf698eb559785b379a82fd77e2f742e631
loadtest sha256 23c04221deb72dde601d491452d8cc9a99211df99b2cd39a386272141f2db8e3
```

| Case | Expect | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first text ms | p95 total ms | FD start→peak→end | Pass |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | --- | --- |
| `l4_proxy_restart_during_long_stream` | any | 80 | `{"200":8,"transport_error":72}` | 8 / 72 | 536 | 537 | 5014 | 30→45→26 | yes |
| `l4_proxy_restart_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 19 | 19 | 22 | 29→32→32 | yes |
| `l4_rate_limit_burst_c20_r40` | error | 40 | `{"429":40}` | 0 / 40 | 59 | 0 | 59 | 30→49→49 | yes |
| `l4_rate_limit_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 33 | 33 | 34 | 29→32→32 | yes |
| `l4_server_error_burst_c20_r40` | error | 40 | `{"429":20,"502":20}` | 0 / 40 | 32 | 0 | 32 | 30→49→49 | yes |
| `l4_server_error_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 17 | 18 | 23 | 29→32→32 | yes |
| `l4_invalid_tool_burst_c20_r40` | error | 40 | `{"502":40}` | 0 / 40 | 39 | 0 | 39 | 30→50→49 | yes |
| `l4_invalid_tool_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 20 | 20 | 31 | 29→32→32 | yes |
| `l4_client_drop_c20_r40` | error | 40 | `{"200":40}` | 0 / 40 | 34 | 0 | 34 | 30→30→29 | yes |
| `l4_client_drop_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 9 | 9 | 10 | 29→32→32 | yes |
| `l4_mixed_chaos_c24_r96` | mixed | 96 | `{"200":28,"429":65,"502":3}` | 23 / 73 | 8050 | 343 | 8050 | 30→66→52 | yes |
| `l4_mixed_chaos_recovery_normal_c3_r12` | success | 12 | `{"200":12}` | 12 / 0 | 41 | 41 | 43 | 28→31→31 | yes |

The r8 L4 pass closes the pending restart/failure/client-drop/mixed-chaos regression gate for this frozen candidate. The mixed-chaos case intentionally includes expected 429/502 responses and is judged by bounded behavior plus recovery.

## Current release impact

Release remains `NO-GO` until these still-open gates pass:

- remaining real upstream and real Claude Code CLI long-session/tool/search/image/MCP gates;
- UI/browser, upgrade, two-instance/Redis-chaos and final build-artifact inventory gates.
