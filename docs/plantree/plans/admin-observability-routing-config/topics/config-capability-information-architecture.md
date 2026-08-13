# Config And Capability Information Architecture

## Backend Current State

- Backend already has several separated modules:
  - `src/anthropic/handlers/parsed_body_pipeline.rs` for parsed Anthropic preprocessing.
  - `src/anthropic/handlers/local_body_pipeline.rs` for local Kiro body preparation.
  - `src/external_pool/body_pipeline.rs` for external raw/normalized body preparation.
  - `src/external_pool/model_pipeline.rs` for external pool model processing.
  - `src/external_pool/usage_projection.rs` for usage projection.
  - `src/external_pool/retry_pipeline.rs` for external retry helpers.
  - Converter internals are split into schema/content/tools/history-related modules.
- Raw external body mode already returns before normalized payload processing unless raw model probing/rewrite is configured.
- Model processing is separable from body mode at the backend level, but UI wording can still make it look subordinate to raw passthrough.

## UI Current State

- External pool form has sections for connection, scheduling, usage/cost, request body, model processing, and error handling. This is a good direction but not complete:
  - `rawModelMode` is rendered only inside the raw body branch, making model processing look like a body-mode sub-option.
  - Supported models are missing.
  - Model support eligibility and model mapping are not distinguished.
- Runtime settings page still uses broad sections such as "请求容量", "请求体处理", "缓存策略", "模型映射", and "兼容行为".
  - Retry is mixed with capacity.
  - Model parsing is in compatibility.
  - Payload guard, image handling, compression, and body conversion share a broad body surface.
  - Switches and subordinate parameters are visually close but not always grouped by dependency.
- Legacy UIs have even larger single panels and need the same field coverage, even if the visual polish is simpler.

## Target Grouping

- Routing and capacity:
  - local/global concurrency, queues, weighted capacity, local/external fallback.
- Credential and pool eligibility:
  - enabled/disabled, priority, supported models, per-account RPM/concurrency.
- Model resolution and mapping:
  - global model resolution mode, global model mapping, external pool outbound model mapping.
- Request body processing:
  - parsed preprocessing, image mode, body conversion, payload guard, payload shaping.
- Usage reporting and pricing:
  - reported usage policy, cache path shaping, external pool usage projection, pricing catalog.
- Retry and error handling:
  - credential retry max, prompt-logic retry, cooldowns, auto-disable policies.
- Observability:
  - usage log search, attempt trace fields, slow-stage timing, diagnostics toggles.

## Acceptance Criteria

- Backend remains capability-oriented: body, model, usage, retry, and eligibility are separate concepts.
- UI makes parent switch and subordinate config visually obvious.
- Raw body passthrough plus optional model handling is shown as "body mode" plus "model handling", not one forced feature.
- Old and new UIs expose the same new controls even if component structure differs.

## Implemented In This Pass

- External pool forms in all three UIs now separate:
  - connection information
  - scheduling settings
  - usage/cost projection
  - dispatch eligibility through supported models
  - request body processing mode
  - model processing and mapping
  - error handling and notes
- `normalizeModelVersionDots` moved under model processing in the edited surfaces instead of scheduling.
- Supported models are presented as dispatch eligibility, not model mapping.
- Raw body passthrough remains a body mode, while optional top-level model rewrite remains model handling.
- Runtime config pages now expose prompt-logic retry under compatibility/error handling controls with conservative disabled defaults.

## Remaining Design Debt

- The legacy admin UIs still have large single-file panels. They now expose the right controls, but deeper component extraction remains deferred.
- A future plugin-style backend ABI should further formalize body, model, usage, retry, and eligibility as mountable stages. This pass did not attempt that larger refactor.
