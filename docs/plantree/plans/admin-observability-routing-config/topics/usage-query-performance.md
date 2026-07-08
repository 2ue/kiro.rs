# Usage Query Performance

## Original Facts

- Admin query DTOs are `UsageRecordsQueryParams` and `UsageRecordsPageQueryParams` in `src/admin/handlers.rs`.
- Current frontend query surfaces send `q`, `model`, `endpoint`, `conversationId`, route target, status, source, stream, and cache-read filters.
- Before this change there was no dedicated request id field. A request id was searched through generic `q`.
- Even with no filters, the recorder tried Redis usage-record pagination before PgSQL. Redis pagination scans the recent-record sorted set, GETs record JSON in batches, deserializes records, and filters in memory.
- That Redis-first path affects plain page refresh, previous page, and next page, not only search.
- PgSQL paged query calls `UsagePostgresStore::load_matching`, which does `SELECT data FROM usage_records`, applies filters, then orders by `created_at DESC, id DESC`.
- Generic PgSQL `q` used `ILIKE '%q%'` across many fields and included `data::text`.
- The `usage_records` table had indexes for created time, credential, model, status, conversation, and soft-delete cleanup. It did not have expression indexes for upstream/external model JSON fields.
- The table primary key is `id`, but generic `q` used `id ILIKE '%...%'`, so a request-id lookup could not use the primary key.
- Redis paged query scans the sorted recent-record index, deserializes records, checks filters in memory, and returns `None` after a scan limit so PgSQL can be used.
- Redis model filtering checked only `record.model`; memory and PgSQL also checked upstream/external outbound model fields.
- The usage page also loaded trend-series data and account/external-pool filter options on initial render. Those are useful, but not required for the first page of records.

## Implemented Behavior

- Admin usage query DTOs now include `requestId`.
- `/ui` usage page exposes a dedicated request-id field and also auto-detects `req_...` in the generic search input, then sends `requestId` instead of generic `q`.
- `/ui` usage page uses labeled fast filters for request id, model, endpoint, and conversation. Broad fuzzy search is kept under advanced filters instead of being the primary input.
- `/ui` usage page no longer loads account/external-pool filter option lists until advanced filters are opened or active.
- `/ui` trend charts are collapsed by default, so the series endpoint is not requested on initial page load.
- Usage record pagination uses PgSQL first when PgSQL is configured. Redis record pagination remains as a fallback when PgSQL is unavailable or fails.
- Memory usage recorder applies exact request id matching before generic search.
- Redis usage query does direct record GET for `requestId`.
- PgSQL usage query uses `id = $1` for `requestId`.
- Redis model filtering now checks `record.model`, `record.upstreamModel`, and `record.externalOutboundModel`.
- PgSQL generic `q` no longer scans `data::text` by default. It still searches core columns and selected top-level JSON fields such as upstream model, external outbound model, external pool id/name, route kind/subtype, and model resolution source.
- PgSQL migration adds expression indexes for `data->>'upstreamModel'`, `data->>'externalOutboundModel'`, and `data->>'externalPoolId'`.

## Problems

- Pagination limits returned rows, not scanned rows. With selective filters, Redis can scan many cached records before finding enough matches or before falling back to PgSQL.
- Redis-first pagination made no-filter refresh and previous/next page slower than necessary when PgSQL exists.
- Initial UI render made auxiliary requests that were not required for the records table.
- Generic request-id search is unnecessarily expensive because it becomes a broad text search instead of `id = request_id`.
- `data::text ILIKE` makes broad search expensive as usage payloads grow.
- Model search can appear to do nothing when the model exists only as upstream or external outbound model in Redis-cached records.
- Exact model filtering is not obvious in UI, so users may type partial model names and see no result.

## Target Behavior

- Add an exact request id query field. UI can either expose a request-id field or auto-detect `req_...` in the search input and send the exact field.
- Exact request id filtering must use `id = $1` in PgSQL and a direct Redis record GET when possible.
- No-filter records refresh and previous/next pagination should not scan Redis when PgSQL is available.
- First render should avoid loading optional filter option lists and trend series.
- Align model filtering across memory, Redis, and PgSQL to check reported model, upstream model, and external outbound model.
- Keep generic search for human inspection, but avoid JSON-wide text scan by default. If deep JSON search is kept, make it explicit.
- Add indexes or stored filter columns where the PgSQL filter uses fields that are frequently queried.

## Acceptance Criteria

- Searching a known request id returns the record quickly and does not depend on scanning `data::text`.
- Opening the usage page with no filters issues the records query through PgSQL, not Redis scan pagination, when PgSQL is configured.
- Previous/next page uses the same PgSQL-first path.
- The initial UI render does not fetch the account/external-pool filter lists or dashboard series until the operator asks for those views.
- Searching by upstream/external outbound model works the same with Redis cache, PgSQL, and memory fallback.
- Existing filters still work.
- UI makes request-id lookup and model lookup semantics clear.

## Verification

- Added memory usage recorder tests for exact request id, upstream model, and external outbound model filtering.
- `cargo test usage_records_query -- --nocapture`: pass.
- `cargo test usage_records_page_query_accepts_http_request_id_names -- --nocapture`: pass.
- `cargo test recorder_query_supports_exact_request_id_and_model_aliases -- --nocapture`: pass.
- `pnpm --dir ui build`: pass.
- `pnpm --dir admin-ui build`: pass.
