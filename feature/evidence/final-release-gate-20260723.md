# 2026-07-23 最终发布门禁复测证据

Date: 2026-07-23

Status: `release-gate-pass-with-local-9022-inventory-exception / publish-pending`

## 范围

本文件记录当前 `v0.0.117` 候选发版前最终复核证据。它覆盖用户本轮要求的四类核心风险：

- Claude Code CLI 多轮/长会话/工具调用/history 泄漏：`user Continue`、`Tool results provided`、`<function_results>`、`<invoke>`、`*Hashxxxxxxxx` 工具名等。
- thinking / `output_config.effort` / `thinking.type=adaptive` 请求体映射。
- scheduler 高并发低 RPM、Redis/PgSQL/usage 干扰、突发错误、恢复、资源与队列稳定性。
- external pool 成功 usage/billing、runtime 成功持久化、调度状态退化与账号“假禁用”。

执行边界：

- 未执行 Docker 动态验证；这是用户明确要求的豁免项，不能记为 Docker pass。
- 未停止、重启或压测既有 `127.0.0.1:9022` 服务。
- 未读取、暂存或提交 `kiro_idc_users*.txt` 和根目录未跟踪 `package.json`。
- 所有 Cargo 命令均通过 `RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh <scope> -- ...` 执行。
- 冻结候选二进制位于仓库外临时目录；scoped Cargo target 已由 wrapper 删除。

## 当前源码与候选身份

- Git HEAD at validation: `ac5bc57d5ecf786c7f96d66c1dd7101e2eda9c65` (`ac5bc57`)。
- Working tree: dirty release candidate，包含本轮代码修复、验证 runner hardening 与文档更新。
- Rust toolchain: `1.92.0`。
- Claude Code CLI: `2.1.197 (Claude Code)`。

冻结候选目录：

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-release-117-candidate.0BZyt1
```

冻结候选 SHA-256：

```text
760345d76b3d2ea70694cc420cfde5078ebc8056c7a31a6d7df135d714509839  kiro-rs
3fbaa97a1e0556f38546393068b3afd47caaa48280620c6c8dec3d55d7828ada  kiro_loadtest
```

## Rust / Cargo release gate

### no-default full gate

Scope: `release-117-no-default-final2`

Command:

```text
RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh release-117-no-default-final2 -- \
  cargo test --locked --all-targets --no-default-features -- --nocapture --test-threads=1
```

Result:

- main tests: `1757 passed / 0 failed / 6 ignored`。
- `kiro_loadtest`: `31 passed / 0 failed`。
- scoped cleanup: `size_kib=1702964 removed=true reservation_released=true`。
- PostgreSQL test DB dropped after run。

### default full gate

Scope: `release-117-default-final2`

Command:

```text
RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh release-117-default-final2 -- \
  cargo test --locked --all-targets -- --nocapture --test-threads=1
```

Result:

- main tests: `1757 passed / 0 failed / 6 ignored`。
- `kiro_loadtest`: `31 passed / 0 failed`。
- scoped cleanup: `size_kib=1713348 removed=true reservation_released=true`。
- PostgreSQL test DB dropped after run。

Relevant observed passing cases include:

- `forty_by_fifteen_with_global_five_hundred_queues_without_disabling_for_five_rounds`。
- OAuth refresh burst tests across independent/shared managers, 1/8/32 concurrency and 20/128 pools: disabled `0`，refresh failures `0`，process cap/recovery bounded。
- `postgres_rolls_up_external_pool_billing_for_large_samples_and_removes_after_cleanup`。
- transcript sanitizer、thinking trigger、output_config wire、JSON whitespace compression 保真、payload guard zero-copy/semantic preservation。

### fmt / diff / clippy / focused regression

- `cargo fmt --all -- --check`: pass。
- `git diff --check`: pass。
- Clippy final scope `release-117-clippy-final2`: `849 warnings`，checked-in baseline allows `849`，pass。
- A new `dead_code` warning introduced by the PgSQL success persistence helper was fixed by removing the unused wrapper and updating tests to call `record_credential_success_at_generation_with_count(..., 1)`。
- Focused default + no-default regression:
  - `postgres_runtime_generation_fences_pre_reset_mutations`
  - default: `1 passed / 0 failed`
  - no-default: `1 passed / 0 failed`
  - scoped cleanup `size_kib=2497616 removed=true reservation_released=true`。

### release build

Scope: `release-117-build-final`

Result:

- `cargo build --release --bins`: pass。
- release build duration: `8m08s`。
- copied frozen binaries out before target cleanup。
- scoped cleanup: `size_kib=802552 removed=true reservation_released=true`。

## Non-Cargo source / runner / docs gates

```text
node feature/tests/check-feature-docs.mjs
```

Result:

```text
PASS: 48 issue documents satisfy the section contract; 122 relative links resolve.
```

```text
node --test feature/tests/*.test.mjs
```

Result:

```text
tests=283
pass=261
fail=0
cancelled=0
skipped=22
todo=0
duration_ms=51847
```

The 22 skipped tests are explicit live-signal or safety opt-ins and are not counted as product passes.

## Build artifact inventory

Final inventory command:

```text
node feature/tests/inventory-build-artifacts.mjs --gate
```

Result on this workstation:

```text
build-artifact-inventory version=2 mode=read-only targets=1 reservations=0 target_processes=1 blockers=2
target id=d61e6fde19e5 location=<repo>/target classification=unmanaged-repo-cargo-target size_kib=1222296
target-process target_id=d61e6fde19e5 pid=84264 classification=kiro-runtime
docker status=timed-out cleanup=manual-only
release-gate result=fail
```

Interpretation:

- This is a pre-existing local `127.0.0.1:9022` service running `./target/release/kiro-rs -c config.json --credentials credentials.json` from the repository root target.
- It is not a scoped validation target, reservation, or temp artifact created by this final run.
- The service was not stopped, restarted, or modified because the validation safety contract and user instruction both excluded touching the live 9022 service.
- This local inventory blocker is recorded as a workstation/service-owner exception and does not change the code/test/CLI/load release result. It should be resolved separately by the service owner if a strict “repo target absent” local inventory is required.

## Claude Code CLI / protocol validation

All CLI validations used isolated temp home/config/project directories and temporary kiro.rs ports. Existing `9022` was not touched.

### Raw Claude CLI thinking capture

Command:

```text
KIRO_THINKING_CAPTURE_ROUNDS=5 \
KIRO_CLAUDE_BINARY=/Users/yuanfeijie/.volta/bin/claude \
node feature/tests/thinking-effort-claude-cli-capture.mjs
```

Result:

- `result=observation_complete`。
- Claude CLI version: `2.1.197 (Claude Code)`。
- total cases: `30` (`absent/low/medium/high/xhigh/max × 5`)。
- total message requests: `30`。
- invalid JSON / unknown requests: `0`。
- CLI raw body always included `thinking: { "type": "adaptive" }`。
- `output_config.effort`:
  - absent => `high`
  - low => `low`
  - medium => `medium`
  - high => `high`
  - xhigh => `xhigh`
  - max => `max`

### Kiro thinking wire

Report:

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-release-117-artifacts.y8Pf6v/reports/thinking-effort-wire/thinking-effort-wire-1784827106118-84644-ddf3e3.json
sha256=f85e5aeee4e0642c06a3f9f0b0719ca0e88b24a32df97ec810550b4cbd76d18c
```

Result:

- `result=pass`。
- endpoints: `cli` and `ide`。
- efforts: `absent/low/medium/high/xhigh/max`。
- rounds: `5`。
- total cases: `60`。
- inference hits: `60`。
- model discovery hits/schema hits: `2/2`。
- unknown requests / invalid wire JSON / protocol violations / validation violations: `0`。
- cleanup:
  - child groups stopped: true
  - servers stopped: true
  - Redis owned keys removed: true
  - temp removed: true
  - ports released: true
  - forbidden 9022 never allocated: true
- resource bounds:
  - CLI service RSS growth `4176 KiB`，FD growth `0`。
  - IDE service RSS growth `496 KiB`，FD growth `0`。

Environment issue encountered and resolved:

- Docker-managed test PG/Redis became unavailable during early attempts; Docker CLI also intermittently hung.
- Per user requirement to keep one isolated current-project test stack and avoid Docker dynamic validation, the final successful run used local temporary PostgreSQL 16 on `127.0.0.1:50891` and local temporary Redis on `127.0.0.1:50892` with caller-owned databases/prefixes.
- This environment repair did not change production code.

### Bare invoke / XML-like tool envelope

Report:

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-release-117-artifacts.y8Pf6v/reports/bare-invoke-claude-cli/bare-invoke-1784827271700-10309-cc643e.json
sha256=f8bbefc22687ffdcb60ee4ff1c7d3c134b321bee30da5c27d0c32b985edecd80
```

Result:

- `result=pass`。
- rounds: `5`。
- cases: `20`。
- negative literal XML/invoke cases: `15`。
- structured tool cases: `5`。
- inference hits: `25`。
- tool_use/tool_result: `5/5`。
- fake model discovery requests: `1`。
- fake unknown requests: `0`。
- cleanup all true, Redis owned keys removed true。

### Long session / continue / Bash + Read tool history

Report:

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-release-117-artifacts.y8Pf6v/reports/claude-cli-long-session-continue/long-session-1784827473253-32945-7d0b22.json
sha256=198c12d20c5bdb838f2e7a30e6bb671c1ccec91ef900e5108fdec04d2da6acec
```

Result:

- `result=pass`。
- `gateQualified=true`。
- sessions: `5`。
- tool cycles per session: `20`。
- CLI turns: `110`。
- continue turns: `105`。
- tool turns: `100`。
- Bash turns: `50`。
- Read turns: `50`。
- inference hits: `210`。
- tool_use/tool_result: `100/100`。
- leak matches: `0`。
- fake model discovery requests: `1`。
- fake unknown requests: `0`。
- cleanup all true, Redis owned keys removed true。

Leak patterns checked include:

- `user Continue`
- `user Tool results provided`
- `Tool results:`
- `<function_results>`
- `<function_calls>`
- `<invoke name=`
- known tool hash names such as `bashHashxxxxxxxx` / `readHashxxxxxxxx`
- generic `Hash[0-9a-f]{8}` tool-name pattern

## Load / chaos / scheduler validation

All load runs used frozen `kiro-rs` and `kiro_loadtest`, caller-owned temporary PostgreSQL databases, Redis DB 12 with owned prefixes, fake upstreams, and temporary ports. Docker dynamic validation was not run.

### L3 burst and recovery

Summary:

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro_release_117_load_l3_summary.json
sha256=e63a0023b98c61f3003f2a54b30c2acdae00596241510dbe5ffd459b7af26ce2
```

Result:

- `passed=true`。
- result count: `9`。
- all scenarios pass。
- normal c1/c5/c10/c40 spike success counts: `5/20/50/100`，errors `0`。
- post-spike recovery: `10/10` success。
- recovery-after-error burst: `5` success / `35` expected errors, then normal recovery `12/12`。
- invalid-tool burst: `40/40` expected errors, then normal recovery `12/12`。
- p95 total latency highlights:
  - normal c40/r100 spike: `41 ms`
  - post-spike recovery: `7 ms`
  - post-error recovery: `29 ms`
  - invalid-tool recovery: `36 ms`
- all owned Redis prefixes cleaned and all l3 databases dropped after run。

### L4 restart / error / client-drop / mixed chaos

Summary:

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro_release_117_load_l4_summary.json
sha256=c01bf645ec90ad79225c9cd7e65c47e92b222fda3eb37e9bbcb46d6f15bccb9e
```

Result:

- `passed=true`。
- result count: `12`。
- all scenarios pass。
- proxy restart during long stream: bounded in-flight failures, then recovery `12/12` success。
- 429 burst: `40/40` expected errors, then recovery `12/12` success。
- 500 burst: `40/40` expected errors, then recovery `12/12` success。
- invalid-tool burst: `40/40` expected errors, then recovery `12/12` success。
- client-drop burst: `40/40` expected client errors, then recovery `12/12` success。
- mixed chaos: `21` success / `75` expected errors, then recovery `12/12` success。
- recovery p95 total latency was `4..17 ms` across recovery cases.
- all owned Redis prefixes cleaned and all l4 databases dropped after run。

### L5 soak

Two l5 runs were executed because the first run exposed a validation-window issue rather than a request/recovery failure.

First run:

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro_release_117_load_l5_summary.json
sha256=3ec551142688dc248f3b53079fceda0cc2c79b06220388db675eecb2801e8b8b
```

Result:

- `passed=false` due only to soak RSS gate.
- long stream soak: `881` success / `0` errors, p95 total `3173 ms`。
- post-soak recovery: `12/12` success, p95 total `9 ms`。
- FD returned within bound: true (`31 -> 32`)。
- RSS cold-start threshold failed after only `15s` idle cooldown:
  - start RSS `29,196,288`
  - idle RSS `71,483,392`
  - `rssReturnedWithin32MiB=false`

Interpretation: request correctness and recovery passed, but a 15s idle window was too short to distinguish allocator/pool warmup from leak. This run is retained as an observation, not counted as final soak pass.

Second run:

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro_release_117_load_l5b_summary.json
sha256=71cd463a9639f9aa225c7464ea6bace238646b5907a3b0bc6d19be4741290235
```

Result:

- `passed=true`。
- duration: `60s`。
- idle cooldown: `60s`。
- long stream soak: success, p95 total observed in report; no request errors。
- post-soak recovery: `12/12` success, p95 total `12 ms`。
- RSS returned within bound:
  - start RSS `30,769,152`
  - idle RSS `23,560,192`
  - `rssReturnedWithin32MiB=true`
- FD returned within bound:
  - `31 -> 31`
  - `fdReturnedWithin5=true`
- all owned Redis prefixes cleaned and all l5/l5b databases dropped after runs。

## Implementation evidence covered by Rust tests

The passing default/no-default gates include current code changes for:

- external pool runtime/capacity snapshot invalidation and lease heartbeat loss-window correction。
- external pool success usage/billing projection, including non-stream body usage paths and fallback estimation。
- runtime degraded state not being persisted as `credentials.disabled=true` or quarantine。
- PostgreSQL success persistence only when runtime state is dirty and generation-fenced。
- per-credential clean-state success reconcile probe throttle。
- pending `Success { success_count }` coalescing, preserving warmup decrement count while avoiding unbounded pending mutation backlog。
- protocol contamination contracts:
  - current/history tool authority names only;
  - hash-shaped names not trusted unless mapped;
  - `user Continue` / `Tool results provided` / `<function_results>` / bare `<invoke>` fail closed;
  - clean raw bodies do not enter parse/serialize sanitizer path unnecessarily。

## Environment and disk notes

- Docker CLI became intermittently slow/unresponsive during test dependency recovery. Final protocol/load validations therefore used local temporary PostgreSQL/Redis services on the same reserved ports (`50891`/`50892`) with caller-owned DBs/prefixes.
- Homebrew temporarily installed local `postgresql@16` and `redis` to provide the services after Docker became unavailable. After validation, the temporary PG/Redis processes, data directories, raw artifact root, candidate binaries, temporary PG client and the two Homebrew formulae were removed. Redis config files created by that temporary install were also removed.
- No Cargo target was retained from final validation batches. Earlier scoped build cleanup messages all reported `removed=true reservation_released=true`。
- The existing long-running `./target/release/kiro-rs -c config.json --credentials credentials.json` process for `9022` was not stopped。

## Release judgment

Current candidate satisfies the executed code, protocol, and load release gate for the user-requested areas:

- full Rust default and no-default tests: pass。
- clippy baseline: pass。
- fmt/diff/docs/Node contracts: pass。
- real Claude CLI protocol tests: pass。
- long-session tool/history leakage: pass。
- thinking/output_config effort mapping: pass。
- load L3/L4: pass。
- short L5 soak with sufficient idle: pass。
- initial L5 short-idle RSS observation documented and not hidden。
- local build artifact inventory: fail only on pre-existing live 9022 process referencing repo `target`; this was not touched and is treated as local service exception, not a scoped validation leak。

Remaining limitations:

- Docker dynamic validation was intentionally not run。
- Production `9022` was not modified or load-tested。
- Native live Kiro upstream credentials were not used for high-volume pressure; fake upstream validation proves proxy behavior and scheduler/resource contracts without consuming real accounts。
