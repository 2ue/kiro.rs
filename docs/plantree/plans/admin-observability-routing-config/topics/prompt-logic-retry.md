# Prompt Logic Retry

## Current Facts

- Local Kiro API retry is handled by `KiroProvider::call_api_with_retry` in `src/kiro/provider.rs`.
- The retry loop already tracks `excluded_ids`, and `acquire_context_for_session_with_mode` receives that exclusion set.
- 400 responses are classified by `classify_bad_request_reason`.
- Current classes include `assistant_prefill_bad_request`, `profile_arn_bad_request`, `tool_use_format_bad_request`, `malformed_request`, and `bad_request`.
- Only `profile_arn_bad_request` currently retries by clearing profile ARN, excluding the current credential, and trying another credential.
- Other 400 classes fail immediately because they are usually deterministic request problems.

## Target Behavior

- Add an opt-in runtime switch for selected prompt/protocol logic bad requests.
- Add a bounded max retry count for that switch.
- Only retry when model resolution succeeded before the upstream call.
- Retry must exclude the current credential and must not retry an already-tried credential.
- Default must remain off.

## Error Classes

Initial candidate classes:

- `tool_use_format_bad_request`: upstream reports invalid tool-use/request body shape. This is the closest current class to the requested "提示逻辑报错".
- `assistant_prefill_bad_request`: final assistant-prefill/last-message protocol mismatch. This may be deterministic, so include only if the switch explicitly says prompt/protocol retry, or expose class selection later.

Do not include:

- `profile_arn_bad_request`: already has specialized handling.
- `malformed_request`: usually local request construction/body syntax; retrying another account is unlikely to help.
- `bad_request`: too broad; could include unknown model and should not hide real client errors.

## Acceptance Criteria

- Default config performs exactly as today for these 400 classes.
- When enabled and max retry is set, a matching 400 pushes an attempt with a clear action, excludes the current account, and tries another eligible account.
- The retry stops when the configured prompt-logic retry cap or overall credential retry cap is reached.
- Attempt traces show each retry with duration units and reason.

## Implemented Notes

- Runtime config fields:
  - `credentialPromptLogicRetryEnabled`
  - `credentialPromptLogicRetryMaxAttempts`
- Defaults are disabled and zero. When enabled with max `0`, the provider treats that as one prompt-logic retry.
- Retry classes are currently limited to:
  - `tool_use_format_bad_request`
  - `assistant_prefill_bad_request`
- The branch requires a nonblank resolved model, excludes the current credential, unbinds the current session from that credential, and records attempt action `prompt_logic_retry_next`.
- The branch does not change `profile_arn_bad_request` handling and does not retry broad `bad_request` or `malformed_request`.

## Verification

- Added provider unit test confirming default disabled behavior, allowed classes, model requirement, and retry cap.
- `cargo test --locked --no-default-features`: pass.
