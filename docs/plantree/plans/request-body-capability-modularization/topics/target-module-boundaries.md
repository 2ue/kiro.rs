# Target Module Boundaries

## Capability Profiles

Each route target should be represented by a small profile:

- `LocalCredential`: parsed Anthropic preprocessing, local Kiro conversion, local payload guard, local token counting, local diagnostics.
- `ExternalNormalized`: parsed Anthropic payload, external payload guard when enabled, external model processing, external thinking normalization, external usage projection.
- `ExternalRaw`: raw bytes, optional raw model processing, external usage projection, no parsed body processing unless explicitly enabled later.

## Modules

- `ParsedAnthropicBodyPipeline`: implemented as `src/anthropic/handlers/parsed_body_pipeline.rs`; owns thinking and multimodal preprocessing for parsed requests.
- `LocalKiroBodyPipeline`: implemented as `src/anthropic/handlers/local_body_pipeline.rs`; owns Anthropic-to-Kiro conversion, Kiro payload guard, local token count, warnings, retry payloads.
- `ExternalBodyPipeline`: implemented as `src/external_pool/body_pipeline.rs`; owns raw/normalized outbound bytes after a concrete external pool is selected.
- `ExternalModelPipeline`: implemented as `src/external_pool/model_pipeline.rs`; owns external pool model mapping and raw top-level model probe/rewrite.
- `ExternalRetryPipeline`: implemented as `src/external_pool/retry_pipeline.rs`; owns normalized-only payload-too-long retry.
- `ExternalUsageProjection`: implemented as `src/external_pool/usage_projection.rs`; owns external usage projection and prompt-cache tracker commits.
- `LocalConverterModules`: implemented under `src/anthropic/converter/`:
  - `schema.rs`
  - `model.rs`
  - `content.rs`
  - `tools.rs`
  - `tool_pairing.rs`
  - `history.rs`
- `PayloadSizing`: owns byte counting and diagnostics without mutating payloads.
- `PayloadGuard`: owns mutation, repair, shaping, and trimming.
- `UsageProjection`: owns downstream usage reporting and cost accounting.
- `RoutePlanner` (deferred): owns target selection before deciding which expensive stages to run.

## Invariants

- Raw body mode never invokes normalized body stages.
- Normalized body mode never requires raw top-level rewrite.
- Usage projection can run for raw or normalized requests.
- Model processing can run without enabling general body processing.
- Payload guard disabled means the guard stage is skipped, not entered and then no-op'd for expensive work.
- A body-mode filter is used only when the caller requires a capability, such as pre-parse raw direct.

## Current Configuration Boundary

- `BodyConversionConfig` controls local Kiro converter compatibility capabilities only.
- External raw `rawModelMode` controls whether raw bytes are probed or top-level `model` is rewritten.
- External `requestBodyMode` controls raw vs normalized body bytes.
- External `usageProjectionMode` controls usage projection and billing independently from `requestBodyMode`.
- UI toggles expose local converter capabilities in the runtime/compatibility area; they are intentionally not attached to external raw body passthrough.
