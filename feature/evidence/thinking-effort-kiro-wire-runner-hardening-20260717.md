# Thinking/Effort Kiro Wire Runner Hardening

Status: `infrastructure-pass / real-wire-not-run / release-NO-GO`

## Scope

This evidence covers only the safety and reproducibility of
`feature/tests/thinking-effort-kiro-wire.mjs` and its pure Node fixtures. No
Cargo command, kiro.rs binary, Claude binary, PostgreSQL, real Redis, or real
upstream was started. All listeners used ephemeral loopback ports and each
fixture asserted that none of its three selected ports was `9022`.
The current runner no longer queries the existing 9022 listener before or after
a run; it rejects that numeric port during allocation and proves release only
for ports allocated by this process.

It does not prove the final Kiro request body, thinking output, usage, model
mapping, or upstream support. The A09/D07 product gate and the release remain
`NO-GO` until the same runner is executed by the caller-owned database harness
against one repository-external frozen release candidate.

## Problems Found And Fixed

- The source manifest enumerated and hashed every untracked file. That crossed
  the minimum source boundary, could read unrelated sensitive data, and made
  runtime proportional to arbitrary worktree contents. It now hashes tracked
  diffs only for explicit build inputs and applies an explicit protected
  credential pathspec exclusion. It does not enumerate, stat, or hash
  untracked files.
- `Promise.race` timeout branches in child/server cleanup left successful-path
  timers alive. Command and Claude timeouts also scheduled uncancelled nested
  kill timers, while still waiting indefinitely for `close`. Child waits now
  settle at a deadline and always enter bounded process-group TERM, wait,
  KILL, wait cleanup; every safety timer is cleared and unreferenced.
- Servers stopped listening but did not own or destroy accepted sockets. Each
  tracked server now records connections, destroys them during cleanup, waits
  for both sockets and `server.close`, and fails if either remains.
- `spawnOwned` previously assumed every caller used `detached: true`. It now
  rejects any child not placed in its own process group before spawning.
- Non-signal fixtures used `process.exit`, which hid active timers and sockets.
  Business error, timeout, Redis fault, command failure, and held-socket modes
  now set only `process.exitCode` and must drain the event loop naturally.
- The external runtime path contract rejected repository paths but still
  accepted an external path directly under `target/debug` or `target/release`.
  Both lexical and canonical direct Cargo outputs are now rejected; the caller
  must pass a copied frozen candidate.
- The runner previously used `lsof` on port 9022 before and after a run to
  report that the protected listener was unchanged. Even a read-only listener
  probe was outside the current validation boundary. The runner now keeps
  separate forbidden and allocated port sets, never queries 9022, and reports
  `forbiddenPortsNeverAllocated` plus release of its own allocated ports.

## Executed Commands And Results

```text
node --check <five changed .mjs files>
PASS 5/5

node --test feature/tests/runtime-validation-paths.test.mjs \
  feature/tests/thinking-effort-kiro-wire-contract.test.mjs
PASS 11/11, current-source rerun duration 0.574s

node --test --test-name-pattern=server_socket_hang \
  feature/tests/thinking-effort-kiro-wire-signal.test.mjs
PASS 3/3 selected, duration 2.189s

node --test feature/tests/thinking-effort-kiro-wire-signal.test.mjs
PASS 42/42, current-source rerun duration 56.363s
```

The final 42-case lifecycle run contains: idle HUP/INT/TERM `9/9`, concurrent
spawn-race HUP/INT/TERM `9/9`, business error `3/3`, TERM-to-KILL timeout `3/3`,
Redis error retry `3/3`, Redis timeout retry `3/3`, command timeout `3/3`,
command spawn error `3/3`, held server sockets `3/3`, and startup failure before
readiness `3/3`. Contract behavior and fail-closed path groups execute five
rounds internally. A pre-fix baseline also passed the older `30/30` matrix;
that result was used only to show why stronger fixtures were required.

Final fixture source identities:

```text
runtime-validation-paths.mjs                         ada44bb384bffa062181ef1da95251316e53d42aa3821ac7d31d33abc01c9b59
runtime-validation-paths.test.mjs                    c2668cd5fa84069dc1082d78be411754633ab8fd1f396c81f3a1623420d1e04d
thinking-effort-kiro-wire.mjs                        939d64f88720a0403eaecd920c2e31e1be58029a367a76a8d5f44c0eaead963e
thinking-effort-kiro-wire-contract.test.mjs          ace687a4bc6cc3f9e77d5ec78484a5ecd17e29305660e578af327ed8cbed07a8
thinking-effort-kiro-wire-signal.test.mjs            107038f36c11f33f2d54f10b973f64c38c98c7fbb3a5f43c896af82d10651fab
```

## Cleanup And Resource Evidence

Every lifecycle case checks the original PID start identity and PGID before a
group signal. It then verifies the child group is empty, all three ephemeral
ports reject connections, both owned Redis prefixes and exact root keys are
gone, the foreign sentinel survived until its explicit final removal, the
owned TEMP_ROOT is absent, and the parent fixture root is deleted. Redis error
and timeout modes each prove one injected failure followed by bounded recovery.
The held-socket mode keeps one connection open on every server and proves all
three close before natural process exit.

The final independent scan reported `ownedTempEntryCount=0`; a process-name
scan found no wire runner. No build artifact, retained capture, or repository
target was produced.

## Remaining Risk

- The tracked-only source identity deliberately excludes untracked files. The
  frozen binary SHA-256 and before/after executable identity remain the runtime
  authority; callers must retain the build wrapper's source manifest as the
  build provenance record.
- The explicit source allowlist must be updated if future release builds embed
  a new directory. Omitting a path does not make the binary gate pass silently,
  but it weakens the supplemental dirty-tree attribution.
- PID start identity uses the platform `ps` start timestamp. PGID leader and
  member checks make reuse fail closed, but this is not a kernel pidfd.
- Pure fixtures cannot validate PostgreSQL ownership, real Redis behavior,
  Claude CLI compatibility, stream/non-stream thinking events, or the actual
  Kiro `output_config`/`thinking` wire fields. Those remain separate frozen
  candidate gates.
