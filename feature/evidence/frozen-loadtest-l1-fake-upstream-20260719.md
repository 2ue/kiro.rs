# Frozen loadtest L1 fake-upstream matrix

Date: 2026-07-19

Status: `L1 fake-upstream small matrix passed / L3-L5, recovery/soak, real upstream still pending`

## Scope

This evidence covers the first frozen fake-upstream load gate after the Claude CLI thinking/native-reasoning fixes. It validates that a repository-external frozen `kiro-rs` binary can handle representative `/cc/v1/messages` traffic through a Kiro-shaped fake upstream without touching the protected local `9022` service.

The run intentionally used:

- one temporary `kiro-rs` service per scenario;
- one temporary fake Kiro upstream per scenario;
- one caller-owned PostgreSQL database per scenario;
- one caller-owned Redis key prefix per scenario;
- random loopback ports, excluding `9022`;
- raw reports/logs under an owned temp root, deleted after recording summaries and SHA-256 hashes.

Docker dynamic validation was not run. The only Docker use was the current-project isolated PostgreSQL/Redis pair:

- PostgreSQL: `kiro-final-20260718-pg`, loopback `127.0.0.1:50891`
- Redis: `kiro-final-20260718-redis`, loopback `127.0.0.1:50892`

## Frozen binaries

Product binary:

```text
/tmp/kiro-frozen-20260719-r2/kiro-rs
sha256 e16df13a0ded4d53ac255f26ddc24056c4d385dde418a63944a2e00d122c642a
```

Final loadtest binary:

```text
/tmp/kiro-frozen-20260719-r5/kiro_loadtest
sha256 23c04221deb72dde601d491452d8cc9a99211df99b2cd39a386272141f2db8e3
```

The loadtest binary was rebuilt through the scoped Cargo wrapper:

```bash
env RUSTUP_TOOLCHAIN=1.92.0 \
  KIRO_FROZEN_LOADTEST=/tmp/kiro-frozen-20260719-r5/kiro_loadtest \
  feature/tests/run-cargo-scoped.sh frozen-loadtest-20260719-r5 -- \
  bash -lc 'cargo build --release --bin kiro_loadtest && install -m 755 "$CARGO_TARGET_DIR/release/kiro_loadtest" "$KIRO_FROZEN_LOADTEST"'
```

Cleanup line:

```text
validation-build-cleanup scope=frozen-loadtest-20260719-r5 size_kib=751344 available_kib=85421568 removed=true reservation_released=true
```

## Validation-tool defects found before the final pass

The initial L1 attempts were red, but the evidence showed test fixture defects rather than product regressions:

1. `kiro_loadtest --fake-only` did not emulate Kiro `ListAvailableModels`.
   - Symptom: startup/background model discovery hit the fake server but got an empty model list; native reasoning remained `Unknown`.
   - Fix: fake Kiro model discovery now returns `claude-sonnet-4`, `claude-sonnet-4-20250514`, and `claude-sonnet-4.6`, including `additionalModelRequestFieldsSchema.properties.output_config.properties.effort.enum = ["low", "medium", "high", "max"]`.

2. The fake server treated CLI `GenerateAssistantResponse` as JSON when the path was `/fixture/`.
   - Real CLI-family Kiro requests use `x-amz-target=AmazonCodeWhispererStreamingService.GenerateAssistantResponse`; the path does not necessarily contain `generateAssistantResponse`.
   - Fix: fake stream detection now recognizes `x-amz-target` and `Accept: application/vnd.amazon.eventstream`, not only `text/event-stream` or path suffixes.

3. `normal-non-stream` returned JSON even when the upstream protocol was Kiro EventStream.
   - Real behavior is: Kiro upstream stays EventStream; `kiro.rs` aggregates it into Anthropic non-stream JSON for the downstream client.
   - Fix: fake `normal-non-stream` returns Kiro EventStream when the request is a Kiro EventStream request.

4. The loadtest `--thinking true` request was invalid.
   - Previous body used `max_tokens=256` with `thinking.budget_tokens=1024`; the proxy correctly rejected it at request entry as `thinking.budget_tokens must be less than max_tokens`.
   - Fix: loadtest now uses `max_tokens=4096` for thinking payloads.

5. The fake server did not detect native Kiro reasoning controls.
   - Product wire body used `additionalModelRequestFields.output_config.effort`; the fake only detected Anthropic `thinking` or compatibility prompt tags.
   - Fix: fake thinking detection now recognizes `additionalModelRequestFields.output_config.effort` and `additionalModelRequestFields.reasoning.effort`.

6. The first slow-first-byte assertion used HTTP TTFB, but the proxy can flush protocol metadata before the first visible text.
   - Final L1 acceptance uses `firstTextMs` for user-visible slow-first-byte behavior and still records HTTP TTFB separately.

## Focused test coverage for the loadtest fixes

Scoped test batch:

```bash
env RUSTUP_TOOLCHAIN=1.92.0 \
  feature/tests/run-cargo-scoped.sh loadtest-fake-protocol-20260719-r2 -- \
  bash -lc '
    cargo test --bin kiro_loadtest fake_kiro_server_detects_cli_eventstream_by_accept_and_target -- --nocapture
    cargo test --bin kiro_loadtest fake_kiro_server_detects_model_discovery_and_reports_reasoning_schema -- --nocapture
    cargo test --bin kiro_loadtest thinking_loadtest_payload_keeps_budget_below_max_tokens -- --nocapture
  '
```

Result:

```text
3/3 focused tests passed
validation-build-cleanup scope=loadtest-fake-protocol-20260719-r2 size_kib=1124576 available_kib=85582280 removed=true reservation_released=true
```

Additional scoped test batch:

```bash
env RUSTUP_TOOLCHAIN=1.92.0 \
  feature/tests/run-cargo-scoped.sh loadtest-native-thinking-fake-20260719 -- \
  bash -lc '
    cargo test --bin kiro_loadtest fake_server_detects_native_kiro_reasoning_fields -- --nocapture
    cargo test --bin kiro_loadtest thinking_loadtest_payload_keeps_budget_below_max_tokens -- --nocapture
  '
```

Result:

```text
2/2 focused tests passed
validation-build-cleanup scope=loadtest-native-thinking-fake-20260719 size_kib=1124616 available_kib=85639348 removed=true reservation_released=true
```

## Final L1 result

Suite:

```text
l1_fake_upstream_12_case_matrix_r3
startedAt 2026-07-19T04:32:27.249Z
passed 12 / total 12 / failed 0
```

Summary table:

| Case | Requests | Status counts | Success / errors | p95 TTFB ms | p95 first thinking ms | p95 first text ms | p95 total ms | RSS start → peak → end bytes | FD start → peak → end | Report SHA-256 |
| --- | ---: | --- | --- | ---: | ---: | ---: | ---: | --- | --- | --- |
| `normal_stream` | 8 | `{"200":8}` | 8 / 0 | 32 | 0 | 32 | 34 | 27,672,576 → 35,586,048 → 35,586,048 | 29 → 30 → 30 | `3659ead5dcba92ae97a650012c3c82b368b9e7e7d585ae144e599197d1b7e143` |
| `normal_nonstream` | 6 | `{"200":6}` | 6 / 0 | 14 | 0 | 0 | 14 | 28,966,912 → 35,586,048 → 35,586,048 | 30 → 31 → 31 | `2fc2e476962c6ee5800bc9ab26b8c563f63b57e50a48938b2500e099928360f4` |
| `thinking_stream` | 6 | `{"200":6}` | 6 / 0 | 12 | 12 | 12 | 14 | 29,032,448 → 36,044,800 → 36,044,800 | 30 → 31 → 31 | `4da0da7baa8563ec257b04cc9ca9927f21cdce5e2613339d04a97600bb014f3f` |
| `tool_use_stream` | 6 | `{"200":6}` | 6 / 0 | 24 | 0 | 0 | 25 | 28,983,296 → 36,749,312 → 36,749,312 | 30 → 31 → 31 | `f7f0cc1197f495ed785f891393f241a6d9e757e8fbd07aedfe22aa120d50c278` |
| `slow_first_byte` | 4 | `{"200":4}` | 4 / 0 | 33 | 0 | 535 | 536 | 28,852,224 → 44,187,648 → 44,187,648 | 30 → 33 → 31 | `68602fe9227a0253c11206c50202cd77acfdb972d982dd6784175080213a82ba` |
| `slow_thinking_then_text` | 4 | `{"200":4}` | 4 / 0 | 15 | 517 | 517 | 519 | 29,442,048 → 44,482,560 → 44,482,560 | 30 → 33 → 31 | `958cede9c33858fe8bfcce6738ea67d777f6f274e57cae4072fd5b896d1f8dfd` |
| `json_exception200` | 4 | `{"429":4}` | 0 / 4 | 15 | 0 | 0 | 15 | 28,852,224 → 35,340,288 → 35,340,288 | 30 → 31 → 31 | `1b2ef43abf1b62b20203eb3a415a342b4af066132959c052aaeb2c3ca9b266fe` |
| `rate_limit429` | 4 | `{"429":4}` | 0 / 4 | 16 | 0 | 0 | 16 | 28,803,072 → 36,110,336 → 36,110,336 | 30 → 31 → 31 | `08e711be303d66cd9bdb378f02d6c551488fe7e95637c921660799a87a2f2a61` |
| `server_error500` | 4 | `{"429":2,"502":2}` | 0 / 4 | 29 | 0 | 0 | 29 | 28,983,296 → 36,356,096 → 36,356,096 | 30 → 31 → 31 | `7072dc99dec0326827d4e54e2910a6c94a2815db076bc743b546bc9c42e9e295` |
| `invalid_tool_format` | 4 | `{"502":4}` | 0 / 4 | 17 | 0 | 0 | 17 | 28,917,760 → 36,192,256 → 36,192,256 | 30 → 31 → 31 | `07fbee0aa4853bbf61237a9cce2cf476b212816798273b89432e2b77988a91bc` |
| `malformed_sse` | 4 | `{"429":2,"502":2}` | 0 / 4 | 20 | 0 | 0 | 20 | 28,524,544 → 34,635,776 → 34,635,776 | 30 → 31 → 31 | `78232b1c864cf496f22fb1e705a5e77d64f2890319f4994b0d478e4ce977c912` |
| `client_drop` | 4 | `{"200":4}` | 0 / 4 | 12 | 0 | 0 | 12 | 29,294,592 → 38,486,016 → 38,486,016 | 30 → 30 → 29 | `9074d55b533e25e27b8a060820dccdfd5a88665a7c51e5c7de98e4e2334f3101` |

Interpretation notes:

- `thinking_stream` now has real fake-upstream reasoning evidence: `firstThinkingMs.p95 = 12`.
- `slow_first_byte` has low HTTP TTFB because the proxy can emit protocol metadata early; the user-visible first text delay is captured by `firstTextMs.p95 = 535`.
- `client_drop` intentionally records status `200` with `errors=4` because the load client drops after the response begins; FD end is below start/peak (`30 → 30 → 29`), so the cleanup condition passed.
- Error scenarios intentionally return non-2xx public statuses and request/error IDs. With a single local fake credential, some upstream failures transition into local cooldown and therefore return `429` after the first typed upstream errors. This is acceptable for L1 classification smoke, but does not close recovery-after-burst or external-fallback behavior.

## Cleanup verification

After the final L1 run:

```text
df -h . => 81 GiB available
du -sh target => 711M
```

PostgreSQL cleanup:

```sql
SELECT datname
FROM pg_database
WHERE datname LIKE 'kiro_l1_%'
   OR datname LIKE 'kiro_l1_debug_%'
   OR datname LIKE 'kiro_l1_focus_%'
   OR datname LIKE 'kiro_l1_full_%';
```

Result: no rows.

Redis cleanup:

```bash
docker exec kiro-final-20260718-redis redis-cli --scan --pattern 'kiro_l1*'
```

Result: no keys.

Build artifact inventory:

```text
build-artifact-inventory version=2 mode=read-only targets=1 reservations=0 target_processes=1 blockers=2
target id=d61e6fde19e5 location=<repo>/target classification=unmanaged-repo-cargo-target size_kib=728088
target-process target_id=d61e6fde19e5 pid=84264 classification=kiro-runtime
release-gate result=fail
```

This fail is expected and not counted as validation residue: PID `84264` is the user’s existing `./target/release/kiro-rs -c config.json --credentials credentials.json` process. It was not started by this validation and was not stopped. No scoped Cargo target or reservation remained.

## Remaining release blockers

This evidence closes only L1 fake-upstream smoke for the current frozen product binary and the patched frozen loadtest tool. It does not close:

- L3 burst and recovery;
- L4 restart/failure chaos;
- L5 soak;
- real upstream low-concurrency validation;
- two-instance Redis/PostgreSQL scheduler validation;
- Redis usage writer + scheduler joint pressure;
- external fallback under `SchedulerRedisDegraded`;
- long Claude CLI sessions, Read/MCP/search/image/agent/resume/cache cases;
- UI browser gates;
- upgrade smoke on the final candidate;
- final C0/release inventory, since the current repo target is still owned by a user process.

Release status therefore remains `NO-GO`.
