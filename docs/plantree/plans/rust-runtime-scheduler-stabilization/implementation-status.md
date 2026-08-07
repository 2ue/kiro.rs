# Implementation Status

Last reviewed: 2026-08-07 Asia/Shanghai

Current phase:

- Current Rust runtime/scheduler stabilization plan created.
- External pool hot-path fix is implemented and locally validated.
- Wave 1 local-account WebSearch direct/CLI focused fix is implemented and live-verified on `127.0.0.1:9022`.
- Documentation migration/archival is in progress.
- Issue-status governance added: `feature/issues` now has a current blocker/status rollup, and this plan records when issue docs, the rollup index, and plan-tree state must be updated together.
- Downstream usage standard-field guard residual code fixes are implemented and focused-tested; final candidate, isolated usage-shape smoke, UI/API rollup distinction, and production recurrence remain pending.
- Account-card subscription classification now distinguishes `Pro Max` from generic `Pro`; focused Rust/UI/admin-ui validation passed, while final candidate/browser validation remains open.
- Current scoped patch release gate passed on 2026-08-01 and was published as `v0.0.130`. Broader production recurrence, image-source matrix, browser, load/chaos, and architecture gates remain post-release work rather than closed issues.
- 2026-08-02 route-policy config-authority focused pass is implemented and tagged as `v0.0.131`: built-in routes remain fixed entrypoints, but cache, usage, prompt-steering, external-pool route rules, and cache namespace are selected by configuration, not by hardcoded `/cc`/`/v1`/`/ha`/`/na` path checks. The handler configuration matrix and frozen-candidate Claude Code CLI fake-upstream suite passed. The first remote Docker publish run `30757990049` failed in the Clippy baseline before image build; the bucket regression was fixed without loosening the baseline, the failed tag was explicitly recreated, and Docker publish run `30800052601` (`#162`) completed successfully. Live service reload, browser interaction, real CLI dynamic configuration, and production recurrence remain open as post-release observation.
- 2026-08-02 user ordering clarification recorded: the language-constraint first-language-lock issue and the usage-cleanup UI/semantics consistency issue were raised before the later `152.53.243.159` / `152.53.194.170` production audit, so they must be completed first. The production audit remains registered as the third task. All three remain open and are not treated as fixed by `v0.0.131`.
- 2026-08-02 language-constraint validation expanded: source inspection found no server-side first-language state; direct HTTP, real Claude Code CLI short/reverse/long-history sessions, simulated compacted-summary switches and opposite-language concurrent sessions followed the latest user language. Actual Claude Code automatic compact threshold and user-failure-transcript evidence remain open; no forced language override was implemented.
- 2026-08-02 usage cleanup UX/cache validation expanded: both UIs now share the 7-day default, queued status label, optional preview semantics and stale-preview invalidation; usage summary/dashboard Admin cache writes are synchronous to prevent cleanup-time stale-cache resurrection. UI checks/builds, focused cleanup tests, isolated PostgreSQL/Redis cleanup `41/41 x 3`, and real browser interaction passed; dynamic multi-instance cache race and production-scale performance remain open.
- 2026-08-03 usage cleanup batch limit follow-up implemented and focused-verified: `每批数量` default remains 250, backend/UI max is now 5,000, old PostgreSQL CHECK constraints migrate via `usage-cleanup-batch-size-limit-v1`, migration-disabled old schemas fail compatibility early, and both UIs disclose the same bound. Validation passed: `usage_cleanup_request 4/4`, migration test `1/1`, schema guard test `1/1`, cleanup group `42/42`, UI check/build, admin-ui build, cargo fmt/check, and Clippy baseline. Production-scale throughput and dynamic multi-instance/Redis chaos remain open.
- 2026-08-03 `v0.0.131` release recovery completed: repaired commit `511cebb` was pushed to `main`, the failed `v0.0.131` tag was deleted/recreated on that commit, and `Publish Docker Images #162` completed successfully in `25m 36s` with quality, amd64/arm64 image builds, and manifest creation all green.
- 2026-08-03 `159/170` usage-error audit completed as read-only evidence pass: both hosts run `v0.0.123`; P001 external prompt-too-long preflight and P002 usage-standard large fields map to known later fixes, P003 external retryable 5xx already exhausts both enabled pools before client 502, and P004 external 400 is a diagnostic gap rather than a retry candidate. Evidence is under `tmp/prod-evidence/20260803-025431-usage-audit-159-170/`; production recurrence after upgrade and Admin-only upstream diagnostic enhancement remain open.
- 2026-08-04 external-pool priority/direct/model/retry audit updated across 159/170/142 evidence: existing records do not prove external direct silently rescues to local credentials; the local-error samples are only-local entries such as `/dfcache/onlylocal`. `usage` 明细只保存请求决策轨迹，不保存完整运行时配置快照。Body/model P0 fixes and retry mechanics remain useful: selection is based on route/config/capacity/cooldown/model support, `请求正文模式` is only selected-pool body processing, Raw routes can retry into a normalized pool when the body parses as `MessagesRequest`, runtime external-pool requests default `anthropic-version: 2023-06-01` when the client omits it, and “外部池最多尝试” / “同池重试次数” / 跨池状态码 / 网络 / 协议 / 同池重试配置 are separated. The old 2026-08-04 conclusion that ordinary consecutive transient failures should escalate pool cooldown up to 300s is superseded by the 2026-08-05 HA target and must not be used for release. Evidence: [External pool body-mode/model routing fix 2026-08-04](../../../../feature/evidence/external-pool-body-mode-model-routing-fix-20260804.md) and superseded [External pool retry/cooldown focused validation 2026-08-04](../../../../feature/evidence/external-pool-retry-cooldown-fix-20260804.md).
- 2026-08-04 external-pool usage billing/raw-cost verification passed: stream OpenAI-compatible usage capture, raw-vs-shaped usage separation, PgSQL usage/pricing persistence, PgSQL external billing rollup, Redis Dashboard materialization, Admin UI build, docs contract, diff check and fmt all passed locally. Evidence: [External pool billing verification 2026-08-04](../../../../feature/evidence/external-pool-billing-verification-20260804.md). Production observation after rollout remains open.
- 2026-08-04 local-rescue boundary refinement is implemented and focused-verified: after a
  local-first request falls back to an external pool, external failure may return to local
  only when the fresh local route state is `Ready` with dispatchable capacity remaining.
  External direct, no-local-credential, all-disabled, unsupported-model, Redis-degraded and
  risk-circuit states remain external-only. Capacity-recovery and exhausted-capacity matrices
  passed through scoped Cargo; production recurrence remains a separate observation gate.
- 2026-08-04 scheduler architecture analysis is source-verified and design-complete for
  the current planning phase. The issue document records the real request-admission,
  local-account, external-pool, retry, cooldown, queue, fallback/rescue and WebSearch/MCP
  paths; a target RoutePlan/finite-state-machine, shared deadline/attempt semantics,
  health-aware priority overflow, configuration regrouping, `sub2api` comparison and
  validation matrix are included. On 2026-08-05 the user confirmed the core target:
  all upstream errors default to temporary turbulence, priority cannot block healthy-pool
  takeover, cooldown must be strict and auto-recovering, and external direct never falls
  back to local credentials. Implementation is now ready under the focused execution plan.
- 2026-08-04/05 target scheduler contract and compliance records:
  - [Decision 001](decisions/001-local-external-scheduler-target-contract.md) is
    `Accepted / user-core-target-confirmed / implementation-ready`; external direct
    boundaries, local-first fallback, bounded local rescue, retry/cooldown, configuration
    authority, shared deadline and observability requirements are separated from tunable
    implementation parameters.
  - [Unified target state machine and test contract](topics/scheduler-target-state-machine-and-test-contract.md)
    records the full route-mode matrix, error-action matrix, health-aware priority requirement,
    page-field semantics, staged implementation plan and real sustained HTTP validation gates.
  - [Scheduler target compliance matrix](topics/scheduler-target-compliance-matrix.md)
    records current status as focused/local partial/non-conformant/not-yet-tested. It
    explicitly identifies hard priority ordering, non-unified wait budgets and missing
    candidate/attempt observability as remaining structural gaps.
  - [Sustained scheduling validation](topics/sustained-scheduling-validation.md) defines the
    required isolated L0-L5 fake-upstream, multi-account/multi-pool, fault-wave, two-instance,
    Redis/PgSQL and 15–30 minute soak gates. No complete run has been claimed yet.
- 2026-08-05 external-pool HA scheduler execution plan added:
  [外部池高可用调度执行计划](topics/external-pool-ha-scheduler-execution-plan-20260805.md)
  defines the current P0 work: ordinary upstream errors are request-level exclusion and soft
  health signals first, not default long pool cooldown; validation must cover sustained
  errors, random errors, high concurrency, client retry amplification, recovery, route
  boundaries, usage independence and resource cleanup.
- Focused confirmation evidence is recorded in
  [scheduler target contract focused validation 2026-08-04](../../../../feature/evidence/scheduler-target-contract-focused-validation-20260804.md):
  external pool `10/10`, handler fallback/rescue `9/9`, Node contracts `104 total / 92 passed /
  12 skipped`, docs/diff pass. The evidence explicitly keeps health-aware priority overflow,
  shared end-to-end deadline, candidate/attempt observability, multi-instance races and L1–L5
  sustained scheduling open.
- 2026-08-05 Redis scheduler/usage joint-fault boundary was rechecked. The previously observed
  `latency-75-round-1` failure was not deterministic: after adding elapsed/breaker/route-state
  diagnostics to the assertion, a complete one-round matrix passed, then a three-round matrix
  passed `24/24` exact invocations. Measured 75ms scenarios stayed at `77–101ms`, while 500ms
  scenarios failed closed and recovered as designed. No production deadline or breaker threshold
  was relaxed. Evidence: [scheduler shared deadline and Redis chaos 2026-08-05](../../../../feature/evidence/scheduler-shared-deadline-and-load-chaos-20260805.md).
- 2026-08-05 final frozen candidate dynamic load gate completed with fresh caller-owned
  PostgreSQL databases and isolated Redis DBs. L3 passed `9/9`, L4 passed `12/12`, external
  priority failover passed with high-priority failure takeover/recovery and no direct-to-local
  rescue, and L5 passed `3/3` (`180s` long-stream, `1380/1380` success, `60s` idle recovery,
  RSS/FD settled). The L5 runner now warms the service with 12 successful requests before
  taking the resource baseline so allocator/connection-pool warm-up is not reported as a leak.
  Evidence is appended to [scheduler shared deadline and Redis chaos 2026-08-05](../../../../feature/evidence/scheduler-shared-deadline-and-load-chaos-20260805.md).
- 2026-08-05 external-pool HA P0 root cause fixed and release-candidate verified:
  the current process was consuming its own Redis external-pool mutation event and clearing
  the freshly merged authoritative snapshot. A per-process `origin` marker now prevents
  self-invalidation while preserving peer invalidation and legacy events without an origin.
  Three rounds of real HTTP multi-pool failover/recovery, 256-concurrency plus 1800 RPM/60s
  sustained traffic, external-direct route boundary, isolated PgSQL/Redis regression,
  full Rust (`1896/0/6 ignored` plus `kiro_loadtest 31/31`), format/diff and artifact gates
  passed. The candidate was released as `v0.0.133`; GitHub Actions `Publish Docker Images #164`
  completed successfully for quality, amd64/arm64 builds and manifest. Evidence:
  [external-pool HA scheduler validation 2026-08-05](../../../../feature/evidence/external-pool-ha-scheduler-validation-20260805.md).
- 2026-08-06 external-pool stream pre-output retry focused implementation validated:
  production/user sample
  `req_01KaWrDY5oZkY13XQqdJB9PH` shows `外部直连` stream `HTTP 200` followed by
  `external upstream emitted an error event` from `#18 yuenan-1`. Sampling against `yuenan`
  and `yuenan-1` found a recoverable-looking `message_start -> error` pattern before any
  content/thinking/tool output, while non-stream calls in the same sample succeeded. The root
  cause was code shape: external stream `HTTP 2xx` returned `Response` before reading the SSE
  body, so body-phase errors could not re-enter the cross-pool retry loop. The working tree now
  implements protocol-only SSE buffering before downstream commit, retryable external-pool
  failover on pre-output error/read/idle/EOF, global default plus per-pool override, storage/admin/UI
  wiring, and focused fake-upstream validation. User-requested normal-output and scheduling
  regression also passed for external stream/non-stream, local stream/non-stream, external direct
  stream/non-stream, local-first fallback/rescue classifier boundaries and route config authority.
  Real Claude Code CLI, load/chaos, production rollout observation and `yuenan` / `yuenan-1`
  recurrence checks remain open. Handoff:
  [external-pool stream pre-output retry](topics/external-pool-stream-pre-output-retry-20260806.md).
- 2026-08-07 user-requested normal scheduling/output rerun passed for the same stream
  pre-output retry working tree. `cargo +1.92.0` scoped batches covered external stream
  recovery/storage, external normal non-stream/stream usage, external direct stream/non-stream
  with zero local hits, local stream/non-stream success, local-first fallback/rescue classifier
  boundaries, fresh local Ready, Redis degraded and route config authority. Rust fmt/check,
  diff hygiene, feature-doc links, artifact inventory and UI/admin-ui builds also passed. This is
  still focused local evidence; real Claude Code CLI, load/chaos and production observation remained open at that checkpoint.
- 2026-08-07 final frozen candidate gate for the stream pre-output retry working tree passed:
  `kiro-rs` SHA-256 `eec71c67ce49ee9003d2cd70fae0d8ebfef1d44f72ee56bda8bb7c7ee592b688` and
  `kiro_loadtest` SHA-256 `023f3e961cdbc56e32f46f896ac66494b1a92d0e182728ddaddbeb5b8ed90e4d`.
  The scoped C0 batch passed Rust fmt, full `cargo +1.92.0 test --locked`, all-target check,
  and release build for `kiro-rs` / `kiro_loadtest`. Real Claude Code CLI `2.1.221`
  fake-upstream gates passed as bare `20/20`, long-session `5 sessions / 110 turns /
  100 tool pairs / leakMatches=0`, and thinking-wire rerun `60/60`. Frozen load/chaos passed
  L3 `9/9`, L4 `12/12`, and L5 `900s` soak with `6820/6820` long-stream successes plus
  `300s` idle RSS/FD recovery and post-soak normal recovery. Production rollout observation and
  renewed `yuenan` / `yuenan-1` recurrence checks remain post-release work.
- 2026-08-07 first remote `v0.0.134` tag publish attempt failed in GitHub Actions
  `Publish Docker Images #165` at the `quality / Frontend and Rust quality gate` before image
  publication. Local release-quality reproduction found only Clippy baseline bucket regressions
  in the new stream pre-output retry code (`derivable_impls` for the retry-mode default and
  `single_match` in the SSE commit classifier). The fix derives `Default` and rewrites the
  single-pattern match without changing runtime behavior; local release-quality Clippy baseline
  now passes with `813 <= 849`. Final local static/UI/document/artifact gates also passed after
  this lint fix. The failed tag must be deleted/recreated before republishing so GitHub treats
  the replacement tag as a new tag-created event.

Last landed evidence:

- 2026-08-06/07 external-pool stream pre-output retry focused validation:
  [External-pool stream pre-output retry validation](../../../../feature/evidence/external-pool-stream-pre-output-retry-validation-20260806.md).
  `external_pool_stream_` real local PgSQL/Redis `6/6`; storage persistence `1/1`;
  pre-output effective mode/classifier `2/2`; normal external stream/non-stream usage/body
  targeted tests `8` commands passed; routing/config classifier batch passed across external
  fallback, direct external, local preflight, fresh local Ready, Redis degraded, local rescue,
  normalized external stream/non-stream fallback and route config authority; `cargo check
  --all-targets --locked`, `cargo fmt --all -- --check`, `git diff --check`, UI/admin-ui
  gates and build artifact inventory passed. 2026-08-07 rerun used `cargo +1.92.0` and added
  exact external normal-output/usage checks plus UI/admin-ui build reruns. Final frozen candidate
  CLI/load gates also passed: Claude CLI bare `20/20`, long-session `110 turns`, thinking-wire
  rerun `60/60`, load L3 `9/9`, L4 `12/12`, L5 `900s` soak `6820/6820` with RSS/FD recovery.
  The release-quality Clippy recovery rerun passed after the behavior-neutral lint fix:
  `813 <= 849`; the post-fix Rust fmt/check, diff hygiene, feature docs, UI/admin-ui builds and
  build artifact inventory also passed.
  Production rollout observation remains pending.
- `external_pool_cached_immediate_availability` real local PgSQL/Redis: `2 passed / 0 failed`.
- `external_pool_immediate_availability_requires_current_capacity_and_recovers`: `1 passed / 0 failed`.
- `external_fallback`: `9 passed / 0 failed`.
- `raw_external`: `2 passed / 0 failed`.
- `preflight_external_error`: `1 passed / 0 failed`.
- `cargo fmt --check`: pass.
- `cargo check --all-targets --locked`: pass.
- clippy baseline: `813 <= 849`.
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
  - Tagged as `v0.0.131`: work commit `4981285`, follow-up task registration commit `89cb4fc`, release commit `59b4c26`, tag `v0.0.131`; remote Docker publish failed before image build because Clippy baseline detected one new `src/model/config.rs` bucket warning.
- 2026-08-03 usage cleanup batch-limit and `v0.0.131` failure evidence:
  - Evidence: [Usage cleanup batch limit and v0.0.131 publish failure](../../../../feature/evidence/usage-cleanup-batch-limit-and-release-131-20260803.md).
  - `每批数量` default 250 / max 5,000 implemented across backend, PostgreSQL constraint migration, migration-disabled compatibility guard, `ui`, and `admin-ui`.
  - Validation passed: `cargo fmt --all -- --check`, `cargo check --locked --all-targets`, Clippy baseline `811 <= 849`, `usage_cleanup_request 4/4`, migration test `1/1`, schema guard test `1/1`, cleanup group `42/42`, `pnpm --dir ui check/build`, and `pnpm --dir admin-ui build`.

Active TODO:

1. Publish the verified `v0.0.134` candidate.
2. Observe the three production deployments after rollout without changing the local `9022`
   service or usage calculation chain.
3. Finish first archive batch for old slow-first-token/stream-fluidity analysis.
4. Continue candidate rejection observability and model-field display improvements as separate
   follow-up work.

Blocked by:

- External-pool HA P0 `v0.0.133` release candidate itself has no local blocker. The current
  working tree contains a separate stream pre-output retry release candidate whose local CLI/load
  gates have passed. Production observation is post-release and must not be claimed before rollout.
- Thinking signature Branch A vs B, image-source matrix, browser follow-up, and broader
  architecture gates remain independent open items and are not part of this P0 release.

Next target:

- Publish `v0.0.134`. Keep production observation for the new stream follow-up separate from
  already-published `v0.0.133`.
