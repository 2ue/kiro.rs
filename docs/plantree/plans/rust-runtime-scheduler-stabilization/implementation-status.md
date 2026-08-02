# Implementation Status

Last reviewed: 2026-08-02 Asia/Shanghai

Current phase:

- Current Rust runtime/scheduler stabilization plan created.
- External pool hot-path fix is implemented and locally validated.
- Wave 1 local-account WebSearch direct/CLI focused fix is implemented and live-verified on `127.0.0.1:9022`.
- Documentation migration/archival is in progress.
- Issue-status governance added: `feature/issues` now has a current blocker/status rollup, and this plan records when issue docs, the rollup index, and plan-tree state must be updated together.
- Downstream usage standard-field guard residual code fixes are implemented and focused-tested; final candidate, isolated usage-shape smoke, UI/API rollup distinction, and production recurrence remain pending.
- Account-card subscription classification now distinguishes `Pro Max` from generic `Pro`; focused Rust/UI/admin-ui validation passed, while final candidate/browser validation remains open.
- Current scoped patch release gate passed on 2026-08-01 and was published as `v0.0.130`. Broader production recurrence, image-source matrix, browser, load/chaos, and architecture gates remain post-release work rather than closed issues.
- 2026-08-02 route-policy config-authority focused pass is implemented: built-in routes remain fixed entrypoints, but cache, usage, prompt-steering, external-pool route rules, and cache namespace are selected by configuration, not by hardcoded `/cc`/`/v1`/`/ha`/`/na` path checks. The handler configuration matrix and frozen-candidate Claude Code CLI fake-upstream suite passed; live service reload, browser interaction, real CLI dynamic configuration, and production recurrence remain open until after release.

Last landed evidence:

- `external_pool_cached_immediate_availability` real local PgSQL/Redis: `2 passed / 0 failed`.
- `external_pool_immediate_availability_requires_current_capacity_and_recovers`: `1 passed / 0 failed`.
- `external_fallback`: `9 passed / 0 failed`.
- `raw_external`: `2 passed / 0 failed`.
- `preflight_external_error`: `1 passed / 0 failed`.
- `cargo fmt --check`: pass.
- `cargo check --all-targets --locked`: pass.
- clippy baseline: `811 <= 849`.
- build artifact inventory: `targets=0 reservations=0 target_processes=0 blockers=0`.
- 2026-07-31 documentation governance:
  - [Current issue status index](../../../../feature/issues/current-issue-status-index-20260731.md) created.
  - [Issue analysis priority queue](../../../../feature/issues/issue-analysis-priority-queue-20260731.md) created.
  - [Issue status governance](indexes/issue-status-governance.md) created.
  - [Single issue index](../../../../feature/issues/README.md) links to the rollup.
- 2026-07-31 Wave 1 local-account focused fixes:
  - Native WebSearch now supports the official version-family shape `web_search_YYYYMMDD`; known current versions are tracked for observability and future-looking versions are allowed through the same basic server-side WebSearch path.
  - Mixed native WebSearch now executes the server-side MCP/WebSearch branch instead of falling through to ordinary `tool_use name="web_search"`.
  - Claude Code-style `<search_web><query>...</query></search_web>` pseudo XML recovery is covered by focused stream/non-stream tests.
  - Real direct local-account validation on `127.0.0.1:9022` passed for `web_search_20250305`, `web_search_20260318`, future-looking `web_search_20270101`, and mixed native + `echo_value`: all HTTP 200 with `server_tool_use=1` and `web_search_tool_result=1`; request ids `req_01GDJcJ8QCyBm5q4j6MZzzhW`, `req_01FuBYdXdnQBk8f3rmGgMnE1`, `req_01Z1k1iXfg8qLFX38PerhnXd`, `req_01DoTcTMR8zc37cqd584eopP`; usage records show `routeKind=local_credential`, `routeSubtype=local_success`, `upstreamModel=claude-sonnet-4.5`.
  - Real Claude Code CLI `2.1.220` against local `9022/cc` with `--tools=WebSearch --allowedTools=WebSearch` passed: session `10c87c84-4b3f-4b74-81e1-de663161621f`, `toolUseNames=["WebSearch"]`, `toolResultCount=1`, no internal leak, latest usage rows local success on `claude-sonnet-4.5`.
  - Tool-name mapping observability now reports total/sanitized/overlong mapping counts.
  - Tool parsing focused matrix passed: direct live validation covered `Bash`, hyphenated names, names with spaces, overlong names, invalid schema property keys, ambiguous normalized `tool_choice`, raw-vs-mapped collision rejection, and schema-key reverse mapping.
  - Current tool-result-only turns now use Kiro content marker `Tool result received.` instead of `"."`; pre-fix direct/CLI reproduced ignored tool_result content, post-fix direct returned `direct-fixed-ok` and real Claude CLI `Bash` returned `cli-fixed-ok` with `toolResults=["cli-fixed-ok"]`; CLI request ids `req_017Ak2o1y98qaae2jLUPueiZ` and `req_01zLtw32ZEke7gsrReKEaStw`.
  - HTML-like output tag analysis refined: [HTML `<br>` output tag contamination](../../../../feature/issues/html-br-output-tag-contamination-20260731.md) has not reproduced unsolicited `<br>` in normal prose across direct, stream, tool-result, history, ambiguity, and real Claude CLI matrices; web-display and explicit standalone `<br>` prompts are legitimate pass-through controls, so no filter is selected without a real abnormal sample or bounded product rule.
  - Production usage anomaly analysis recorded: [Downstream standard usage field over 1m](../../../../feature/issues/downstream-usage-standard-field-over-1m-20260731.md) shows `input_tokens`, `cache_creation_input_tokens`, and `cache_read_input_tokens` can exceed 1m in final/persisted standard usage fields. The current-high-cache cache-creation class now has a focused fix: `finalCacheCreationMaxTokens` defaults to `400000` with deterministic `20000..45000` jitter, applied after input-delta movement and after external-pool uplift. The `/dfcache/team` `kiro_rs_tool` no-reportedUsage class and failure request-estimate standard-field split were later covered by the 2026-08-01 usage standard-field guard focused pass below.
  - Focused cache-creation usage validation passed: `anthropic::cache` `46 passed / 0 failed`; `reported_usage` `46 passed / 0 failed` plus loadtest unit `1 passed / 0 failed`; `usage_projection_final_cache` `2 passed / 0 failed`; `test_stream_final_usage_caps_cache_creation_after_input_delta` `1 passed / 0 failed`; `pnpm --dir ui check` passed.
  - Focused tests passed through scoped Cargo: `test_tool_name_mapping_summary_distinguishes_sanitized_and_overlong_names`, `native_websearch_detection`, `native_websearch_current_official_and_future_version_formats_route_to_mcp`, `literal_search_web_protocol`, `websearch_canonical_detection_and_current_long_history_query_are_exact_for_five_rounds`.
  - Additional focused tests passed through scoped Cargo: `current_tool_result_only_message_gets_content_placeholder`, `history_tool_result_only_message_gets_content_placeholder`, `current_empty_user_message_gets_inert_placeholder`, `test_schema_key_mapping_sanitizes_only_invalid_keys_and_reverses_input`, tool-name mapping/collision tests.
  - `cargo check --all-targets --locked`: pass.
- Full scoped candidate build passed before the tool-result placeholder fix: main tests `1831 passed / 0 failed / 6 ignored`, `kiro_loadtest` `31 passed / 0 failed`, release candidate SHA-256 `46ce4540fc23f121c4cc5f349e4da722db3da04f790fb3ff438b54e6a7129711`.
- Post-placeholder-fix release candidate built and restarted on local `9022`: SHA-256 `6aa907e78f26ce9eda8d36ea30fb104e73981abc05caeeb1f95d7715c2927cff`, PID `13048`.
- 2026-07-31 final candidate static/runtime recheck: Node feature tests `283 tests / 261 pass / 22 explicit skips / 0 fail`; UI and admin-ui typecheck/build plus shared API contract passed; Rust no-default/default all-target tests and release build passed; Clippy emitted `811` warnings against baseline `849`; final artifact inventory passed with `targets=0 reservations=0 target_processes=0 blockers=0`.
- 2026-07-31 current frozen-candidate Claude CLI thinking wire `60/60` passed using Claude Code CLI `2.1.220` and candidate SHA-256 `00b318aa66fa139e876acd88f7472388c7da4358aa2fef21e925c5f240cb27d7`. The first Volta-shim launch failure was isolated to the runner executable path; rerunning with `volta which claude` used the real binary and passed. This closes only the thinking/effort sub-gate; release remains blocked by the active P0/NO-GO matrix.
- 2026-08-01 local exhausted API-key quota guard focused pass:
  - [Local credential exhausted overage-disabled 400](../../../../feature/issues/local-credential-exhausted-overage-disabled-400-20260731.md) moved from implementation pending to focused verified.
  - Startup/reload now loads `credentials`, `credential_runtime_state`, and `credential_account_info` together; fresh `remaining<=0 + credit_remaining<=0 + overage_status=DISABLED` API-key snapshots derive a non-persistent quota guard and are excluded from dispatch/fallback.
  - Opaque `bad_request` / `request_body_invalid` 400 is treated as credential quota only when the selected credential already has that guard; normal generic/tool/image/malformed 400 remains fail-fast.
  - Validation passed: `quota_guard_`, isolated PgSQL manager reload regression `reload_account_info_quota_guard_reselects_healthy_credential`, `bad_request_retry_matrix_bounds_real_provider_http_hits`, `postgres_persists_runtime_config_credentials_stats_usage_and_pricing`, `kiro::token_manager` (`308` tests before adding the reload regression), full Rust all-targets (`1843 passed / 0 failed / 6 ignored` plus `kiro_loadtest` `31/31`), Node feature tests (`283 tests / 261 pass / 22 skipped / 0 fail`), `ui` check, `admin-ui` build, release build, `cargo fmt --check`, and `git diff --check`.
- 2026-08-01 usage standard-field guard focused pass:
  - [Downstream standard usage field over 1m](../../../../feature/issues/downstream-usage-standard-field-over-1m-20260731.md) moved from residual code fixes pending to focused verified.
  - No-full-`reportedUsage` local prompt-cache paths now apply final downstream-standard cache read/write caps for `CurrentHighCache` and `KiroRsTool`, covering the observed `/dfcache/team` class without enabling full projection.
  - Local credential and external pool failure records now keep request-estimate diagnostics in `total_input_tokens`/raw paths and zero downstream-standard usage fields for non-success statuses.
  - Validation passed: `standard_cache_field` (`3/3`), `usage_record` (`13/13`), `usage_projection_final_cache` (`2/2`), `external_failure_standard_usage_fields_are_zeroed_for_all_non_success_statuses` (`1/1`), `external_error` (`4/4`), `cargo fmt --check`, and `git diff --check`; evidence recorded in [Usage standard field guard focused validation](../../../../feature/evidence/usage-standard-field-guard-20260801.md).
- 2026-08-01 Pro Max subscription card focused pass:
  - [Pro Max account card subscription label](../../../../feature/issues/subscription-pro-max-card-label-20260801.md) records the root cause and fix: UI `Pro Max` branch before generic `Pro`, backend `pro_max` key/rank, and Power/Pro Max filter options.
  - Validation passed: `subscription_key_and_rank_distinguish_pro_max_from_pro` (`1/1`), `admin::service::tests` (`31/31`), UI `npm run check`, admin-ui `npm run build`, and `cargo fmt --check`.
- 2026-08-01 current scoped patch final release gate:
  - Evidence: [Final release gate - 2026-08-01](../../../../feature/evidence/final-release-gate-20260801.md).
  - Full Rust default all-targets passed: main `1850 passed / 0 failed / 6 ignored`, `kiro_loadtest` `31/31`.
  - Full Rust no-default all-targets passed: main `1850 passed / 0 failed / 6 ignored`, `kiro_loadtest` `31/31`.
  - Release build passed for both bins; a frozen `kiro-rs` candidate for CLI validation was copied outside `target` with SHA-256 `bca03a67e3744685e19f95e49b7601fd7d744575e421f140a9d895b1a7c8f3a6`.
  - UI `npm run check`, UI `npm run build`, admin-ui `npm run build`, Node contracts `283 tests / 261 pass / 22 skipped / 0 fail`, feature docs, `git diff --check`, Cargo fmt, and build-artifact inventory all passed.
  - Real Claude Code CLI `2.1.220` fake-upstream suite passed: bare-invoke, long-session `5 sessions / 110 turns / 100 tool pairs / leakMatches=[]`, and thinking-wire `60/60`.
  - The thinking-wire validation harness now treats `KIRO_EXPECTED_CLAUDE_VERSION` as optional exact mode and otherwise accepts recognizable CLI versions at or above supported minimum `2.1.197`, recording the actual version in the report.
- 2026-08-02 route-policy config-authority focused pass:
  - [Route policy config authority](../../../../feature/issues/route-policy-config-authority-20260802.md) moved from implementation-in-progress to backend/UI implemented and focused verified.
  - Runtime strategy selection no longer treats `/cc`、`/v1`、`/ha`、`/na` as immutable strategy switches; prompt steering uses `routeMode` / `routeRules`, cache namespace uses `routeNamespace`, and built-in cache defaults no longer override explicit route configuration.
  - `builtin_routes_follow_runtime_cache_and_prompt_config_matrix` passed through the scoped Cargo wrapper; it verifies `/cc -> no_cache`, `/na -> current_high_cache` shared namespace, `/ha -> current_high_cache` independent namespace, prompt steering only on `/ha`, and local `count_tokens` behavior across all built-ins.
  - Validation passed: full Rust all-targets `1864 passed / 0 failed / 6 ignored`, `kiro_loadtest 31/31`, `cargo fmt --check`, `cargo check --all-targets --locked`, `pnpm --dir ui check/build`, `pnpm --dir admin-ui build`, docs contract, prompt independence/default parity, and `git diff --check`.
  - Frozen candidate SHA-256 `fba89eb1e57947b481f38051341481662ca1c7f927a25c4ec167351cef0fcf77` passed Claude Code CLI `2.1.220` fake-upstream `bare-invoke`, `long-session` (`5 sessions / 110 turns / 100 tool pairs / leakMatches=[]`), and `thinking-wire` (`60/60`) using the real package binary rather than the Volta shim.

Active TODO:

1. Complete first archive batch for old slow-first-token/stream-fluidity analysis.
2. Decide and implement explicit local capacity overflow policy.
3. Design cooldown policy controls and manual recovery.
4. Continue Wave 1 in [priority queue](../../../../feature/issues/issue-analysis-priority-queue-20260731.md) order: image-source matrix, model-reporting clarity, broader multi-tool/history regressions, image-bearing tool_result cases, HTML-like output tag filtering decision, and any future complete mixed native/client WebSearch state-machine design.
5. For route-policy config authority, run later C1/C2 gates only when needed: isolated service reload, browser save/reload interaction, real Claude CLI dynamic behavior, and production recurrence observation.

Blocked by:

- Thinking signature Branch A vs B still requires Kiro wire capture.
- Full load/chaos requires scoped fake/local test plan and frozen binary.
- Broader production recurrence, image-source matrix, browser validation,
  multi-instance/load/chaos, and architecture gates remain open post-release.
  They are not closed by the current `v0.0.130` scoped patch release evidence.

Next target:

- Continue Wave 1 after the WebSearch/tool parsing focused fixes: image-source matrix first, then model-reporting clarity and broader multi-tool/history regressions.
