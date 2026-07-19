# Kiro Reasoning Field Fidelity And Forced Thinking

Role: Current Rust runtime protocol-correctness analysis and implementation plan

Status: Plan Ready; implementation and live-upstream validation not started

As of: 2026-07-18

Current-state evidence revision: `401473c`

Authority: Defines the required target behavior for top-level Anthropic reasoning controls, Kiro `additionalModelRequestFields`, per-credential model capability handling, operator injection/force policy, and reasoning response visibility

Related: [Plan root](../README.md), [roadmap](../roadmap.md), [protocol contracts](../../../baseline/protocol-and-api-contracts.md), [module map](../../../baseline/module-map.md), [test and release gates](../../../baseline/test-and-release-gates.md), [configuration IA](../../admin-observability-routing-config/topics/config-capability-information-architecture.md)

## Executive Conclusion

The current runtime cannot guarantee that `thinking` or an explicitly requested effort reaches Kiro unchanged.

This is not evidence that Kiro or Claude only supports `high`. It is caused by local behavior at several points:

1. Input parsing caps `budget_tokens` and silently normalizes missing, invalid, or future effort values to `high`.
2. Native reasoning support is inferred from a hardcoded model-name list rather than the selected credential's advertised schema.
3. Unsupported values can be replaced by the last hardcoded value; for example, an `xhigh` request can become `max`.
4. The native-field builder always sets `thinking` to `None`.
5. The IDE endpoint conditionally invents `thinking`, while the CLI endpoint unconditionally removes it.
6. The complete request body is converted and serialized before a concrete credential, endpoint, region, and account scope are selected.
7. Response handling is gated by a pre-conversion `thinking_enabled` value, so a later injected/forced upstream request could still have its real reasoning response discarded locally.

The target must therefore be schema-driven and attempt-local:

```text
Anthropic request
  -> immutable ReasoningIntent
  -> upstream model resolution
  -> credential selection for this attempt
  -> exact capability lookup for credential + endpoint + region + model
  -> ReasoningDecision and attempt-local field materialization
  -> endpoint envelope-only transformation
  -> final outbound body
  -> response decoding using the effective ReasoningDecision
```

The non-negotiable fidelity rule is:

```text
low    -> low
medium -> medium
high   -> high
xhigh  -> xhigh
max    -> max
```

The same rule applies to any future effort value explicitly advertised by an upstream schema. A value may be translated between a schema-declared `output_config.effort` path and a schema-declared `reasoning.effort` path, but its value must not be downgraded, upgraded, clamped, or replaced to make a request succeed.

## Requirement Interpretation

For this topic, "thinking must not be deleted" means:

- An explicit top-level client `thinking` control must remain represented in the final local-Kiro request when the selected upstream capability supports it.
- An explicit client effort must remain the same value in the final local-Kiro request when the selected capability advertises that value.
- If the selected capability is known not to represent the request, the proxy must choose another compatible credential or return a clear error. It must not silently remove or rewrite the request.
- If capability information is unavailable, explicit client fields may be forwarded optimistically and left for upstream validation; they still must not be silently deleted.
- A configured injection policy may add missing fields, but may not alter an existing field.
- A configured force policy may override an explicit `disabled` value only because the operator deliberately selected an override mode. That action must be visible and auditable.

This guarantee is local protocol fidelity, not a promise that a changing closed-source upstream will accept every request. Upstream acceptance is proven by the exact model schema and outbound/response capture for the selected credential scope.

## Scope

In scope:

- Local Kiro IDE and Kiro CLI request paths.
- Streaming and non-streaming Anthropic Messages requests.
- Every model whose exact Kiro schema supports native reasoning fields, including future models not known to this repository.
- `thinking.type`, `thinking.display`, legacy `budget_tokens` where advertised, `output_config.effort`, and `reasoning.effort`.
- All schema-advertised effort values, explicitly including `high`, `xhigh`, and `max`.
- Model aliases and `-thinking` suffixes as request-intent sources, without using the name as proof of upstream support.
- Per-credential, per-endpoint, per-region/account capability discovery and persistence.
- Credential selection, sticky routing, retry, and failover.
- Runtime/Admin configuration, both maintained React UIs, persistence, reload, peer invalidation, and audit.
- Request diagnostics and reasoning response/signature presence diagnostics.
- Real Claude Code CLI capture and isolated real-Kiro A/B validation as release gates.

Explicitly separate but adjacent:

- Historical assistant `thinking` and `redacted_thinking` content blocks are conversation-history data, not top-level request controls. The current Kiro history representation cannot round-trip every Anthropic signature/redacted field, and payload shaping can remove history reasoning. That is a separate history-fidelity change and must not be falsely claimed as solved by this plan.
- Synthetic XML/prompt-based thinking is a compatibility presentation mechanism. It must not be confused with native Kiro reasoning fields.

Out of scope:

- Revealing hidden chain-of-thought that an upstream does not expose.
- Logging reasoning text, signatures, prompt bodies, credentials, or raw production requests.
- Applying Kiro schemas to arbitrary external providers.
- Changing byte-authoritative external raw passthrough.
- Reopening the Greenfield architecture plan. This is scoped maintenance for the current Rust runtime.
- Hardcoding a domain, account, region, endpoint, or a single model such as Opus 4.8.

## Evidence And Confidence

### Repository Evidence

The following findings are direct source evidence at revision `401473c`.

| Area | Current behavior | Consequence |
| --- | --- | --- |
| `src/anthropic/types.rs:79-129` | `budget_tokens` is capped to `24576` during deserialization | The original request is already lost before model capability is known |
| `src/anthropic/types.rs:231-252` | Missing, empty, invalid, and future effort values become `high` | Missing cannot be distinguished from explicit invalid input; future schema values cannot survive |
| `src/anthropic/converter/model.rs:128-146` | Native schemas are hardcoded for a small model list | A newly supported model silently misses native fields |
| `src/anthropic/converter/model.rs:166-186` | An unsupported requested effort is replaced by the last hardcoded effort | `xhigh`, `max`, or future values can change rather than fail clearly |
| `src/anthropic/converter/model.rs:189-222` | Both native branches construct `thinking: None` | Explicit thinking intent is not materialized by the central converter |
| `src/anthropic/converter.rs:558-582` | Native fields are decided before provider dispatch | The decision cannot use the actual attempt credential's schema |
| `src/kiro/endpoint/ide.rs:166-211` | IDE adds `adaptive/summarized` only when `output_config` exists | Endpoint code owns a semantic decision and only covers one shape |
| `src/kiro/endpoint/cli.rs:227-250` | CLI removes `thinking` unconditionally | CLI cannot satisfy an explicit or forced native-thinking request |
| `src/anthropic/handlers.rs:7529-7581` | Existing `Always` preserves `disabled`, rewrites unknown types, and inserts hardcoded `high` | It is neither "inject only when absent" nor an operator force override |
| `src/model/config.rs:136-193` | `nativeReasoningFields=false` can disable native output | A compatibility toggle can authorize silent loss of explicit caller intent |
| `src/kiro/model/available_models.rs:18-40` | The DTO can deserialize and retain `additionalModelRequestFieldsSchema` when upstream supplies it | The data shape is available, but a fresh retained capture is still required to prove the exact CLI/IDE schemas currently returned |
| `src/anthropic/model_capabilities.rs:452-479` | Conversion into the global catalog discards that schema | Capability data cannot guide request materialization |
| `src/kiro/provider.rs:1826-1864` | Global model sync returns the first non-empty credential result | One account's view is treated as global even when scopes differ |
| `src/anthropic/handlers.rs:4764-4794` | Local body preparation and serialization happen before provider dispatch | One prebuilt body is reused across potentially different attempts |
| `src/kiro/provider.rs:2646-2740` | Credential and endpoint are selected inside the retry loop, then that attempt transforms the prebuilt body | Correct schema-driven materialization must happen after selection and before endpoint transformation |
| `src/anthropic/handlers/local_body_pipeline.rs:189` and `src/anthropic/stream.rs:2358-2365` | Response reasoning is gated by pre-materialization input state | Forced/injected upstream reasoning could be received and then dropped locally |

Existing tests also encode lossy behavior. In particular, converter coverage currently expects Sonnet 4.6 `xhigh` to become `max`, and CLI endpoint coverage expects `thinking` to be removed. Those tests are current-state evidence, not target requirements.

The historical `docs/kiro-cli-capture-protocol-completeness-analysis-20260702.md` reports seeing this schema on a CLI model-list response, but its referenced temporary raw capture is no longer present. It is useful orientation, not reproducible current evidence; the real-capture gate below must replace it with retained, redacted evidence for both CLI and IDE scopes.

### Public Protocol Evidence

Evidence must be weighted by authority:

1. The AWS-published internal streaming client package is the strongest public Kiro wire evidence. `@aws/codewhisperer-streaming-client` added `additionalModelRequestFields` in `1.0.40` and retains it in `1.0.45`. Its type comment describes "thinking" and "effort" fields validated against a model schema before merging with server defaults. See the published [1.0.45 type declarations](https://unpkg.com/@aws/codewhisperer-streaming-client@1.0.45/dist-types/models/models_0.d.ts). It proves the wire entrypoint, but its internal-package status means it is not a supported public service contract.
2. Anthropic's official [adaptive thinking](https://platform.claude.com/docs/en/build-with-claude/adaptive-thinking) and [effort](https://platform.claude.com/docs/en/build-with-claude/effort) documentation proves that `high` is not a universal ceiling. Adaptive-capable models may support `max`, and Opus 4.8 additionally documents `xhigh`. Effort and `thinking.type=adaptive` are separate controls.
3. Current third-party Kiro runtime integrations, including [pi-kiro](https://github.com/javargasm/pi-kiro) and [pi-provider-kiro](https://github.com/simonsmh/pi-provider-kiro), parse `additionalModelRequestFieldsSchema` and send `thinking` plus effort to CLI-origin requests. These are useful corroboration, not official specifications.
4. Older gateways that only accept `low/medium/high` or inject XML tags prove only their own compatibility limits. They do not prove a current Kiro upstream ceiling.

The official Kiro CLI is closed source, so a real isolated capture remains required before implementation is declared validated. A missing `thinking` field in a captured request also does not by itself prove the server disables thinking, because server defaults may exist. The validation tuple must include the advertised schema, final request body, and reasoning response events/signature presence.

## Required Invariants

### Presence And Source

- The runtime must distinguish missing, explicit, injected, and forced values.
- Input shape must distinguish missing, explicit `null`, an empty object, and a populated object. An explicit malformed/null control is not silently treated as absence for injection.
- Parsing must retain the original effort string, original budget, thinking type, relevant extension fields, and source.
- Missing effort must not become an explicit `high` during deserialization.
- Invalid or unsupported explicit values must never become a different valid value.
- Model-name aliases may contribute intent, but a model name must never be used as the sole capability authority.
- Duplicate JSON keys at `thinking`, `thinking.type`, `thinking.budget_tokens`, `output_config`, or `output_config.effort` must be rejected rather than relying on last-key-wins parsing.

### Effort Fidelity

- An effort advertised by the selected capability must be sent exactly.
- Effort enum order must not be interpreted as a fallback preference.
- `xhigh` must not become `max`, even when `max` is supported.
- `max` must not become `high`, even when `high` is the schema default.
- Missing effort uses the upstream schema default only when a field must be materialized. It is not globally defaulted to `high`.
- `model_default` with no schema-declared default means omit an optional effort field. If the schema requires effort but declares no usable default, policy injection fails clearly; never choose `high`, the first enum item, or the last enum item.
- If the schema has no default and injection requires an effort, use an operator-selected value only when that exact value is advertised; otherwise fail clearly.
- Future schema-advertised values must be retained as strings and supported without a source-code model whitelist.
- Legacy budgets must be preserved and validated against the selected capability. They must not be clamped.

### Thinking Fidelity

- Explicit `thinking` must survive central conversion and endpoint transformation.
- CLI and IDE must have the same reasoning semantics for the same capability.
- Endpoint transforms may change envelope/origin/profile/header fields only. They must not add, delete, clamp, or reinterpret reasoning.
- `inject_if_missing` may add `thinking` only when the entire client field is absent. Existing `enabled`, `adaptive`, `disabled`, unknown/future values, and supported extension fields remain untouched.
- `force` may override missing or explicit `disabled`; this is the only mode allowed to override explicit disabled intent. It does not rewrite an existing non-disabled type.
- Force must use the capability-correct type: typically `adaptive` for adaptive schemas, or `enabled` plus a validated budget for legacy schemas.
- If no known capability can represent the forced request, force must fail. It must not claim success after omitting the field.
- A feature toggle such as `nativeReasoningFields=false` must never delete explicit client intent.

### Attempt And Response Fidelity

- Every credential attempt must independently materialize its final reasoning fields.
- Retry/failover must never reuse fields validated for a different credential scope.
- A retry must never remove thinking or lower effort as a compatibility fallback.
- The effective attempt decision must flow into streaming and non-streaming response handling.
- A real upstream `reasoningContentEvent`, redacted event, or signature must not be ignored because the original client omitted thinking when local policy injected or forced it.

## Target Domain Model

### `ReasoningIntent`

Capture client/operator semantics before validation or normalization:

```text
ReasoningIntent
  thinking_presence: missing | null | empty_object | explicit
  thinking_type: optional raw string
  thinking_fields: retained supported/future fields
  explicit_disabled: bool
  budget_tokens: optional original integer
  effort_presence: missing | null | empty_object | explicit
  effort: optional original raw string
  client_effort_path: output_config | reasoning | none
  sources: client | model_suffix | request_trigger | admin_inject | admin_force
```

Requirements:

- Presence must be stored separately from value.
- `effort` and `thinking_type` must not be schema-fixed Rust enums at this layer; upstream schemas can evolve.
- Unknown fields are retained only for later capability validation. They are not blindly forwarded to an unrelated provider.
- The immutable intent is shared by retries. A retry derives a new decision rather than mutating the intent.
- Numeric parsing must reject overflow and retain zero/negative/out-of-range budgets until selected-capability validation can produce a precise error; it must not clamp them into a valid range.

### `ReasoningCapability`

Parse the selected model's exact `additionalModelRequestFieldsSchema` into an operational form while retaining the raw schema:

```text
ReasoningCapability
  key:
    credential_id
    credential_revision_or_scope_fingerprint
    endpoint_kind
    api_region
    account_or_profile_scope_hash
    upstream_model_id
  effort_path: output_config | reasoning | none
  allowed_efforts: ordered set of raw strings
  default_effort: optional raw string
  supports_thinking_field: yes | no | unknown
  allowed_thinking_types: set of raw strings
  default_thinking_type: optional raw string
  supports_display: yes | no | unknown
  allowed_display_values: set of raw strings
  default_display: optional raw string
  supports_budget_tokens: yes | no | unknown
  budget_min: optional integer
  budget_max: optional integer
  raw_schema
  schema_hash
  source
  synced_at
```

Parser requirements:

- Read both `output_config.effort` and `reasoning.effort` shapes.
- Read nested `properties`, `required`, `enum`, `const`, `default`, numeric limits, and schema branches such as `oneOf`/`anyOf` when present.
- Do not guess when a schema shape cannot be interpreted. Retain the raw schema and mark the relevant capability unknown.
- A static compatibility registry may be an explicitly versioned fallback with provenance, but it must never override a fresh exact schema or infer support from a model family name.

### `ReasoningDecision`

Produce one decision per actual upstream attempt:

```text
ReasoningDecision
  requested_intent
  effective_thinking_fields
  effective_effort_path
  effective_effort
  capability_key_and_source
  action: preserve | inject | force | path_translate | reject
  validation_result
  override_reason
  response_reasoning_expected
```

The decision is the authority for the final request and response decoder. It is metadata only; it must not contain prompt or reasoning content.

## Capability Discovery And Storage

The current global first-successful-credential catalog is insufficient. Reasoning capability can vary across credential, endpoint, region, account/profile, and model.

The target must:

1. Save the complete model schema when a specific credential discovers supported models.
2. Key it by a non-secret scope fingerprint, credential ID/revision, resolved endpoint, API region, account/profile scope, and upstream model ID.
3. Atomically save model eligibility and its reasoning schema so scheduling does not observe mismatched generations.
4. Invalidate a snapshot when endpoint, region, auth/profile scope, or credential revision changes.
5. Sync all enabled credentials with bounded concurrency, or lazily refresh an exact scope before a reasoning-dependent request. Startup sync must not stop after the first non-empty account.
6. Retain schema hash, source, and timestamp for diagnostics and audit.
7. Never store raw keys or tokens in the fingerprint or diagnostic output.
8. Fetch every model-catalog page. A response that still has `nextToken` after a safety limit is `incomplete`, not a successful proof that omitted models are unsupported.
9. Bound raw schema bytes, nesting depth, branch count, enum count, and string length before persistence or Admin rendering.
10. Treat effective agent mode as part of the capability scope when it can affect the schema, or record real evidence proving that it does not.
11. Preserve last-known-good snapshots across a transient refresh failure, while marking freshness and completeness explicitly.

Capability freshness policy:

- Fresh exact capability is authoritative.
- A stale exact snapshot may be used to route while an asynchronous refresh runs, but any upstream schema-validation error invalidates it immediately.
- For explicit client fields with unknown capability, optimistic exact forwarding is allowed so that local code does not delete future protocol values; upstream validation remains authoritative.
- For synthetic injection or force with unknown capability, perform a bounded refresh. If capability is still unknown, return `reasoning_capability_unknown`; do not guess a type/path/value.
- A capability snapshot that is incomplete, malformed, or beyond its hard-expiry threshold is unknown, not proof of non-support.

## Runtime Configuration

Introduce a dedicated top-level local-Kiro policy object. It must be separate from synthetic visible-thinking prompt controls and from response extraction:

```json
{
  "reasoningForwarding": {
    "thinkingPolicy": "request_only",
    "injectedEffort": "model_default",
    "injectedDisplay": "summarized",
    "legacyBudgetTokens": 20000
  }
}
```

### `thinkingPolicy`

| Value | Meaning | Existing `thinking` | Missing `thinking` | Explicit `disabled` |
| --- | --- | --- | --- | --- |
| `request_only` | Preserve request-level intent; do not synthesize from admin policy | Validate and preserve | Leave missing, except an explicit `-thinking` request alias remains a request source | Preserve |
| `inject_if_missing` | Add capability-correct thinking only when absent | Ignore for injection; validate and preserve exactly | Inject | Preserve; do not override |
| `force` | Operator requires native thinking for every applicable local request | Preserve and validate every non-disabled value; incompatible means reroute/error | Inject | Override with capability-correct enabled/adaptive value and audit |

This three-mode policy is required. A boolean cannot distinguish the user's two requested operations: "add when absent, ignore when present" and "force thinking even when explicitly disabled."

### Injection Values

`injectedEffort`:

- `model_default` is the conservative default.
- Explicit strings such as `low`, `medium`, `high`, `xhigh`, and `max` are accepted as configuration values.
- The UI should populate the union of currently advertised schema values and must not assume the five known values are exhaustive.
- An explicit configured value is validated for every selected attempt. Unsupported means reroute or error, never fallback.

`injectedDisplay`:

- `model_default`: omit the field and use the schema/server default.
- `summarized`: request supported summary events.
- `omitted`: request hidden/omitted display when the schema advertises it.
- Unsupported configured display is an explicit error in injection/force mode.

`legacyBudgetTokens`:

- Used only when the selected schema requires `thinking.type=enabled` with a budget and the request did not provide one.
- Validated against the selected schema without clamping.
- Ignored for adaptive schemas.

Recommended defaults:

```text
thinkingPolicy   = request_only
injectedEffort   = model_default
injectedDisplay  = summarized
legacyBudgetTokens = 20000
```

## Policy Precedence

Apply sources in this order while filling only fields that the active mode is authorized to fill:

1. Client explicit `thinking`, including `disabled`.
2. Client explicit effort, with the exact raw value retained.
3. Explicit model `-thinking` suffix or equivalent request alias, which may fill fields absent from the client request.
4. `inject_if_missing`, which may fill only an absent thinking object and any transport fields required for that injected object.
5. `force`, which may additionally override explicit disabled, but may not rewrite any other existing thinking type or replace a supported explicit effort.
6. Exact selected capability validates and chooses the wire path. It never supplies an arbitrary fallback value.

Conflict rules:

- Explicit `disabled` plus explicit effort is contradictory in non-force modes. Return an Anthropic-compatible 400 rather than discard one side.
- Explicit effort supported by a selected schema remains exact in force mode.
- An explicit non-disabled thinking type remains exact in force mode. If it is incompatible with every eligible schema, return a caller error; do not turn legacy `enabled` into `adaptive` or rewrite a future type.
- Explicit unsupported effort causes another compatible credential to be selected or a 400. Force does not authorize effort substitution.
- Explicit legacy `enabled + budget_tokens` is preserved only on a schema that represents it. An adaptive-only schema receives a clear incompatibility error rather than an implicit conversion.
- A schema-declared path translation from client `output_config.effort` to upstream `reasoning.effort` is allowed because the semantic value remains exact. Record `path_translate` in diagnostics.

## Attempt-Local Materialization Algorithm

For each provider attempt:

1. Select a credential using model eligibility plus the immutable reasoning requirements.
2. Resolve its endpoint, API region, profile/account scope, and upstream model ID.
3. Load the exact `ReasoningCapability`; refresh if required by policy and unavailable.
4. Merge the immutable `ReasoningIntent` with the configured policy according to the precedence table.
5. Validate thinking type, display, budget, effort value, required fields, and wire path.
6. If known incompatible, skip this credential without changing intent. Record the reason and try another eligible credential.
7. If compatible, materialize `additionalModelRequestFields` on a fresh clone of the canonical Kiro request for this attempt.
8. Serialize, then let the endpoint add transport-only envelope fields such as `origin` and `profileArn`.
9. Capture the `ReasoningDecision` alongside the successful attempt so response handling knows whether native reasoning is expected.
10. On retry/failover, restart at step 1. Never mutate or reuse the prior attempt's materialized fields.

The canonical request must not retain a materializer-owned field from a previous attempt. If attempt A uses `output_config.effort` and attempt B uses `reasoning.effort`, B's final body contains only B's schema-declared path, not both paths.

Scheduler requirements:

- Dispatch requirements must include model, requested thinking type, effort, budget constraints, and required wire fields.
- A sticky credential known to be incompatible must be bypassed for this request. Sticky affinity cannot justify deleting reasoning.
- A schema-validation upstream error should invalidate/refresh that exact capability and may retry a different compatible credential.
- A request-specific reasoning incompatibility must not automatically disable an otherwise healthy credential.
- There must be no "retry without thinking" or "retry at high" path.
- Token refresh, quota/risk-control failover, payload-too-long retry, cache-point retry, and any pre-commit stream retry must rematerialize from the same immutable intent.
- Runtime reasoning configuration is snapshotted once per incoming request. Its revision stays fixed across retries even when the global configuration changes mid-request.
- Once a stream has committed reasoning, text, tool, or signature output to the client, it must not fail over and replay a second model response.

## Endpoint Responsibilities

IDE and CLI endpoints may:

- Select the URL and headers.
- Rewrite request origin/agent mode where required.
- Inject a profile ARN or other envelope identity.
- Apply transport compression after semantic materialization.

IDE and CLI endpoints must not:

- Insert `thinking` based on the presence of `output_config`.
- Delete `thinking` based on endpoint kind.
- Normalize or clamp effort.
- Change `adaptive` to `enabled`, or the reverse.
- Select a model default.
- mutate reasoning fields as part of an upstream-error retry.

The final semantic `additionalModelRequestFields` for IDE and CLI must be identical when their exact advertised schemas are identical. Differences are permitted only when the schemas differ, and the difference must come from the central attempt-local materializer.

## Response-Side Contract

Request fidelity is incomplete unless the matching response is retained.

- Streaming and non-streaming handlers must use the successful attempt's `ReasoningDecision`, not the original payload's pre-conversion `thinking_enabled` flag.
- If the effective decision enabled native reasoning, `reasoningContentEvent` must be decoded even when the client originally omitted `thinking` and Admin injected or forced it.
- A native reasoning/redacted/signature event unexpectedly returned by upstream is still protocol data and must not be silently discarded. `response_reasoning_expected` is a diagnostic expectation, not permission to delete an event.
- Text, redacted content, and signature presence must follow the existing Anthropic-compatible stream/non-stream ordering contracts.
- A missing visible reasoning summary is not proof that reasoning was disabled when display is `omitted` or the upstream returns no visible text.
- Acceptance must inspect event/signature presence together with the request and capability schema.
- The product must not expose private hidden chain-of-thought beyond what the upstream protocol explicitly returns.

## Errors

Known caller incompatibilities return an Anthropic-compatible `400 invalid_request_error` before sending an altered request. A constraint that exists only because of Admin `inject_if_missing`/`force` is operator policy: if no eligible capability can honor it, return 503 rather than blame the caller. Capability refresh/storage failure is also operational and returns 503 when policy cannot otherwise be honored.

| Code | HTTP | Meaning |
| --- | --- | --- |
| `reasoning_effort_invalid` | 400 | Client-explicit effort is empty/malformed under the client contract |
| `reasoning_effort_unsupported` | 400 | No eligible known schema advertises a client-explicit effort |
| `thinking_type_unsupported` | 400 | No eligible known schema represents a client-explicit thinking type |
| `thinking_budget_unsupported` | 400 | A client-explicit budget is invalid or has no compatible representation |
| `reasoning_conflict` | 400 | Client-explicit fields contradict, such as disabled plus effort in a non-force mode |
| `reasoning_not_supported` | 400 | No eligible credential supports reasoning explicitly requested by the client/model alias |
| `reasoning_policy_not_supported` | 503 | No eligible credential can honor a field/value created only by Admin injection/force |
| `reasoning_capability_unknown` | 503 | Injection/force cannot be honored because exact capability remains unknown/unavailable |
| `reasoning_schema_invalid` | 503 | Upstream returned a schema the parser cannot safely interpret for required policy injection |

Client messages should name the requested model/value and list available values without exposing credential identity or private upstream details.

When credential A is known not to support an explicit value but credential B supports it, A is skipped and B is selected. A 400 is valid only after all eligible known capabilities are incompatible with a client/model-alias constraint. If the missing capability is needed only to satisfy Admin injection/force, or every otherwise eligible capability is unavailable/unknown and strict policy prevents optimistic forwarding, return 503.

## Admin API And UI

Add a distinct "Reasoning 转发" section in both maintained React Admin UIs (`ui/src/features/runtime/runtime-page.tsx` and `admin-ui/src/components/runtime-config-panel.tsx`), following the existing configuration IA.

Controls:

- Three-mode segmented selector: `按请求`, `缺失时补充`, `强制开启`.
- Injected effort selector: `模型默认` plus the dynamic union of advertised values.
- Display selector: `模型默认`, `summarized`, `omitted`, filtered/validated by capability.
- Legacy budget numeric input, shown only as relevant help for enabled-style models.
- A clear warning in force mode: explicit client `thinking.type=disabled` can be overridden, and reasoning may increase cost and latency.
- A read-only capability table showing model, credential label, endpoint, region, effort path, allowed efforts, default effort, thinking types, display support, source, schema hash prefix, and sync age.

Behavior:

- Saving must validate the config shape, persist it, reload the local runtime, invalidate peer runtime state, and emit an Admin audit event.
- Force-mode changes require an explicit confirmation in the UI but no per-request confirmation.
- Audit records contain old/new policy metadata, operator action, and outcome, never secrets or reasoning content.
- Capability sync UI must preserve full schemas, not only write a list of model IDs.
- The existing `thinkingTriggerMode` UI must not remain as a second ambiguous authority.
- Partial/legacy Admin updates that omit `reasoningForwarding` must preserve the stored value rather than reset it to defaults.
- Every admitted request records the chosen config revision so concurrent save/reload activity cannot make an attempt sequence internally inconsistent.

## Migration From Existing Settings

The existing settings mix wire forwarding with synthetic visible-thinking behavior. Migration must be deterministic:

| Existing state | New wire policy | Notes |
| --- | --- | --- |
| `thinkingTriggerMode=real_request` and no new object | `request_only` | Preserves conservative request-driven behavior |
| `thinkingTriggerMode=always` and no new object | `inject_if_missing` | Existing `always` does not override explicit disabled, so it must not silently become force |
| New `reasoningForwarding` object present | Use new policy | It is the sole wire authority |

Additional migration rules:

- Keep or deprecate `thinkingTriggerMode` only for synthetic/visible output behavior; rename its UI meaning so it cannot be confused with upstream forwarding.
- `thinkingPromptControls` controls synthetic prompts only.
- `nativeReasoningFields=false` may temporarily disable proxy-generated compatibility fields during rollout, but it must no longer delete explicit client reasoning. Once the new policy is stable, remove this overlapping authority from the UI/config contract.
- Existing stored config must deserialize with the conservative defaults above.
- `force` is never inferred from an old value. It must be explicitly selected after upgrade.

## External Pool And Special-Route Boundaries

External raw passthrough:

- Remains byte-authoritative.
- Preserves whatever reasoning fields the client supplied in the raw bytes.
- Does not receive Admin injection or force, because modifying the body would violate raw passthrough.

External normalized pools:

- Require their own provider-specific reasoning capability contract.
- Must not use Kiro's `additionalModelRequestFieldsSchema` or Kiro path rules.
- Can adopt the same intent/decision abstractions later, with a separate capability implementation.

The new injection/force policy applies only after a route enters a local-Kiro attempt. A local-to-external raw rescue must not carry locally injected fields into the byte-authoritative body, and an external-to-local rescue materializes only when the local attempt is actually selected.

WebSearch or another special local route:

- Is included only if that actual upstream operation advertises and accepts the same reasoning fields.
- Must otherwise reject an explicitly required reasoning request or route to a compatible normal path. It must not silently discard the fields.

## Observability And Privacy

Record metadata needed to prove decisions:

```text
reasoning_requested
thinking_requested_type
effort_requested
thinking_policy
reasoning_source
effective_thinking_type
effective_effort
effort_path
capability_source
capability_schema_hash
credential_id_or_admin_label
endpoint
transform_action
reasoning_event_seen
reasoning_signature_seen
```

Do not record by default:

- Prompt or message text.
- Thinking/reasoning text.
- Signature or redacted content bytes.
- Raw production request/response bodies.
- API keys, bearer tokens, profile secrets, or raw credential fingerprints.

For protocol tests, use an isolated fake upstream or an explicitly isolated real-call capture sink with redaction and cleanup. Production debug logging is not an acceptable substitute for an exact test capture.

## Acceptance Matrix

| Input | Exact selected capability | Policy | Required final result |
| --- | --- | --- | --- |
| explicit `high` | supports `high` | any | Exact `high` at the schema-declared path |
| explicit `xhigh` | supports `xhigh` | any | Exact `xhigh`; never `max` or `high` |
| explicit `max` | supports `max` | any | Exact `max`; never `high` |
| future explicit effort | schema advertises it | any | Exact future value without a model whitelist |
| explicit unsupported effort | known schema | any | Choose compatible credential or local 400; no rewrite |
| missing effort | schema has default | injection/force | Use exact schema default when materialization requires effort |
| missing effort | schema has no default | request only | Remain missing unless the request otherwise requires a value |
| missing effort | optional effort with no default | injection/force + `model_default` | Omit effort; never guess an enum value |
| explicit adaptive | supports adaptive | request only | `thinking.type=adaptive` retained in final body |
| missing thinking | supports adaptive | inject if missing | Capability-correct adaptive field injected |
| explicit disabled | supports disabled | inject if missing | Disabled retained; no injection |
| explicit disabled | supports adaptive | force | Adaptive injected/overridden, warning and audit recorded |
| explicit enabled + budget | supports legacy enabled/budget | any | Exact budget or explicit range error; never cap |
| explicit enabled + budget | adaptive-only schema | any | Clear caller incompatibility error; force also must not silently convert it |
| output-config client path | schema uses reasoning path | any | Value unchanged, path translated, action recorded |
| CLI endpoint | schema supports thinking | any enabled intent | Thinking retained in exact outbound body |
| IDE endpoint | same schema as CLI | any enabled intent | Same semantic fields as CLI |
| retry to another credential | schemas differ | any | Re-materialize from immutable intent for the new scope |
| sticky credential incompatible | another credential compatible | any | Bypass sticky for this request; preserve fields |
| capability unknown + explicit fields | unknown | request only | Optimistic exact forwarding or explicit upstream error; no deletion |
| capability unknown + force | unknown after refresh | force | `reasoning_capability_unknown`; no false success |
| injected/forced request gets reasoning event | supports response event | inject/force | Stream/non-stream retains reasoning and signature presence |
| external raw pool | any | any | Raw bytes unchanged; local force documented as not applicable |

## Test And Evidence Plan

### Unit And Contract Tests

- DTO/presence tests prove missing differs from explicit `high`, invalid, and future values.
- Parsing tests cover missing/null/empty-object/wrong-type/duplicate-key shapes for both thinking and effort.
- Budget tests prove original values are retained and never capped.
- Capability parser fixtures cover `output_config`, `reasoning`, adaptive, enabled/budget, defaults, enums, unknown branches, and future values.
- Policy table tests cover all three modes, disabled precedence, model suffix intent, conflicts, and exact configured defaults.
- Materializer tests assert exact JSON for every schema-advertised effort and thinking type.
- Endpoint tests assert CLI and IDE preserve the materialized semantic object.
- Payload guard/shaping tests assert top-level reasoning controls are not trimmed or repaired away.
- The same matrix runs through `/v1`, `/na/v1`, `/ha/v1`, `/cc/v1`, and `/dfcache/{route}/v1` Messages routes, in stream and non-stream modes.

### Provider And Retry Tests

- Fake two credentials with different schemas and prove every retry is re-materialized.
- Prove a `high`-only credential is skipped for explicit `max` when another eligible credential advertises exact `max`.
- Prove failover from an `output_config` schema to a `reasoning` schema leaves only `reasoning.effort` in the second final body.
- Prove incompatible sticky credentials are bypassed without mutating intent.
- Prove schema-validation errors refresh only the affected scope and never trigger a no-thinking retry.
- Prove stream and non-stream response decoders use the successful attempt's decision.
- Prove force-injected requests do not lose `reasoningContentEvent` or signature output.

### Admin Tests

- Config API round-trip for all three modes and injection values.
- PostgreSQL persistence, restart reload, Redis/peer invalidation, and audit coverage.
- Both frontend type checks/builds and control-state tests.
- Capability table coverage for credential/endpoint/region distinctions and stale/error states.
- Catalog pagination, incomplete-snapshot, schema-size/depth limits, concurrent config save, restart, and old-client partial-update coverage.

### Exact Outbound Capture

The fake upstream must capture the body after endpoint transformation and compression handling, not merely inspect the converter object. For each IDE and CLI case, assert the object under:

```json
{
  "additionalModelRequestFields": {
    "output_config": { "effort": "max" },
    "thinking": { "type": "adaptive", "display": "summarized" }
  }
}
```

Also cover a schema whose path is `reasoning.effort`, legacy enabled/budget, and a future effort enum value.

### Real Claude Code CLI And Kiro A/B Gate

Run in an isolated environment with an explicitly scoped test credential. Save redacted evidence for each supported model family and both endpoint kinds:

1. Exact `ListAvailableModels.additionalModelRequestFieldsSchema` and schema hash.
2. Final `GenerateAssistantResponse` request body after all local transforms.
3. Upstream HTTP status and normalized validation error when applicable.
4. Whether `reasoningContentEvent` occurred.
5. Whether a reasoning signature/redacted event occurred, without saving its content.
6. Stream and non-stream client-visible event ordering.

Required cases include `high`, `xhigh`, `max`, effort-only, thinking-only, and effort plus adaptive/summarized. Do not assume every model supports every value; derive the matrix from each captured schema.

An implementation is not validated merely because the request returned 200. The evidence must prove the exact requested value and thinking field survived into the final wire body.

## Delivery Sequence

1. Preserve raw intent and presence during input parsing; remove early budget/effort loss behind tests.
2. Parse and persist exact per-credential reasoning capability without changing outbound behavior.
3. Introduce attempt-local materialization and make endpoints transport-only.
4. Propagate the successful `ReasoningDecision` into stream/non-stream response handling.
5. Add scheduler requirements, sticky bypass, retry rematerialization, and explicit errors.
6. Add the dedicated Admin config/API and both UI surfaces; migrate old settings conservatively.
7. Complete fake-upstream exact-body and response-event matrices.
8. Run isolated real Claude Code CLI/Kiro A/B validation per observed schema and endpoint.
9. Enable `request_only` as the default. Keep injection and force operator-controlled.
10. Refresh baseline/evidence documents only after implementation and validation land.

## Rollout And Rollback

Rollout:

- Land intent/capability observation first without changing default wire behavior.
- Expose per-scope capability status before enabling force.
- Default to `request_only`.
- Enable `inject_if_missing` or `force` only by explicit operator choice.
- Treat real validation by model family and endpoint as a release gate, not as optional smoke coverage.

Rollback:

- A protocol-safe rollback disables `inject_if_missing` and `force` and returns to `request_only`, while retaining explicit-field fidelity, attempt-local materialization, and transport-only endpoints.
- Keep or ship a request-only compatibility build if the Admin policy/UI must be rolled back separately.
- Never restore CLI's unconditional thinking deletion as a rollback mechanism.
- Never restore effort substitution or budget clamping as a success fallback.
- Preserve the capability snapshots and audit trail so a rollback can be diagnosed.
- A binary predating explicit-field fidelity is not a protocol-safe rollback target. Downgrade tooling must reject that path or require an explicit operator acknowledgement that this contract will be unavailable; it must never accidentally retain or invent force behavior.

## Completion Definition

This plan is complete only when all of the following are true:

- Every schema-advertised effort value is preserved exactly for all eligible local Kiro models.
- Explicit thinking survives both IDE and CLI final-body transformation.
- `inject_if_missing` ignores every existing thinking value.
- `force` can deliberately override missing/disabled thinking and is auditable.
- Unknown or unsupported capabilities result in exact optimistic forwarding or a clear error, never silent mutation.
- Retry/failover rematerializes against each exact credential scope.
- Injected/forced reasoning response events and signature presence survive stream and non-stream conversion.
- Both Admin UIs, persistence, reload, peer invalidation, and audit are verified.
- Fake-upstream exact captures and isolated real-upstream schema/request/response evidence are indexed.
- No temporary proxy, capture process, credential, port, database, Redis namespace, or raw secret artifact remains after validation.
