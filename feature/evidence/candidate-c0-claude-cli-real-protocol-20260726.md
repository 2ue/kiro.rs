# Candidate C0 real Claude Code CLI protocol validation - 2026-07-26

## Scope

This evidence validates the current release candidate with the real Claude Code CLI binary, using fake local Kiro upstreams so the protocol behavior can be tested without consuming production accounts.

Covered user-reported failure classes:

- leaked internal tool transcript markers such as `user Continue`, `Tool results provided`, `<function_results>`, and `bashHash...`;
- multi-turn Claude Code CLI continuation with repeated tool_use/tool_result pairing;
- `thinking.disabled` / `output_config` compatibility;
- thinking effort propagation for `absent`, `low`, `medium`, `high`, `xhigh`, and `max`;
- CLI and IDE Kiro upstream wire-body shape;
- cleanup of temporary services, Redis prefixes, ports, and caller-owned PostgreSQL databases.

## Candidate

- Product binary SHA-256: `7268b3e722f03a40179d205e7b5917b86d696cd8bf1d5f6533d3b1347ea30bec`
- Product binary path during validation: `/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-cli-candidate.c0-20260726035013.8MWRn4/kiro-rs`
- Claude Code CLI version: `2.1.197`
- Claude executable used for the final thinking-wire gate: package binary `.../@anthropic-ai/claude-code/bin/claude.exe`, not the Volta shim.

The first full-suite attempt used the Volta shim path and failed only at the thinking-wire gate with `volta-shim failed or timed out`. Bare-invoke and long-session had already passed in that run. The thinking-wire gate was then rerun with the package binary and passed.

## Local services

- PostgreSQL: local Docker container `kiro-rs-postgres-local`, port `25432`.
- Redis: local Docker container `kiro-rs-redis-local`, port `26379`.
- Test databases were caller-owned and dropped after each run.
- Redis test prefixes were runner-owned and removed.
- Port `9022` was not touched; the tests used isolated temporary service ports.

## Bare-invoke Claude CLI gate

Report result: pass.

Key counters:

- total cases: 20
- negative literal contamination cases: 15
- structured tool cases: 5
- inference hits: 25
- tool_use count: 5
- tool_result count: 5
- fake model discovery requests: 1
- fake unknown requests: 0
- violations: 0

Cleanup:

- child process groups stopped: true
- service stopped: true
- fake upstream stopped: true
- temp directory removed: true
- ports released: true
- protected `9022` probe skipped: true
- Redis keys removed: true

Interpretation: the Claude Code CLI path did not expose internal transcript markers or malformed tool records in the bare invoke matrix.

## Long-session continuation/tool gate

Report result: pass.

Key counters:

- sessions: 5
- CLI turns: 110
- continue turns: 105
- tool turns: 100
- bash turns: 50
- read turns: 50
- inference hits: 210
- tool_use count: 100
- tool_result count: 100
- leak matches: 0
- fake model discovery requests: 1
- fake unknown requests: 0

The leak matcher covered the previously observed signatures:

- `user Continue`
- `user Tool results provided`
- `Tool results:`
- `<function_results>` / `<function_calls>`
- `<invoke name=...>`
- known hashed tool names such as `bashHash[0-9a-f]{8}`
- generic `NameHash[0-9a-f]{8}` tool signatures

Cleanup:

- child process groups stopped: true
- service stopped: true
- fake upstream stopped: true
- Redis keys removed: true
- temp directory removed: true
- ports released: true
- protected `9022` probe skipped: true

Interpretation: long, repeated Claude Code CLI continuation did not regress into the internal transcript/tool history leak pattern. The tool_use/tool_result counts remained paired.

## Thinking/output_config wire gate

Report result: pass.

Matrix:

- endpoints: `cli`, `ide`
- efforts: `absent`, `low`, `medium`, `high`, `xhigh`, `max`
- rounds per endpoint/effort: 5
- total cases: 60
- violations: 0

Important observed behavior:

- `absent` effort is normalized to upstream `output_config.effort=high`.
- explicit `low`, `medium`, `high`, `xhigh`, `max` are preserved to the upstream wire body.
- all tested CLI and IDE requests used `thinking.type=adaptive` when `output_config` is present.
- no case emitted the invalid combination that previously caused `400 output_config is only compatible with adaptive thinking or an omitted thinking field`.
- model `claude-opus-4-8` remained `claude-opus-4-8` in this fake-upstream wire test and was advertised/resolved consistently.
- protocol violations: 0
- invalid wire JSON: 0
- unknown fake upstream requests: 0

Cleanup:

- child process groups stopped: true
- fake servers stopped: true
- Redis keys removed: true
- temp directory removed: true
- ports released: true
- forbidden ports never allocated: true

Interpretation: the current normalizer does not rely on a single hard-coded `thinking.disabled` branch. For the tested official Claude Code CLI shapes, the candidate preserves or supplies the compatible `thinking.type=adaptive` contract whenever `output_config` is sent.

## Runner improvement

Added local runner:

- `feature/tests/run-claude-cli-release-suite-local.sh`

Purpose:

- use the existing local Docker PostgreSQL/Redis test containers;
- create and drop caller-owned databases for the CLI validation scripts;
- provide a Docker-backed `psql` wrapper only for local validation when host `psql` is unavailable;
- allow `KIRO_CLI_SUITE_ONLY=bare|long|thinking|all` for targeted reruns;
- avoid touching the user's active `9022` service.

## Cleanup status

- Caller-owned PostgreSQL databases: dropped.
- Runner-owned Redis prefixes: removed by the validation scripts.
- Temporary service and fake upstream processes: stopped.
- Raw local artifact directories: removed after this evidence was written.

## Result

Pass for real Claude Code CLI fake-upstream protocol validation. This proves the local protocol transformations, transcript cleanup, tool pairing, thinking/output_config wire shape, and cleanup behavior. It does not prove that a currently blocked real production credential will succeed; real-upstream availability remains dependent on account/provider state.
