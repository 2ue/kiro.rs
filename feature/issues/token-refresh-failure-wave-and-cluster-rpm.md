# Token Refresh Failure Wave And Cluster RPM

Status: `reproduced / process-local-isolated-redis-and-cluster-pg-redis-pass / provider-and-frozen-candidate-unverified / NO-GO`

Severity: P0

Evidence: [2026-07-17/20 focused and isolated Redis record](../evidence/token-refresh-failure-wave-and-cluster-rpm-20260717.md), [2026-07-18 process-local budget/cancellation record](../evidence/oauth-auxiliary-budget-and-cancellation-20260718.md), and [2026-07-19 isolated Redis storage validation](../evidence/storage-integration-and-artifact-gate-20260719.md)

## Scope And Authority

This document is the current authority for OAuth token-refresh amplification, invalid-bearer automatic recovery, single-process and cross-instance failure-wave coordination, refresh-specific RPM admission, and refresh health-action ownership. The broader request/inference/external retry ledger remains in [Retry Budget, Admission, And RPM Amplification](retry-budget-admission-and-rpm-amplification.md).

The source tree contains an active implementation, but it is not a frozen candidate and has not passed the matrix below. Static source presence, formatting/check success, or isolated Redis protocol tests must not be reported as an end-to-end fix.

## User-Visible And Operational Symptoms

- A downstream request rate that looks normal can coexist with a much higher OAuth token endpoint RPM. Inference upstream RPM can also remain low because amplification happens before an inference request is sent.
- A short-lived but valid refreshed token caused a shared-manager burst with 16 callers to produce 30 refresh HTTP sends. The trigger was a six-minute token being accepted by one five-minute check and immediately considered refreshable by a separate ten-minute check.
- In the timeout failure-wave reproduction, 32 callers reaching one expired credential could produce 32 refresh sends. The mutex serialized the sends but did not share the leader's failure.
- The old invalid-bearer path called the unconditional Admin force-refresh operation once per downstream request. A burst of `N` API or MCP 401/403 responses could therefore become `N` serialized OAuth refreshes for the same credential.
- A no-Redis deployment must use a process-local refresh bucket. It must not surface a distributed queue, `SchedulerRedisDegraded`, or Redis coordination failure. An earlier capacity run exposed a local guard misclassification; the correction has now passed the five-round 60/8 focused gate, while full no-Redis provider/load evidence remains pending.
- A two-replica Redis/PostgreSQL deployment had a separate fast-failure amplification edge: a 500 response could complete before a second waiter woke up. The Redis waiter poll interval was 500 ms, while the jittered negative failure replay window could be only 400-500 ms. A waiter that slept through that narrow window became a second leader and sent a second OAuth request for the same failure wave.
- Related failures can appear as auth cooldowns, an account being excluded after refresh admission rejection, generic retry exhaustion after a successful refresh, or an invalid refresh token being disabled more than once. They do not require any tool-hash or transcript-leak fingerprint.

## Reproduction Contracts

### Short-TTL Success Burst

Use one expired external-IdP credential and a fake OAuth endpoint that returns a valid access token with 360 seconds of lifetime. Start 16 callers against the same manager and credential, then immediately issue one more request.

The historical red result was 30 refresh HTTP sends for the 16-caller recovery phase. The green contract is one refresh send for the wave, all waiters reusing the result, the immediate follow-up producing no new refresh, and all auxiliary/in-flight permits returning to zero. Repeat five rounds.

### Timeout And Typed Failure Wave

Use one expired credential and fake OAuth scenarios for HTTP 500, header timeout, disconnect, malformed success, invalid client, invalid grant, and 429 with `Retry-After`. Start 32 callers simultaneously.

The historical timeout shape was 32 callers to 32 sends. The green contract for shareable transient failures is one send, one typed leader result, 31 typed followers, one request-level auxiliary consumption in aggregate, and no duplicate health mutation. Invalid grant, credential auth, and 429 each have exactly one appropriate credential-wide health action. Repeat every class five rounds.

### Invalid-Bearer Automatic Recovery

Return the exact invalid-bearer response from fake Kiro API and MCP endpoints for concurrency 1, 8, and 32. Record inference/MCP hits, refresh hits, credential ID, token generation, storage revision, health state, and request-level inference/auxiliary budgets.

The old red contract is `N` invalid-bearer callers causing `N` unconditional force refreshes. The green contract is one conditional recovery per rejected token generation, no use of the Admin force-refresh API, one retry only when inference/MCP send capacity remains, and no credential health mutation for local budget/concurrency/admission rejection.

### No-Redis Admission

Construct the manager without Redis, set `tokenRefreshMaxRpm=60` and `tokenRefreshBurst=8`, and exhaust the process-local bucket. The authority must remain `process_local`; rejection must be typed as refresh rate limiting with bounded retry-after. Global dispatch queue depth must remain unchanged, and no error or metric may claim Redis coordination degradation.

### Cross-Instance Success And Failure

Run two managers against the same PostgreSQL credential and Redis namespace. Cover both rotating and non-rotating refresh-token responses. Delay credentials-changed pubsub consumption so the second process retains a stale local snapshot when the first process completes.

The cluster green contract requires one leader per credential/auth identity, a follower reading the Redis outcome and PostgreSQL authoritative token before any new send, and a stale result being unable to overwrite a newer access token. Failure outcomes and health-action claims must also be shared across processes, not merely serialized by a lock.

## Root Causes

1. Request readiness and warning UI used different expiration windows. A token with about six minutes remaining passed the five-minute request-safety boundary but failed a ten-minute “expiring soon” check after each waiter acquired the mutex.
2. The original per-credential mutex protected only concurrent execution. It did not retain a typed positive or negative result, so every waiter became the next leader after failure.
3. The invalid-bearer provider path reused the unconditional Admin force-refresh method. Its per-request `HashSet` prevented only a second refresh within the same request, not refreshes from other concurrent requests.
4. Request-scoped auxiliary budgets bound one caller but did not aggregate callers or replicas. A global concurrency semaphore reduced simultaneous work but did not impose a sustained refresh RPM ceiling.
5. Process-local and Redis-global authorities were not represented as a separate refresh-admission channel, making queue/capacity errors easy to misclassify.
6. A storage revision change does not necessarily mean the rejected access token changed. Treating any revision mismatch as `CredentialChanged` can retry the same invalid token and suppress the one allowed recovery.
7. A refresh-field PostgreSQL CAS that fences only refresh-token/auth fields cannot distinguish two successful refreshes when the provider does not rotate the refresh token. Without an old-access-token or equivalent generation fence, a late result can overwrite a newer token.
8. Health ownership was split between manager result publication and later provider mutation. Cancellation between claim and mutation can lose the sole action owner; cancellation after a committed HTTP send but before result publication can leave no shared outcome.
9. The provider can initiate recovery on the last allowed inference/MCP attempt. The OAuth send then has no remaining inference capacity with which to validate the recovered token.
10. Redis cluster failure replay originally allowed the negative replay window to be shorter than the waiter poll interval. With `TOKEN_REFRESH_POLL_AFTER_MS=500`, first-round jitter could reduce the delay to 400-500 ms, so a fast 500 leader result expired before a waiter polled and caused duplicate leader election.

## Selected Fix And Design

### Single-Process Wave

- Keep one bounded state entry per credential, not per request or waiter.
- Key a wave by a secret-free SHA-256 identity covering the authentication and transport context. Never log or persist raw refresh/access tokens, client secrets, proxy credentials, or the hash preimage.
- Share typed committed failures for a bounded negative window. Local validation, client construction, request-budget rejection, process-concurrency rejection, and refresh-RPM admission rejection remain health-neutral and do not open an upstream failure wave.
- Use the five-minute boundary only for request-path token usability. The ten-minute function remains advisory and cannot drive request refresh.

### Cluster Wave And Fencing

- Redis owns a credential/auth-identity state machine with leader, wait, replay, success, and failed states; all state and diagnostics are closed, low-cardinality representations.
- A leader publishes success only after PostgreSQL commit. Followers receiving success reload PostgreSQL and verify that the authoritative access-token generation supersedes the rejected token before retrying.
- A process obtaining leadership after another replica released the lock must perform the same PostgreSQL authoritative check before reserving or sending OAuth HTTP.
- PostgreSQL refresh commit must fence the old access-token generation, while preserving unrelated concurrent admin field updates. Revision alone is too broad; refresh-token hash alone is too weak.
- Redis failure outcomes carry one renewable/expiring health claim. The claim is acknowledged only after the credential health mutation completes. Cancellation or a failed mutation must leave the claim reclaimable.
- Redis shareable failure outcomes now enforce a replay-window floor greater than the waiter poll interval. The current lower bound is `TOKEN_REFRESH_POLL_AFTER_MS + 250 ms`, so a fast 500/429/etc. result stays replayable long enough for existing waiters to observe it instead of immediately becoming new leaders.

### Automatic Versus Admin Refresh

- Request traffic uses conditional recovery tied to the rejected access token and authoritative context. A revision-only change with the same access token is not success.
- Recovery is admitted only when at least one inference or MCP send remains for the retry.
- Admin force refresh stays unconditional and operator-triggered. It does not consume the downstream request's auxiliary budget and must not be called by provider 401 handling.
- OAuth credential-auth and refresh 429 health are credential-wide, so the health mutation uses `model=None`; it must not cool only the inference model that happened to expose the token failure.

### Refresh RPM Admission

- `tokenRefreshMaxRpm` defaults to 60 and accepts 1 through 6000.
- `tokenRefreshBurst` defaults to 8 and accepts 1 through 256.
- With Redis, one token bucket is shared across replicas. Without Redis, each process uses a local bucket and explicitly reports `process_local` authority.
- Redis timeout/error fails closed as `redis_global_degraded` with a bounded retry-after. It must not silently fall back to a per-process bucket and multiply the configured cluster limit by replica count.
- Refresh admission is independent from the inference budget, dispatch queue, prompt-steering master, account RPM, and ordinary scheduler Redis breaker.

## Risks, Performance, Privacy, And Compatibility

- The ready-token and API-key paths must not allocate refresh wave state, hash secrets, touch Redis, or build an HTTP client.
- A refresh path may do fixed-size hashing and O(1) bounded map access. Cluster coordination must use a bounded number of Redis/PostgreSQL operations; 32 waiters must not become 32 polling tasks or sequential OAuth sends.
- Wave entries, health claims, client-cache entries, and token-bucket state require hard bounds/TTL. Cancellation must release auxiliary concurrency permits and cannot leave a permanent leader lock.
- Public errors, attempts, usage, and logs may expose only stage, kind, authority, status, retry-after, counters, and error ID. OAuth bodies, endpoint URLs, bearer tokens, refresh tokens, client credentials, proxy credentials, and identity hashes remain private.
- Historical configs deserialize with the 60/8 defaults. Runtime/Admin/UI round-trip must preserve explicit values independently of prompt steering. Older nodes that do not understand the Redis wave protocol make mixed-version rolling unsafe; rollout requires a full drain/stop and one-version restart unless a versioned compatibility proof is added.
- Fail-closed Redis admission can reduce availability during Redis degradation. External fallback eligibility and public classification must therefore be tested explicitly; availability must not be “fixed” by reopening unbounded local refreshes.

### 2026-07-18 invalid-configuration amplification correction

The full default unit tree showed that two locally invalid refresh tokens could still build a refresh HTTP client before `validate_refresh_token` rejected them. Under concurrent provider/client-cache tests that avoidable initialization broke a 500 ms fixture bound even though OAuth HTTP hits stayed at zero. Validation now runs after the final Redis/PostgreSQL refresh source is selected, but before client construction, auxiliary concurrency, refresh RPM admission, or request budget consumption. This placement does not reject a newer PostgreSQL authority merely because the original local snapshot was invalid.

Five focused rounds completed in `2890/117/89/87/87 us`; refresh-client entries/builds/hits/misses/saturation, auxiliary in-flight/peak, OAuth sends and health mutations all remained zero. The then-current 1712-test tree passed, and the later current 1714-test tree also passed after unrelated warning/deadline work. This closes local invalid-configuration client/RPM amplification only; it does not close Redis/PG cluster or provider invalid-bearer gates. See [the full-tree red/green record](../evidence/full-unit-tree-red-green-20260718.md).

## Validation And Evidence Matrix

| ID | Scenario | Repetitions | Required result | Current state |
| --- | --- | ---: | --- | --- |
| TR01 | Six-minute token, 16 same-credential callers | 5 | One refresh send per wave; immediate reuse | Historical red; implementation present; post-fix run not accepted here |
| TR02 | 500/timeout/disconnect/malformed, 32 callers | 5 per class | One send, 1 leader + 31 followers, health neutral | Process-local focused pass: every class 1 hit, 31 followers, aggregate auxiliary consumption 1 |
| TR03 | invalid-client/invalid-grant/429, 32 callers | 5 per class | One send and exactly one credential health action | Tests exist; cancellation-safe and cluster proof pending |
| TR04 | API and MCP invalid bearer, c1/c8/c32 | 5 per cell | Conditional generation recovery; no Admin force path | Implementation exists; end-to-end protocol matrix pending |
| TR05 | Inference/MCP budget has no retry send left | 5 | Zero refresh HTTP; original auth classification retained | API/MCP fake HTTP focused pass |
| TR06 | Revision changes but rejected access token does not | 5 | Not reported as recovered; no same-token retry loop | Pure revision/token fence focused pass；完整 provider/PG pending |
| TR07 | Two replicas, success without refresh-token rotation | 5 | One OAuth send; stale result cannot overwrite new token | Isolated PostgreSQL/Redis cluster gate pass: rotating + non-rotating success, direct stale CAS fence |
| TR08 | Two replicas, shareable failure and health claim | 5 per class | One cluster send/wave/action | Isolated PostgreSQL/Redis cluster gate pass for fast 500 replay and health-claim reclaim |
| TR09 | Cancel before send/during send/during unlock/after claim | 5 per phase | No leak, duplicate wave, or lost health owner | Process-local live-send permit and deadline-drop pass; Redis unlock/health-claim phases open |
| TR10 | No Redis, defaults 60/8, 128 immediate reservations | 5 | Process-local authority; bounded admission; queue unchanged | Focused pass：每轮 8 admitted / 120 rejected |
| TR11 | Redis 60/8 across two replicas | 5 | Aggregate bucket bound; one authority/counter view | Open |
| TR12 | Redis slow/error/restart | 5 per phase | Bounded fail-closed rejection and recovery; no local multiplier | Open |
| TR13 | Admin force refresh | 5 | Remains unconditional and separate from request recovery | Static path present; focused compatibility rerun required |
| TR14 | Error/body/log privacy | 5 per failure class | No secret/body/hash marker in public or debug surfaces | Four protocol tests passed only; full runtime scan pending |
| TR15 | 1/20/60 credentials, c1/c8/c32 and recovery | 5 per cell | Per-request and cluster outbound sends bounded independently of pool size | Process-local partial: 20-account independent and 128-account shared 12-class matrices pass; exact 1/60 provider, cluster and frozen load open |

## Executed Results And Non-Claims

- Scoped batch `focused-r3` reported Rust formatting and check success. This establishes syntax/type-check evidence for that dirty-tree snapshot only.
- Four refresh Redis protocol tests passed in the focused batch. They cover closed decoding, malformed-wire rejection, bounded deterministic backoff, and a secret-free bounded wire contract. They do not prove manager/provider integration or live Redis behavior.
- `queue-refresh-integration-r3` 已关闭旧 process-local capacity red：60/8、128 immediate reservations 连续 5 轮均为 8 admitted/120 rejected；limit update 与 config fail-fast 各 5 轮通过。API/MCP 最后一 inference attempt 的 fake HTTP fixture 各 5 轮均为 inference 1、OAuth 0、auxiliary consumed 0；metadata-only revision fence 也完成 5 轮聚焦验证。
- `oauth-shared-burst-r4` 和 `oauth-independent-burst-r1` 分别完成 128-account shared 与 20-account independent 的 12 类失败 x c1/c8/c32 x 5 轮；请求 hits 不超过 2、process peak 不超过 16、账号未持久禁用且恢复成功。`oauth-singleflight-wave-r1` 完成同 credential 32 waiter 四类失败 x 5 轮的严格 1-hit 合同。
- 首次取消测试在 `auxiliary-rpm-focus-r1` 以 21/22 暴露 detached child permit 延迟释放；refresh future 改为请求任务内结构化 timeout 后，`auxiliary-rpm-focus-r3` 为 23/23，cancel/deadline drop 均在返回前释放 permit。完整红绿证据见 [2026-07-18 record](../evidence/oauth-auxiliary-budget-and-cancellation-20260718.md)。
- 2026-07-19 当前仓库专属隔离 Redis 动态通过 3 outer rounds × 5 exact filters：leader election、failure replay health claim + identity fence、stale leader overwrite fence、cancel-before-send immediate new leader、bucket TTL refill/version switch。scope `1690676 KiB removed=true reservation_released=true`，日志哈希见 [2026-07-19 storage evidence](../evidence/storage-integration-and-artifact-gate-20260719.md)。
- 2026-07-20 使用当前项目隔离 Redis 再执行 `token-refresh-redis-provider-r1`：上述五项各 3 outer × 5 internal 全部通过；同一编译批次新增的 API/MCP final-attempt fixture 内部各 5 轮也严格 inference=1/OAuth=0/auxiliary=0。scope `1698460 KiB removed=true reservation_released=true`，详见 [更新后的 focused evidence](../evidence/token-refresh-failure-wave-and-cluster-rpm-20260717.md)。
- 2026-07-22 使用当前项目隔离 PostgreSQL `127.0.0.1:25433/kiro_rs` 与 Redis `127.0.0.1:26379/2` 执行 token-refresh cluster gate。修复前同一 gate 在 fast 500 failure replay 上复现 `endpoint hits = 2`（期望 1）；修复后最小 failure replay exact 通过，随后完整 7-test matrix 先 1 outer pass，再默认 3 outer pass。默认 run 覆盖 7 个 exact tests × 3 outer × 每测试 5 internal rounds，共 105 个内部轮次；Redis DB 结束为空，`dockerStarted=false`，`protected9022ProbeSkipped=true`，scoped target `size_kib=1715296 removed=true reservation_released=true`。详见 [更新后的 focused evidence](../evidence/token-refresh-failure-wave-and-cluster-rpm-20260717.md)。
- No accepted evidence yet covers the full automatic API/MCP 401 c1/c8/c32 matrix, Redis slow/error/restart for token refresh specifically, exact 1/60 provider account cells, persistent usage attribution, real upstream/Claude CLI native capability behavior, or a frozen release binary.
- The current release decision is `NO-GO`.

## Minimal Repair Order

1. Complete the full automatic API/MCP invalid-bearer c1/c8/c32 provider matrix, including persistent usage attribution and public error/privacy scan.
2. Add token-refresh-specific Redis slow/error/restart dynamic coverage. General scheduler Redis chaos is not a substitute for the OAuth refresh channel.
3. Cover exact 1/60 provider account cells and 1/20/60 credential load/resource matrix on one scoped dirty candidate.
4. Rerun the relevant protocol and load gates on one repository-external frozen release binary, not only test binaries.
5. Any red or missing cell keeps `NO-GO`.
