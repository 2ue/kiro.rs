# Protocol Contamination Source Contract Evidence

Status: `source-contract-pass / dynamic-native-gates-still-open`

Date: 2026-07-21

Source authority: current dirty tree on HEAD `401473c` (`v0.0.109`) plus local remediation changes. This evidence is source-contract evidence only: it does not start `kiro.rs`, does not invoke Claude Code CLI, does not run Cargo, and does not replace the native upstream/fault/load gates.

## Purpose

This pass locks the protocol-contamination safeguards against regressions that would not necessarily have the same `bashHash...` / `readHash...` visible fingerprint. The contract deliberately covers hash-shaped and non-hash-shaped leakage classes:

- current `user Continue` transcript scaffolding;
- legacy `user Tool results provided.` plus `Tool results:` scaffolding;
- roleless `Tool results:` scaffolding;
- original tool names such as `Bash`;
- deterministic mapped tool names such as `bashHashd1e9567d`;
- historical overlong mapped tool names;
- signed/redacted thinking contamination;
- marker-free raw request bodies that must remain byte-identical;
- stream/non-stream response paths that must not turn suppression into blank or partial success;
- external-pool normalized/SSE fail-closed paths.

## Executed Commands

```bash
node --test feature/tests/protocol-contamination-source-contract.test.mjs
```

Result:

```text
10 tests / 10 pass / 0 skip / 0 fail
```

Follow-up combined contract run:

```bash
node --test \
  feature/tests/protocol-contamination-source-contract.test.mjs \
  feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs
```

Result:

```text
56 tests / 47 pass / 9 explicit live-signal skips / 0 fail
```

The 9 skips are inherited from the Redis fault-domain live signal cases and require explicit live fixture inputs. They are not protocol-contamination skips and are not counted as product passes.

## Locked Source Contracts

The new contract file is:

- [`../tests/protocol-contamination-source-contract.test.mjs`](../tests/protocol-contamination-source-contract.test.mjs)

It verifies the following source-level invariants:

1. `ToolTranscriptSanitizer` does not trust arbitrary `Hashxxxxxxxx` text. It expands only request-known tool names into:
   - original lowercased tool names;
   - current deterministic mapped names;
   - legacy overlong mapped names.

2. Tool transcript confirmation requires the full scaffold plus a known internal tool name. Normal prose such as `artifactHashdeadbeef`, fenced examples, quoted examples, indented examples, and isolated markers remain visible according to existing Rust fixtures.

3. Raw request history inspection does not unconditionally parse/serialize clean bodies. The marker-free path returns `Ok(None)` before JSON DOM parsing, and rewritten bytes are produced only after a confirmed assistant-history mutation.

4. Raw marker scanning includes literal and escaped JSON marker forms, so escaped `user Continue` / `Tool results:` cannot bypass the prefilter while large clean escaped bodies still skip DOM rewriting.

5. Request-history sanitization mutates only assistant text/thinking. User text, tool result data, tool inputs, and unmodeled fields remain data rather than cleanup targets.

6. Signed and redacted thinking are atomic. A polluted signed/redacted block is removed/fails closed; sanitized text is not recombined with stale signature or redacted integrity metadata.

7. `converter/history.rs` builds sanitizer scope from all relevant tool-name authorities: request `tools`, current `tool_name_map` keys/values, and historical `tool_use` names. It also applies a second pass after assistant text flattening/joining, except under strict Anthropic compatibility.

8. `handlers/request_entry.rs` fails strict-profile request contamination before upstream and prevents raw external-route bypass once assistant history has been sanitized.

9. Stream response contamination is terminally fail-closed: pre-commit contamination uses the existing stream retry path; post-commit or exhausted-budget contamination emits stream error semantics and does not emit `message_delta` / `message_stop` success terminal events.

10. Non-stream response contamination records usage as `Error`, marks suppressed leak fields, and returns a sanitized 502 processing error instead of a blank/partial 200.

11. External-pool normalized/SSE contamination maps to `protocol_contamination`, sets fatal SSE processing state, and emits a safe `event: error` without success terminal events.

## What This Proves

This proves that the current implementation has source-level guardrails against the class of leak the user reported, including but not limited to the current `*Hash<8hex>` visible fingerprint.

It also proves the cleanup path is not a blanket body-normalization pass: clean raw bodies are protected from parse/serialize churn, and signed/redacted thinking is not partially rewritten.

## What This Does Not Prove

This does not close the release-blocking dynamic gates:

- real native Kiro upstream with current upstream protocol;
- active/passive thinking long sessions;
- MCP/search/image/agent tool histories;
- 429/500/partial/malformed recovery with real Claude Code CLI;
- long-session native upstream, not just frozen fake-upstream;
- final C0-C4/L3-L5, UI, upgrade and inventory gates.

Release remains `NO-GO`.
