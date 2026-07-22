# Upstream Error Privacy And Bounds Evidence 2026-07-18

Status: `focused-provider-handler-pass / persistent-storage-frozen-load-pending`

Source: HEAD `401473ca1649997bdeccf4468e3add1bdb187248` plus the current unreleased dirty tree.

Related issue: [Upstream Error Diagnostic Privacy And Bounds](../issues/upstream-error-diagnostic-privacy-and-bounds.md)

## Current Source Contract

The provider may read a bounded response body locally to classify an error, but the body is not a diagnostic value after classification. The retained diagnostic is limited to fixed fields:

- error class;
- upstream/public status;
- body byte count;
- bounded retry-after;
- normalized content type;
- a low-cardinality reason token.

`read_upstream_body_strict` enforces the byte/deadline/UTF-8 boundary. `api_failure_diagnostic` never receives the body text. Ordinary API, MCP, ListAvailableProfiles and model discovery paths use this fixed diagnostic or equally bounded MCP tokens. Current static searches found no production tracing field/interpolation or attempt/scheduler error that formats the raw response body.

## Provider Status And JSON Matrix

The exact current test is:

```text
kiro::provider::tests::provider_status_and_json_error_matrix_is_private_typed_and_bounded
```

It covers 13 response classes:

- HTTP 400, 401, 403, 408, 429, 500 and 503;
- HTTP 200 JSON invalid-request, throttle, server, timeout and unknown exceptions;
- HTTP 200 non-EventStream protocol failure.

Every case runs stream and non-stream across pool sizes 1/20/60 and five rounds: `13 x 2 x 3 x 5 = 390` independent provider-call outcomes. Retryable cells use the shared four-send budget; deterministic cells use one send. Total expected and observed budgeted sends are 990.

For every outcome the test asserts:

- fixed typed class and status;
- attempts equal real sends;
- error text remains below 1024 bytes;
- the per-request private marker is absent from error text, serialized attempts and scheduler snapshot;
- cooldown/reason state matches the fixed classification;
- the marker is absent from the captured DEBUG log.

After the test-only internal concurrency was bounded at four, the focused matrix passed `1/1` in `141.73s`.

## Transport And Body Matrix

The exact current test is:

```text
kiro::provider::tests::provider_transport_and_body_fault_matrix_is_private_typed_and_bounded
```

It covers header timeout, declared Content-Length over limit, chunked over limit, body timeout, mid-body disconnect and malformed UTF-8. Stream/non-stream, pool 1/20/60 and five rounds produce `6 x 2 x 3 x 5 = 180` independent outcomes and 540 budgeted sends.

Every cell asserts fixed error type/status, attempt/send equality, bounded scheduler cooldown reason, and zero private-marker matches in error, serialized attempts, scheduler snapshot and DEBUG logs. The initial focused matrix passed `1/1` in `243.67s`.

The complete tree later exposed that Tokio's future-first timeout could accept the fake 500 after the configured one-second header deadline when the executor resumed late. After the shared header/body helper was changed to deadline-first selection, the unchanged transport/body matrix passed again in `245.74s`; all 180 outcomes, 540 sends and marker assertions remained intact. See [deadline evidence](http-deadline-runtime-starvation-20260718.md).

Both exact tests shared scope `provider-fault-matrix-r2`; its target used `1673816 KiB` and ended with `removed=true` and `reservation_released=true`.

## Handler And Auxiliary Coverage

The complete default-bin unit tree also includes:

- five Router rounds proving an HTTP-200 JSON exception marker is absent from public response, UsageRecord and DEBUG logs;
- unknown/missing-terminal EventStream fixtures proving private markers do not enter success/error usage;
- five rounds covering manual provider, model discovery and profile discovery errors without raw-body persistence;
- MCP body-limit/status spoofing tests proving body text cannot override status classification or enter attribution.

The post-deadline tree was `1708 passed / 0 failed / 6 explicit perf probes ignored` in `full-unit-current-r11`. After queue/storage/provider fixtures, current `full-unit-current-r12` is `1715 passed / 0 failed / 6 ignored`; Rust 1.92.0 `cargo check --all-targets` remains zero-warning. See [the complete red/green record](full-unit-tree-red-green-20260718.md).

## Remaining Boundary

This evidence is not a persistent-storage or frozen-candidate pass. Still required:

- run the same representative markers through a frozen temporary HTTP service and inspect downstream responses plus finalized usage records;
- run isolated PostgreSQL/Redis storage queries or the checked-in fail-closed validation program to prove no marker is persisted in usage/scheduler data;
- run mixed 400/429/500/malformed bursts and recovery while measuring RSS, FDs and amplification;
- bind the result to the final binary SHA and C1-C4/L1-L5 evidence.

The current supported conclusion is limited: the enumerated dirty-tree provider/handler/auxiliary paths retain typed bounded diagnostics and did not expose their private response markers in the tested in-memory surfaces. Unknown future response shapes and unexecuted persistent-storage/frozen paths remain fail-closed release gates.
