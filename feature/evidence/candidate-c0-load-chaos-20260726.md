# Candidate C0 load/chaos validation - 2026-07-26

## Scope

This evidence covers the current release candidate binary after the production-issue fixes for:

- local scheduler / usage writer isolation from non-core Redis and PostgreSQL work;
- recovery after sudden upstream error bursts;
- bounded resource use under burst, chaos, and soak traffic;
- invalid tool / malformed upstream response recovery;
- no Cargo target accumulation during validation.

The run used fake local upstreams only. It did not send load to production and did not consume real accounts.

## Candidate

- Product binary: `/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-cli-candidate.c0-20260726035013.8MWRn4/kiro-rs`
- Product SHA-256: `7268b3e722f03a40179d205e7b5917b86d696cd8bf1d5f6533d3b1347ea30bec`
- Load runner binary: `/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-final-candidate.current.ITUNUQ/kiro_loadtest`
- Load runner SHA-256: `9bcfd4fe05f5ee476664c626dbb97cb3abf95f77cdee056d67ce8227eaea3654`

## Local dependencies

- PostgreSQL: local Docker container `kiro-rs-postgres-local`, port `25432`.
- Redis: local Docker container `kiro-rs-redis-local`, port `26379`, DB `15`.
- Temporary PostgreSQL databases used a `kiro_load_chaos_1785011475_*` prefix and were dropped after the run.
- Redis test prefixes were removed by the runner.
- The active local service on `9022` was not load-tested.

## L3 burst and recovery

Overall result: pass.

| Scenario | Result | Status distribution | p95 TTFB | p95 latency | Resource peak |
|---|---:|---|---:|---:|---|
| `l3_normal_c1_r5` | 5/5 success | 200=5 | 11 ms | 24 ms | RSS 37.7 MB / FD 31 |
| `l3_normal_c5_r20` | 20/20 success | 200=20 | 12 ms | 48 ms | RSS 52.5 MB / FD 37 |
| `l3_normal_c10_r50` | 50/50 success | 200=50 | 21 ms | 44 ms | RSS 69.0 MB / FD 41 |
| `l3_spike_c40_r100` | 100/100 success | 200=100 | 36 ms | 47 ms | RSS 121.7 MB / FD 72 |
| `l3_recovery_after_spike_c3_r10` | 10/10 success | 200=10 | 7 ms | 8 ms | recovered |
| `l3_recovery_after_error_burst_c12_r40` | expected mixed | 200=5, 429=28, 502=7 | 24 ms | 46 ms | bounded |
| `l3_post_error_recovery_normal_c3_r12` | 12/12 success | 200=12 | low | low | recovered |
| `l3_invalid_tool_burst_c20_r40` | expected errors | 502=40 | bounded | bounded | bounded |
| `l3_invalid_tool_recovery_normal_c3_r12` | 12/12 success | 200=12 | low | low | recovered |

Interpretation: a sudden success spike, an invalid-tool burst, and a mixed error burst did not leave the candidate in a stuck scheduler/resource state. Later normal traffic recovered.

## L4 chaos

Overall result: pass.

Observed scenarios:

- proxy restart during long stream: 8 successful completions and 72 expected transport errors; recovery traffic passed 12/12;
- rate-limit burst: 40/40 returned 429 as expected; recovery passed 12/12;
- server-error burst: 40 expected errors split across 429/502; recovery passed 12/12;
- invalid-tool burst: 40/40 returned 502 as expected; recovery passed 12/12;
- client disconnect burst: 40 dropped-client paths were cleaned up; recovery passed 12/12;
- mixed chaos: 30 success / 66 expected errors, status distribution 200=30, 429=63, 502=3; p95 TTFB and latency were about 8314 ms during chaos, then the recovery scenario passed 12/12.

Interpretation: high-error phases did not permanently poison account scheduling, did not keep sockets/tasks stuck, and did not prevent post-chaos normal traffic from completing.

## L5 soak

Overall result: pass.

| Scenario | Result | p95 TTFB | p95 latency | Resource peak | Idle recovery |
|---|---:|---:|---:|---|---|
| `l5_long_stream_soak_60s_c20` | 421/421 success | 952 ms | 3220 ms | RSS 87.9 MB / FD 72 | checked |
| `l5_post_soak_recovery_normal_c3_r12` | 12/12 success | 11 ms | 19 ms | recovered | pass |

Idle recovery:

- `rssReturnedWithin32MiB=true`
- `fdReturnedWithin5=true`
- idle RSS: 57.7 MB
- idle FD: 32

Interpretation: after one minute of sustained long streaming traffic, RSS and FD counts returned near baseline and normal post-soak traffic stayed low-latency.

## Disk/artifact gate

Post-run artifact inventory:

```text
build-artifact-inventory version=2 mode=read-only targets=0 reservations=0 target_processes=0 blockers=0
release-gate result=pass
```

No unmanaged Cargo `target/` directory remained after this load/chaos validation.

## Cleanup status

- Temporary PostgreSQL databases: dropped.
- Temporary Redis keys/prefixes: runner-cleaned.
- Load runner and fake upstream processes: stopped.
- Raw result directory: removed after this evidence file was written.

## Result

Pass for fake-upstream L3/L4/L5 scheduler/resource validation. This is not a replacement for the real Claude Code CLI protocol validation and small real-upstream smoke; those remain separate evidence gates.
