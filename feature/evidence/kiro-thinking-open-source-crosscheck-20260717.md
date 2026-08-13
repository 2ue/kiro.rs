# Kiro Thinking Protocol Cross-Check - 2026-07-17

Status: read-only protocol evidence; implementation and final wire validation remain open.

Last updated: 2026-07-18.

## Scope and authority

This review compares the current `kiro.rs` reasoning design with the installed official Kiro
application bundle and three local clones of public GitHub projects. The authority order is:

1. captured Claude Code CLI ingress and the final `kiro.rs` wire body;
2. the installed official Kiro bundle;
3. public-project source as secondary evidence only.

No project below is treated as an official protocol specification. No remote refs were changed and
no credentials, request bodies, or production traffic were inspected.

## Reviewed identities

| Source | Identity reviewed | Relevant surface |
| --- | --- | --- |
| Official Kiro app/agent | app `1.0.89`, agent `1.0.165`, bundle SHA-256 `8e6152abd4ae223f0efb1d7642005619432b3c710ec0ca7e107dadb2463aa7bb` | dynamic reasoning schema, signed/redacted history, signature-invalid retry |
| `chaogei/Kiro-account-manager` | `447adcdb468157312621b1f09448278bd9bca748` | dynamic effort mapping, reasoning history and compatibility retry |
| `TWLW9784/freedom-kirors` | `3c56390ab427f3e906c754e37b69af0fd0bce798` | hard-coded model/effort compatibility and native reasoning response |
| `nopperabbo/kiroxy` | `2ac2ea9232634d718f5a99aad88036f78e75aebf` | client effort parsing and native reasoning response |

Repository origins were read from each clone's Git configuration on 2026-07-17. The local commits
are point-in-time evidence and are not claims about each remote default branch's current head.

On 2026-07-18 a fresh GitHub web search rechecked current public signals. The strongest current hit
remains `chaogei/Kiro-account-manager` and its release notes:

- <https://github.com/chaogei/Kiro-account-manager>
- <https://github.com/chaogei/Kiro-account-manager/releases>

The public release notes now explicitly describe the same high-level facts found in the local source:
dynamic `additionalModelRequestFieldsSchema` handling, `output_config`/`reasoning` schema paths,
streaming reasoning output, and a `THINKING_SIGNATURE_INVALID` compatibility retry. They also state
that multi-turn history now drops `reasoningContent` because Kiro rejects that field in request
history. This reinforces the rule that third-party projects are useful drift detectors, not
authority for this repository's final contract.

## Official Kiro evidence

The official bundle reads `ListAvailableModels.additionalModelRequestFieldsSchema` and emits one of:

```json
{"output_config":{"effort":"<advertised-value>"}}
```

```json
{"reasoning":{"effort":"<advertised-value>"}}
```

The observed history request union is exactly one of:

```json
{"reasoningContent":{"reasoningText":{"text":"<exact-text>","signature":"<opaque>"}}}
```

```json
{"reasoningContent":{"redactedContent":"<canonical-base64-opaque-blob>"}}
```

The bundle retains reasoning text, signature, and the formatted model ID together. It only restores
signed history when that model ID matches the current formatted model. If Kiro returns the exact
structured reason `THINKING_SIGNATURE_INVALID`, it performs one pre-first-chunk retry after removing
history `reasoningContent`. It does not parse or modify the opaque signature.

Relevant minified-bundle offsets recorded during the read-only review are `143069`, `143220`,
`143924`, `195483`, `223446`, `227870`, `228575`, `228827`, `508797`, `509109`, and `510073`.

Anthropic history does not carry the official client's stored source model ID. A stateless proxy
therefore cannot reliably pre-detect every model transition. The safe compatibility path is to send
valid structured history first and handle the exact signature-invalid response once, before any
downstream commit.

## Public-project findings

### Kiro-account-manager

Useful corroboration:

- `proxyServer.ts` reads the model schema and recognizes both `output_config` and `reasoning` paths.
- `translator.ts` and `kiroApi.ts` map current-turn / response signed text and redacted content into
  Kiro/Anthropic-compatible reasoning content.
- `kiroApi.ts` retries after removing all history `reasoningContent`.

Behavior that must not be copied:

- Current local source intentionally drops history `reasoningContent` before sending request history
  to Kiro, citing backend `400 Improperly formed request`. That may be a valid compatibility choice
  for that project, but it cannot close this repository's signed/redacted history contract; our
  implementation still needs exact frozen Kiro wire and signature-invalid fixtures.
- The retry trigger uses `errMsg.includes("THINKING_SIGNATURE_INVALID")`, so an untrusted message
  substring can trigger it instead of an exact structured reason.
- After a failed compatibility retry, the surrounding endpoint loop can continue. That is broader
  fan-out than the official one-retry boundary.
- Unsupported effort is replaced with the final advertised enum value. This can silently turn one
  requested strength into another.
- Missing capability falls back to an invented adaptive field, and the `output_config` branch also
  invents `thinking:{type:"adaptive",display:"summarized"}`.
- Multiple redacted blocks are concatenated, although the observed Kiro history field is a strict
  union rather than an arbitrary merge container.

### freedom-kirors

Useful corroboration:

- It recognizes native `reasoningContentEvent` text, signature, and redacted output.
- It records a real compatibility distinction between models that accept `output_config` and models
  that reject `additionalModelRequestFields`.

Behavior that must not be copied:

- The request mapping is a hard-coded model table rather than the authoritative catalog schema.
- An explicit client `output_config` is silently skipped for unsupported models.
- The documented legacy thinking budget is clamped to `24576`, which is not a valid basis for the
  current Claude Code `xhigh`/`max` contract.

### kiroxy

Useful corroboration:

- It accepts `low`, `medium`, `high`, `xhigh`, and `max` at the client edge and handles native signed
  and redacted response events.

Behavior that must not be copied:

- Unknown effort falls back to `medium` instead of returning a clear protocol error.
- It does not provide evidence for signed Kiro request-history round trips or the exact
  signature-invalid compatibility retry.

## Resulting `kiro.rs` contract

- Accept only `low`, `medium`, `high`, `xhigh`, and `max` at ingress; invalid explicit values return
  a normalized 400 and are never silently clamped.
- Preserve an explicit supported value, including `max`, when the authoritative schema advertises
  it. Use a schema default only when the client omitted effort.
- Treat absent, invalid, ambiguous, and heterogeneous capability data as distinct states. Explicit
  reasoning intent must map to a proven compatible schema or return a clear error.
- Do not assume one credential's model catalog applies to every endpoint, region, subscription, or
  failover credential. Capability publication must be consistent across active credential cohorts,
  without request-path PostgreSQL, Redis, or upstream discovery.
- Preserve signed/redacted history as the exact strict union. Do not concatenate, sanitize, decode,
  re-sign, or otherwise mutate opaque integrity fields.
- Match `THINKING_SIGNATURE_INVALID` only at structured JSON `/reason` or `/error/reason`, exactly.
  Retry on the same credential at most once, consume the shared inference-attempt budget, and never
  cooldown, rotate credentials, route externally, or retry after downstream commitment.
- Build the stripped retry body on demand. The normal path must not clone or serialize a second
  large conversation body.

## Remaining proof

- Compile and execute the current Rust tests after all active patches settle.
- Capture the frozen candidate's final CLI and IDE-family Kiro wire bodies for all effort values.
- Exercise same-model signed history, changed-model invalid signatures, canonical redacted history,
  spoofed error messages, second-failure behavior, and stream/non-stream paths for at least five
  rounds per critical cell.
- Run heterogeneous endpoint/region/subscription capability and credential-failover fixtures.
- Bind all runtime evidence to one immutable candidate binary SHA-256 and remove raw captures after
  producing a redacted summary.
