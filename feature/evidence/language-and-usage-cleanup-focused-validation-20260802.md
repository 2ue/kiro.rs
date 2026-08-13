# Language And Usage Cleanup Focused Validation - 2026-08-02

Status: `focused-pass / implementation-not-authorized-for-language / dynamic-usage-gates-open`

## Scope

This evidence records the ordered follow-up validation for:

- language behavior after a simulated compacted/old system summary;
- two concurrent sessions with opposite first languages;
- both Usage cleanup UIs in a real local browser;
- queued-state labels, defaults, optional preview, stale-preview invalidation, hard-delete wording, and confirmation cancellation.

The validation used only loopback services. It did not restart `127.0.0.1:9022`, change runtime configuration, modify PostgreSQL/Redis data, or accept a destructive cleanup job.

## Language Boundary Results

Local service: `http://127.0.0.1:9022/cc`, local request key redacted, model `claude-sonnet-4.5`.

| Case | Result | Request ID |
| --- | --- | --- |
| English compacted-summary simulation, latest Chinese request | `200`, exact output `中文` | `msg_01UjbHgAhTLYibLJMSd6UaYM` |
| Chinese compacted-summary simulation, latest English request | `200`, exact output `english` | `msg_01EhR1kPqeWhVTzLPPmsedG4` |
| Concurrent session with English first, latest Chinese | `200`, exact output `并发` | `msg_01MeP8MaBp8bFotMxvYUe25E` |
| Concurrent session with Chinese first, latest English | `200`, exact output `parallel` | `msg_01kbjXHia73Brwap1SQYfSAu` |

The simulated compacted summaries were deliberately repeated long system text. This is protocol-level evidence, not a claim that Claude Code's own automatic `/compact` threshold was reached. The actual Claude Code automatic-compaction boundary remains open because forcing a large real context solely for this issue would create unnecessary local traffic without a user failure transcript.

## Usage Cleanup Browser Results

Browser runner: isolated Playwright context using the installed local Chrome binary.

- New UI: `http://127.0.0.1:9023/ui/runtime`
- Old UI: `http://127.0.0.1:9025/admin/`
- API proxy: existing local `http://127.0.0.1:9022`

The runner injected the local `adminApiKey` only into an isolated browser context and redacted it from output. For queued-state checks, only the cleanup-status response was mocked in the browser; no cleanup mutation was accepted.

| Surface | Result |
| --- | --- |
| New UI defaults | `保留天数=7`、`每批数量=250`、`批次间隔=100ms` |
| Old UI defaults | `创建时间早于/删除时间早于=7`、`每批数量=250`、`批次间隔=100ms` |
| Preview | Both UIs expose preview as optional; payload is `mode=soft_delete, olderThanDays=7, batchSize=250, pauseMsBetweenBatches=100` |
| Stale preview | Both UIs clear the previous preview after changing the retention value |
| Hard-delete wording | Both UIs show the physical-delete contract after switching mode |
| Start without preview | New UI confirmation was dismissed and emitted no `/cleanup/start`; old UI start remains enabled without a prior preview |
| Queued state | Both UIs show `排队中` and do not show `空闲` for a queued job |

Focused browser output:

```text
new-ui-default: 7 / 250 / 100, optionalPreview=true, stalePreview=false,
  hardDeleteVisible=true, startRequestAfterDismiss=false
new-ui-queued: queuedLabel=true, noIdleLabelForQueued=true
old-ui-default: 7 / 250 / 100, optionalPreview=true, stalePreview=false,
  hardDeleteVisible=true, startEnabledWithoutPreview=true
old-ui-queued: queuedLabel=true, noIdleLabelForQueued=true
```

## Release Meaning

This closes the browser interaction gap for the current UI source changes and strengthens the language non-reproduction evidence. It does not close:

- real Claude Code automatic compaction at its production context boundary;
- multi-instance Admin cache writer/cleanup race under live concurrent processes;
- production-scale cleanup batch latency and Redis fault/chaos gates;
- the later `152.53.243.159` / `152.53.194.170` read-only audit.
