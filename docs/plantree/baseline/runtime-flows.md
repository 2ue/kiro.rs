# Runtime Flows

## Messages Entry

1. `request_entry::handle_messages_endpoint` receives raw bytes.
2. Raw external direct and raw external local-preflight fallback can run before full body parsing.
3. If not short-circuited, the raw body is parsed into `MessagesRequest`.
4. `post_messages_inner` applies parsed request processing, resolves model, and decides local or external routing behavior.

## Local Credential Flow

1. Parsed payload is adjusted for thinking trigger behavior.
2. Multimodal preprocessing may materialize file or URL sources and normalize base64 image media types.
3. Model is resolved through the local model catalog/mapping.
4. `local_body_pipeline::prepare` converts Anthropic payload to `KiroRequest`.
5. Payload guard may repair/shape/trim the Kiro body depending on runtime config.
6. Local stream or non-stream Kiro provider call is made.
7. Usage, latency, cache simulation, errors, and retries are recorded.

## External Pool Flow

1. Raw pre-parse direct/preflight routes require a raw-passthrough external pool.
2. Parsed external fallback/direct routes can select either normalized or raw pools unless a caller explicitly sets a body-mode filter.
3. `external_pool/body_pipeline.rs` prepares outbound bytes after the concrete pool is selected.
4. Raw passthrough keeps original bytes, with optional top-level model probing/rewrite.
5. Normalized mode prepares a parsed Anthropic body, optional external payload guard, model rewrite, and thinking normalization.
6. Usage projection is independent from body mode and follows route/path/external pool settings.
