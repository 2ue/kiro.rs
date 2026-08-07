# External Pool Stream Error Yuenan Sampling - 2026-08-06

Status: `sampling-summary / raw-transcript-not-yet-archived / implementation-input / focused-validation-linked`

Date: 2026-08-06 Asia/Shanghai

Related:

- [Stream terminal errors and precommit retry](../issues/stream-terminal-errors-and-precommit-retry.md)
- [External pool stream pre-output retry handoff](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/external-pool-stream-pre-output-retry-20260806.md)
- [External pool stream pre-output retry focused validation](external-pool-stream-pre-output-retry-validation-20260806.md)
- [External pool HA scheduler cooldown regression](../issues/external-pool-ha-scheduler-cooldown-regression-20260805.md)

## Scope

This evidence note records the observed sampling summary for the `152.53.243.159` production service using the configured `yuenan` and `yuenan-1` external pools.

It is not a full release-gate evidence package:

- Raw command transcript and per-request response bodies were not found in the repository at the time this note was written.
- The result represents only `yuenan` and `yuenan-1`; it must not be generalized to all external pools.
- A fake-upstream reproducible matrix has since been added in
  [External pool stream pre-output retry focused validation](external-pool-stream-pre-output-retry-validation-20260806.md).
  Real Claude CLI, load/chaos, production rollout observation and renewed `yuenan` / `yuenan-1`
  recurrence checks remain separate release gates.

## User-Provided Trigger Sample

| Field | Value |
| --- | --- |
| Time | `08/06 09:35:35` |
| Request ID | `req_01KaWrDY5oZkY13XQqdJB9PH` |
| Entry | `/cc/v1/messages` |
| Requested model | `claude-sonnet-5` |
| Upstream model | `claude-sonnet-5` |
| External account | `#18 yuenan-1` |
| Route | `外部直连 · external_pool · external_direct_policy` |
| Status | `流错误` |
| Stream | `stream` |
| Client error type | `api_error` |
| Client status code | `200` |
| Error type | `stream_error` |
| Error stage | `external_account_stream` |
| Internal error | `external upstream emitted an error event` |
| Direct reason | `explicit_direct` |

## Sampling Summary

| Pool | Stream calls | Normal end | Empty/protocol-only stream error | Non-stream calls |
| --- | ---: | ---: | ---: | ---: |
| `yuenan` | 12 | 7 | 5 | 5/5 success |
| `yuenan-1` | 12 | 10 | 2 | 5/5 success |

Observed stream-error shape:

```text
HTTP 200
  -> event: message_start
  -> event/data: error
  -> no content_block_start
  -> no content_block_delta
  -> no thinking
  -> no tool_use
  -> no input_json_delta
```

## Interpretation

The sample supports these conclusions:

1. The failure is stream-phase, not HTTP dispatch-phase. The downstream HTTP status can remain `200` while the SSE body carries an error event.
2. The current external-pool retry loop cannot recover this class because the code returns `Response` before reading the SSE body.
3. The sample is a plausible candidate for pre-output replay because no effective assistant content, thinking, or tool event was observed before the error.
4. A safe fix still requires buffering protocol-only events before forwarding them downstream. Replaying after the client has already received the old attempt's `message_start` would risk duplicate stream state and usage confusion.
5. Non-stream success in the same pool window suggests the request class is not always invalid. It does not prove every stream empty/error is recoverable.

## Required Follow-Up Evidence

The focused implementation evidence now covers fake-upstream recovery, post-output no-retry,
external direct boundaries and final-success usage cleanliness. Remaining follow-up evidence:

1. Real Claude Code CLI frozen-binary gate for normal stream, pre-output recovery and post-output no-retry.
2. Load/chaos gate for normal traffic plus pre-output error waves and resource cleanup.
3. Production rollout observation.
4. If using remote `yuenan`/`yuenan-1` again, store redacted per-request event summaries and request ids under `tmp/` or a dated evidence package.
