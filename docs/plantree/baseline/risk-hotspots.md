# Risk Hotspots

## CPU And Memory

- Base64 image media type normalization can decode large inline images.
- Remote/file source materialization can allocate full source bytes and base64 output.
- PDF/document conversion can expand payloads and keep large strings in memory.
- Tool/schema normalization recursively walks nested JSON.
- Payload guard can repeatedly serialize, clone, trim, and reserialize long contexts.
- Token counting scans messages/tools/system and can be expensive under high concurrency.
- Usage diagnostics and payload byte breakdown can rescan large bodies.

## Compatibility

- Claude Code CLI requires stable SSE event order, final usage, tool-use pairing, thinking behavior, and clear normalized errors.
- Kiro upstream has stricter body format expectations than generic Anthropic-compatible upstreams.
- External raw passthrough must remain cheap and not accidentally enter normalized body handling.

## Routing

- Body mode is an outbound body preparation capability, not a scheduler availability condition unless explicitly requested.
- External usage projection must not be disabled simply because body mode is raw.
- Local preflight should avoid heavy work when it can safely route raw external before parsing.
