# Roadmap

## Done

- Confirmed current usage records UI sends only generic `q` plus exact `model`, `endpoint`, `conversationId`, route, status, source, stream, and cache-read filters.
- Confirmed PgSQL generic `q` search includes `data::text ILIKE '%q%'`, which can scan large JSON payloads even when the user searches a request id.
- Confirmed Redis paged query scans cached records in batches and falls back to PgSQL after a scan limit, so selective filters can be slow despite pagination.
- Confirmed Redis model filtering only compares `record.model`, while memory and PgSQL also check upstream/external outbound model fields.
- Confirmed local credential model eligibility is currently only the Opus/free heuristic in `credential_is_usable_for_model`.
- Confirmed external pool selection currently filters enabled/auto-disabled/body-mode/capacity/cooldown, but not supported models.
- Confirmed `KiroProvider::list_available_models` can list upstream models, but it chooses any enabled credential; a per-credential sync entrypoint is needed.
- Confirmed API 400 handling only retries `profile_arn_bad_request`; prompt/tool/body logic bad requests fail immediately.
- Confirmed request/body backend is partially modularized, but config UI still mixes switches and subordinate settings across large sections.
- Implemented exact `requestId` query handling in admin DTOs, usage recorder, Redis cache, and PgSQL.
- Removed default PgSQL generic search over `data::text`; explicit lightweight JSON fields remain searchable.
- Added PgSQL expression indexes for `upstreamModel`, `externalOutboundModel`, and `externalPoolId`.
- Aligned model filtering across memory, Redis, and PgSQL for reported model, upstream model, and external outbound model.
- Added normalized supported-model lists to local credentials and external pools. Empty list means unrestricted.
- Wired supported-model lists into local credential eligibility and external pool selection before dispatch.
- Added admin APIs to manually set supported models and sync from a chosen local credential.
- Updated `ui` with local credential and external pool supported-model controls.
- Added opt-in runtime config for selected prompt/protocol 400 retry classes with an untried credential and bounded max attempts.
- Improved external pool form grouping so dispatch eligibility, body processing, model processing, usage projection, and error handling are visually separate.
- Completed local regression: Rust tests, frontend builds, and fake upstream smoke/chaos.

## In Progress

- None for this scope.

## Next

- Optional low-volume real upstream smoke for model sync and supported-model dispatch, only when explicitly requested.
- Optional UI follow-up: add a dedicated request-id field or explanatory placeholder in usage search.

## Deferred

- Full plugin ABI for body/model/usage/retry processing.
- Heavy full-text search across arbitrary usage `data` JSON by default. If needed, implement as an explicit deep-search mode.
- Replacing existing legacy admin UIs with only the new React surface.
