# Repository Cleanup And Filesystem Plan

Role: Generated-artifact ownership, tracked-document disposition, retention, cleanup, and disk-safety contract

Status: Accepted cleanup policy; no unlisted cleanup is authorized merely by this document

Authority: Defines how future validation and diagnostics own files; deletion still requires current inventory and manifest ownership

As of: `v0.0.102`, commit `e9479df71ee0`, updated 2026-07-12

Read when: Creating tests/reports/build output, enabling diagnostics, cleaning validation residue, moving files, or retaining evidence

Related: [Verification](verification-rollout-and-rollback.md), [Legacy document disposition](../../indexes/legacy-document-disposition.md), [Current deployment](../../../../baseline/deployment-and-operations.md), [Decision 009](../../decisions/009-single-program-modular-build-and-final-cutover.md), [Decision 010](../../decisions/010-fixed-operational-and-acceptance-policies.md), [Decision 011](../../decisions/011-explicit-secret-envelope-and-resource-governor-authorities.md), [Superseded decision 002](../../decisions/002-complete-module-by-module-rewrite.md)

## Purpose

Validation and debugging must not consume unbounded disk, create one artifact per request by default, or force broad global cleanup that risks system capability. This plan separates durable source/evidence from reproducible local output and requires manifest/provenance proof before deletion.

It does not authorize deleting existing files by category or age. The operator authorized removal of some obsolete documents on 2026-07-12; the evidence-backed scope and recovery data are recorded in the [legacy document disposition](../../indexes/legacy-document-disposition.md). Every other cleanup starts with a fresh inventory, protected-path review and explicit disposition/manifest authority.

## Current Inventory Snapshot

At the 2026-07-11 read-only audit:

- source and durable plan documents were tracked under the repository;
- project-local ignored inventory was approximately: `target` 10 GiB/68,802 files, `.local-run` 228 MiB/15,087 files, `tmp` 33 MiB/400 files, and `logs` 144 KiB/4 files;
- installed frontend dependencies occupied approximately 147 MiB under `admin-ui/node_modules` and 220 MiB under `ui/node_modules`;
- build and load reports under `target/` were ignored and reproducible;
- `target/loadtest` contained approximately 28,967 files and 197 MiB;
- one historical modular validation subtree accounted for approximately 151 MiB;
- several plan status files referenced ignored raw reports as supporting evidence;
- tool-format debug files roll by time/size but have no total directory retention limit;
- registered `target/worktrees` were explicitly identified as protected, not disposable validation output.

These counts are a dated snapshot, not permission to delete. They demonstrate why future runs require file-count/byte budgets and small versioned summaries.

## File Classes

| Class | Examples | Authority | Default action |
| --- | --- | --- | --- |
| Durable source | `src/`, frontend source, scripts, migrations, checked config examples | Git | Keep; never cleanup as generated output |
| Durable planning/spec | `docs/plantree`, accepted ADRs, baseline, roadmap | Git | Keep and review |
| Tracked legacy document | dated analysis, old proposal, implementation record, research | Git history plus classified active index | Keep/reference-only until an explicit keep/archive/delete decision; never infer from age |
| Durable sanitized evidence | Small history summary, manifest, hashes, commands, exit codes | Git | Keep according to plan history |
| Build output | `target/debug`, `target/release`, frontend `dist`, caches | Reproducible local | Delete only when owned/not needed; no proof value alone |
| Raw validation output | `target/loadtest/<run-id>`, screenshots, traces, verbose JSON | Ephemeral | Retain briefly within quota, summarize, then delete by manifest |
| Runtime diagnostics | `logs/`, tool-format JSONL | Operational local | Retention/quota worker; body capture off by default |
| Isolated infrastructure | temp PgSQL schema/database, Redis prefix, Docker builder/image/container, temp port | Runtime resource | Delete only by unique run ID/label after process stop |
| Isolated client state | temp HOME, Claude config, ccman-selected test config, fixture files | Ephemeral sensitive | Protect ordinary user state; delete only test-owned directory |
| Registered worktree | `target/worktrees` or tool-managed worktrees | User/tool project state | Protected; never infer disposable from `target` parent |
| Unknown/unregistered | Any path not in manifest or current inventory | Unknown | Do not delete; investigate provenance |

## Tracked Legacy Documentation

Tracked documentation follows a stricter rule than reproducible build output. A document may be deleted only when all are true:

1. a current baseline, registered plan, accepted decision, or retained later document clearly supersedes its active authority;
2. repository-wide inbound Markdown and literal-reference searches find no active consumer, or every consumer is updated in the same change;
3. the document is not the only source of measurements, commands, implementation evidence, protocol vectors, public-contract rationale, or operational instructions;
4. current code and accepted decisions remain sufficient to answer the questions the document previously owned;
5. the disposition record identifies the last source commit/blob and a scoped Git-history restore path;
6. link, authority, diff, and protected-untracked-file checks pass after deletion.

Historical documents that fail only the current-authority test are archived or classified as reference-only, not deleted. Active-looking operations/runbook material requires an explicit authority/replacement decision before removal. Large archive moves occur one technical-authority domain at a time and preserve retrieval links; they are not mixed with unrelated implementation changes.

## Target Repository Layout

```text
docs/plantree/.../history/          small sanitized durable evidence
target/loadtest/<run-id>/           ephemeral aggregate reports and manifest
target/validation/<run-id>/         other ephemeral test output
logs/                               bounded runtime logs/diagnostics
```

Do not add a second permanent evidence tree outside the registered plan. Large binary/raw reports must not be committed merely to make them durable; store a small sufficient summary and an optional external artifact reference/hash.

## Existing Residue Adjudication

R0 includes one bounded project-only adjudication of the existing ignored directories. It is not a blanket cleanup command.

1. Inventory immediate run subdirectories under `target/loadtest`, `.local-run`, and `tmp` with size, file count, newest/oldest timestamp, known run ID, and references from durable docs.
2. Classify each as protected/active, evidence-needing-summary, reproducible expired output, or unknown provenance.
3. Preserve `target/worktrees`, active processes, ordinary CLI state, databases/Redis namespaces without exact run identity, and every unknown item.
4. For evidence-needing runs, create a small sanitized versioned summary/manifest before any deletion.
5. Delete only a run root whose manifest/provenance and non-use are proven; no global `target`, `.local-run`, `tmp`, cache, Docker, database, or Redis prune.
6. Record reclaimed bytes/files, retained/protected paths, unresolved provenance, secret scan, and post-cleanup project capability checks.

The adjudication is complete when every immediate run directory has a recorded disposition/authority decision, not when disk usage reaches an arbitrary target.

## Validation Run Manifest

Before creating resources, write one manifest in the run directory containing:

- run ID, source commit, start time, invoking command/process;
- output root and maximum bytes/files;
- child processes and PIDs after spawn;
- ports;
- PgSQL server/database/schema;
- Redis URL/database/key prefix;
- Docker builder/container/image labels;
- isolated HOME/config paths;
- expected logs/debug directories;
- cleanup commands scoped to those exact identifiers;
- protected resources that must not be touched.

The harness updates actual resource identity and provenance as resources are created. Cleanup reads the manifest rather than rediscovering the whole machine repeatedly.

## Accepted Artifact Budgets

These accepted values are binding initial hard stops; no validation or diagnostic run may omit them:

| Scope | Accepted hard stop |
| --- | ---: |
| One raw validation run | 128 MiB and 2,000 files |
| All retained raw validation runs | 256 MiB and 5,000 files |
| Retained raw run count | 3 |
| Raw report retention | 7 days unless linked to an active investigation |
| Durable summary | Prefer under 1 MiB and fewer than 20 files/run |
| Tool-format debug directory | 256 MiB, 1,024 files, 24-hour age and 4 MiB per-record maxima under decisions 010 and 011 |

The harness must stop before exceeding a budget. Implementation may lower a hard stop when compatibility and evidence remain valid. It may not raise, disable, or remove one through a per-run override; doing so requires a superseding accepted decision with measured disk/RSS evidence and an updated bound.

## Avoiding File Explosion

- Aggregate request results in memory and flush bounded summaries rather than one JSON file per request.
- Use one JSONL/CSV/Parquet-style stream per scenario only when individual rows are necessary.
- Sample detailed failures by bounded fingerprint count.
- Store histograms/quantiles and representative sanitized IDs instead of every successful event.
- Do not duplicate the same report into multiple plan/status directories.
- Compress only after verifying compression will not temporarily exceed disk/memory budget.
- Rotate and delete by total quota, not only by per-file size.
- Keep screenshots only for UI/visual failures and required viewport gates, not every interaction.

## Runtime Diagnostic Policy

Normal structured logs may contain request/error ID, path/profile, model family, anonymized target ID, stage durations, byte counts, config version, and normalized error class.

They must not contain request bodies, prompts, tool results, file contents, images, credentials, API keys, cookies, tokens, proxy passwords, or arbitrary headers.

Tool-format/body capture:

- disabled by default;
- explicit break-glass enablement with automatic expiration;
- validated configured root, no symlink traversal;
- `0600` file permissions;
- field allowlist/redaction/hash;
- per-record, per-file, file-count, total-byte, and age limits;
- stop-and-count drops at quota;
- startup and periodic retention enforcement;
- Admin-visible state showing enabled-until, current bytes/files, oldest record, and drops.

## Keep, Move, Archive, Delete Rules

### Keep

- tracked source, tests, scripts, fixtures, migrations, plan history, and documents not explicitly approved by a reviewed disposition;
- registered worktrees and user-created files;
- active run manifest and minimum raw evidence needed for an unresolved failure;
- backups until restore verification and retention expiry.

### Move Or Summarize

- extract a sanitized durable summary from ignored raw reports before deletion when the report proves a roadmap Done claim;
- move only when the destination has a registered index and authority role;
- preserve source commit, command, exit code, artifact hash/size/count, and cleanup state.

### Delete

- tracked legacy documents that satisfy every tracked-document rule above and appear in a reviewed disposition with Git-history recovery;
- only files/resources explicitly owned by a completed/aborted run manifest;
- reproducible build outputs when not needed by a protected process/worktree;
- expired raw reports after any required durable summary exists;
- isolated databases/schemas, Redis prefixes, Docker objects, ports/processes, and CLI homes created by the run.

### Never Delete By Assumption

- a whole `target` directory when it contains registered worktrees or another active run;
- global compiler/package caches merely because a project build used them;
- ordinary Docker images/builders/volumes without unique validation labels;
- the user's normal Claude Code/ccman HOME/config;
- PgSQL databases or Redis keys without exact run ownership;
- unknown logs/temp paths found by a broad filesystem search.

## Cleanup Sequence

1. Stop new requests/work for the validation run.
2. Gracefully stop and await manifest-owned application, fake upstream, CLI, monitor, and helper processes.
3. Capture final aggregate, failure summary, resource recovery, and manifest.
4. Produce/verify any required sanitized durable evidence.
5. Scan retained evidence for credential/body-shaped values.
6. Delete manifest-owned database/schema and verify absence.
7. Delete manifest-owned Redis prefix and verify zero remaining keys.
8. Remove manifest-labeled Docker container/builder/image only when not shared.
9. Verify and release temporary ports.
10. Remove isolated CLI HOME/config and fixture copies.
11. Remove raw run files within the exact run root.
12. Verify output bytes/files, processes, ports, DB, Redis, Docker, and debug directories.
13. Record cleanup completion and any residue requiring an explicit disposition decision.

No cleanup should interleave with active load unless the harness explicitly owns rolling retention; otherwise evidence and process ownership become ambiguous.

## Safety Checks Before Deletion

- `git status --short` recorded; tracked/untracked project changes identified.
- Exact path resolves under the allowed run root after symlink canonicalization.
- Manifest run ID matches directory/resource labels.
- No protected/active PID has an open dependency on the artifact where detectable.
- Database and Redis targets match the isolated test identity, not production/default namespaces.
- Docker resource has the unique run label.
- Worktree registry does not reference the path.
- Durable summary exists for any roadmap/evidence claim depending on the raw artifact.
- Deletion command lists exact paths/IDs; no broad wildcard crosses run roots.

## Cleanup Failure And Rollback

- If manifest/provenance is uncertain, stop and leave the resource in place with a recorded question.
- If a summary is missing, retain the raw run within quota until the summary is produced.
- If a delete command partially succeeds, inventory only that run's resources and record exact residue.
- If source/tracked content is unexpectedly affected, stop immediately; do not use destructive Git reset/checkout. Restore from known source/backup only after manifest/provenance review.
- Cleanup success is not inferred from command exit alone; verify the resource is gone and protected resources remain.

## CI And Harness Enforcement

Future validation tooling should fail early when:

- no run ID or manifest exists;
- output is outside an allowed root;
- byte/file/concurrency/duration limits are absent;
- reports would create one file per request without explicit bounded reason;
- an active protected port/process/worktree conflicts;
- cleanup provenance cannot be determined;
- retained evidence contains raw body/credential-shaped material;
- raw artifact totals exceed retention policy.

The harness should expose `plan`, `run`, `summarize`, and `cleanup --run-id` steps so cleanup never depends on remembering an interactive session.
