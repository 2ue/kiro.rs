# Request Body Capability Modularization

Status: In Progress

## Scope

Refactor request/body processing so local credentials, external normalized pools, and external raw pools mount explicit processing capabilities instead of relying on scattered handler branches.

## Non-Negotiable Requirements

- Caller-visible API behavior must remain compatible for existing successful requests.
- Raw external passthrough must not enter body processing unless an enabled stage explicitly requires it.
- Model processing, body processing, usage projection, pricing, logging, retry, and error normalization must be separable.
- Defaults must preserve existing behavior before UI/config semantics are changed.
- Long context, image, tool/schema, and payload guard paths must be tested under fake upstream load and chaos.

## Reading Path

1. [Body processing inventory](topics/body-processing-inventory.md)
2. [Target module boundaries](topics/target-module-boundaries.md)
3. [Load and chaos validation plan](topics/load-chaos-validation-plan.md)
4. [Roadmap](roadmap.md)
5. [Implementation status](implementation-status.md)
