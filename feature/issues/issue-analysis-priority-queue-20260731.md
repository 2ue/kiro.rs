# Issue analysis priority queue - 2026-07-31

Status: `active-analysis-queue / ordered-easy-to-hard-with-urgency-first`

Severity: P0/P1 execution control. This queue defines the order for analyzing current open issues one by one, balancing easiest safe progress against urgent release/user-impact blockers.

Last reviewed: 2026-07-31 Asia/Shanghai

## 范围与结论

This document is the execution order for issue analysis. It does not replace:

- [Current issue status index](current-issue-status-index-20260731.md)
- [Issue status governance](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/indexes/issue-status-governance.md)
- Individual `feature/issues/*.md` root-cause files
- Dated evidence in `feature/evidence/`

Ordering rule:

1. Start with urgent user-facing P0/P1 problems that have a small blast radius and fast local feedback.
2. Then run release-blocking validation gaps that are broad but already have focused fixes.
3. Then handle hard distributed scheduler/storage gates.
4. Then close UI/dashboard/upgrade/general product polish.

This gives an easy-to-hard path without ignoring urgency. If a later issue is actively breaking production, it can be promoted, but the promotion must be recorded in this queue and in the owning plan-tree status if it changes roadmap priority.

## 根因

The project currently has many issues in mixed states: some are true `NO-GO`, some are `fixes-pending`, some are implemented but awaiting final candidate or real CLI/upstream/browser/load evidence. Without a single ordered queue, work can drift into broad hard gates before closing smaller urgent regressions, or focused fixes can be mistaken for final closure.

The analysis queue therefore separates:

- immediate local-account compatibility defects;
- release-gate validation work;
- hard distributed scheduler/storage work;
- product/UI/upgrade follow-up.

## 复现 / How to refresh the queue

Before starting a batch, refresh status lines:

```bash
for f in feature/issues/*.md; do
  st=$(rg -m1 '^Status:' "$f" | sed 's/^Status: //')
  sev=$(rg -m1 '^Severity:' "$f" | sed 's/^Severity: //')
  printf '%s\t%s\t%s\n' "$(basename "$f")" "$st" "$sev"
done | sort
```

Then check whether any item below has moved category. If yes, update this queue, [current issue status index](current-issue-status-index-20260731.md), and the owning issue file.

## Priority queue

### Wave 1: urgent and comparatively small

| Order | Issue | Why first | First analysis action | Exit criteria |
| ---: | --- | --- | --- | --- |
| 1 | [Claude Code local accounts WebSearch/tools/image](claude-code-local-accounts-websearch-tools-image-analysis-20260729.md) | Current user-facing fatal symptoms; local accounts are available; scope is concrete | Split into four sub-issues: mixed WebSearch, CLI WebSearch capability, tool mapping observability, image-source matrix | Each sub-issue has a chosen behavior, focused test plan, and either a small fix PR target or a clear defer decision |
| 2 | Tool name mapping observability inside the local-account issue | Low-risk code/log/test work; directly explains "tools parse may be wrong" | Inspect `src/anthropic/converter/tools.rs` and response reverse mapping; decide log wording and collision/tool_choice tests | Log no longer says only "overlong" for normal sanitized names; tests cover collision and original-name tool_choice |
| 3 | Mixed native WebSearch behavior | High user impact; source gate is narrow (`tools.len() == 1`) | Decide fail-closed vs server-side execution for native WebSearch mixed with normal tools | Mixed request no longer returns ordinary `tool_use web_search` without an executor |
| 4 | Image intermittent paths | User-facing; simple valid/invalid controls already exist | Add source-path matrix: inline, media mismatch, invalid, tool_result text+image, multiple images, file, remote URL, size/timeout | Each image-source class has direct evidence and the image issue status is updated |
| 5 | Model reporting for local accounts | Small observability fix; prevents false 4.6/5 diagnosis | Trace response `model`, requested model, resolved model, upstream model in usage/logs | Docs and/or response diagnostics make requested vs upstream model unambiguous |

Wave 1 progress on 2026-07-31:

- Order 1 WebSearch focused capability fix landed and was live-verified through local accounts: direct native `web_search_YYYYMMDD` accepts official `20250305` / `20260318`, accepts future-looking `20270101`, and mixed native + ordinary tool returns server-side `web_search_tool_result`; real Claude Code CLI `2.1.220` with `--tools=WebSearch --allowedTools=WebSearch` produced `toolUseNames=["WebSearch"]` and one `tool_result`.
- Order 2 focused fix landed: tool-name mapping logs now report total mapped, sanitized, and overlong counts instead of implying every mapping is an overlong shortening. Focused regression: `test_tool_name_mapping_summary_distinguishes_sanitized_and_overlong_names`.
- Order 2/1 tool parsing focused fix landed: direct live matrix now covers `Bash`, hyphenated names, names with spaces, overlong names, invalid schema property keys, ambiguous normalized `tool_choice`, and raw-vs-mapped collisions. A real bug was found and fixed where current tool-result-only turns used `"."` as Kiro content and could make CLI follow-up ignore valid `tool_result`; new marker is `Tool result received.`, with post-fix direct `direct-fixed-ok` and real CLI `cli-fixed-ok`.
- Order 3 focused fix landed: mixed native WebSearch no longer falls through to ordinary `tool_use name="web_search"`; it executes the native server-side MCP/WebSearch branch. Focused regression: `websearch_canonical_detection_and_current_long_history_query_are_exact_for_five_rounds` now asserts pure native MCP, same-name custom normal path, and mixed native MCP.
- User-reported HTML-like output tag issue refined after clarification: [HTML `<br>` output tag contamination](html-br-output-tag-contamination-20260731.md) has not reproduced unsolicited `<br>` in normal prose across direct, stream, tool-result, history, ambiguity, and real Claude CLI matrices. Web-display and explicit standalone `<br>` prompts are pass-through controls, not abnormal reproductions; filtering remains unselected unless a real unsolicited sample or bounded product rule is captured.
- Production usage anomaly recorded: [Downstream standard usage field over 1m](downstream-usage-standard-field-over-1m-20260731.md) shows final/persisted standard fields above 1m on three deployments. Focused standard-field guards are now implemented and tested: reported-usage cache creation has `finalCacheCreationMaxTokens=400000` plus deterministic `20000..45000` jitter; no-full-`reportedUsage` local prompt-cache paths cap standard cache read/write fields, including `kiro_rs_tool`; local credential and external pool failures keep request estimates in diagnostics and zero downstream-standard fields. Remaining gates are frozen/isolated usage-shape smoke, dashboard/API rollup distinction, and production recurrence.
- Remaining Wave 1 work starts with the residual usage classes from Order 19 plus Order 4/5 and broader multi-tool/history regressions from Order 1.

### Wave 2: release-blocking protocol gates with existing focused evidence

| Order | Issue | Why now | First analysis action | Exit criteria |
| ---: | --- | --- | --- | --- |
| 6 | [Protocol capability regression matrix](protocol-capability-regression-matrix.md) | P0 release gate; many fixes depend on it | Convert matrix into executable gate batches: text, thinking, tools, media, WebSearch/MCP, errors, resume | Frozen candidate gate plan exists and failed/missing cells are assigned to owning issues |
| 7 | [Thinking signed content safety](thinking-and-signed-content-safety.md) | P0, implemented-unit-verified but CLI/e2e missing | Define C2/C3/C4 real CLI and long-history gates | Real CLI/e2e gaps are either run or explicitly blocked with requirements |
| 8 | [Payload guard semantics](payload-guard-semantics-limits-and-performance.md) | P0/P1, focused pass but final CLI/load missing | Identify remaining B05/L5 and 50 MiB/RSS cases | Frozen load/CLI residual list is short and executable |
| 9 | Stream terminal/fault state group | P0/P1 stream correctness; multiple historical docs | Group [idle timeout](02-stream-upstream-idle-timeout.md), [status error](06-stream-upstream-status-error.md), [internal read](07-stream-internal-read-error.md), [precommit retry](stream-terminal-errors-and-precommit-retry.md) into one fault-gate run | One unified stream fault matrix replaces scattered pending gates |

### Wave 3: urgent but harder scheduler/storage blockers

| Order | Issue | Why harder | First analysis action | Exit criteria |
| ---: | --- | --- | --- | --- |
| 10 | [Local capacity preflight race](local-capacity-preflight-race-and-external-fallback-latency.md) | `NO-GO`, needs real PG/Redis and burst evidence | Start with LQ04 dynamic storage cases, then LQ05 40x15 burst | LQ04-LQ08 statuses updated with pass/fail evidence |
| 11 | [Strict local-first and multi-instance](strict-local-first-distribution-and-multi-instance.md) | `release-blocked`, distributed service runner required | Run or prepare E01/E02 dynamic distribution before E05 takeover | E01/E02/E05 no longer remain only runner-contract evidence |
| 12 | [Retry budget / RPM amplification](retry-budget-admission-and-rpm-amplification.md) | `NO-GO`, cross-path attempts and client retry | Map unfinished gates to request API key, token refresh, external eligibility and real CLI runs | Remaining retry/RPM gates are bounded and assigned |
| 13 | [Token refresh failure wave](token-refresh-failure-wave-and-cluster-rpm.md) | `NO-GO`, two-replica Redis/provider/frozen gaps | Start TR11/TR12 because they prove aggregate bound and Redis failure behavior | Cluster open cells are reduced to provider/frozen-only or closed |
| 14 | [Redis usage writer atomicity](redis-usage-writer-atomicity-cardinality-and-scheduler-isolation.md) | `NO-GO`, correctness + scheduler isolation | Run multi-instance and production-cardinality validation design first | Production-cardinality and scheduler p95/p99 acceptance are explicit |

### Wave 4: external pool and WebSearch/MCP follow-up

| Order | Issue | Why after Wave 3 | First analysis action | Exit criteria |
| ---: | --- | --- | --- | --- |
| 15 | [External pool Redis coordination/release](external-pool-redis-coordination-and-release.md) | P0 but focused coordinator evidence exists | Reconcile eligibility hot-path remediation with local capacity policy | Release-candidate gates are concrete and not duplicated with Wave 3 |
| 16 | [External pool authoritative selection/fence](external-pool-authoritative-selection-and-dispatch-fence.md) | Important, but mostly fixed-in-dirty-tree | Identify frozen load and two-instance missing cells | Selection-to-send race evidence is final-candidate bound |
| 17 | [External SSE/profile safety](external-pool-profiles-and-sse-safety.md) | P0 external compatibility | Define handler CLI/load pending cases | External raw/normalized/SSE behavior has a single pass/fail matrix |
| 18 | [WebSearch/MCP protocol](websearch-mcp-protocol-usage-and-privacy.md) | Implemented/focused, but auxiliary/prod/full-mixed gates open | Reconcile direct native `web_search_YYYYMMDD`, current mixed native server-side execution, CLI client tool evidence, auxiliary attribution, production recurrence, and any future true mixed state-machine design | WebSearch issue statuses no longer contradict each other |

### Wave 5: storage, dashboard, UI, release hygiene

| Order | Issue | Why later | First analysis action | Exit criteria |
| ---: | --- | --- | --- | --- |
| 19 | [Downstream standard usage field over 1m](downstream-usage-standard-field-over-1m-20260731.md), [Usage cleanup](usage-cleanup-safety-and-redis-isolation.md), and [Usage dashboard P95](usage-dashboard-p95-and-window-semantics.md) | P1 usage correctness/perf; production evidence shows implausible final standard fields | Standard-field code residuals are implemented and focused-tested; next split frozen/isolated usage-shape validation from dashboard display/API semantics and production recurrence | Standard usage fields are bounded and dynamic multi-instance/runtime re-verification gates are explicit |
| 20 | [Dashboard redesign](dashboard-observability-redesign.md) | Implementation-in-progress but less urgent than request success | Turn product contract into phase checklist: time semantics, cost, account quality, errors, query isolation | Each dashboard phase has API/UI/test acceptance |
| 21 | [Two UI cost/config authority](two-ui-cost-precision-and-config-authority.md) and [AWS key lifecycle](aws-kiro-api-key-region-lifecycle.md) | Browser/final build gates pending | Prepare browser gate scripts or document blocker | UI/browser pending status is closed or accurately blocked |
| 22 | Upgrade/release/artifact group | Release hygiene after blockers | Rebind [upgrade smoke](upgrade-v101-v102-v103-smoke.md), [migration atomicity](postgres-startup-migration-atomicity.md), [artifact lifecycle](validation-build-artifact-lifecycle-and-disk-safety.md) to final candidate | Final release binary and inventory states are current |
| 23 | Historical issue refresh | General cleanup | Reclassify historical production docs that only have pending gates | Historical docs cannot be mistaken for current blockers |

## Per-issue analysis template

Use this template when starting an item:

```md
### <Issue name>

- Current status:
- User impact:
- Why now:
- Current evidence:
- Suspected root cause:
- Smallest next validation:
- Expected fix direction:
- Files likely touched:
- Evidence needed to close:
- Docs to update:
```

## 方案 / Operating plan

Work one issue at a time inside the current wave:

1. Read the owning issue and linked evidence.
2. Confirm whether the issue is still true against current source/config when cheap.
3. Decide whether the next action is code fix, test/evidence run, product decision, or archive/status update.
4. Update the owning issue before or with implementation.
5. Update [current issue status index](current-issue-status-index-20260731.md) if the category changes.
6. Update plan-tree if the issue changes roadmap, release blocker, or active TODO state.

Do not skip ahead to broad load/chaos gates while Wave 1 local-account regressions are still unresolved, unless a production outage or user instruction reprioritizes the queue.

## 验收与证据

This queue is accepted when:

- it is linked from [feature issue index](README.md);
- it is linked from [current issue status index](current-issue-status-index-20260731.md);
- plan-tree governance references it as the current execution order;
- `node feature/tests/check-feature-docs.mjs` passes;
- touched relative links resolve.

## 残余风险与回滚

Residual risks:

- The queue is manually ordered from documented state. It does not independently prove current runtime truth.
- Urgency can change if production evidence appears or a user explicitly reprioritizes.
- Some easy-looking items can uncover cross-cutting behavior; if so, promote the discovered dependency rather than continuing blindly.

Rollback:

- If this queue becomes stale, keep it as a dated snapshot and create a new current queue; do not rewrite old evidence as if it was current.
- Do not delete individual issue files when reordering; update links and status instead.
