# 012: Tool-Definition Compatibility And Reversible Schema Mapping

Date: 2026-07-12

Status: Accepted

Scope: Empty tool descriptions, explicit-null input schemas, upstream property-key constraints, raw passthrough, normalized/local repair, response reverse mapping and stable local rejection

Affected requirements/findings: `FUN-004`, `FUN-005`, `FUN-045`, `FUN-046`, `INV-012`, `QA-COMP-001`-`QA-COMP-003`, `COR-006`, `COR-007`

Related: [Problem evidence](../topics/problems/correctness-security-and-resource-bounds.md), [Module contracts](../topics/architecture/module-boundaries-and-contracts.md), [Modular work map](../indexes/execution-slice-map.md), [Protocol verification](../topics/delivery/verification-rollout-and-rollback.md)

## Context

Two unregistered 2026-07-12 feature reports and current source inspection show three tool-definition failures:

- a missing or empty description becomes an empty string and Kiro rejects the request;
- explicit `input_schema: null` fails entry deserialization even though a missing field already becomes an empty schema;
- property names outside the target's accepted pattern reach Kiro/external upstreams and cause an opaque 400.

Blindly replacing invalid property characters is not safe. It can collapse two names, desynchronize `required`/dependency keywords and cause the model's returned `tool_use.input` keys to differ from the names the downstream client understands. `patternProperties` keys are regular expressions and `$defs` keys are schema-definition identifiers, not ordinary object property names.

## Decision

### Route/Profile Separation

Raw external mode remains byte-preserving. It performs only bounded raw facts needed for authentication/routing and never parses, repairs or renames tool definitions. Any destination rejection is returned through normalized error handling without mutating or replaying the request.

Kiro/local and explicitly normalized external profiles apply a versioned target-capability policy before sending one attempt:

- missing, empty or whitespace-only tool descriptions become one deterministic neutral nonempty description; an existing nonempty description remains semantically unchanged;
- absent or explicit-null `input_schema` becomes the same empty object schema only for these normalized profiles;
- malformed non-null schema types receive a stable local validation error rather than permissive conversion;
- diagnostics record bounded counters/reason codes and schema fingerprints, never raw descriptions, schemas or tool arguments.

Route selection and raw-byte preservation therefore occur before a profile-specific typed tool parse. One permissive DTO cannot silently normalize raw passthrough.

### Property-Key Mapping

Valid property names remain byte-identical. When a normalized target rejects an object `properties` name, the mapper may construct a request-local reversible map only under all rules below:

1. Every mapped name satisfies the target capability pattern/length and is deterministic for the original UTF-8 name plus tool/schema path.
2. A safe prefix plus SHA-256 suffix is used; collisions are detected in the concrete schema and resolved deterministically or cause local rejection. A lossy many-to-one map is prohibited.
3. The same object scope updates `required`, `dependentRequired`, `dependentSchemas` keys and legacy `dependencies` keys/array property-name entries; schema-valued dependencies recurse normally. Nested object scopes, including object schemas located inside `$defs`, receive independent maps.
4. `patternProperties` regular expressions, `$defs` identifiers, `$ref` targets, dynamic/additional-property semantics and arbitrary string values are not rewritten as property names. Recursive/union/reference traversal that cannot prove the response path and round trip is rejected locally with a stable reason.
5. The map is tied to the prepared tool-definition revision and mapped tool identity. Streaming and non-streaming response paths reverse-map every returned `tool_use.input` object at the matching schema path before Anthropic/Claude Code output.
6. Unknown keys returned by the model are preserved only when the original schema permits them; otherwise they follow the accepted validation/error policy. They are never guessed into an original name.

Normalized external destinations use their own declared capability. Property mapping is never assumed globally merely because Kiro requires `^[a-zA-Z0-9_.-]{1,64}$`.

### Failure And Replay

If normalization cannot prove semantic and reverse-mapping safety, reject before upstream execution with a stable `invalid_request_error`, error ID and bounded diagnostic reason. Do not send the original request merely to obtain an upstream 400, and do not retry a possibly executed POST.

## Authority

- `MOD-PROTO-ANTHROPIC` owns public tool DTO semantics, including profile-aware null handling.
- `MOD-PAYLOAD` owns pure normalized target validation/mapping and produces a versioned map in `MOD-REQUEST-ARTIFACTS`.
- `MOD-PROTO-KIRO` and `MOD-PROTO-EXTERNAL` own target capability codecs, not product repair policy.
- `MOD-RESPONSE` plus `MOD-PROTO-SSE` own streaming/non-streaming reverse translation through the prepared map.
- `MOD-TRANSPORT-PUBLIC` preserves the raw body until route/profile selection and maps local validation errors.

## Alternatives

### Forward every invalid schema unchanged

Rejected for normalized/Kiro profiles because it converts a deterministic local compatibility issue into an opaque upstream failure and unnecessary request/cost.

### Replace every invalid character with underscore

Rejected because it is non-bijective and does not repair response arguments or all property-reference keywords.

### Reject every nonconforming tool without repair

Safe but unnecessarily incompatible for schemas that admit a provably reversible map. It remains the required fallback when round-trip proof is unavailable.

## Verification

- Missing/empty/whitespace descriptions and absent/null/valid/malformed schemas cover every public route and raw/normalized profile.
- Valid property names remain unchanged; invalid, empty, long, Unicode and colliding names cover nested `properties`, required/dependency updates and deterministic mapping.
- Streaming/non-streaming tool-use outputs round-trip to original names; tool result pairing remains unchanged.
- `patternProperties`, `$defs`, `$ref`, additional/dynamic properties and unsupported combinations prove preserve-or-local-reject behavior.
- Raw external golden bytes are identical and no unselected profile performs tool parsing.
- Deterministic fake-upstream tests run before bounded, independent real-client validation; no production traffic mirroring or duplicate mutating comparison is allowed.

## Current Implementation Note, 2026-07-12

The current dirty tree implements the normalized Kiro/local schema-key subset of this decision:

- `sanitize` is the default and only maps invalid property names; valid names remain byte-identical and create no response map.
- Invalid property names are mapped to deterministic `key<16hex>` ids using SHA-256 over the mapped tool identity, schema path, original name and attempt.
- The reversible map is request-local and grouped by mapped/upstream tool name; no Redis/global mapping state is introduced.
- `reject` returns a local validation error before upstream execution; `disabled` preserves old passthrough behavior.
- `/v1/messages` non-stream and `/cc/v1/messages` stream real local-service calls round-tripped `bad key` back to the client without leaking the generated key.

This note records current-code evidence only. It does not mark any modernization module `Integrated` or close the future target-candidate gates.
