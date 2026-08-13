# Token Refresh Failure Wave And Cluster RPM Evidence

Status: `focused-evidence / process-local-final-attempt-isolated-redis-and-cluster-pg-redis-pass / provider-load-and-release-gates-pending / NO-GO`

Date: 2026-07-17

Issue authority: [Token Refresh Failure Wave And Cluster RPM](../issues/token-refresh-failure-wave-and-cluster-rpm.md)

## Evidence Boundary

This record separates observed red baselines, source-level implementation, executed focused checks, and work that has not run. It is not release evidence and is not tied to a frozen candidate binary.

No production service, credential file, OAuth secret, bearer token, refresh token, request body, or upstream response body is included.

## Observed Red Baselines

| Case | Observed result | Meaning |
| --- | --- | --- |
| Short-TTL shared-manager recovery | 16 callers, 30 refresh HTTP sends | A six-minute token was immediately refreshable under the wider warning window; mutex serialization did not provide positive-result reuse |
| Timeout failure wave | 32 callers could produce 32 refresh sends | Mutex serialization did not share the leader's typed failure |
| Invalid-bearer request path | `N` concurrent request owners could each enter unconditional force refresh | Per-request deduplication did not aggregate the credential generation across callers |
| No-Redis capacity/admission | Earlier capacity-focused run was red due to a local guard misclassification | Historical red baseline；后续 `queue-refresh-integration-r3` 已通过，不再是当前阻断结论 |
| Cross-instance fast 500 failure | Two managers could produce 2 OAuth refresh hits for one fast 500 wave | Redis waiter polling at 500 ms could outlive the jittered 400-500 ms negative replay window |

The first two numbers are narrow reproduction facts, not production-frequency estimates. The invalid-bearer `N` is the structural fan-out of the old per-request force-refresh path, not a claimed measured production count.

## Implemented Source Surfaces Seen In The Dirty Tree

- Request-scoped auxiliary attempt accounting and a process-wide auxiliary concurrency controller.
- Five-minute request token usability separated from ten-minute warning semantics.
- A per-credential typed local negative-result state.
- A conditional invalid-bearer recovery entrypoint separate from Admin force refresh.
- Closed Redis refresh leader/wait/replay/success/failure and health-claim wire representations.
- A refresh-specific token bucket with defaults `tokenRefreshMaxRpm=60` and `tokenRefreshBurst=8`, using Redis-global authority when configured and process-local authority otherwise.
- Low-cardinality refresh failure and admission errors that do not retain OAuth response bodies or credential material.

These are source observations only. Several surfaces were being integrated concurrently when this record was written; their presence cannot be converted into a pass.

## Executed Focused Results

| Batch | Result | Accepted scope | Explicit non-claim |
| --- | --- | --- | --- |
| `focused-r3` formatting/check | pass | Rust formatting and type/check gate for that dirty-tree batch | Not full tests, live Redis, provider protocol, load, or release evidence |
| Four Redis refresh protocol tests | pass | Closed decode, malformed-wire rejection, bounded backoff, secret-free bounded contract | Does not prove manager/provider integration or cross-instance behavior |
| Historical refresh capacity-focused test | red | Exposed a local guard/process-local classification problem | Superseded by the post-correction focused batch below；不作为当前 red |
| `queue-refresh-integration-r3` | pass | process-local 60/8、limit/config、revision fence、API/MCP final-attempt zero-refresh；每项内部 5 轮 | 不证明 live Redis、两实例、PG CAS、取消、chaos 或 frozen candidate |

The four passing protocol tests are:

```text
token_refresh_coordination_closed_decode_is_stable_for_five_rounds
token_refresh_coordination_rejects_malformed_wire_for_five_rounds
token_refresh_failure_backoff_is_bounded_and_deterministic_for_five_rounds
token_refresh_redis_contract_stays_secret_free_and_bounded_for_five_rounds
```

No command transcript, scoped target manifest, or frozen-binary hash is attached to this partial record. Before release, the owning validation run must bind exact commands, Rust 1.92 toolchain, source manifest, target cleanup, and binary identity.

### Post-correction focused batch

```text
scope=queue-refresh-integration-r3
toolchain=1.92.0
wall=231.2s
nine exact-name filters: each running 1 / passed 1
each filter: five internal rounds
cargo fmt --all -- --check: pass
cargo check --all-targets: pass
size_kib=2016724
removed=true
reservation_released=true
```

Refresh-specific results:

- process-local `60 RPM / burst 8`：每轮 128 reservations，严格 8 admitted、120 rate-limited，5 轮；authority 恒为 `ProcessLocal`。
- limit update：5 轮证明切换到 60/8 不追溯补 token；startup/runtime 非法 0 值 fail-fast，原配置不被部分覆盖。
- API 与 MCP final attempt：各 5 轮真实 fake HTTP，逐轮 inference hit=1、OAuth refresh hit=0、auxiliary consumed=0。
- metadata-only revision advance：5 轮保持 rejected token context current，不误称 recovered；token replacement 结束 recovery；revision regression fail closed。

## Static Review Findings Still Open At This Checkpoint

1. A storage-revision-only change with the same rejected access token could be treated as `CredentialChanged`, retried, and then blocked from its one recovery opportunity.
2. The pre-integration path could acquire a distributed lock and check only stale local credentials before sending; PostgreSQL authoritative access-token fencing was required.
3. Refresh-field CAS did not inherently fence a non-rotating refresh-token response by old access-token generation.
4. Health-action ownership could be lost if cancellation occurred after claim but before provider mutation.
5. Automatic recovery could run on the last inference/MCP attempt even though no retry send remained.
6. A process-local failure result alone cannot stop one leader per replica. The latest two-instance Redis/PostgreSQL gate now proves the normal cluster success/failure/health-claim path, but token-refresh-specific Redis slow/error/restart remains open.

The implementation was changing after this review. Each item must be re-reviewed against the settled source and then dynamically tested; this list does not assert that every item still exists in the latest unsaved editor state.

## Required Next Evidence

- 隔离 Redis gate 已于 2026-07-19 和 2026-07-20 两次完成；两实例 PostgreSQL/Redis cluster gate 已于 2026-07-22 完成；下一步是 token-refresh-specific Redis slow/error/restart 与 frozen provider/load。
- API and MCP invalid-bearer c1/c8/c32, five rounds each, with fake inference/MCP and fake OAuth hit accounting.
- Last-inference-attempt and auxiliary-budget-exhaustion zero-refresh contracts.
- Revision-only same-token and access-token-changed contracts.
- Token-refresh-specific Redis slow/error/restart and recovery.
- Cancellation at pre-send, committed send, result publication, Redis unlock, health claim, and post-mutation acknowledgement.
- 1/20/60 credential and c1/c8/c32 load/resource matrix, followed by the same gates on one repository-external frozen release binary.
- Public/debug/usage/attempt log scan using unique fake markers, proving no token, body, client secret, proxy secret, endpoint, or identity hash leakage.

## Release Decision

`NO-GO`. Process-local capacity、final-attempt、live isolated Redis 与 two-manager PostgreSQL/Redis cluster gate 已从 red/pending 收敛为 focused pass，但完整 invalid-bearer provider/load/chaos、token-refresh-specific Redis slow/error/restart、真实 upstream/Claude CLI native capability 和 frozen-candidate 门禁仍 pending。

## Redis Runner Preflight Evidence

`feature/tests/run-token-refresh-redis-validation.sh` 的 `bash -n` 通过。以下五个 Cargo 前拒绝分支均返回 64：缺 Redis URL、`KIRO_RS_TEST_REDIS_ISOLATED!=1`、outer rounds 为 0、URL 指向受保护 9022、非 loopback 且未 opt-in。

这段 preflight 运行发生在尚未提供隔离 Redis 的阶段：五分支执行前后均没有 `target/.validation-build-token-refresh*` 或 Git-common reservation；根 target 保持约 708 MiB，未产生本 runner 构建产物。当时五个 Redis 状态机测试只能算编译覆盖；该限制后来由 2026-07-19 和下节 2026-07-20 的真实隔离 Redis 动态结果取代。

## 2026-07-20 Redis + provider final-attempt revalidation

当前项目隔离 Redis `redis://127.0.0.1:26379/0` 上执行了单个 scoped build
batch `token-refresh-redis-provider-r1`。Rust 1.92.0 首次编译后，同一批次执行：

- `api_and_mcp_final_attempt_fixtures_do_not_start_oauth_refresh_for_five_rounds`
  一次；测试内部 API/MCP 各 5 轮，逐轮 inference/MCP send 严格为 1，
  OAuth refresh 与 auxiliary consumption 为 0；
- 五个真实 Redis exact tests 各 3 outer rounds，每个测试内部 5 轮，共
  15 个 Redis exact invocation/75 个内部场景轮次；
- leader election、failure replay/health claim/identity fence、stale leader
  overwrite fence、cancel-before-send immediate new leader、bucket dynamic TTL/
  refill/version switch 全部通过；
- `cargo fmt --all -- --check` 与 `git diff --check` 通过。

批次 wall `199.8s`，scoped target `size_kib=1698460`，退出时
`removed=true reservation_released=true`；对应 `/tmp` target 与 reservation
均不存在。fixture 使用随机 Redis prefix，并在 panic-safe finally 中 bounded
delete 后重新 SCAN 断言 namespace 为零。测试没有读取真实凭据、没有访问
或探测 `9022`，也没有产生仓库内 Cargo build evidence。

这次复核关闭“新 fixture 尚未真实执行”的缺口；它不等于两实例/PG CAS、
完整 invalid-bearer c1/c8/c32、Redis slow/error/restart、persistent usage
attribution 或 frozen release 通过。

## 2026-07-22 PostgreSQL/Redis cluster revalidation

Environment:

```text
KIRO_RS_TEST_POSTGRES_URL=postgres://kiro_rs:<redacted>@127.0.0.1:25433/kiro_rs
KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:26379/2
KIRO_RS_TEST_POSTGRES_ISOLATED=1
KIRO_RS_TEST_REDIS_ISOLATED=1
RUSTUP_TOOLCHAIN=1.92.0
dockerStarted=false
protected9022ProbeSkipped=true
```

Red reproduction before the final fix:

```text
scope=token-refresh-cluster
KIRO_TOKEN_REFRESH_CLUSTER_OUTER_ROUNDS=1
failed test:
  token_refresh_two_manager_failure_replay_and_cancelled_leader_recover_without_send_amplification_for_five_rounds
assertion:
  endpoint hits left=2 right=1
marker:
  failure-replay-1
cleanup:
  childGroupsStopped=true
  redisDatabaseEmpty=true
  redisDatabaseFlushed=false
  residualKeyCount=0
  tempRemoved=true
  scoped target removed=true
```

Root cause confirmed in source:

- `TOKEN_REFRESH_POLL_AFTER_MS` was 500 ms.
- First-wave Redis failure delay was `TOKEN_REFRESH_NEGATIVE_BACKOFF_BASE_MS * jitter`, where the first jitter range was 80-100% of 500 ms, i.e. 400-500 ms.
- A fast 500 leader could commit a failed outcome and expire the replay window before an existing waiter woke after 500 ms. The waiter then became a new leader and performed a second OAuth request for the same failure wave.

Code correction:

- Introduced `TOKEN_REFRESH_NEGATIVE_MIN_REPLAY_MS = TOKEN_REFRESH_POLL_AFTER_MS + 250`.
- `refresh_failure_delay` now floors all shareable Redis failure windows to that minimum, including tiny `Retry-After` values.
- `token_refresh_failure_backoff_is_bounded_and_deterministic_for_five_rounds` now asserts the replay window outlives waiter polling and that tiny Retry-After values still protect waiters.
- Cluster fixtures now provide a stable `machine_id` and assert that both managers compute the same refresh identity before exercising distributed coordination. This prevents test bootstrap metadata writes from obscuring CAS/revision assertions and locks the original identity-split regression.

Focused green checks:

```text
scope=failure-replay-fix
tests:
  storage::redis_cache::tests::token_refresh_failure_backoff_is_bounded_and_deterministic_for_five_rounds
  kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_failure_replay_and_cancelled_leader_recover_without_send_amplification_for_five_rounds
result:
  2/2 exact tests passed
scoped cleanup:
  size_kib=1713280
  removed=true
  reservation_released=true
```

Cluster smoke:

```text
scope=token-refresh-cluster
outerRounds=1
tests=7
internalRoundsPerTest=5
result=pass
redisDatabase=2
postgresDatabase=kiro_rs
cleanup.redisDatabaseEmpty=true
cleanup.tempRemoved=true
scoped cleanup:
  size_kib=1714304
  removed=true
  reservation_released=true
```

Default repeated cluster gate:

```text
scope=token-refresh-cluster
outerRounds=3
tests=7
internalRoundsPerTest=5
exact invocations=21
internal scenario rounds=105
result=pass
redisDatabase=2
postgresDatabase=kiro_rs
cleanup:
  childGroupsStopped=true
  redisDatabaseEmpty=true
  redisDatabaseFlushed=false
  residualKeyCount=0
  tempRemoved=true
scoped cleanup:
  size_kib=1715296
  removed=true
  reservation_released=true
```

The seven exact tests covered:

```text
kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_rotating_and_non_rotating_share_one_send_and_pg_authority_for_five_rounds
kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_pg_cas_fences_stale_rotating_and_non_rotating_results_for_five_rounds
kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_failure_replay_and_cancelled_leader_recover_without_send_amplification_for_five_rounds
kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_cancelled_health_claim_is_reclaimed_once_for_five_rounds
storage::postgres::tests::postgres_refresh_field_cas_fences_non_rotating_refresh_by_access_token_for_five_rounds
storage::redis_cache::tests::token_refresh_redis_stale_leader_cannot_overwrite_success_for_five_rounds
storage::redis_cache::tests::token_refresh_redis_failure_replay_health_claim_and_identity_are_fenced_for_five_rounds
```

Accepted claims:

- Two managers sharing Redis/PostgreSQL coalesce rotating and non-rotating refresh success into one OAuth send and use PostgreSQL authority.
- Direct PostgreSQL refresh-field CAS fences stale rotating and non-rotating results.
- Fast 500 failures no longer cause the waiter to become a second leader inside the same failure wave.
- Redis health-claim replay can be reclaimed once.
- Stale Redis leaders cannot overwrite a later success.
- The validation did not start Docker, did not probe or use port 9022, and left the selected Redis DB empty.

Non-claims:

- This is still not a frozen release-binary gate.
- It does not replace full automatic API/MCP invalid-bearer c1/c8/c32, real upstream, native Claude CLI capability, UI/browser, upgrade, or final release inventory validation.
- It does not prove token-refresh-specific Redis slow/error/restart; those remain separate chaos gates.
