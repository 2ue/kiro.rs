# Final candidate C0d static, Claude CLI capture, loadtest fake matrix, UI build, and artifact inventory

Status: `current-candidate-pass / dynamic-pg-redis-real-upstream-gates-still-open / no-release`

Date: 2026-07-21 UTC (`2026-07-22 00:47-00:57` Asia/Shanghai for the local file mtimes)

Candidate:

- `kiro-rs`: `/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T//kiro-final-candidate-d.KBNESC/kiro-rs`
- `kiro-rs` SHA-256: `fefd6204c1851c9795ae16fb006115997f7884570988622a77200c3e438cd7ec`
- `kiro_loadtest`: `/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T//kiro-final-candidate-d.KBNESC/kiro_loadtest`
- `kiro_loadtest` SHA-256: `f92e91b4f9c2d669e29e6bbb9e4d4b58f38d2f8bfac3f4bd51260c0d2edd6782`
- C0 log SHA-256: `89bc66f8f262b08b1322baf47b91c9a733b2b5eec53b686cac13755ef109435e`
- C0 log lines: `2322`

## Commands and results

### C0d all-target tests and release build

Command:

```bash
KIRO_FROZEN_BINARY="$candidate_root/kiro-rs" \
KIRO_FROZEN_LOADTEST="$candidate_root/kiro_loadtest" \
feature/tests/run-cargo-scoped.sh final-candidate-c0d -- bash -lc '
  cargo fmt --check &&
  cargo test --all-targets &&
  cargo build --release --bins &&
  install -m 755 "$CARGO_TARGET_DIR/release/kiro-rs" "$KIRO_FROZEN_BINARY" &&
  install -m 755 "$CARGO_TARGET_DIR/release/kiro_loadtest" "$KIRO_FROZEN_LOADTEST"
'
```

Result:

- `cargo fmt --check`: pass.
- `cargo test --all-targets`: pass.
  - main test binary: `1742 passed; 0 failed; 6 ignored; finished in 405.86s`.
  - `kiro_loadtest` tests: `31 passed; 0 failed; finished in 2.39s`.
- `cargo build --release --bins`: pass, `Finished release profile [optimized] target(s) in 7m 57s`.
- scoped target cleanup: `validation-build-cleanup scope=final-candidate-c0d size_kib=2446284 available_kib=82943832 removed=true reservation_released=true`.

The prior stale prompt-master expectation and OAuth recovery flake were both rechecked in this run. The OAuth recovery exact test passed inside the full all-target run after the test-only fake endpoint header timeout was relaxed to `1000ms`; the production timeout path was not changed by that test-only constant.

### Static/document/UI/source contracts

Commands:

```bash
git diff --check
node feature/tests/check-feature-docs.mjs
node feature/tests/cost-format-contract.mjs
node feature/tests/mcp-attempt-channel-contract.mjs
node feature/tests/request-api-key-id-contract.mjs
node feature/tests/prompt-control-independence.mjs
node feature/tests/prompt-default-parity.mjs
node --test feature/tests/*.test.mjs
```

Result:

- `git diff --check`: pass; empty log hash `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.
- feature docs: `47 issue documents`, `108 relative links`, pass.
- cost format contract: pass for `ui` and `admin-ui`.
- MCP attempt channel contract: pass for both UI contracts.
- request API key ID contract: pass for `ui` and `admin-ui`.
- prompt control independence: pass; both UI surfaces keep prompt master state separate from body conversion state and document the total prompt gate.
- prompt default parity: pass; Rust, UI, and Admin UI task-quality defaults match and contain no internal transcript fingerprints.
- full Node source/runner contract batch: `280 tests`, `258 pass`, `22 explicit skips`, `0 fail`, duration `75617.155833ms`.

Selected log hashes:

- `check-feature-docs.log`: `d7f0da8c1fb64962b4b805a98244173cfdd22eae401ac0dec7594e50bd16ae77`
- `node-all.log`: `b1dd7f1b08de16d2a72fc3d25658437761150d3644fdf6d659e77ac63c5c6c67`
- `prompt-control-independence.log`: `24c1b217320028296c4f3a8aa3cf25dcca5548250683dc0aa2317dfdaaee50da`
- `prompt-default-parity.log`: `ce829b647ff7406a8cbe594b7673093849fbf0b6659e2b165ad163bf7c82c5c1`

The `22` Node skips are explicit live-fixture skips and are not counted as dynamic product passes.

### Claude Code CLI raw thinking/effort capture

Command:

```bash
node feature/tests/thinking-effort-claude-cli-capture.mjs
```

Result:

- Claude Code CLI version: `2.1.197 (Claude Code)`.
- `absent`, `low`, `medium`, `high`, `xhigh`, `max`: each `5` isolated sessions.
- For every effort class, captured top-level body keys included `thinking` and `output_config`.
- `thinkingVariants`: always `{ "type": "adaptive" }`.
- `outputConfigVariants`:
  - absent case defaults to `{ "effort": "high" }`.
  - explicit `low`, `medium`, `high`, `xhigh`, `max` are preserved exactly.
- Model in captures: `claude-opus-4-8`.
- `stream`: always `true`.
- isolation: per-case isolated HOME, Claude config and project; forbidden port list includes `9022`; `protected9022ProbeSkipped: true`.
- cleanup: children stopped, fake port released, temp root removed.
- wall duration: `23027ms`.
- log SHA-256: `95faf7c33eee5e8a3286d18c3bb304679bd18670ceb8032f498c93a5b1f9b0e9`.

This confirms the current installed Claude CLI itself does not clamp `max` to `high` before the proxy. It also confirms the CLI sends `thinking.type=adaptive` together with `output_config.effort`.

### Public protocol cross-check for thinking/effort

The external documentation used for this check is current as of the crawl/search on 2026-07-21:

- Kiro CLI `/effort` docs describe the effort command and effort levels for reasoning behavior: <https://kiro.dev/docs/cli/chat/effort/>.
- Kiro model docs describe Opus adaptive thinking availability in Kiro IDE/CLI: <https://kiro.dev/docs/cli/models/>.
- AWS Bedrock Claude adaptive thinking docs state that `effort` belongs in a separate `output_config` object and not inside `thinking`: <https://docs.aws.amazon.com/bedrock/latest/userguide/claude-messages-adaptive-thinking.html>.
- Claude Platform effort/adaptive thinking docs describe `output_config.effort` and `thinking: { "type": "adaptive" }` as related but separate controls: <https://platform.claude.com/docs/en/build-with-claude/effort> and <https://platform.claude.com/docs/en/build-with-claude/adaptive-thinking>.

This cross-check supports the local contract: do not silently clamp `max` to `high`, do not invent unsupported upstream `thinking` fields, and keep structured client capability fields independent from prompt-steering text switches.

### UI builds

Commands:

```bash
pnpm --dir ui build
pnpm --dir admin-ui build
```

Result:

- `ui`: pass; `tsc -b && vite build`, `2458 modules transformed`, built in `5.85s`.
- `admin-ui`: pass; `tsc -b && vite build`, `1777 modules transformed`, built in `4.67s`.
- `ui` emitted one Vite chunk-size warning for chunks above `500 kB`; this is not a build failure.
- `git status --short ui admin-ui` after build showed only existing source changes/new files, no tracked `dist` churn.
- log hashes:
  - `ui-build.log`: `f4da16ca2999ad82837378a8cb2f84d513419b7faefe119320a2ff190018b1c7`
  - `admin-ui-build.log`: `e526e2b5d71699562db8424265648d14e8444a2fca0124792a4a5a70f03200e6`

Browser/UI interaction gates are still open; this evidence only closes local TypeScript/Vite build for both UI packages.

### `kiro_loadtest` fake-upstream smoke/matrix

The C0d frozen `kiro_loadtest` was run directly against its own loopback fake Kiro server. This does not prove the `kiro-rs` proxy runtime because it does not start the proxy or use PG/Redis. It does bind the current loadtest parser/reporting binary and fake upstream fixtures to the C0d candidate.

Single normal stream smoke:

- requests: `5`
- success/errors: `5/0`
- status: `200:5`
- `ttfbMs.p95=5`, `firstTextMs.p95=5`
- report hash: `f0c32e39ae31beae20e4df735c5034fd9fa56ae8c24c72a3fa5c238271f18fa5`

Matrix result:

| Scenario | Requests | Success | Errors | Status counts | Notable p95 |
| --- | ---: | ---: | ---: | --- | --- |
| normal-stream | 5 | 5 | 0 | `200:5` | `ttfb=3ms`, `firstText=3ms` |
| normal-non-stream | 5 | 5 | 0 | `200:5` | `ttfb=1ms` |
| slow-first-byte | 5 | 5 | 0 | `200:5` | `ttfb=253ms`, `firstText=253ms` |
| slow-thinking-then-text | 5 | 5 | 0 | `200:5` | `firstThinking=1ms`, `firstText=254ms` |
| tool-use-stream | 5 | 5 | 0 | `200:5` | `ttfb=1ms` |
| json-exception200 | 5 | 0 | 5 | `200:5` | classified as errors |
| rate-limit429 | 5 | 0 | 5 | `429:5` | `errorIds=5` |
| server-error500 | 5 | 0 | 5 | `500:5` | `errorIds=5` |
| invalid-tool-format | 5 | 0 | 5 | `400:5` | `errorIds=5` |
| malformed-sse | 5 | 0 | 5 | `200:5` | classified as errors |
| client-drop | 5 | 0 | 5 | `200:5` | classified as errors |
| mixed-chaos | 12 | 9 | 3 | `200:9`, `429:1`, `500:1`, `transport_error:1` | `ttfb=10003ms`, `firstText=10003ms` |

Recovery-after-burst was rerun with enough requests to cross the recovery threshold:

- `--requests 15 --concurrency 3 --fake-recover-after 5`
- result: `15 requests`, `11 success`, `4 errors`, status `200:11`, `500:4`
- `ttfbMs.p95=6`, `fd start/peak/end=13/19/19`
- report hash: `1f0f6d92adee7484bdb80ff2a2eb87f67c7e2a0f2cc413754c72effd95b32faf`

Selected report hashes:

- `normal-stream.json`: `d8b2ac5b4ae1caf46b224f79425cc3ab819251c65017af29534f6f3e8b0802cc`
- `slow-thinking-then-text.json`: `dafd6407fd620bbca7261dfe856a095582292769ce1060c4952bf40ecf1c1aaa`
- `tool-use-stream.json`: `b3cc6669f14ad781b3f9af9baa9806243b14783e77a3f9197d3c6f93a7270de7`
- `mixed-chaos.json`: `23a4b5fca887c3e706ff8419f2819d8f99e3a77cad71f61520628e0e5ec42b3d`

Raw temp roots for the static, Node, CLI capture, UI build and loadtest runs were deleted after hashes and summaries were recorded.

### Artifact inventory and cleanup

After C0d:

```bash
node feature/tests/inventory-build-artifacts.mjs --gate
```

Initial inventory failed because root `target/` had been recreated by editor/flycheck artifacts and PID `84264` still referenced a historical `./target/release/kiro-rs` executable/log for the protected `9022` service. The active service was not stopped and its referenced files were not deleted.

Read-only inspection showed the visible root target contents were only:

- `target/debug`: about `916M`
- `target/flycheck0`: about `332K`
- `target/.rustc_info.json`

`lsof` showed PID `84264` as an existing user/historical service on `127.0.0.1:9022`:

```text
./target/release/kiro-rs -c config.json --credentials credentials.json
```

No live references existed to the visible `target/debug`, `target/flycheck0`, or `.rustc_info.json`, so only those visible, reproducible artifacts were removed. The protected service was not touched.

Final inventory:

```text
build-artifact-inventory version=2 mode=read-only targets=0 reservations=0 target_processes=0 blockers=0
process-inspection complete=true ps=complete open_files=lsof-cwd-txt
temp-scan roots=1 entries=3402 unreadable=0 truncated=false strategy=bounded-known-prefixes
docker status=inspected-read-only cleanup=manual-only hint=docker-system-df-and-builder-prune-require-manual-review
release-gate result=pass
```

## Release interpretation

This evidence moves the C0/static/build/Node/CLI-ingress/UI-build/loadtest-fake-tooling gates forward for the C0d candidate, but it does not make the release GO.

Still open / not counted as pass:

- caller-owned PostgreSQL/Redis dynamic service runners:
  - thinking wire full service run,
  - bare-invoke service run,
  - long-session rebind to C0d candidate,
  - external takeover dynamic,
  - E01/E02 distribution/sticky/lease race dynamic,
  - E05 strict-local-first full matrix,
  - F06 lifecycle dynamic,
  - request API key admission multi-instance dynamic,
  - final L3/L4/L5 `kiro-rs` proxy load/chaos rebind,
  - token-refresh cluster and multi-instance Redis coordination reruns.
- real Kiro upstream thinking delta/usage and active/passive thinking long-session gates.
- native Claude CLI MCP/search/image/agent capability gates.
- UI browser interaction gates.
- v101/v102/v103 upgrade smoke on a release-bound candidate.
- final production recurrence audit and final inventory immediately before release.

Current release status remains `NO-GO`.
