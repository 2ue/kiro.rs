# Modernization Indexes

Role: Retrieval index for coverage, technical authority, modular implementation, traceability and legacy-document ledgers

Status: Current accepted-plan index

Authority: Navigation only; each linked ledger defines its own authority

As of: 2026-07-12

Read when: Checking whether a source, target module, finding, work unit, decision, evidence slot or legacy document is accounted for

## Ledgers

| Index | Answers |
| --- | --- |
| [Authority and source map](authority-and-source-map.md) | Which current, target, plan or historical source wins when documents disagree? |
| [Complete rewrite inventory](rewrite-inventory.md) | Does every first-party source/harness path have a target treatment? |
| [Target module ledger](target-module-ledger.md) | Which of the 50 technical authority modules receives each responsibility, state and public contract? |
| [Modular implementation work map](execution-slice-map.md) | What exact target-only coding/integration/evidence unit implements each dependency-group responsibility without creating a production phase? |
| [Finding candidate ledger](finding-candidate-ledger.md) | Which observations still require triage, decision routing or promotion? |
| [Traceability matrix](traceability-matrix.md) | Does each verified finding and each independent requirement reach a technical authority, work unit, decision/policy, gate and evidence state? |
| [Legacy document disposition](legacy-document-disposition.md) | Which old documents stay, move, archive or delete, and how are they recovered? |

## Reading Shortcuts

- Start module implementation: target module ledger, modular work map, implementation entry contract, accepted decisions, traceability rows and selected gates.
- Audit a current problem: finding candidate ledger, problem catalog, source baseline and traceability matrix.
- Delete legacy implementation: rewrite inventory, target module ledger, module evidence and post-deletion checks; production rollback remains the previous complete artifact.
- Move or delete documentation: legacy disposition, authority/source map, inbound-reference search and archive index.

These ledgers complement one another. Source coverage does not prove module ownership, module ownership does not prove failure-mode coverage, and a planned gate does not prove completion.
