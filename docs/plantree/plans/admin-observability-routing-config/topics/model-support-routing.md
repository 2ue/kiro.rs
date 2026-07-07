# Model Support Routing

## Current Facts

- Local credential dispatch eligibility is checked in `src/kiro/token_manager/capacity.rs`.
- `credential_is_usable_for_model` currently rejects disabled credentials and rejects Opus requests for credentials whose subscription title looks Free.
- `MultiTokenManager` passes the request model into acquire/selection and uses this eligibility function in local preflight, selection, summaries, and failure breakdown.
- External pool selection is in `src/external_pool.rs`. It filters enabled pools, auto-disabled pools, body-mode filter, capacity, and cooldown.
- External pools currently have model mapping configuration, but no supported-model eligibility list.
- `KiroProvider::list_available_models` can list upstream models, but it loops over enabled credentials and stops at the first non-empty result. There is no per-credential model-sync API.
- External pools have `/v1/models` test URL support, but the requested requirement is also to sync an external pool's support list by selecting a local account.

## Target Data Model

- Add `supported_models: Vec<String>` to `KiroCredentials`.
- Add `supported_models: Vec<String>` to `ExternalPool`.
- Empty list means unrestricted.
- Normalize model ids by trimming, lowercasing for comparison, removing duplicates, and preserving a readable stored value.
- Do not use supported-model lists for model rewriting. They are a dispatch eligibility boundary only.

## Matching Rule

- For parsed local credential requests, match the resolved upstream model when available; fall back to requested model.
- For parsed external pool requests, match resolved upstream model and requested model. External pool model mapping can still produce a different outbound model after the pool is selected, so eligibility must not be confused with mapping.
- For raw external routes where only `model_hint` is available, match the model hint. If no model can be extracted, unrestricted pools remain eligible and restricted pools are not eligible unless explicitly configured later.
- Keep the existing Opus/free guard as an additional local-credential condition.

## Admin Controls

- Local credential:
  - Show configured supported models.
  - Manual add/remove/edit supported models.
  - Sync supported models from this credential by calling upstream with that credential.
- External pool:
  - Show configured supported models.
  - Manual add/remove/edit supported models.
  - Sync supported models from a selected local credential.

## Acceptance Criteria

- If a credential has `supportedModels = ["claude-sonnet-4"]`, an Opus/Haiku request is not dispatched to it.
- If an external pool has the same list, that pool is skipped before HTTP forwarding for unsupported models.
- Empty lists preserve existing behavior.
- Sync failures do not change the existing model list unless the user explicitly saves successful results.
- Scheduler failure diagnostics include a clear model-not-supported reason.

## Implemented Notes

- `src/model/model_support.rs` owns normalization and matching helpers.
- `KiroCredentials.supported_models` is persisted inside credential JSON and normalized on load/import/update.
- External pool `supported_models` is stored as JSONB and normalized on create/update/read.
- Local credential eligibility checks the supported-model list before Opus/free gating.
- External pool selection filters supported-model lists before capacity/cooldown selection when a route has model candidates.
- External pool status snapshots without a route do not apply model filtering, so a restricted pool is not globally hidden.
- Admin APIs now support manual set and sync:
  - `POST /credentials/{id}/supported-models`
  - `POST /credentials/{id}/supported-models/sync`
  - `POST /external-pools/{id}/supported-models`
  - `POST /external-pools/{id}/supported-models/sync`
- All three admin UIs expose credential and external pool supported-model controls.

## Verification

- Added unit tests for normalization/matching, credential `supports_model`, and external pool supported-model filtering.
- `cargo test --locked --no-default-features`: pass.
- `pnpm --dir ui build`, `pnpm --dir admin-ui build`, and `pnpm --dir admin-ui-daisy build`: pass.
