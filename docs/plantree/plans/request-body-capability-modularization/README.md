# Request Body Capability Modularization

Status: Implemented Base; Reasoning Fidelity Plan Ready

## Scope

Refactor request/body processing so local credentials, external normalized pools, and external raw pools mount explicit processing capabilities instead of relying on scattered handler branches. The plan also owns scoped protocol-correctness maintenance for top-level Anthropic reasoning intent and local Kiro `additionalModelRequestFields` in the current Rust runtime.

## Non-Negotiable Requirements

- Caller-visible API behavior must remain compatible for existing successful requests.
- Raw external passthrough must not enter body processing unless an enabled stage explicitly requires it.
- Model processing, body processing, usage projection, pricing, logging, retry, and error normalization must be separable.
- Defaults must preserve existing behavior before UI/config semantics are changed.
- Long context, image, tool/schema, and payload guard paths must be tested under fake upstream load and chaos.
- Explicit reasoning intent and schema-supported effort values must not be silently deleted, clamped, upgraded, or downgraded across conversion, credential selection, endpoint transformation, retry, or response handling.

## Relationship And Authority

This plan remains authoritative for the request/body capability boundaries, compatibility defaults, raw-versus-normalized behavior, reasoning-field fidelity, and validation evidence landed in its scope. The [Greenfield AI Gateway plan](../greenfield-ai-gateway/README.md) owns later target architecture, request-pipeline orchestration and state ownership; the superseded Rust modernization plan is historical reference only.

The reasoning-fidelity topic is a maintenance plan for the current Rust runtime, not a reopening of the Greenfield target architecture. Future route-planner or module work must preserve this plan's non-negotiable behavior unless an accepted greenfield decision explicitly supersedes a contract and defines compatibility, cutover and regression coverage.

## Reading Path

1. [Body processing inventory](topics/body-processing-inventory.md)
2. [Kiro reasoning field fidelity and forced thinking](topics/kiro-reasoning-field-fidelity-and-forced-thinking.md)
3. [Target module boundaries](topics/target-module-boundaries.md)
4. [Load and chaos validation plan](topics/load-chaos-validation-plan.md)
5. [Roadmap](roadmap.md)
6. [Historical implementation snapshot](history/implementation-snapshot-2026-07-06.md)
