# Claude Code ↔ Kiro 协议互转 P1 当前 HEAD 证据

Status: `focused-pass / runtime-validated / cli-pass`

Date: 2026-09-18 Asia/Shanghai

## Scope

This checkpoint is based on commit `09c0021` (`v0.0.164`). The working tree
also contains the uncommitted payload-guard fixture and Rust contract described
below, so `09c0021` is the source baseline rather than the exact tested tree
state. It does not promote the historical `v0.0.163` runtime report to
current-HEAD runtime evidence.

The implementation scope is intentionally narrow:

- add an independently readable payload-trim pairing fixture;
- execute the existing payload guard behavior against that fixture;
- prove the final serialized Kiro body contains no orphan `tool_use` or
  `tool_result` IDs;
- add a production-path, final fail-closed pairing invariant immediately before
  Kiro dispatch;
- keep the public external-pool error generic when that invariant rejects a
  body;
- reconcile the plan with current source facts for model canonicalization and
  retry boundaries.

No broad JSON repair, schema whitelist, synthetic thinking signature, unknown
event fallback, WebSearch index rewrite, or minimal tool-use reconstruction was
added.

## Current Source Behavior

- History trimming removes complete logical turns.
- An active current tool pair is preserved even when the history cannot be
  reduced below the configured limit.
- Orphan tool results are deleted.
- The guard does not invent a tool name or reconstruct a missing `tool_use`.
- After repair, shaping, and trimming, the Kiro guard validates the final
  pairing invariant again; any remaining orphan pair fails closed before
  dispatch.
- `redacted_thinking.data` is treated as an opaque string: empty, non-base64,
  and transcript-like strings are preserved byte-for-byte rather than decoded,
  canonicalized, or rejected.
- Missing or non-string `redacted_thinking.data` in history fails closed; a
  valid string does not get rewritten or used as a decoded-byte count.
- Signed/redacted reasoning attached to the active tool turn remains intact
  when historical thinking is discarded, and the corresponding
  `tool_use`/`tool_result` pair remains intact.
- The local Anthropic handler returns a generic `api_error`, and external-pool
  responses use a generic request-preparation error without pool or scheduler
  details.
- External-pool Claude Code model-name canonicalization is present in current
  code and focused contracts.
- `model_mapping_miss` and `model_unavailable` exclude the current external
  pool without same-pool replay; ordinary request 400 remains fail-closed.
- Local provider `MODEL_UNAVAILABLE` 400/404 can switch to another usable
  credential, while ordinary malformed/schema/tool/image/body-invalid 400
  remains fail-closed.

## Focused Evidence

Baseline:

```text
base HEAD: 09c0021
git tag: v0.0.164
branch: main
working tree: includes the payload-guard fixture and contract changes in this checkpoint
```

Current-tree release build:

```text
release profile: passed
binary SHA-256: 5c228febec2509e26c92100bea410006f1edb0cc9f46d65969effd4c09ddef5d
binary: frozen outside the scoped Cargo target and used for the runtime run
```

## C0 Verification

The scoped current-HEAD batch ran `cargo fmt --check`, the complete Rust test
suite, and `cargo build --release --locked` before copying the release binary
outside the scoped Cargo target.

```text
main binary: 2129 passed / 0 failed / 6 ignored
kiro_loadtest: 31 passed / 0 failed / 0 ignored
release build: passed
git diff --check: passed
Node model/leak contracts: 6 passed / 0 failed
artifact inventory: targets=0 reservations=0 target_processes=0 blockers=0
```

Focused contracts:

```text
model support aliases: 5 passed / 0 failed
provider bad-request retry matrix: 1 passed / 0 failed
payload_guard focused: 3 passed / 0 failed / 1 ignored
```

Fixture:

```text
feature/tests/fixtures/payload-guard-trim-pairing.json
```

Contract:

```text
anthropic::payload_guard::tests::payload_guard_trim_pairing_fixture_has_explicit_metadata_and_no_serialized_orphans
```

Command:

```text
KIRO_VALIDATION_RESERVE_KIB=1048576 \
feature/tests/run-cargo-scoped.sh payload-guard-trim-pairing -- \
  cargo test --locked \
  anthropic::payload_guard::tests::payload_guard_trim_pairing_fixture_has_explicit_metadata_and_no_serialized_orphans \
  -- --exact --nocapture
```

Result:

```text
payload_guard_trim_pairing_fixture_has_explicit_metadata_and_no_serialized_orphans:
  1 passed / 0 failed / 0 ignored
payload_guard_pairing_invariant_fails_closed_for_unpaired_kiro_messages:
  1 passed / 0 failed / 0 ignored
```

The scoped Cargo target and reservation were removed by the wrapper after each
focused and C0 batch.

## Runtime Gate Disposition

The designated project-owned runtime instance was started once on
`127.0.0.1:19023` with `tmp/thinking-budget-local/config.json` and the
user-provided credential file supplied directly as `--credentials`. The
credential file was not copied, printed, staged, or written into evidence.
The listener was verified as the frozen candidate during the run and was
absent after shutdown; the transient PID is intentionally omitted from the
durable redacted summary.

```text
service startup: passed; healthz HTTP 200
loaded credential configurations from the runtime input file: 222
database-authoritative credentials after batch import: 237
configured database: kiro_thinking_budget_20260901
Redis: 127.0.0.1:26379/0
request API key: database-authoritative key used in-process; secret omitted
Admin API key: database-authoritative key used in-process; secret omitted
listener after cleanup: absent
```

The first narrow attempt used only credentials `76` and `77`. Both discovery
calls succeeded, but both accounts had already exhausted their real quota. The
system's existing batch import and balance-refresh workflow was then used
before retrying:

```text
batch credential import file: 235 credentials (IDs 76-310)
batch import result: total=235, success=15, skipped=220, failed=0
database credentials after import: 237
batch balance refresh: total=237, success=198, failed=39
balance-refresh failures: 22 refresh_rate_limited; 15 auth_403; 2 auth_401
target-model candidates with successful real discovery: 299, 302
```

Credentials `296` and `297` are mock API-key rows that advertise only
`claude-sonnet-4`; they were not used for the requested Sonnet 4.5/Haiku 4.5
runtime cases. The account model-discovery API was exercised against the real
Kiro upstream for credentials `299` and `302`, and both supported-model lists
were synchronized successfully:

During the subsequent real calls, the existing scheduler automatically disabled
credentials `304`, `305`, `306`, `307`, and `308` after upstream quota-exhausted
responses.
Those state changes are recorded as `system-scheduler/auto_disable_credential`
events, not manual test mutations. Credentials `76/77` were restored to their
original disabled/empty-whitelist state; `296/297` stayed enabled and unchanged.

```text
POST /api/admin/credentials/299/supported-models/discover: 200, count=9
POST /api/admin/credentials/302/supported-models/discover: 200, count=9
POST /api/admin/credentials/299/supported-models/sync: 200
POST /api/admin/credentials/302/supported-models/sync: 200
discovered model set includes:
  auto, claude-sonnet-4.5, claude-sonnet-4, claude-haiku-4.5,
  deepseek-3.2, minimax-m2.5, minimax-m2.1, glm-5, qwen3-coder-next
```

`GET /cc/v1/models` returned `200`. After refreshed-account selection, direct
C1 requests used only the permitted models `claude-sonnet-4.5` and
`claude-haiku-4.5`; all four returned HTTP `200`. PostgreSQL usage rows
recorded `routeKind=local_credential`, `localAttempted=true`, and
`upstreamModel` equal to the requested model:

```text
POST /cc/v1/messages non-stream:
  claude-sonnet-4.5: 200, request-id req_01b5D9UDHJ5arY7FiFvxcVsT,
    credential_id=299, output_tokens=1, cache_creation_input_tokens=4958
  claude-haiku-4.5: 200, request-id req_01rfYSV8j6uTJDFdi5Nk5YCF,
    credential_id=299, output_tokens=1, cache_creation_input_tokens=4958
POST /cc/v1/messages stream:
  claude-sonnet-4.5: 200, request-id req_01G3t2epX67YbrTnajNnuSVH,
    credential_id=299, output_tokens=1, cache_creation_input_tokens=4897,
    terminal_reason=completed
  claude-haiku-4.5: 200, request-id req_01pR6AoQZA7jwgefYN6F6EQc,
    credential_id=299, output_tokens=1, cache_creation_input_tokens=4949,
    terminal_reason=completed
```

The direct non-stream bodies contained successful assistant text (`pong` after
redaction). The direct stream bodies contained the expected
`message_start -> content_block_start -> content_block_delta ->
message_delta -> message_stop` lifecycle. The earlier 429 records are retained
as evidence of the incorrect 76/77-only selection attempt, not the final
account-availability result.

```text
credential 299: remaining=29.26, creditRemaining=29.26
credential 302: remaining=50.00, creditRemaining=50.00
credential 303: remaining=40.29, creditRemaining=40.29
credentials 76/77: restored disabled=true with empty supported-model whitelist
credentials 296/297: unchanged enabled mock rows with ["claude-sonnet-4"]
```

C2 used the installed Claude Code CLI `2.1.273`, isolated `HOME` and
`CLAUDE_CONFIG_DIR`, and the same service process:

```text
claude-sonnet-4.5: success; stream-json lifecycle contained thinking and text
  blocks, final output_tokens=130 and thinking_tokens=129, no internal-term
  leak; stdout SHA-256
  7979a8c56ff0907340a852337bd20855d801e46e9d406fc76b10893a2974a323
claude-haiku-4.5: success; stream-json lifecycle contained a text block,
  final output_tokens=1 and non-zero input/cache usage, no internal-term leak;
  stdout SHA-256
  b3c07480e89b127970f59b1d16ccf8db5531765a0d2caba04686e49f1d3ad020
claude-sonnet-4.5 with Bash: success; two-turn stream-json run contained
  tool_use and tool_result, final output_tokens=76 and thinking_tokens=55,
  no internal-term leak; stdout SHA-256
  8dc1ce9c411af75f904ad8b75e9fd35420e6c07aa0f5d42043acf0db903e7038
```

The successful CLI usage rows used credentials `299`, `302`, and `303`; all
recorded `status=success`, `routeKind=local_credential`,
`localAttempted=true`, exact requested `upstreamModel`, non-zero output usage,
and completed stream terminal state. The Sonnet CLI run produced a real
thinking block/delta, and the Bash run produced a real tool-use/tool-result
round trip.

The C1/C2 dynamic gate is therefore `runtime-validated / cli-pass`: account
selection after system balance refresh, model discovery, successful text,
thinking, tool-use, non-zero usage, SSE lifecycle, CLI consumption,
internal-term leak scanning, and cleanup are verified for the requested
models. This evidence is a validation result for the current candidate, not
an official upstream protocol specification.

## Deferred Or Blocked

- The historical `sub2api-kiro-protocol-interop-p0-20260916.md` report is
  scoped to `v0.0.163` and is not a current-HEAD PASS.
- Real third-party external-pool discovery/runtime combination is not rerun.
- Schema profile, multiple thinking blocks, WebSearch index remapping,
  endpoint/origin/profileArn acceptance, payload threshold semantics, and
  synthetic signature remain deferred pending real evidence.

## Secret Handling And Cleanup

No credential contents, refresh tokens, API keys, raw CLI transcript, or raw
request body was recorded. The credential file was only passed to the service
as an input path; it was not copied, staged, or included in evidence. The
unrelated untracked credential file in the working tree was not modified.
After validation, credentials `76` and `77` were returned to their original
disabled/empty-whitelist state, credentials `296` and `297` remained enabled
with `claude-sonnet-4`, and the runtime config was restored to
`kiroUpstreamBaseUrl=http://127.0.0.1:39124` with
`externalDirectPolicyEnabled=true`. The refreshed balance/account-info facts
were intentionally retained in PostgreSQL as the latest upstream evidence.
