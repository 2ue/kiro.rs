# Redis 两实例协调门禁

Date: 2026-07-20

Status: `pass / shared isolated Redis namespace / post-fix r3 regression pass / real service-process E03 separately passed`

## Scope

This gate verifies the Redis coordination contract across independent
`ConnectionManager` and `MultiTokenManager` instances. It is deliberately
separate from the single-process scheduler chaos runner and does not claim that
two real kiro.rs HTTP processes have been killed and restarted yet.

## Command and isolation

```text
KIRO_MULTI_INSTANCE_REDIS_URL=redis://127.0.0.1:26379/15 \
KIRO_RS_TEST_REDIS_ISOLATED=1 \
KIRO_MULTI_INSTANCE_REDIS_OUTER_ROUNDS=3 \
KIRO_MULTI_INSTANCE_REDIS_SCOPE=multi-instance-redis-coordination-r1 \
node feature/tests/run-multi-instance-redis-coordination-validation.mjs
```

The runner required an explicitly empty nonzero loopback DB, used the current
project's existing Redis at `26379`, did not start Docker, did not inspect or
touch the protected `9022` service, and removed its scoped Cargo target on exit.

## Result

The exact test ran in three outer rounds. Each invocation contains five
independent internal rounds, for `15/15` coordination rounds:

- four independent Redis connections and managers acquired the same account's
  leases without duplicate lease IDs;
- releasing one manager's lease never removed a peer manager's live lease;
- active leases were touched while held;
- a deliberately stale crash-instance lease expired and was removed before a
  fresh manager acquired capacity;
- two managers sharing a queue admitted exactly `4/16` requests under the
  configured queue bound;
- two managers sharing the RPM deadline produced eight unique reservations with
  the required spacing;
- every round ended with zero queue entries and zero live leases.

Observed output for each internal round was equivalent to:

```text
lease_ids_unique=true crash_ttl_recovered=true queue_admitted=4/16 rpm_reservations=8 final_leases=0
```

## 2026-07-21 post-fix regression

After the scheduler Redis deterministic response-error classification fix, the
same runner was rerun to make sure the `commit_unknown=false` path did not
weaken cross-manager lease/RPM coordination:

```text
KIRO_MULTI_INSTANCE_REDIS_URL=redis://127.0.0.1:26379/7
KIRO_RS_TEST_REDIS_ISOLATED=1
KIRO_MULTI_INSTANCE_REDIS_OUTER_ROUNDS=3
KIRO_MULTI_INSTANCE_REDIS_SCOPE=multi-instance-redis-coordination-20260721-r3
node feature/tests/run-multi-instance-redis-coordination-validation.mjs
```

Result: `3 outer × 5 internal = 15/15` passed. The observed contract remained
`lease_ids_unique=true crash_ttl_recovered=true queue_admitted=4/16
rpm_reservations=8 final_leases=0`. Scoped cleanup reported
`size_kib=1708432 removed=true reservation_released=true`.

## Cleanup

```text
outer_rounds=3
internal_rounds_per_invocation=5
total_coordination_rounds=15
redis_database=15 for initial run; 7 for post-fix r3 regression
databaseEmpty=true
childGroupsStopped=true
tempRemoved=true
scoped_target_size_kib=1716248
removed=true
reservation_released=true
```

The runner's independent Node contract passed `13/13` pure early-rejection
cases with the live case explicitly skipped, then `14/14` when the caller
provided current-project nonempty DB14. Missing URL, isolation values other than
the exact string `1`, DB0, and numeric port 9022 were rejected before Cargo.
The live nonempty check was read-only: DB14 was `10863` keys before and after;
DB15 remained `0`. No scoped target, Cargo process, or temp directory was
created by those rejected runs.

## Boundary

This closes the coordination-layer connection/manager contract. The real
service-process E03 gate is recorded separately in
[`e03-real-two-process-scheduler-runner-20260720.md`](e03-real-two-process-scheduler-runner-20260720.md)
and has also passed for scheduler/RPM. Token-refresh PostgreSQL CAS,
external-pool takeover, two-instance fault/fallback coordination, and
production-cardinality Redis contention remain separate release gates.
