# Storage And State

## Runtime State

- `AppState` owns shared runtime config, model capabilities, file store, prompt-cache trackers, usage recorder, and external pool manager.
- `KiroProvider` owns local credential pool state, runtime config snapshot, token manager, and upstream call clients.
- `ExternalPoolManager` owns external pool availability, leases, failover, error classification, and usage recording hooks.

## Request State

- `Bytes raw_body` is the authoritative inbound request body for raw external passthrough.
- `MessagesRequest payload` is the parsed Anthropic representation used by local and normalized external processing.
- `KiroRequest` is only needed for local credential calls.
- Usage records combine downstream reported usage, upstream raw usage, route subtype, latency, cost, and diagnostics.

## State Boundaries To Preserve

- Raw passthrough should not mutate or fully deserialize body unless an enabled stage explicitly needs it.
- Usage projection should not require body mutation.
- Payload guard reports are diagnostics and retry inputs; they should not become routing prerequisites.
