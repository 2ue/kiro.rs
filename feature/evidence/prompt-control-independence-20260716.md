# Prompt control master focused evidence - 2026-07-16

Status: focused backend/converter/source-contract evidence; browser and real CLI pending.

## 2026-07-17 current-source revalidation

The zero-build source contracts were rerun against the current shared worktree:

- `prompt-control-independence.mjs`: both UI surfaces pass.
- `prompt-default-parity.mjs`: Rust/UI/Admin defaults remain byte-identical and contain zero internal transcript fingerprints.
- `check-frontend-contracts.mjs`: 169 shared API types match.
- `cost-format-contract.mjs`, `mcp-attempt-channel-contract.mjs`, and `request-api-key-id-contract.mjs`: all pass for both UI surfaces.

The mandatory in-app Browser bootstrap was retried after these checks. The browser runtime rejected setup before any page could open because the product request metadata still lacks `sandboxPolicy`; even the required troubleshooting-documentation call is rejected at the same boundary. No standalone browser was substituted and no browser result is claimed. This remains an external F05 gate while backend, CLI, and load work continues.

## Focused results

- `cargo test prompt_steering -- --nocapture`: 6/6 pass. Covers endpoint scope, strict profile, duplicate injection, and messages/count_tokens operator-prompt parity; the orthogonal matrix uses five rounds per cell.
- `cargo test operator_prompt_master_disables_all_proxy_prompt_additions`: 1/1 pass; master OFF suppresses chunked, thinking and tool-choice compatibility prompt helpers.
- `cargo test operator_prompt_master_off_preserves_structured_tool_filtering`: 1/1 pass; `none=0`, `any=N`, `named=1` remain structured when master is off.
- `cargo test disabled_prompt_master_suppresses_automatic_thinking_additions`: 1/1 pass; model suffix/text signal no longer synthesizes thinking while master is off.
- `cargo test disabled_tool_choice_prompt_subtoggle_keeps_structured_named_filtering`: 1/1 pass.
- `cargo test prompt_master_protocol_capabilities_and_prompt_subtoggles_round_trip_independently`: 1/1 pass; internally runs every 7-bit combination (128) for five rounds, 640 complete Config JSON round trips.
- `node feature/tests/prompt-control-independence.mjs`: 2/2 UI surfaces pass. Prompt setters contain no `bodyConversion` mutation and save paths normalize both objects independently.
- `node scripts/check-frontend-contracts.mjs`: 167 shared frontend API types match.
- `node feature/tests/prompt-default-parity.mjs`: first failed because both UI defaults differed from Rust and each contained six internal transcript markers; after replacing only the legacy line, Rust/UI/Admin defaults match byte-for-byte and all marker checks pass.
- Both `npm run build` production builds pass after the default-prompt fix. The new UI retains an existing 545.35 kB chunk warning; Admin UI's main entry is 427.53 kB.
- Runtime migration v6 exact-match coverage passes: a V5 config containing the old UI's exact V3 built-in is replaced; V3 plus suffix, leading whitespace, and arbitrary custom prompts remain byte-identical.

## Contract proven

`promptSteering.enabled` is the total gate for proxy-added language/task/custom, tool-choice, thinking and Write/Edit chunked prompts, plus automatic thinking triggers. Structured tool filtering remains owned by `bodyConversion.toolChoiceSteering`; client-provided `tool_choice`, `thinking` and `output_config` are preserved and mapped by protocol capability. Disabling the master therefore removes only proxy additions and does not change `none/any/named` semantics.

## Remaining gates

The in-app Browser gate is currently blocked before page open by `sandbox-state metadata missing sandboxPolicy`. Do not replace it with an unrelated browser and claim pass. After the product gate is available, test both UIs saving deliberately contradictory prompt/body values and cross-reloading them. Also run isolated PostgreSQL Admin API round trips, external raw/normalized routes, and real Claude Code CLI thinking/tool/count_tokens comparisons on the frozen candidate.
