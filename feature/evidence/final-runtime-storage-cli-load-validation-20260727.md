# Final runtime/storage/Claude CLI/load validation - 2026-07-27

Status: `release-candidate validated / release-pending`

This document is the current release-candidate evidence for the runtime completion, storage
fault-domain, scheduler resilience, Claude Code CLI compatibility, thinking/output_config, tool
history leakage, dashboard query, and load/chaos fixes.

It supersedes older candidate identities inside
`runtime-completion-storage-coupling-validation-20260727.md`. Historical sections in that file are
kept as investigation context; the candidate identity and pass/fail status in this file are the
current source of truth.

## 1. Candidate identity

- Git HEAD during validation: `57d8c1ed1cff3fcd0f49935f1415294c9f0f13f9`
- Working tree: dirty by design; validation covers the pre-release candidate diff.
- Frozen product binary:
  `/private/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-cli-candidate.rUvj61/kiro-rs`
- Frozen product SHA-256:
  `40ec70c7036826807f3d59701fe02de8eada7c8d88f265ad4a68fde55ff3c9d3`
- Frozen loadtest binary:
  `/private/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-cli-candidate.rUvj61/kiro_loadtest`
- Frozen loadtest SHA-256:
  `a9b03d0dbe3f4456939641b434fcc3781ea6f6909a31dff393100d2bcbcc81c8`
- Rust toolchain used for release/static gates: `1.92.0`
- Claude Code CLI used for final real CLI gates: `2.1.220 (Claude Code)`

All Cargo builds/tests were run through `feature/tests/run-cargo-scoped.sh`; scoped targets were
removed by the wrapper after each batch.

## 2. Product fixes covered by this gate

The current candidate includes these product fixes and hardening changes:

- Request completion and failure paths release local scheduler capacity before durable PgSQL/Redis
  persistence.
- Token-manager request-safe deferred variants are used for request failure, quota/risk disable,
  refresh-token-invalid, profileArn persistence, session unbind, and disabled credential cleanup.
- Pre-send token refresh setup/admission failures are typed and health-neutral; they no longer
  broadcast a distributed Redis refresh failure wave when no upstream send was committed.
- Stale PgSQL runtime mutation with `applied=false` cannot overwrite an already-disabled credential
  state.
- MCP/WebSearch auxiliary completion failures do not poison the main model credential health.
- Usage/dashboard PgSQL work uses an observability/usage fault domain rather than the main request
  pool, and PgSQL fallback summary/dashboard cache revisions include writer and cleanup watermark
  state to avoid stale summary reuse after queued writes finish.
- Dashboard query endpoints are split/gated so slow usage aggregation is non-core work.
- Thinking/output_config mapping preserves explicit effort values, including `max`, and sends
  Kiro-compatible `thinking.type=adaptive` with `output_config`.
- Tool-history sanitizer keeps tool_use/tool_result structure without leaking Claude-internal
  transcript markers such as `Tool results provided`, `<function_results>`, or hashed tool names.
- Test runner compatibility was updated for Claude Code CLI `2.1.220`, whose reachability probe is
  now `HEAD /cc/api/hello` rather than only `HEAD /cc`.

## 3. Rust/static/frontend gates

### 3.1 Focused manager/PgSQL batch

Result: pass.

Covered token refresh and PgSQL runtime persistence fixes, including:

- `token_refresh_admission_rejections_are_typed_pre_send_failures`
- `postgres_*` focused group

Observed result:

```text
17 focused PostgreSQL/manager tests passed
validation-build-cleanup removed=true reservation_released=true
```

### 3.2 Full token manager tests

Result: pass.

```text
running 248 tests
test result: ok. 248 passed; 0 failed
```

### 3.3 Full Rust all-target tests

Result: pass.

```text
kiro-rs main tests: 1816 passed; 0 failed; 6 ignored
kiro_loadtest tests: 31 passed; 0 failed
validation-build-cleanup removed=true reservation_released=true
```

Important covered groups:

- thinking/output_config protocol matrix;
- websearch/MCP error and recovery matrix;
- transcript sanitizer and tool hash leakage regressions;
- scheduler/storage fault-domain tests;
- previous CI-flaky PgSQL cleanup/watermark/pricing persistence tests.

### 3.4 Static gates

Result: pass.

```text
cargo fmt --all -- --check: pass
git diff --check: pass
clippy baseline: 813 warnings emitted; baseline allows 849
cargo check --locked --all-targets --no-default-features: pass
```

### 3.5 Frontend gates

Result: pass.

```text
npm --prefix ui run check: pass
npm --prefix admin-ui exec -- tsc -b admin-ui/tsconfig.json --pretty false: pass
npm --prefix ui run build: pass
npm --prefix admin-ui run build: pass
```

Non-blocking note: `ui` Vite build still warns about a chunk above 500k. This belongs to the
dashboard/code-splitting backlog, not the runtime/storage release blocker.

### 3.6 Build artifact gate

Result: pass after release binary build.

```text
build-artifact-inventory version=2 mode=read-only targets=0 reservations=0 target_processes=0 blockers=0
release-gate result=pass
```

## 4. Local 9022 service smoke

The frozen product binary was started on the local configured service port `127.0.0.1:9022`.

Health and auth smoke:

```text
/healthz       200 0.003297s
/readyz        200 0.006299s
/cc/v1/models 401 0.002737s with normalized authentication_error
/v1/models    401 0.002635s with normalized authentication_error
```

Process baseline:

```text
RSS: 34864 KiB
FD count: 37
```

Startup log confirmed the intended fault-domain split:

- usage/dashboard PgSQL uses an independent pool;
- usage/statistics/Admin cache do not use business Redis when observability Redis is not configured;
- usage store authority is PostgreSQL.

### 4.1 Local real-account limitation

`credentials.json` contains 5 non-disabled social credentials, but the local PgSQL authority for
the active `9022` environment has 6 persisted credentials and all 6 are disabled:

- total: `6`
- available: `0`
- disabled: `6`
- disabled reasons observed: `Manual`, `TemporarilySuspended`, `QuotaExceeded`

Because PgSQL is authoritative for this local runtime, real upstream success could not be proven on
`9022` without overriding or re-enabling local credentials. That was not done to avoid accidentally
calling accounts that the local state intentionally disabled.

### 4.2 Direct protocol error normalization on 9022

Six direct `/cc/v1/messages` requests were sent to 9022:

- normal non-stream;
- normal stream;
- adaptive thinking + `output_config.effort=max`;
- disabled thinking + explicit effort;
- tool-use stream;
- invalid model.

All six returned local preflight `503` because no local account was dispatchable. The public error
was normalized and did not leak internal scheduler/fallback/credential wording:

```text
No account is ready for this request right now. Please retry shortly.
internalLeakMarkers=[]
```

This proves public error redaction, but not real upstream success, because the local credential pool
was intentionally unavailable.

### 4.3 Real Claude CLI error-path behavior on 9022

A real `claude --bare --print --output-format=stream-json` invocation was run against
`http://127.0.0.1:9022/cc` with isolated `HOME` and `CLAUDE_CONFIG_DIR`.

Result:

- no internal leak markers in stdout/stderr;
- Claude CLI emitted repeated `api_retry` system events;
- the command did not exit within 120 seconds because the CLI retries the local 503 capacity error;
- no request IDs were exposed in the CLI event stream.

Interpretation:

- public error text is redacted correctly;
- local “all credentials disabled” is not a valid success-path real account gate;
- this retry behavior should remain documented as a source of apparent internal RPM amplification
  when clients retry local capacity errors.

## 5. Real Claude Code CLI fake-upstream gates

These gates use the real installed `claude` binary and a fake local Kiro upstream. They do not
consume real accounts. They prove Claude Code CLI protocol compatibility, tool history round-trip,
long session behavior, thinking/output_config wire behavior, and leakage protection for the frozen
candidate.

### 5.1 Bare invoke / literal tool marker safety

Report:
`reports/bare-invoke-claude-cli/bare-invoke-1785171059117-98431-bfd686.json`

Result: pass.

- Claude CLI version: `2.1.220 (Claude Code)`
- Cases: `20`
- Structured tool cases: `5`
- Inference hits: `25`
- `tool_use`: `5`
- `tool_result`: `5`
- Non-zero usage in all successful assistant/tool cases.
- Literal `<invoke ...>` text did not execute as a tool.
- Unknown fake-upstream requests: `0`

### 5.2 Long session / Continue / tool history leakage

Report:
`reports/claude-cli-long-session-continue/long-session-1785171077008-3897-2a5e8a.json`

Result: pass.

- Sessions: `5`
- CLI turns: `110`
- Continue turns: `105`
- Tool turns: `100`
- Tool use/result pairs: `100/100`
- Leak matches: `0`
- All tool turns had non-zero usage.

The leak matcher covered the previously reported classes:

- `user Continue`
- `user Tool results provided`
- `Tool results:`
- `<function_results>` / `<function_calls>`
- `<invoke name=...>`
- hashed tool names such as `bashHash[0-9a-f]{8}`
- generic `NameHash[0-9a-f]{8}` signatures

The report still records upstream internal tool names such as `bashHashd1e9567d` as wire facts; the
important result is that these did not leak into the Claude CLI user-visible transcript.

### 5.3 Thinking/output_config wire

Initial thinking-wire run with `KIRO_CLAUDE_BINARY=$(command -v claude)` failed before protocol
validation because the runner canonicalized the Volta symlink to `volta-shim`. Executing the shim
directly loses the `claude` argv0 identity.

Fix applied to validation harness:

- use the real Claude package entrypoint:
  `/Users/yuanfeijie/.volta/tools/image/packages/@anthropic-ai/claude-code/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe`;
- set `KIRO_EXPECTED_CLAUDE_VERSION=2.1.220`;
- classify Claude CLI `2.1.220` probe `HEAD /cc/api/hello` as the expected `cc_head_probe`.

Contract test after harness update:

```text
node --test feature/tests/thinking-effort-kiro-wire-contract.test.mjs
3/3 tests passed
```

Final thinking-wire report:
`reports/thinking-effort-wire/thinking-effort-wire-1785171450125-45974-ed1dda.json`

Result: pass.

- Endpoints: `cli`, `ide`
- Efforts: `absent`, `low`, `medium`, `high`, `xhigh`, `max`
- Rounds per endpoint/effort: `5`
- Total cases: `60`
- Inference hits: `60`
- Model discovery hits: `2`
- Unknown requests: `0`
- Invalid wire JSON: `0`
- Protocol violations: `0`
- Case violations: `0`

Observed wire contract:

- absent effort normalized to `output_config.effort=high`;
- explicit `low`, `medium`, `high`, `xhigh`, and `max` were preserved;
- `max` was not clamped to `high`;
- all cases sent compatible `thinking.type=adaptive`;
- final wire model resolved to an advertised fake upstream model.

## 6. Load/chaos gates

All load/chaos tests used isolated local PgSQL/Redis containers and fake upstreams. No production
traffic was generated. Temporary `kiro_load_chaos_*` databases were created before each run and
dropped after validation. Redis prefixes were cleaned by the runner.

Validation dependency ports:

- PostgreSQL: `127.0.0.1:32770`
- Redis: `127.0.0.1:32771`

### 6.1 L3 burst and recovery

Report: `l3-summary.json`

Result: pass.

- Result count: `9`
- Normal c1/c5/c10: all success.
- Spike c40/r100: `100/100` success, p95 TTFB `39ms`.
- Recovery after spike: `10/10` success.
- Recovery-after-error burst: expected mixed `200/429/502`, then normal recovery `12/12` success.
- Invalid tool burst: expected `502`, then normal recovery `12/12` success.

Representative results:

| Scenario | Result | p95 TTFB | p95 total |
|---|---:|---:|---:|
| `l3_normal_c1_r5` | 5/5 success | 9ms | 10ms |
| `l3_normal_c5_r20` | 20/20 success | 21ms | 21ms |
| `l3_normal_c10_r50` | 50/50 success | 12ms | 13ms |
| `l3_spike_c40_r100` | 100/100 success | 39ms | 40ms |
| `l3_post_error_recovery_normal_c3_r12` | 12/12 success | 17ms | 17ms |

### 6.2 L4 restart and failure chaos

Report: `l4-summary.json`

Result: pass.

- Result count: `12`
- Proxy restart during long stream produced bounded expected transport errors; recovery `12/12`
  success with p95 TTFB `8ms`.
- 429 burst: `40/40` expected 429; recovery `12/12` success.
- 500 burst: expected `429/502`; recovery `12/12` success.
- Invalid-tool burst: `40/40` expected 502; recovery `12/12` success.
- Client-drop burst: expected client-drop classification; recovery `12/12` success.
- Mixed chaos: `96` requests with mixed `200/429/502`; recovery `12/12` success with p95 TTFB
  `10ms`.

Representative recovery checks:

| Scenario | Recovery result | p95 TTFB | p95 total |
|---|---:|---:|---:|
| proxy restart recovery | 12/12 success | 8ms | 8ms |
| 429 recovery | 12/12 success | 8ms | 9ms |
| 500 recovery | 12/12 success | 9ms | 9ms |
| invalid-tool recovery | 12/12 success | 8ms | 8ms |
| client-drop recovery | 12/12 success | 6ms | 6ms |
| mixed-chaos recovery | 12/12 success | 10ms | 10ms |

### 6.3 L5 long-stream soak

Two L5 runs were performed.

First L5 run:

- Duration: `60s`
- Idle cooldown: `15s`
- Long stream: `461/461` success
- Post-soak recovery: `12/12` success
- FD returned within threshold.
- RSS did not satisfy the runner's strict `baseline + 32MiB` gate after only 15 seconds idle:
  `30.8MB -> 78.7MB`.

Interpretation: this was not a functional failure or FD leak, but the idle window was too short to
distinguish delayed allocator/runtime settling from persistent growth.

Second L5 run:

- Duration: `60s`
- Idle cooldown: `60s`
- Result: pass.
- Long stream: `461/461` success.
- Post-soak recovery: `12/12` success.
- Long-stream p95 TTFB: `288ms`.
- Long-stream p95 total latency: `2735ms`.
- Recovery p95 TTFB: `11ms`.
- Recovery p95 total latency: `11ms`.

Resource recovery:

```text
RSS start:       30,523,392 bytes
RSS peak:        82,870,272 bytes
RSS idle sample: 51,920,896 bytes
FD start:        36
FD idle sample:  35
rssReturnedWithin32MiB=true
fdReturnedWithin5=true
```

Interpretation: long streams and deferred completion work did not leave sockets or memory growing
after traffic stopped when given a realistic idle cooldown.

## 7. Dashboard/admin query smoke

The frozen binary on local 9022 passed dashboard/admin smoke with the local PgSQL data set:

```text
/api/admin/usage-dashboard/windows                 200 0.042629s
/api/admin/usage-dashboard/top?windowKey=today     200 0.044117s
/api/admin/usage-dashboard/breakdown?windowKey=today 200 0.016130s
/api/admin/usage-summary                           200 0.012037s
```

This is a smoke test only. It proves the local endpoints return quickly and the split endpoints are
usable; it does not replace the separate dashboard product redesign described in
`feature/issues/dashboard-observability-redesign.md`.

## 8. Cleanup performed

Deleted only the temporary validation databases created in this run:

- `kiro_load_chaos_l3_170406_84952_a/b/c`
- `kiro_load_chaos_l4_170536_95280_a/b/c/d/e/f`
- `kiro_load_chaos_l5_170727_8938_a`
- `kiro_load_chaos_l5r2_170947_33708_a`

Redis prefixes were cleaned by the load/chaos runner:

- L3: 3 prefixes
- L4: 6 prefixes
- L5: 1 prefix
- L5 second run: 1 prefix

Raw temporary CLI/load artifacts were used only to extract this summary and should be deleted after
this document is committed.

## 9. Final interpretation

The current candidate satisfies the release validation target for the P0 runtime/storage/scheduler
class:

- normal and burst traffic recover;
- upstream 429/500/invalid-tool/client-drop/mixed-chaos errors recover;
- proxy restart during long streams does not poison later traffic;
- long streams release resources after idle;
- request completion/failure persistence is no longer allowed to synchronously hold the primary
  request path in the covered tests;
- real Claude Code CLI long sessions preserve tool history without leaking internal tool hash or
  transcript markers;
- thinking/output_config preserves explicit effort, including `max`, and sends adaptive thinking;
- local dashboard split endpoints return without blocking the local service.

Remaining non-release follow-ups:

- Production real-account success was not proven on local 9022 because local PgSQL has all
  credentials disabled. A low-concurrency real-account smoke can be run only after the operator
  confirms which local credentials are safe to enable or after a dedicated test credential is
  provided.
- Dashboard/UI product redesign is documented but not complete.
- Production Redis 7 vs Redis 8 controlled comparison remains a follow-up; current load/chaos
  evidence shows the product-level fault-domain behavior on isolated local dependencies.
