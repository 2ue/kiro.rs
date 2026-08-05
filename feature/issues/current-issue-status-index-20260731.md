# Current issue status index - 2026-07-31

Status: `active-index / derived-from-feature-issues / update-required-with-issue-state-changes`

Severity: P0/P1 documentation control. This file prevents open blockers, partially fixed issues, and final validation gaps from being lost across sessions.

Last reviewed: 2026-08-05 Asia/Shanghai

## 范围与结论

This index summarizes the current state recorded in `feature/issues/*.md`. It is a navigation and status-control document, not a replacement for the individual issue files.

Current execution order: [Issue analysis priority queue - 2026-07-31](issue-analysis-priority-queue-20260731.md).

Current release note for 2026-08-01: the user authorized releasing the
current scoped patch batch once its validation completed. That batch passed
the final release gates and was published as `v0.0.130` from commits
`209ad30` and `d05a959`, with annotated tag `v0.0.130`. The `NO-GO` rows below
are broader production,
load/chaos, browser, image-source, and architecture gates that remain open as
post-release work; they are not reclassified as closed by this scoped release.

Current release note for `v0.0.131`: the first remote Docker publish attempt
(`30757990049`) failed in “Check Clippy warning baseline” before Docker
build/manifest. The bucket regression was fixed without loosening the baseline;
the failed tag was explicitly recreated on repaired commit `511cebb`, and
workflow `30800052601` (`Publish Docker Images #162`) completed successfully
with quality, amd64/arm64 builds, and manifest creation all green.

Current inventory from the 2026-08-05 external-pool HA scheduler target update:

| Metric | Count | Meaning |
| --- | ---: | --- |
| Issue documents scanned | 74 | Markdown files under `feature/issues/` covered by the feature-doc contract, excluding the local README |
| `NO-GO` / `release-blocked` / `release-blocking` statuses | 6 | Broad gates normally block a full closure release unless explicitly superseded or scoped out by the current release decision |
| Implementation/fix still pending | 3 | Analysis exists, but implementation or productization is not complete |
| Any `pending` / `open` / `partial` / `gates-open` / `not released` status | 53 | Most issue records still require a validation, rollout, or status-refresh action |
| Fixed/implemented but final validation still pending | about 42 | Code or focused tests exist, but final candidate, real CLI, real upstream, browser, load, or production recurrence evidence is missing |

The practical conclusion is:

- `docs/plantree/` already manages durable planning, roadmap state, and release-gate direction.
- `feature/issues/` remains the fine-grained issue and root-cause workspace.
- This file is the current issue-status rollup used to bridge the two.
- A change is not durably complete if it changes a tracked issue but leaves the issue file, this index, or the owning plan-tree state stale.

## 根因与来源

The current drift risk comes from three layers that answer different questions:

| Layer | Authority | What it answers |
| --- | --- | --- |
| `docs/plantree/README.md` and registered plans | Durable planning authority | What is planned, active, done, deferred, or release-blocking at roadmap level |
| `feature/issues/README.md` and this file | Issue navigation and status rollup | Which concrete problem files exist and which categories remain open |
| Individual `feature/issues/*.md` files | Root-cause and acceptance authority for one issue | Facts, repro, source chain, selected fix, validation matrix, residual risk |
| `feature/evidence/*.md` | Dated proof | Commands, binary hashes, request ids, report summaries, and validation results |

Plan-tree already exists and should remain the single durable planning entrypoint. It does not replace the issue files. It should not copy every detailed request id or every focused test result from `feature/issues`; instead, it links to issue indexes and records only roadmap-level status, decisions, open blockers, and release-gate state.

## Active blockers and not-yet-implemented work

These are the highest-signal files that still represent implementation or release blockers according to their own `Status` lines.

### 2026-08-02 用户澄清的逐项顺序

以下问题都没有被 `v0.0.131` 路由策略发布标记为已修复。用户明确要求先处理此前已经提出的语言约束和 usage 清理问题，再处理后来追加的 159/170 现网审计；此前把 159/170 排在最前是顺序理解错误。

1. [语言约束提示词首语言锁定](language-constraint-first-language-lock-20260802.md)：先确认语言状态作用域和复现边界。
2. [Usage 清理安全与 Redis 隔离](usage-cleanup-safety-and-redis-isolation.md)：补齐新旧 UI、`每批数量` 上限、汇总消失/回来的产品语义与动态一致性证据。
3. [159/170 现网 usage 错误审计与体验改进](production-usage-error-audit-159-170-20260802.md)：在前两项完成后只读采集两台机器的 usage 错误、代码版本和脱敏 JSONL，再逐类判断有限重试、请求处理、fallback 或不改。

| Area | Issue | Current status | Remaining work recorded |
| --- | --- | --- | --- |
| Claude Code local accounts / WebSearch / tools / image | [claude-code-local-accounts-websearch-tools-image-analysis-20260729.md](claude-code-local-accounts-websearch-tools-image-analysis-20260729.md) | `analysis-recorded / local-account-real-call-evidence-collected / wave1-websearch-direct-and-cli-focused-verified / fixes-pending` | Native `web_search_YYYYMMDD` direct requests, future-looking WebSearch versions, mixed native WebSearch server-side execution, Claude CLI `WebSearch`, tool-name/schema-key mapping, collision rejection, and current tool-result-only follow-up have focused passes; remaining work is image-source matrix, model-reporting clarity, longer multi-tool history regressions, and any future full mixed native/client tool state-machine design |
| External pool prompt length / model processing | [20260801-production-external-errors-root-cause.md](20260801-production-external-errors-root-cause.md) | `root-cause-confirmed / implementation-focused-pass / frontend-contract-gate-pass / integration-dispatch-focused-pass / released-v0.0.130` | P001: 外部池“输入上限预检”发送前 400 绕过“请求大小保护”，已改为取消内容长度发送前拒绝，保留调度/安全预检，并让“标准处理”按“发送前先处理”或“失败后再处理并重试”执行；Raw 透传直接交给外部上游。P002: 显式直连外部账号与本地失败 fallback route 现在携带“模型（本地解析）”并补齐本地 Kiro 发送链路的兼容模型处理，使“映射后内部处理”和“内部处理后映射”能按配置生效；`admin-ui` 外部池路径策略类型合同与兼容字段文案已同步；focused Rust/UI/doc 与 PG/Redis 外部池 dispatch hit 通过，已随 `v0.0.130` 发布，生产复发观察仍开放 |
| Local credential quota/overage dispatch | [local-credential-exhausted-overage-disabled-400-20260731.md](local-credential-exhausted-overage-disabled-400-20260731.md) | `analysis-recorded / production-evidence-collected / usage-detail-diagnostics-improved / scheduler-quota-guard-implemented / focused-tests-passed / scoped-release-gate-passed / production-recurrence-pending` | Scheduler startup/reload now jointly loads `credential_account_info`, derives a freshness-bounded API-key quota guard for `remaining<=0 + credit_remaining<=0 + overage_status=DISABLED`, excludes guarded credentials from dispatch/fallback selection, and only treats opaque 400 as credential quota when that guard is already present; focused PgSQL manager reload regression passed in an isolated test schema; 2026-08-01 scoped release gate passed; remaining gates are production recurrence and broader isolated/load validation |
| Usage accounting / downstream standard fields | [downstream-usage-standard-field-over-1m-20260731.md](downstream-usage-standard-field-over-1m-20260731.md) | `analysis-recorded / production-evidence-collected / standard-field-guard-implemented / focused-tests-passed / scoped-release-gate-passed / production-recurrence-pending` | Known code residuals are now focused-tested: no-reportedUsage local prompt-cache standard cache fields are capped, including `kiro_rs_tool`; local credential and external pool failure records keep request estimates in diagnostics and zero downstream-standard fields. 2026-08-01 scoped release gate passed; remaining gates are dashboard/API rollup distinction, production recurrence, and broader load validation |
| Account card subscription tier | [subscription-pro-max-card-label-20260801.md](subscription-pro-max-card-label-20260801.md) | `root-cause-confirmed / ui-and-backend-fix-implemented / focused-tests-passed / scoped-release-gate-passed / browser-pending` | `Pro Max` was classified by the generic `Pro` fallback; UI label, backend `pro_max` key/rank, and Power/Pro Max filter options are fixed. 2026-08-01 scoped release gate passed; browser screenshot verification remains useful but did not block this deterministic classifier fix |
| Output formatting / HTML tag contamination | [html-br-output-tag-contamination-20260731.md](html-br-output-tag-contamination-20260731.md) | `analysis-recorded / normal-context-not-reproduced / positive-control-only / fix-decision-pending` | Unsolicited standalone `<br>` in normal prose has not reproduced across direct, stream, tool-result, history, ambiguity, and real Claude CLI matrices; explicit HTML/web-display prompts remain legitimate pass-through controls, so no broad sanitizer is selected |
| Dashboard / UI observability | [dashboard-observability-redesign.md](dashboard-observability-redesign.md) | `analysis-complete / implementation-in-progress` | Finish product information architecture, time semantics, cost/account quality views, partial/stale UI behavior, new/old UI parity, and query isolation validation |
| External pool billing cost/statistics | [external-pool-billing-cost-statistics-20260803.md](external-pool-billing-cost-statistics-20260803.md) | `analysis-confirmed / implementation-complete / focused-verified / release-build-passed / production-observation-pending` | 流式 OpenAI 兼容 usage 已归一化；成功记录的“原始成本”优先使用上游真实 usage，仅在缺失时使用本地估算 fallback；本地整形 usage 保持独立。2026-08-04 复核通过 `external_pool::tests 214/214`、PgSQL usage/pricing、PgSQL external billing rollup、Redis Dashboard materialization、Admin UI build、docs/diff/fmt；Dashboard 快照、供应商真实费用字段和生产升级观察仍开放 |
| External pool direct/model/retry behavior | [external-pool-direct-model-retry-20260804.md](external-pool-direct-model-retry-20260804.md) | `analysis-updated / cross-host-evidence-matrix-recorded / body-model-p0-implemented / retry-mechanics-focused-verified / cooldown-policy-superseded` | 159/170/142 已按“入口、路由、外部尝试、本地尝试、模型字段”补齐对照；`usage` 明细保存的是请求决策轨迹，不是完整运行时配置快照。外部池候选选择、`请求正文模式` 后处理、Raw 入口重选标准处理池、默认 `anthropic-version`、同池/跨池预算分离、跨池状态码/网络/协议开关和“清除冷却”仍作为基础能力保留。2026-08-04 “普通连续瞬态失败上浮为池级长冷却”的旧结论已被 2026-08-05 HA 目标覆盖，不能作为当前发版依据 |
| External pool HA scheduler cooldown regression | [external-pool-ha-scheduler-cooldown-regression-20260805.md](external-pool-ha-scheduler-cooldown-regression-20260805.md) | `P0 / target-confirmed / root-cause-fixed / real-http-verified / released-v0.0.133 / production-rollout-pending` | 根因是本进程消费自己的 Redis 外部池变更事件并清空刚合并的权威快照，不是优先级排序函数本身。已增加 `origin` 事件标识并保留旧事件兼容；3 轮真实 HTTP 多池接管/恢复、256 并发、1800 RPM/60 秒、外部直连边界、隔离 PgSQL/Redis、全量 Rust 和资源回落均通过，`Publish Docker Images #164` 已成功发布 `v0.0.133`。详见 [专项证据](../evidence/external-pool-ha-scheduler-validation-20260805.md)。剩余仅为生产 rollout/观察和更大架构 follow-up |
| Scheduler architecture | [scheduler-architecture-analysis-purpose-and-plan.md](scheduler-architecture-analysis-purpose-and-plan.md) | `analysis-complete / target-accepted / compliance-matrix-recorded / implementation-ready` | Source-verified local/external/fallback/rescue state machine, queue/capacity/cooldown/retry matrix, `sub2api` comparison and configuration regrouping are recorded. [Decision 001](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/decisions/001-local-external-scheduler-target-contract.md) is now accepted for the user-confirmed core target; [current compliance matrix](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/scheduler-target-compliance-matrix.md), [sustained validation plan](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/sustained-scheduling-validation.md), and [execution plan](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/external-pool-ha-scheduler-execution-plan-20260805.md) drive implementation |
| Strict local-first and multi-instance scheduling | [strict-local-first-distribution-and-multi-instance.md](strict-local-first-distribution-and-multi-instance.md) | `release-blocked / ... / external-takeover-dynamic-and-distribution-gates-open` | E01/E02 dynamic distribution, E05 degraded external takeover, production high-cardinality, and final candidate matrix |
| Local capacity preflight race / fallback latency | [local-capacity-preflight-race-and-external-fallback-latency.md](local-capacity-preflight-race-and-external-fallback-latency.md) | `focused-policy-and-manager-pass / storage-burst-and-frozen-candidate-pending / NO-GO` | Real isolated PG/Redis, 40x15 burst, two-instance race, Redis/usage joint chaos, frozen release L3/L4/L5 |
| Retry budget / admission / RPM amplification | [retry-budget-admission-and-rpm-amplification.md](retry-budget-admission-and-rpm-amplification.md) | `... gates-pending / NO-GO` | Persistent usage attribution, live Redis/PG, cross-instance aggregate admission, real handler/CLI, client retry, 429/500/partial and L3-L5 recovery |
| Token refresh failure wave / cluster RPM | [token-refresh-failure-wave-and-cluster-rpm.md](token-refresh-failure-wave-and-cluster-rpm.md) | `... provider-and-frozen-candidate-unverified / NO-GO` | Redis 60/8 two replicas, Redis slow/error/restart, cancellation phases, provider/PG/frozen evidence |
| Redis usage writer / scheduler isolation | [redis-usage-writer-atomicity-cardinality-and-scheduler-isolation.md](redis-usage-writer-atomicity-cardinality-and-scheduler-isolation.md) | `... multi-instance-and-production-cardinality-pending / NO-GO` | Multi-instance, production-cardinality, scheduler isolation pressure, production p95/p99 |
| 159/170 production usage error audit | [production-usage-error-audit-159-170-20260802.md](production-usage-error-audit-159-170-20260802.md) | `read-only-evidence-collected / problem-clusters-recorded / no-new-runtime-fix-selected-yet` | Both hosts run `v0.0.123`; P001 prompt-too-long preflight and P002 usage-standard old behavior overlap later fixes, P003 external 5xx already retries across both enabled pools before returning 502, and P004 external 400 is a diagnostic gap rather than a retry candidate. Redacted problem folders live under `tmp/prod-evidence/20260803-025431-usage-audit-159-170/problems/`; production recurrence after upgrade and Admin-only upstream diagnostic enhancement remain open |
| Language constraint first-language lock | [language-constraint-first-language-lock-20260802.md](language-constraint-first-language-lock-20260802.md) | `analysis-confirmed / compressed-summary-and-concurrent-matrix-not-reproduced / implementation-not-authorized` | Direct HTTP, real Claude Code CLI short/reverse/long-history, simulated compacted-summary and opposite-language concurrent sessions follow the latest message; actual Claude Code automatic compact threshold and user-provided failure transcript remain open, so no forced language override is selected |

## Fixed or implemented but not finally closed

The documents below record substantial implementation or focused validation, but they still require final candidate, true CLI/upstream/browser/load, rollout, or production recurrence evidence before they can be treated as closed.

| Category | Issue records | Open gate |
| --- | --- | --- |
| Protocol regression | [protocol-capability-regression-matrix.md](protocol-capability-regression-matrix.md), [protocol-transcript-and-tool-history-leak.md](protocol-transcript-and-tool-history-leak.md), [thinking-and-signed-content-safety.md](thinking-and-signed-content-safety.md), [thinking-effort-adaptive-upstream-mapping.md](thinking-effort-adaptive-upstream-mapping.md) | Current frozen-candidate thinking/effort wire `60/60` passed; native real upstream, real CLI C3/C4, active/passive signed thinking, mixed long sessions, and final release binding remain open |
| Tools and schema compatibility | [empty-tool-description-400-invalid-tool-use-format.md](empty-tool-description-400-invalid-tool-use-format.md), [tool-property-key-invalid-400-tool-schema-invalid.md](tool-property-key-invalid-400-tool-schema-invalid.md), [prompt-policy-tool-choice-and-count-tokens.md](prompt-policy-tool-choice-and-count-tokens.md) | Current direct/CLI focused tool-name and tool-result-only gates passed; remaining gates are unified candidate MCP/long-history, repeated/multi-tool pairing, image-bearing tool_result, browser/count_tokens |
| WebSearch / MCP | [websearch-mcp-protocol-usage-and-privacy.md](websearch-mcp-protocol-usage-and-privacy.md), [websearch-normalized-external-fallback-preflight.md](websearch-normalized-external-fallback-preflight.md), [prod-websearch-mcp-error-clusters-159-170-20260725.md](prod-websearch-mcp-error-clusters-159-170-20260725.md) | Direct native `web_search_YYYYMMDD` and current Claude CLI `WebSearch` have focused local-account passes; remaining gates are auxiliary attribution, production recurrence, MCP upstream body evidence, and any future design for complete mixed native/client tool alternation |
| Image / payload / remote resources | [08-image-format-unsupported-400.md](08-image-format-unsupported-400.md), [payload-guard-semantics-limits-and-performance.md](payload-guard-semantics-limits-and-performance.md), [remote-multimodal-resource-and-ssrf-bounds.md](remote-multimodal-resource-and-ssrf-bounds.md) | Broader image source matrix, frozen load/CLI, external profile, 50 MiB/RSS, slow upload, handler/load/release-candidate evidence |
| Stream and terminal states | [02-stream-upstream-idle-timeout.md](02-stream-upstream-idle-timeout.md), [06-stream-upstream-status-error.md](06-stream-upstream-status-error.md), [07-stream-internal-read-error.md](07-stream-internal-read-error.md), [10-stream-end-turn-vs-silent-truncation.md](10-stream-end-turn-vs-silent-truncation.md), [stream-terminal-errors-and-precommit-retry.md](stream-terminal-errors-and-precommit-retry.md), [11-stream-observability-and-trivial-text-optimization.md](11-stream-observability-and-trivial-text-optimization.md) | Unified precommit/transport/fault gates, final CLI/HTTP/load gates |
| Output contamination | [html-br-output-tag-contamination-20260731.md](html-br-output-tag-contamination-20260731.md) | Keep diagnostic-only unless an unsolicited normal-prose sample is captured; any future normalization must preserve explicit HTML/code/web-display answers |
| Scheduler / external pool | [external-pool-redis-coordination-and-release.md](external-pool-redis-coordination-and-release.md), [external-pool-authoritative-selection-and-dispatch-fence.md](external-pool-authoritative-selection-and-dispatch-fence.md), [external-pool-profiles-and-sse-safety.md](external-pool-profiles-and-sse-safety.md), [external-pool-success-zero-billing.md](external-pool-success-zero-billing.md), [external-pool-scheduler-interference-and-fallback-matrix-20260727.md](external-pool-scheduler-interference-and-fallback-matrix-20260727.md), [redis-scheduler-degraded-and-fallback.md](redis-scheduler-degraded-and-fallback.md), [high-concurrency-low-rpm-runtime-quarantine.md](high-concurrency-low-rpm-runtime-quarantine.md), [dispatch-queue-lease-renewal-rpm-amplification.md](dispatch-queue-lease-renewal-rpm-amplification.md) | Frozen load, two-instance, external takeover dynamic, native/CLI/UI/upgrade follow-up, production recurrence |
| Storage / usage / dashboard | [usage-cleanup-safety-and-redis-isolation.md](usage-cleanup-safety-and-redis-isolation.md), [usage-dashboard-p95-and-window-semantics.md](usage-dashboard-p95-and-window-semantics.md), [external-pool-success-zero-billing.md](external-pool-success-zero-billing.md), [upstream-error-diagnostic-privacy-and-bounds.md](upstream-error-diagnostic-privacy-and-bounds.md) | New/old UI cleanup semantics, stale preview drift, queued label and browser interaction are verified; `每批数量` default remains 250 and backend/UI max is now 5,000 with PostgreSQL CHECK migration plus migration-disabled compatibility guard; usage Admin summary/dashboard cache-write race fixed and focused-tested. Dynamic multi-instance recheck and production-scale batch performance remain open |
| Usage projection sanity | [downstream-usage-standard-field-over-1m-20260731.md](downstream-usage-standard-field-over-1m-20260731.md) | Production evidence shows final standard usage fields can exceed 1m; reported-usage cache creation, unreported local prompt-cache cache read/write, and failure diagnostic input separation are now implemented and focused-tested. 2026-08-01 scoped release gate passed; dashboard/API rollup distinction, production recurrence, and broader load validation remain open |
| Route policy config authority | [route-policy-config-authority-20260802.md](route-policy-config-authority-20260802.md) | Backend and both UI surfaces are implemented and focused-verified; the handler matrix and frozen-candidate Claude Code CLI fake-upstream suite also passed. `v0.0.131` was republished successfully after fixing the Clippy bucket regression (`Publish Docker Images #162`, quality/build/manifest green). Built-in routes remain fixed entrypoints, while cache, usage, prompt steering, external pool route rules, and cache namespace resolve from runtime configuration. Live service reload, browser interaction, real CLI dynamic configuration, and production recurrence remain open |
| UI / admin / operations | [two-ui-cost-precision-and-config-authority.md](two-ui-cost-precision-and-config-authority.md), [aws-kiro-api-key-region-lifecycle.md](aws-kiro-api-key-region-lifecycle.md), [business-observability-redis-fault-domain.md](business-observability-redis-fault-domain.md), [mcp-completion-runtime-card-error-source.md](mcp-completion-runtime-card-error-source.md), [local-credential-exhausted-overage-disabled-400-20260731.md](local-credential-exhausted-overage-disabled-400-20260731.md) | Usage detail modal now foregrounds upstream/processing diagnostics in both UIs; local API-key quota guard has focused Rust/provider/storage passes; browser gate, frozen runtime gate, and production recurrence remain pending |
| Release / upgrade / artifacts | [upgrade-v101-v102-v103-smoke.md](upgrade-v101-v102-v103-smoke.md), [postgres-startup-migration-atomicity.md](postgres-startup-migration-atomicity.md), [validation-build-artifact-lifecycle-and-disk-safety.md](validation-build-artifact-lifecycle-and-disk-safety.md), [runtime-stack-overflow-and-handler-future-size.md](runtime-stack-overflow-and-handler-future-size.md) | Final release binary rebind, final inventory, frozen release HTTP/load |

## Superseded or historical documents

These remain useful for provenance, but should not drive current local-account or release decisions without a newer status refresh:

| Issue | Current use |
| --- | --- |
| [claude-code-real-cli-tools-websearch-image-debug-20260729.md](claude-code-real-cli-tools-websearch-image-debug-20260729.md) | Historical external-pool-heavy pass; superseded for local-account diagnosis by [local-account analysis](claude-code-local-accounts-websearch-tools-image-analysis-20260729.md) |
| [03-client-dropped-downstream.md](03-client-dropped-downstream.md) | Historical classification plus cleanup/resource gate |
| [04-external-pool-prompt-too-long.md](04-external-pool-prompt-too-long.md) | Historical classification; its old max-input preflight design is superseded by [2026-08-01 external-pool root cause](20260801-production-external-errors-root-cause.md) |
| [09-intent-preamble-end-turn-no-tool-use.md](09-intent-preamble-end-turn-no-tool-use.md) | Usage observability pass; needs long-session statistical gate |

## 复现/刷新方式

Refresh this file whenever a meaningful issue state changes:

```bash
for f in feature/issues/*.md; do
  st=$(rg -m1 '^Status:' "$f" | sed 's/^Status: //')
  sev=$(rg -m1 '^Severity:' "$f" | sed 's/^Severity: //')
  printf '%s\t%s\t%s\n' "$(basename "$f")" "$st" "$sev"
done | sort
```

Useful rollup counters:

```bash
tmp=$(mktemp)
for f in feature/issues/*.md; do
  st=$(rg -m1 '^Status:' "$f" | sed 's/^Status: //')
  printf '%s\t%s\n' "$(basename "$f")" "$st" >> "$tmp"
done
printf 'total\t'; wc -l < "$tmp"
printf 'NO_GO\t'; rg -i 'NO-GO|release-blocked|release-blocking' "$tmp" | wc -l
printf 'implementation_in_progress_or_fixes_pending\t'; rg -i 'implementation-in-progress|fixes-pending|analysis-planned' "$tmp" | wc -l
printf 'pending_or_open_any\t'; rg -i 'pending|open|gates-open|not released|partial' "$tmp" | wc -l
rm "$tmp"
```

Run the documentation contract after editing issue files:

```bash
node feature/tests/check-feature-docs.mjs
```

## 方案/维护规则

### Authority split

- `docs/plantree/` is the durable planning authority.
- `feature/issues/` is the concrete issue/root-cause workspace.
- `feature/evidence/` is dated proof, not a live status source.
- This index is the current cross-issue rollup. It points upward to plan-tree and downward to individual issue files.

### Required update rule

Every change that materially affects a tracked issue must update documentation in the same change set:

1. Update the owning `feature/issues/*.md` file:
   - `Status`
   - fix/result/evidence section
   - residual risk and open gates
   - links to new evidence or request ids when available
2. Update this file when the issue changes category:
   - blocker added/removed
   - `NO-GO` closed
   - implementation becomes complete
   - final validation, browser, load, or production gate closes
   - a new active issue is added
3. Update plan-tree when the change affects roadmap-level state:
   - release blocker opened/closed
   - current phase changes
   - new cross-cutting plan or decision is created
   - acceptance/release gate changes
   - user-facing priority changes

Small purely local code edits that do not affect a tracked issue do not need a new plan-tree entry, but they still need normal code comments/tests where appropriate.

### Definition of done for tracked issues

An issue can only move to `closed`, `verified-fixed`, `released`, or equivalent when:

- the code/config/docs change exists;
- the individual issue file records what changed;
- this status index no longer lists it as open if it was previously listed here;
- the evidence file or issue section records commands, build identity, request ids, or rollout observation sufficient for the claim;
- plan-tree roadmap/status is updated if the issue was a roadmap or release-gate item.

### Status vocabulary guidance

Use a small, explicit vocabulary in issue `Status` lines:

| Token | Meaning |
| --- | --- |
| `analysis-recorded` | Facts and likely causes are documented; fix is not necessarily chosen |
| `fixes-pending` | At least one code/product fix is still required |
| `implementation-in-progress` | Implementation has started but is incomplete |
| `focused-pass` / `focused-verified` | Narrow tests passed; broader gates may remain |
| `final-candidate-pending` / `final-rebind-pending` | Needs frozen binary or release-candidate binding |
| `browser-pending` | Needs real UI/browser interaction evidence |
| `production-recurrence-pending` | Needs production observation after rollout |
| `NO-GO` / `release-blocked` / `release-blocking` | Do not release until explicitly closed or superseded |
| `historical` / `superseded` | Retained for provenance; do not use as current execution truth |

## 验收与证据

This index is accepted when:

- it links to the current high-priority open issue records;
- it records the authority split between plan-tree, issues, and evidence;
- `feature/issues/README.md` links to it;
- the owning plan-tree index links to the documentation-status governance rule;
- `node feature/tests/check-feature-docs.mjs` passes after edits;
- new relative Markdown links resolve.

## 残余风险与回滚

Residual risks:

- This index is manually curated. It can drift if code changes do not update docs.
- Some issue `Status` lines are historical or stale; this index preserves the current documented state but does not independently prove runtime truth.
- The counts are a snapshot from 2026-07-31. Re-run the refresh commands before making release decisions.

Rollback:

- If this index becomes too large or stale, do not delete the individual issue files. Replace this file with a shorter current rollup and archive older snapshots under an explicitly linked history/evidence location.
- Do not move all issue status into plan-tree; that would duplicate the issue workspace and make root-cause documents harder to maintain.
