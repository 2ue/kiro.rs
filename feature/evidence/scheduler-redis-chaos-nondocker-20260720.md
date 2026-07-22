# Scheduler Redis 非 Docker chaos 复核

Date: 2026-07-20

Status: `pass / current-project isolated Redis / latest accepted r6 / release blockers remain elsewhere`

## Harness

- `feature/tests/redis-chaos-proxy.mjs` is a loopback-only, test-owned TCP proxy
  with a small Toxiproxy-compatible control surface. It supports downstream
  latency, disable/re-enable, bounded control bodies, and signal cleanup.
- `feature/tests/run-scheduler-redis-chaos-validation.mjs` requires an explicit
  loopback Redis URL naming an empty nonzero DB (`1..15`) and
  `KIRO_RS_TEST_REDIS_ISOLATED=1`. It refuses port `9022`, refuses a nonempty
  database, runs each Cargo command through `run-cargo-scoped.sh`, and flushes
  only the confirmed empty DB after the run.
- This validation started no Docker container and never inspected or touched the
  existing `9022` process. DB15 was empty before and after; DB1/13/14 were left
  untouched because they contained pre-existing keys.

## Command and identity

```text
KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL=redis://127.0.0.1:26379/15 \
KIRO_RS_TEST_REDIS_ISOLATED=1 \
KIRO_SCHEDULER_CHAOS_OUTER_ROUNDS=3 \
node feature/tests/run-scheduler-redis-chaos-validation.mjs
```

Rust toolchain: `1.92.0`.

The 2026-07-20 accepted batch was `scheduler-redis-joint-fault-r4`; target size
`1704152 KiB`; the wrapper reported
`removed=true reservation_released=true`. The earlier `r2` result remains a
valid seven-test checkpoint, but `r4` superseded it because it also proved the
simultaneous usage-writer/scheduler fault matrix.

The latest accepted batch is the 2026-07-21 rerun
`scheduler-redis-joint-chaos-20260721-r6`, run on
`redis://127.0.0.1:26379/5` with the same non-Docker loopback proxy contract.
It supersedes `r4` after a real red/green found in `r5`:

- `scheduler-redis-joint-chaos-20260721-r5` on
  `redis://127.0.0.1:26379/4` failed before the fix in outer round 2:
  `wrongtype-round-2: recovery 1/5 failed: 本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=4）`.
- Root cause: scheduler Redis hot-path errors after `arm_redis_commit_unknown()`
  were all treated as commit-unknown. That was correct for timeout/connection
  uncertainty, but wrong for deterministic Redis response/type errors such as
  `WRONGTYPE`, where no lease was created. The over-conservative classification
  enqueued unnecessary release/tombstone reconciliation, adding Redis writes and
  competing with breaker half-open recovery.
- Fix: `SchedulerRedisExecutionOutcome::Failed` and
  `SchedulerRedisHotOutcome::Failed` now carry `commit_unknown`; deterministic
  Redis response/type/script/server errors are `commit_unknown=false`, while
  timeout/connection dropped/I/O and unknown non-Redis failures remain
  conservative. Initial in-flight and dispatch queue acquire paths call
  `confirm_redis_not_acquired()` for deterministic failures and avoid release
  enqueue.
- `scheduler-redis-joint-chaos-20260721-r6` then passed `8 exact × 3 outer`,
  i.e. `24/24`, including WRONGTYPE recovery. Cleanup reported
  `databaseEmpty=true`, `childGroupsStopped=true`, `portsReleased=true`,
  `tempRemoved=true`; scoped target cleanup was `size_kib=1710316
  removed=true reservation_released=true`.

## Matrix

Eight exact tests ran in each of three outer rounds (`24` exact invocations):

1. affinity 500ms latency does not degrade the capacity breaker;
2. capacity 50/500ms boundary and recovery matrix (three rounds per latency);
3. consecutive timeout breaker opens only after the configured threshold and
   never reports `AllDisabled`;
4. 300 lease releases remain non-blocking and drain after latency;
5. proxy disconnect/re-enable recovers the same manager;
6. usage writer and scheduler share the same fault window across latency,
   WRONGTYPE, disconnect and recovery without retry amplification;
7. cancelled provisional acquire rolls back local and remote state;
8. commit-unknown provisional acquire leaves no lease.

All 24 invocations passed. The 50ms capacity cases succeeded; 500ms cases
failed closed around the 250ms hot-path deadline, then recovered. The affinity
case left capacity coordination healthy. Disconnect tests accepted both valid
transport shapes: an immediate transport failure opens the breaker directly;
timeouts require the configured consecutive threshold. In either case the
public route never became `local_all_disabled`.

The new joint-fault exact test has three internal rounds, so each fault point
was exercised nine times across the accepted three outer rounds. Its measured
contracts were:

- at `25/50/74/75/90/150ms`, all 16 usage records per scenario succeeded and
  each record performed exactly one Redis write RTT; scheduler acquire/release
  completed in approximately `26..183ms` and did not open the breaker;
- at `500ms`, exactly the configured three scheduler Redis operations failed
  before the breaker opened. The following 128 scheduler calls failed fast
  locally, with no increase in Redis admitted/failure counters;
- scheduler-key WRONGTYPE produced one coordination failure, never
  `AllDisabled`, while usage writes remained independent and one RTT each;
- disconnect was injected only after a usage RTT was confirmed in flight. Each
  eight-record wave completed with bounded success/error accounting (normally
  seven recovered writes and one disconnected write), with no hidden retry;
- after each hard fault was removed, five consecutive scheduler operations
  succeeded, queue depth returned to zero, and no credential was falsely
  disabled.

## Resource and cleanup result

```text
result=pass
outer_rounds=3
exact_tests=8
exact_invocations=24
redis_database=5 for latest r6; DB15 for earlier accepted r4
databaseEmpty=true
childGroupsStopped=true
portsReleased=true
tempRemoved=true
joint_fault_internal_rounds_per_outer=3
joint_fault_recoveries_per_hard_fault=5/5
joint_fault_rss_delta_about_mib=10..12
joint_fault_fd_delta=4
```

No repository Cargo target remained from the scoped batch. The editor may
recreate an unrelated root `target/debug` later; that directory is not evidence
from this runner and must be rechecked before release.

## Test-contract corrections discovered

The development sequence exposed test-contract and orchestration defects, all
fixed before the accepted `r4` run:

- after the third consecutive capacity timeout, exponential breaker backoff is
  approximately 6--8 seconds, not the base 2 seconds; recovery now waits for the
  actual remaining duration instead of prematurely declaring a failure;
- a proxy disconnect can surface as either an immediate transport error or a
  shared-deadline timeout through `ConnectionManager`; the test now requires
  fail-closed behavior and eventual breaker opening within the configured
  threshold, without weakening the `AllDisabled` prohibition.
- the first dedicated signal-contract run found that the Node proxy closed its
  two servers on `SIGTERM` but retained an unresolved top-level lifetime
  promise; the parent therefore waited for its five-second grace period and
  could enter the kill fallback. The proxy lifetime now resolves only after
  both owned servers close, and the parent uses bounded, race-safe exit waits
  for both TERM and KILL. This is harness lifecycle correctness; it does not
  alter scheduler production behavior.
- joint-fault `r1` did an immediate healthy preflight after a deliberate 500ms
  timeout while the breaker was still in its specified backoff; that redundant
  assertion was removed rather than reducing the production backoff.
- after `r2` passed and `r3` repeated it, inspection found that the disconnect
  branch could disable the proxy before a usage write had actually entered the
  delayed Redis RTT. `r4` installs latency first, releases the writer barrier,
  observes a write RTT in flight, and only then disconnects. Only `r4` is used
  as the simultaneous-fault evidence.

These changes preserve the production backoff and threshold policy; they do not
reduce protection to make the test green.

## Negative and signal contract

`feature/tests/run-scheduler-redis-chaos-validation.contract.test.mjs` keeps
live Redis checks opt-in. Without live URLs it ran 16/16 pure Node cases and
skipped 12 live cases. With these caller-confirmed current-project databases:

```text
KIRO_SCHEDULER_CHAOS_CONTRACT_EMPTY_REDIS_URL=redis://127.0.0.1:26379/15
KIRO_SCHEDULER_CHAOS_CONTRACT_NONEMPTY_REDIS_URL=redis://127.0.0.1:26379/14
node --test feature/tests/run-scheduler-redis-chaos-validation.contract.test.mjs
```

the complete contract passed 28/28:

- missing direct URL: 3/3 rejected before proxy/Cargo;
- isolation marker `0` and textual `true`: 6/6 rejected before proxy/Cargo;
- DB0: 3/3 rejected before proxy/Cargo;
- numeric port 9022: 3/3 rejected without inspecting its listener;
- nonempty DB14: 3/3 rejected before Cargo, with its key count unchanged at
  10863 before and after every round;
- SIGHUP/SIGINT/SIGTERM: 3 rounds each, with exit codes 129/130/143, the exact
  owned proxy PID on both ephemeral ports before the signal, and zero owned
  listeners/TEMP_ROOT/ready file afterward;
- DB15 returned to 0 keys after every signal round.

The contract invokes no Cargo command, creates no scoped target/reservation,
does not start Docker, and does not probe the existing 9022 listener. The final
residue check found no `kiro-scheduler-chaos-*`/`kiro-chaos-contract-*` temp
directory and no `redis-chaos-proxy` or scheduler-chaos runner process. On this
macOS host the final post-hardening live rerun took 126.3 seconds because repeated `lsof`
ownership/release checks take roughly 1.5 seconds each; that is harness wall
time, not scheduler or Redis request latency.

## Remaining scope

This closes the non-Docker single-instance Redis latency/disconnect/recovery,
lease-cleanup, and simultaneous usage-writer/scheduler fault matrix represented
above. It also closes the deterministic Redis response-error
commit-unknown-amplification bug found in `r5`. It does not close
multi-instance Redis/PG fencing, external-pool takeover attribution,
production-cardinality Redis contention, official Kiro upstream, UI/browser,
upgrade, or final release inventory.
