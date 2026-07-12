# Finding Candidate Ledger

Role: Active and recently reconciled observations awaiting confirmation, retraction, promotion, or decision routing

Status: Current accepted-plan candidate ledger as of 2026-07-12

Authority: Owns candidate identity, evidence rule, affected technical authority and routing only; severity belongs to the problem catalog and implementation state belongs to the roadmap

Read when: An audit discovers a possible problem, a gate fails, a candidate is promoted, or a target module prepares to integrate

Related: [Continuous audit](../topics/problems/continuous-audit-and-finding-lifecycle.md), [Problem catalog](../topics/problems/README.md), [Traceability](traceability-matrix.md), [Modular work map](execution-slice-map.md), [Roadmap](../roadmap.md)

No candidate uses a human owner, due date or project schedule. `MOD-*` references identify the code/state/contract authority that must account for the observation.

## State Vocabulary

| State | Meaning |
| --- | --- |
| `Open` | Evidence or scope is incomplete; the confirmation/retraction action is explicit |
| `Confirming` | A bounded reproduction, source review or measurement is underway |
| `Promoted` | Verified and assigned a permanent problem, question or decision identifier |
| `Retracted` | Evidence disproved the candidate; rationale is retained |
| `Bounded` | The broad claim was false, but a narrower candidate or finding remains |
| `Decision Resolved` | The observation concerned target design and an accepted decision now binds it |

Promoted/retracted/bounded/decision-resolved rows remain through this checkpoint, then may move to dated history without losing source evidence.

## Current Ledger

| Candidate | Opened | State | Observation and evidence | Technical authority / required point | Confirmation or retraction rule | Routed outcome |
| --- | --- | --- | --- | --- | --- | --- |
| `CAND-20260712-001` | 2026-07-12 | Promoted | `src/bin/kiro_loadtest.rs` can monitor its own PID, collapse missing metrics to zero, misstate error latency, omit task failures and skip idle recovery | `MOD-LOAD-CHAOS-HARNESS`; before any `G-PERF` result | Retract only if source/self-tests prove target identity, valid missing-data semantics, complete accounting and recovery | `TEST-004`; implement final harness in `R0.5` |
| `CAND-20260712-002` | 2026-07-12 | Promoted | `src/anthropic/websearch.rs` logs query and MCP bodies at ordinary levels | `MOD-DIAGNOSTICS`, `MOD-OBSERVABILITY`; before target integration | Retract only if supported defaults prove sensitive content cannot emit | `SEC-003`; final fixtures `R0.1`, implementation `R1.7` |
| `CAND-20260712-003` | 2026-07-12 | Decision Resolved | Horizontal layers, full snapshot, broad terminal commands and legacy facades could reproduce God Objects | all 50 modules; architecture gate | Close through binding D007/D009 rules and static enforcement | ADR 007 Accepted; target module ledger and `R0.4` |
| `CAND-20260712-004` | 2026-07-12 | Decision Resolved | Earlier R0-R10 stages were treated as independently switched production units and temporary containment caused double implementation | complete delivery model | Close when one target-only work map and final-system cutover replace the old selectors | ADR 009 Accepted; modular work map rewritten |
| `CAND-20260712-005` | 2026-07-12 | Promoted | Kiro/external non-stream/error paths can collect complete unbounded response bodies | `MOD-KIRO-UPSTREAM`, `MOD-EXTERNAL-UPSTREAM`; before R5 integration | Retract only if every reader enforces incremental accepted limits | `RES-003`; fixtures `R0.7/R0.8`, final `R5.1/R5.2` |
| `CAND-20260712-006` | 2026-07-12 | Promoted | External-pool URLs lack one connection-bound DNS/IP/redirect/credential policy | `MOD-EXTERNAL-UPSTREAM`; before R5.2 integration | Retract only with enforced private/metadata/rebinding/redirect/proxy/cross-origin proof | `SEC-004`; fixtures `R0.8`, final `R5.2` |
| `CAND-20260712-007` | 2026-07-12 | Promoted | Admin reads expose reusable secrets and both UIs persist Admin keys in `localStorage` | auth/config/credential/proxy/Admin/generated-contract/both UI authorities; before R8 integration | Retract only when masked/reveal-once/keep-replace-clear and no durable browser secret are proven | `SEC-005`; exact R2/R4/R8 work-map rows |
| `CAND-20260712-008` | 2026-07-12 | Promoted | PgSQL startup combines mutable inline migration, delimiter execution and unbounded repairs/backfills | domain authorities, `MOD-MIGRATIONS`, `MOD-BOOTSTRAP`; before R2 state integration | Retract only when immutable identity, fencing, adoption, resume, prior binary and bounded jobs are executable | `OPS-005`; `R2.0` plus exact domain manifests |
| `CAND-20260712-009` | 2026-07-12 | Promoted | Redis acquire/queue Lua can enumerate/remove every expired member on the request path | both scheduler authorities; before R4 integration | Retract only if each call is constant/batch-bounded and backlog converges | `PERF-009`; fixtures `R0.9/R0.10`, final R4 schedulers |
| `CAND-20260712-010` | 2026-07-12 | Promoted | `KiroProvider::client_for` uses an unbounded proxy-keyed client map retaining rotated proxy identities/secrets | `MOD-KIRO-UPSTREAM`; before R5.1 integration | Retract only with cardinality/age/invalidation/secret-release proof under churn | `RES-004`; fixtures `R0.7`, final R5.1 |
| `CAND-20260712-011` | 2026-07-12 | Promoted | Local/external global concurrency and queue default to `0 = unlimited` | both scheduler authorities; before R4 integration | Retract only if supported profiles reject/replace zero with finite combined bounds | `RES-005`; decision 010 and final R4 schedulers |
| `CAND-20260712-012` | 2026-07-12 | Promoted | Missing/empty tool descriptions reach Kiro as empty and explicit-null `input_schema` fails entry map deserialization | `MOD-PROTO-ANTHROPIC`, `MOD-PAYLOAD`, `MOD-TRANSPORT-PUBLIC`; before R6 integration | Retract only if all route/profile fixtures disprove both source mechanisms | `COR-006`; decision 012, R0.6/R6.3/R7.1 |
| `CAND-20260712-013` | 2026-07-12 | Promoted | Invalid tool property names reach rejecting upstreams, while the proposed blanket rename cannot preserve schema/response semantics | protocol/payload/response authorities; before R6/R7 integration | Retract only if target capability accepts the names or a complete request/response round trip is unnecessary | `COR-007`; decision 012, R6.3/R7.1 |
| `CAND-20260712-014` | 2026-07-12 | Promoted | Credentials JSONB, proxy passwords and external-pool API keys store reusable plaintext and no application crypto authority exists | `MOD-SECRET-ENVELOPE` plus secret-owning authorities; before R2 state integration | Retract only with source proof of application ciphertext and recoverable external key lifecycle | `SEC-006`; decision 011, R1.8/R2.4/R2.8 |
| `CAND-20260712-015` | 2026-07-12 | Decision Resolved | The prior target draft required one combined process budget and one secret envelope but assigned neither a unique technical authority | target architecture and every resource/secret consumer | Close through explicit non-overlapping module contracts and static enforcement | ADR 011; accepted target has 50 modules with R1.8/R1.9 |
| `CAND-20260712-016` | 2026-07-12 | Decision Resolved | Body-byte/depth limits alone do not bound tools, messages, content blocks, schema nodes/properties/references/descriptions or pre-body slow connection work | `MOD-RESOURCE-GOVERNOR`, public/Admin transport, `MOD-PAYLOAD`; before R1.9/R6 integration | Reopen as a permanent finding if characterized accepted limits still permit superlinear or unrecovered CPU/allocation growth | ADR 011 exact server/traversal ceilings; `FUN-047`, R1.9/R6.3 |

## New Candidate Template

Record:

- stable `CAND-YYYYMMDD-NNN` and opened date/source revision;
- short observation and exact source/runtime/evidence location;
- affected audit axes and suspected impact;
- affected `MOD-*` technical authority or unresolved authority boundary;
- evidence that confirms, narrows or retracts the claim;
- overlap with existing problem/decision IDs;
- next bounded safe verification action and the integration/release point it gates.

Do not allocate a permanent problem ID before verification. Do not delete a candidate because the work graph changed; reroute it and retain the reason.
