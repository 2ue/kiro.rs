# Cache Behavior Analysis

## Current Running Instance

- Service URL: `http://127.0.0.1:9022`
- Admin UI: `http://127.0.0.1:9020/admin`
- Observed process: `target/debug/kiro-rs`
- Active local config: `config.json`

## Current Cache Mode

The local instance is running with high-cache prompt-cache usage simulation:

```json
{
  "promptCacheSimulationMode": "high-cache",
  "promptCacheTargetReadRatio": 0.98,
  "promptCacheTokenScale": 1.6,
  "promptCacheMaxSimulatedInputTokens": 300000,
  "promptCacheCapJitterMinTokens": 12000,
  "promptCacheCapJitterMaxTokens": 24000,
  "promptCacheScaleMinInputTokens": 20000
}
```

In high-cache mode, the proxy builds a local prompt cache profile even when the request does not include explicit `cache_control`. If upstream metadata reports zero cache read/write tokens, the local simulated usage is used to fill Anthropic-compatible cache fields so downstream usage records are not all zero. Real upstream cache read/write values remain authoritative. When metadata cache is zero, high-cache simulation uses the larger of upstream metadata total input and local request estimate, then applies the high-cache-only token scale and deterministic soft cap.

For requests without metadata/session, the converter now derives a deterministic conversation id from the stable prompt anchor, which keeps the local prompt cache scope stable across turns.

Continuity gap note: the current implementation can still produce cache gaps in a continuous high-cache session when an intermediate request has a small estimated input. The reason is that prompt-cache simulation currently decides whether cache is possible from the current request's target token count before considering already-created entries in the same scope. The planned fix is documented in `docs/high-cache-continuity-cache-gap-plan.md`: high-cache should use an existing, non-expired entry in the same `credentialId + conversationId + model` scope as a read-only continuity floor for later small requests, while first small requests remain uncached.

## Real Test Summary

Clean logs and admin usage were cleared before the high-cache matrix. The final real run produced:

- Message requests recorded: `46`
- Success: `46`
- Errors: `0`
- `usageSource=local_prompt_cache`: `46`
- `simulated=true`: `46`
- Total cache read tokens: `179606`
- Total cache creation tokens: `251957`
- stderr log: empty

The `highCacheRequests` admin summary field is threshold-based. It counts records with cache read tokens greater than or equal to the configured high-cache threshold, not every request made while high-cache mode is enabled.

## Tested Scenarios

- `/v1/messages` synchronous same-session calls: first call created cache; later calls read cache.
- `/v1/messages` streaming same-session calls: first call created cache; later streaming calls read cache.
- Mixed synchronous and streaming calls in the same session: cache was shared across stream modes.
- Independent sessions with the same prompt: no cross-session cache read.
- Requests without `metadata.user_id`: high-cache derives a deterministic fallback conversation id from the stable prompt anchor. This can keep the local prompt cache scope stable when system/tools/first user message remain stable, but it is weaker than an explicit session id and can be affected by prompt-anchor changes.
- Explicit `cache_control`: still works and remains compatible with the previous local-prompt-cache behavior.
- Long multi-turn conversations: existing stable prefixes were read, and new suffixes could create additional cache.
- Model isolation: `sonnet` and `sonnet[1m]` did not share cache scope.
- Tool / agent-like payloads: tool definitions were included in cacheable prefixes and later read.
- `/cc/v1/messages`: synchronous and streaming requests both used high-cache simulation.
- Node `fetch` code invocation: real code-based HTTP calls hit the local proxy and read cache on subsequent calls.
- Concurrent same-session calls after seeding: all requests succeeded and read existing cache.
- Concurrent independent sessions: all requests succeeded and only created per-session cache.
- Claude Code CLI: fixed `--session-id` first call created cache; `--resume` calls read cache.
- Claude Code CLI `stream-json`: succeeded with `--verbose` and read cache.
- Claude Code CLI custom agent mode: first agent call created cache; resumed agent call read cache.
- Admin pagination and filters: `usage-records-paged` default params, page/limit, source, stream, and model filters returned `200`.
- Non-message endpoints: `/v1/models` and `/v1/messages/count_tokens` returned `200` and did not create usage records.

## Cache Scope

Prompt-cache entries are scoped by:

- `credentialId`
- stable `conversationId`
- request `model`

This scope prevents cache reads across credentials, unrelated sessions, or different model names.

## Stable Conversation Id

The proxy first tries to extract the stable conversation id from `metadata.user_id`. Supported forms include JSON values such as:

```json
{
  "session_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
}
```

If no valid UUID can be extracted in high-cache mode, the converter derives a deterministic fallback conversation id from the stable prompt anchor:

- `system`
- `tools`
- first user message, when present
- otherwise the full messages array

This fallback allows repeated compatible requests to share a local high-cache scope. It is still safer to provide an explicit session id in `metadata.user_id`, because the fallback can change when the prompt anchor changes and can be reused by unrelated requests that share the same anchor within the cache TTL.

In `local-prompt-cache` mode, the proxy should continue to rely on explicit metadata session ids and should not inherit high-cache fallback/continuity behavior.

## Cache Miss / Invalidation Scenarios

- First request for a stable prefix only creates cache.
- No prompt-cache scope/profile can be built.
- The high-cache fallback conversation anchor changes.
- Credential id changes.
- Model changes.
- Prompt prefix changes in system, tool, or message blocks.
- A small request appears before any successful larger request has created a same-scope cache entry.
- Cache entry TTL expires.
- Request fails before success handling updates the local tracker.
- Stream fails or the client disconnects before successful completion.
- Admin disables or deletes a credential; entries for that credential are cleared.
- Process restart clears the in-memory prompt cache.

## Related Admin Balance Cache

Admin balance caching is separate from prompt-cache usage simulation.

- TTL: `300` seconds.
- Persisted as `kiro_balance_cache.json` under the token manager cache directory.
- Invalidated by credential disable/enable, priority changes, failure reset, credential add/delete, and token refresh.

## Static / Client Caches

- Admin UI `index.html`: `no-cache`
- Admin UI `assets/*`: `public, max-age=31536000, immutable`
- Other static files: `public, max-age=3600`
- HTTP clients: `reqwest::Client` instances are cached by effective proxy configuration in-process and are reset by process restart.
