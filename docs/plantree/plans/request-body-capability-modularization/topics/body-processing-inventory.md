# Body Processing Inventory

## Parsed Anthropic Stages

- Thinking override from model name: enables/modifies `thinking` when model aliases include thinking intent.
- Thinking trigger mode: may apply configured thinking based on request/model settings.
- Multimodal preprocessing:
  - Safe mode can materialize file sources from local file store.
  - Safe mode can download remote image/document URL sources up to 20 MB.
  - Safe mode can decode inline base64 images to correct media types.
  - Light mode rejects non-inline sources.

## Local Kiro Body Stages

- Anthropic-to-Kiro conversion, now split into focused modules:
  - conversation state construction
  - system/history/current message mapping
  - image/document conversion
  - tool definition conversion
  - tool name sanitization and mapping
  - schema normalization and unsupported field cleanup
  - tool_choice prompt injection
  - Write/Edit tool description suffixes
  - thinking/native reasoning/synthetic thinking handling
  - tool_use/tool_result pairing repair
  - placeholder tools for historical tool uses
- `BodyConversionConfig` can independently disable selected compatibility capabilities while defaulting to current behavior:
  - `toolSchemaNormalization`
  - `toolNameMapping`
  - `toolChoiceSteering`
  - `chunkedToolPolicy`
  - `thinkingPromptControls`
  - `nativeReasoningFields`
  - `toolPairingRepair`
  - `historyPlaceholderTools`
- Payload guard:
  - serialize request body
  - apply repair/shaping/trimming
  - produce diagnostics and retry body
- Token counting:
  - count model/system/messages/tools for local usage context.
- Diagnostics:
  - payload byte breakdown
  - conversion warnings
  - tool-format diagnostics on upstream rejection

## External Body Stages

- Raw passthrough:
  - keep inbound bytes as outbound bytes by default
  - optionally probe top-level model
  - optionally rewrite top-level model
  - never enters external normalized payload guard or local Kiro converter stages
- Normalized:
  - start from parsed `MessagesRequest`
  - optionally apply external payload guard
  - serialize to JSON value
  - apply external model mapping
  - normalize thinking fields for compatible external upstreams

## Usage/Pricing Stages

- Usage projection reads route policy, upstream raw usage, request facts, and prompt-cache state.
- It is independent from raw vs normalized body mode.
- Pricing uses shaped/original usage fields and model pricing catalog.
- Final validation confirmed a raw fallback record with `usageProjectionApplied=true` and `usageProjectionMode=current_path_policy`.

## Heavy Operations

- Base64 decode and whitespace stripping.
- Remote URL download and base64 encoding.
- PDF/document extraction.
- Recursive schema and JSON traversal.
- Full payload serialization and repeated trimming loops.
- Token counting across long messages/tools.
- Byte breakdown and diagnostics scans.
