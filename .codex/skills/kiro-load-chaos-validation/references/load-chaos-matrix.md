# Load And Chaos Matrix

## L1 Fake Upstream

Use fake upstream before real accounts:

| Case | Scenario | Goal |
| --- | --- | --- |
| L1.1 | `normal-stream` | baseline stream parsing and report generation |
| L1.2 | `normal-non-stream` | non-stream latency and usage path |
| L1.3 | `slow-first-byte` | TTFB accounting and timeout behavior |
| L1.4 | `slow-thinking-then-text` | thinking latency and first text latency |
| L1.5 | `stream-idle-timeout` | idle stream cleanup |
| L1.6 | `json-exception200` | JSON error normalization on 200-like upstream behavior |
| L1.7 | `rate-limit429` | 429 classification and cooldown |
| L1.8 | `server-error500` | retry/failure accounting |
| L1.9 | `invalid-tool-format` | upstream invalid tool format handling |
| L1.10 | `malformed-sse` | malformed stream cleanup |
| L1.11 | `client-drop` | client disconnect cleanup |
| L1.12 | `recovery-after-burst` | recovery after sudden failures |

## L2 Real Low-Concurrency

Start with `requests=5-20`, `concurrency=1-3`.

Required routes:

- `/cc/v1/messages`
- `/v1/messages`
- configured `/dfcache/<name>/v1/messages`
- missing `/dfcache/<name>/v1/messages`

Required request types:

- normal stream
- normal non-stream
- thinking stream
- tool-use stream
- alias model
- invalid model/request

## L3 Burst And Recovery

Run stepwise bursts. Do not jump directly to high real concurrency.

Example sequence:

1. 1 concurrency, 5 requests.
2. 5 concurrency, 20 requests.
3. 10 concurrency, 50 requests.
4. sudden spike to target concurrency for a short burst.
5. sudden invalid traffic burst.
6. return to 1-3 concurrency normal traffic.

Pass criteria:

- normal traffic after the burst succeeds.
- cooldowns do not permanently suppress healthy accounts.
- request ids and error ids are present.
- memory and FD counts return near baseline.

## L4 Restart And Failure Chaos

Run restart and failure cases against the repository's designated
project-owned validation instance from `docs/testing/project-test-instance.md`.
Reuse that single `kiro.rs` process for the whole validation run. A temporary
`kiro.rs` process is permitted only when the case would irreversibly corrupt
the shared validation PostgreSQL/Redis/credential state and the same behavior
cannot be proven with ordered cases, bounded concurrency, unique run IDs, or
cleanup. The evidence report must record the exception, exact resources,
lifetime, and cleanup result. A temporary fake upstream or proxy does not
count as a second `kiro.rs` instance.

Cases:

- restart temp proxy while low-volume traffic is active.
- send client disconnects during stream.
- simulate upstream 429 burst, then normal traffic.
- simulate upstream 500 burst, then normal traffic.
- simulate invalid tool-use errors, then normal tool-use traffic.

Pass criteria:

- in-flight failures are bounded and understandable.
- new requests after restart succeed.
- no orphaned temp processes.
- no long-lived socket or FD growth.

## L5 Soak

Use fake upstream for high concurrency soak first. Use real upstream only with explicit low caps.

Track:

- RSS start/peak/end.
- FD start/peak/end.
- TTFB p95/p99.
- total latency p95/p99.
- first text p95/p99.
- first thinking p95/p99.
- status distribution.
- queueing symptoms.

The run fails if RSS or FD remains elevated after idle cooldown, or if p95 latency worsens without a matching upstream/fake-server delay.
