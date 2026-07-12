# 003: Attempt Replay Safety And Downstream Commitment

Role: Architecture decision record

Status: Accepted

Date: 2026-07-12

Authority: Binding attempt classification, replay safety, downstream commitment, retry, and fallback contract

Scope: Local Kiro and external upstream attempts, stream and non-stream response selection, payload-changing retries, fallback, idempotency, and public error selection

Affected requirements/findings: `FUN-007`, `FUN-014`, `INV-005`, `REL-002`, and the R5/R7 retry and response gates

Decision source: Architecture-contract reconciliation and final-plan convergence on 2026-07-12; conservative replay policy is fixed by decision 010 and delivery mechanics are fixed by decision 009

Related: [Module contracts](../topics/architecture/module-boundaries-and-contracts.md), [Runtime flows](../topics/architecture/runtime-control-and-data-flows.md), [Requirements](../topics/requirements-and-quality-attributes.md), [Rewrite sequence](../topics/delivery/migration-sequence.md), [Verification](../topics/delivery/verification-rollout-and-rollback.md), [Open questions](../open-questions.md)

## Context

The current proposal separates upstream delivery from downstream commitment, but its retry wording is inconsistent. One contract forbids selecting another upstream after downstream headers are committed, while another permits retry until a response body or SSE event has been emitted. The latter leaves a window in which status and headers are already irreversible but the application could still try to substitute another response.

Upstream transport progress is also not equivalent to replay safety. Receiving a status or response body proves that a response was observed; it does not by itself prove that the upstream rejected the request before execution, generation, mutation, or billing. A network error after request bytes may have been transmitted is ambiguous even when no downstream response has started.

## Decision

Retry and fallback decisions use two independent, monotonic dimensions.

Downstream commitment is owned by the downstream response state machine:

```rust
pub enum DownstreamCommitment {
    Uncommitted,
    HeadersCommitted,
    BodyStarted,
    Finished,
}
```

`HeadersCommitted` begins at the earliest point where the handler or transport can no longer replace the selected status and headers. It does not require proof that the client kernel acknowledged bytes. Returning or otherwise irrevocably handing the selected response to the HTTP transport is sufficient. The state can only advance. Any state other than `Uncommitted` prohibits retry, fallback, or credential/pool replacement, including when no body byte has yet been emitted.

The upstream adapter reports transport and protocol facts without deciding retry. One target-aware classifier derives replay safety:

```rust
pub enum ReplaySafety {
    SafeWithoutIdempotency,
    RequiresEffectiveIdempotency,
    Forbidden,
}
```

`SafeWithoutIdempotency` is limited to an attempt proven not to have transmitted request bytes, or a target-specific response contract proven to mean rejection before execution. Once request bytes may have reached the upstream, loss of the response, timeout, reset, cancellation with unknown delivery, or another inconclusive result is `RequiresEffectiveIdempotency`. Observing response headers, a status code, or part of a response body does not automatically make replay safe. `Forbidden` covers successful/terminal outcomes and policy classes that must never be repeated.

An idempotency mechanism is effective only when the selected upstream contract confirms that it deduplicates the same logical operation end to end, the same stable key is used across covered attempts, its scope and retention cover the retry window, and tests demonstrate the behavior. A local request ID, Redis dedupe key, or header ignored by the upstream is not effective idempotency.

A retry or fallback is allowed only when all of the following hold:

1. downstream commitment is `Uncommitted`;
2. the attempt-count, elapsed-time, and resource budgets remain;
3. the route policy permits the target transition;
4. another eligible target can be acquired;
5. replay is safe without idempotency, or an effective idempotency contract covers the operation.

A payload-changing retry, such as a bounded payload-too-long rescue, additionally requires a response class explicitly proven to reject before model execution. A changed payload must not reuse an idempotency key whose upstream contract binds that key to different bytes unless the contract explicitly permits it.

One domain error/retry policy consumes classified upstream facts, replay safety, downstream commitment, route policy, and budgets. Upstream adapters, response adapters, schedulers, and usage projectors must not independently reclassify the same raw failure.

## Ownership

- R5 upstream adapters own transmission facts, response facts, and target-specific protocol evidence. They do not select a retry or fallback.
- The domain error policy owns failure classification and replay-safety derivation from those facts and an accepted target capability matrix.
- R7 response/transport state owns the monotonic downstream commitment value.
- Request orchestration owns the bounded retry/fallback decision using the single policy output.
- R4 scheduler state consumes the classified scheduler effect; it does not infer replay safety from an error string.

## Alternatives And Tradeoffs

### Treat first body or SSE bytes as the commitment boundary

Rejected. Headers may already be irreversible, so substituting another upstream can produce a second action without a coherent downstream response.

### Retry all network errors and selected 5xx responses

Rejected. It improves apparent availability but can duplicate upstream execution or billing when delivery is ambiguous.

### Disable every retry after an upstream call begins

Not selected as the default proposal. It is safe but unnecessarily discards definitely-not-sent attempts and explicitly documented pre-execution rejections. The conservative classifier retains those bounded cases.

### Infer safety directly from the HTTP client error variant

Rejected. Client-library variants do not generally prove how many bytes reached the peer or whether a responding intermediary/upstream executed the operation.

The accepted policy may reduce fallback success compared with permissive legacy behavior. That compatibility cost is intentional unless an effective upstream idempotency contract is established.

## Compatibility And Data Consequences

- Public error schemas and retry-after headers remain compatible; attempt selection may become more conservative.
- Attempt traces add bounded delivery, replay-safety, commitment, idempotency-capability, and decision-reason fields without logging request bodies, credentials, or arbitrary upstream errors.
- Existing external-pool retry counts do not opt a pool into ambiguous replay. A separate explicit capability is required.
- Stable request and attempt IDs remain additive. No durable schema removal is required for final target activation.
- Stream and non-stream paths use the same commitment and replay predicates.

## Target Integration And System Rollback

1. Characterize current retry/fallback behavior by target, status, transport phase, and downstream commitment.
2. Compare the new classifier offline from the same sanitized facts; comparison issues no request.
3. Build the target capability/idempotency matrix. Unknown capability resolves to `NotReplaySafe` under decision 010.
4. Exercise one complete target attempt-policy path with deterministic fake upstreams; no release build contains an old/new policy selector.
5. Activate this policy only as part of the complete target system under decision 009.

Whole-system rollback preserves additive trace fields and attempt evidence needed to explain duplicate risk. It never migrates an in-flight request to the previous binary.

## Verification

- Cover connect failure before send, partial/unknown request transmission, timeout/reset after send, explicit response statuses, malformed/truncated responses, cancellation, and completed responses.
- Disconnect before response selection, after response handoff/headers, after the first body or SSE event, and before final usage.
- Prove that `HeadersCommitted` with zero body bytes still prohibits another upstream.
- Prove that an ambiguous external POST is not retried without accepted idempotency evidence.
- Test any claimed pre-execution rejection and idempotency mechanism against a deterministic fake upstream, then use low-volume real-upstream evidence only where the fake cannot establish the contract.
- Verify bounded attempts, stable idempotency keys, no route loops, one scheduler effect per attempt, and bounded non-secret decision metrics.

## Implementation Parameters

- Decision 010 resolves `Q-008`: ambiguous POST delivery is not replayable.
- The target capability matrix and its configuration schema are implementation artifacts with fail-closed defaults, not open architecture choices.
- A payload-changing retry requires the target's accepted idempotency capability to bind the same logical operation and effective payload; otherwise it is prohibited.
- R5/R7 evidence must still prove this contract before final cutover.
