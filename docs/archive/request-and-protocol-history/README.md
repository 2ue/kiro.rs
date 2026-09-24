# Request And Protocol History Archive

Role: Historical archive for request conversion, Claude/Account Runtime protocol compatibility, malformed payload, image, thinking/tool-signature, and local test-runbook analysis

Status: Archived on 2026-07-28

Authority: Preserves dated request/protocol investigations; does not define current request-body, thinking, signature, tool-use, or upstream protocol behavior

Read when: Retrieving older request/protocol rationale, reproducing historical malformed-request findings, or comparing current behavior against previous investigations

Current authority: current source, [Rust Runtime Scheduler Stabilization](../../plantree/plans/rust-runtime-scheduler-stabilization/README.md), [Request Body Capability Modularization](../../plantree/plans/request-body-capability-modularization/README.md), and active `docs/analysis/*signature*` documents

## Source Paths

| Original path | Archived file |
| --- | --- |
| `docs/anthropic-tools-signature-compatibility-analysis.md` | [anthropic-tools-signature-compatibility-analysis.md](anthropic-tools-signature-compatibility-analysis.md) |
| `docs/claude-code-account-runtime-dialogue-disconnect-investigation-20260630.md` | [claude-code-account-runtime-dialogue-disconnect-investigation-20260630.md](claude-code-account-runtime-dialogue-disconnect-investigation-20260630.md) |
| `docs/claude-code-account-runtime-dialogue-observability-and-optimization-plan-20260630.md` | [claude-code-account-runtime-dialogue-observability-and-optimization-plan-20260630.md](claude-code-account-runtime-dialogue-observability-and-optimization-plan-20260630.md) |
| `docs/account-runtime-400-improperly-formed-request-analysis.md` | [account-runtime-400-improperly-formed-request-analysis.md](account-runtime-400-improperly-formed-request-analysis.md) |
| `docs/account-runtime-cli-capture-protocol-completeness-analysis-20260702.md` | [account-runtime-cli-capture-protocol-completeness-analysis-20260702.md](account-runtime-cli-capture-protocol-completeness-analysis-20260702.md) |
| `docs/account-runtime-compatible-image-passthrough-analysis-20260705.md` | [account-runtime-compatible-image-passthrough-analysis-20260705.md](account-runtime-compatible-image-passthrough-analysis-20260705.md) |
| `docs/account-runtime-context-window-payload-threshold-full-analysis.md` | [account-runtime-context-window-payload-threshold-full-analysis.md](account-runtime-context-window-payload-threshold-full-analysis.md) |
| `docs/account-runtime-official-image-5mb-multimage-investigation-20260702.md` | [account-runtime-official-image-5mb-multimage-investigation-20260702.md](account-runtime-official-image-5mb-multimage-investigation-20260702.md) |
| `docs/account-runtime-protocol-local-before-after-test-runbook.md` | [account-runtime-protocol-local-before-after-test-runbook.md](account-runtime-protocol-local-before-after-test-runbook.md) |
| `docs/account-runtime-small-payload-improperly-formed-fix-plan.md` | [account-runtime-small-payload-improperly-formed-fix-plan.md](account-runtime-small-payload-improperly-formed-fix-plan.md) |
| `docs/account-runtime-upstream-protocol-refactor-analysis-and-test-plan.md` | [account-runtime-upstream-protocol-refactor-analysis-and-test-plan.md](account-runtime-upstream-protocol-refactor-analysis-and-test-plan.md) |
| `docs/account-runtime-upstream-real-protocol-malformed-and-context-20260617.md` | [account-runtime-upstream-real-protocol-malformed-and-context-20260617.md](account-runtime-upstream-real-protocol-malformed-and-context-20260617.md) |
| `docs/request-entry-errors-and-missing-max-tokens.md` | [request-entry-errors-and-missing-max-tokens.md](request-entry-errors-and-missing-max-tokens.md) |

## Current Interpretation

These files contain useful historical evidence and reasoning, but several conclusions are now superseded or narrowed by later protocol/signature work and source changes. Keep them as provenance, not as active implementation requirements.

## Recovery

Moved with `git mv`. Restore a file by moving it back from this archive in a separate change and re-running inbound-link checks.
