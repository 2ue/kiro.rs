# Module Map

## Anthropic Request Surface

- `src/anthropic/handlers.rs`: main orchestration, runtime config snapshot, routing, local/external fallback coordination, usage context creation, stream/non-stream local handling, many tests.
- `src/anthropic/handlers/request_entry.rs`: unified raw-entry handling for messages endpoints; raw external direct/preflight checks before full JSON parse.
- `src/anthropic/handlers/local_body_pipeline.rs`: local Kiro request body preparation after Anthropic payload parsing.
- `src/anthropic/body_processing.rs`: multimodal source materialization, remote URL download, base64 image media type normalization, light-mode source rejection.
- `src/anthropic/converter.rs`: Anthropic-to-Kiro conversion, tool/schema normalization, tool-use repair, image/document conversion, thinking conversion, prompt-cache/cache-point metadata.
- `src/anthropic/payload_guard.rs`: Kiro and external payload guard, byte breakdown, payload shaping, trimming, safety repair, diagnostics.
- `src/anthropic/payload_guard_runtime.rs`: runtime wrappers for Kiro and external payload guard preparation.
- `src/anthropic/request_facts.rs`: lightweight raw body facts, top-level model probing, raw top-level model rewrite.

## External Pool Surface

- `src/external_pool.rs`: external pool config/types, selection, failover, usage recording, response proxying.
- `src/external_pool/body_pipeline.rs`: raw vs normalized external request body preparation.
- `src/external_pool/model_pipeline.rs`: external outbound model resolution/mapping.
- `src/external_pool/retry_pipeline.rs`: external retry request construction.
- `src/external_pool/usage_projection.rs`: external usage shaping and prompt-cache accounting projection.

## Load and Validation

- `src/bin/kiro_loadtest.rs`: fake upstream server and load/chaos client.
- `docs/testing/loadtest.md`: command reference for fake upstream, payload hotspots, resource reports.
