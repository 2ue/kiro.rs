---
name: kiro-prod-evidence-audit
description: Read-only production evidence collection and issue clustering for kiro.rs deployments. Use when the user asks to inspect a live kiro.rs server, collect production errors, diagnose Docker Compose deployments, gather usage/gateway/database/Redis/log evidence, classify recurring issues, or package a local diagnostic archive without modifying production.
---

# Kiro Prod Evidence Audit

Use this skill to investigate a live kiro.rs deployment by collecting read-only evidence from the project's real diagnostic surfaces, clustering errors into problem folders, and packaging a local archive for later reproduction and debugging.

The primary evidence source is not container stdout. For kiro.rs, start from code-defined business/diagnostic data: `usage_records`, Redis usage snapshots/rollups, runtime config, credential runtime state, audit/event tables, and tool-format debug JSONL. Container logs are supporting evidence for startup crashes, stderr warnings, gateway logs, and gaps not persisted elsewhere.

## Safety Contract

- Treat all production credentials, hostnames, IPs, tokens, cookies, database URLs, and config values as secrets.
- Never write secrets into repository files, skill files, shell history, markdown reports, or final answers.
- Do not run commands that modify production state: no `docker compose up/down/restart/pull`, no migrations, no package installs, no `rm`, no `mv`, no config edits, no service reloads, no Redis writes, no SQL writes.
- Prefer streaming command output back to local files. Do not create temp files on the production host unless the user explicitly approves and there is no safer option.
- Use bounded commands: `timeout`, `LIMIT`, indexed predicates, small file samples, and short statement timeouts. Do not pull broad logs before code-defined evidence has identified the likely source.
- Avoid expensive scans on large tables or disks. Inspect metadata first, then sample narrowly.
- Prefer SSH key or ssh-agent access. If the user explicitly authorizes password login, use only a non-persistent path such as a hidden prompt, an in-memory expect/pty handoff, or a one-shot auth helper. Never record the password in repository files, evidence files, reports, archives, or final answers. Do not record auth helper source when it contains the password.
- Redact evidence before summarizing or packaging for sharing. Keep raw local evidence separate and do not include it in the default archive.

## Required Inputs

Before any SSH command, confirm or infer:

- SSH target: user, host, port, and auth mechanism. Password auth is allowed only after explicit user authorization and must be handled without logging or persisting the password.
- Deployment directory, for example `~/docker-compose/<project>`.
- Time window, for example last 2 hours, last 24 hours, or around specific request IDs.
- Scope: request IDs, usage/business diagnostics, runtime/config state, tool-format debug files, gateway evidence, Docker state, container logs, PostgreSQL metadata, Redis metadata, or all of the above.
- Whether raw local evidence may be retained. Default: retain raw under local `tmp/` with local filesystem permissions, but package only redacted evidence.

If any of these are missing, make a conservative assumption only when it does not increase production risk. Otherwise ask one concise question.

## Output Layout

Create one local evidence root per run:

```text
tmp/prod-evidence/YYYYMMDD-HHMMSS-<host-or-project>/
├── README.md
├── manifest.json
├── commands.md
├── raw/                 # local-only raw captures; excluded from default archive
├── redacted/            # sanitized copies of raw evidence
├── summary/
│   ├── timeline.md
│   ├── inventory.md
│   └── open-questions.md
└── problems/
    └── P001-short-slug/
        ├── problem.md
        ├── fingerprints.json
        └── evidence/
            ├── app-log-001.txt
            ├── compose-redacted.yml
            └── db-schema.txt
```

Each problem folder must contain:

- `problem.md`: status, impact, first/last seen, affected route/component, normalized signature, evidence list, analysis, local reproduction hints, and next checks.
- `fingerprints.json`: normalized matching keys such as error code, route, component, upstream status, SQL error, request ID examples, container name, and log signature hash.
- `evidence/`: two or three representative redacted excerpts when the class has multiple shapes; one excerpt plus count is enough when entries are identical.

Use `scripts/package_evidence.py` after files are organized to redact, write `manifest.json`, and create the default archive.

## Workflow

1. Create the local evidence root and `commands.md`.
2. Read `references/kiro-rs-evidence-sources.md` and `references/evidence-map.md` before the first production SSH command.
3. Establish safe SSH access without placing passwords in commands or files.
4. Run Phase 1 inventory only: deployment directory, compose services, container health, app version, mounted volumes, runtime file paths, and database/Redis availability. Do not pull application logs in this phase.
5. Run Phase 2 code-defined evidence discovery: bounded PostgreSQL metadata, usage summary/error fingerprints, Redis keyspace/usage summaries, runtime config redacted shape, credential runtime state summaries, admin audit/event summaries, and tool-format debug file index.
6. Decide which evidence source is authoritative for each suspected issue. Only then run targeted Phase 3 collection from specific request IDs, time windows, tables, Redis keys, debug files, or container logs.
7. Redact and inspect evidence locally.
8. Cluster related errors into `problems/P###-*`.
9. Write `summary/timeline.md`, `summary/inventory.md`, and `summary/open-questions.md`.
10. Run `scripts/package_evidence.py --root <evidence-root>` to create a redacted `.tar.gz`.
11. Final response must include the local archive path, problem folder list, key findings, commands/sources consulted, and limitations. Do not include secrets or long raw logs.

## Evidence Priority

Follow this order unless the user gives a specific request ID or symptom:

1. **Business diagnostics in PostgreSQL**: `usage_records.data`, indexed columns, `admin_audit_logs`, `credential_events`, `credential_runtime_state`, `credential_stats`, `credential_account_info`, `external_upstream_pools`, `runtime_config`, `schema_migrations`.
2. **Redis live/rollup diagnostics**: usage summary/top/errors/recent snapshots, runtime event channels state, scheduler/cooldown/in-flight/rate-limit key families, keyspace and memory.
3. **Project disk diagnostics**: `logs/tool-format-debug/*.jsonl` and configured `toolFormatDebug.dir`; sample by file metadata and tail/head only, never wholesale.
4. **Service health and deployment state**: `/healthz`, `/readyz`, `docker compose ps`, `docker inspect`, image labels/tags, mounts, restart/OOM state.
5. **Container/gateway logs**: only targeted samples for startup crash, stderr warnings, gateway/edge errors, or when persisted business diagnostics are missing.

Do not start an investigation by pulling `docker compose logs`. First ask: which persisted data source should already contain this error?

## Clustering Rules

Group entries into the same problem when they share the same probable root cause, not merely the same severity. Useful grouping keys:

- component: app, gateway, PostgreSQL, Redis, Docker, host OS, external pool, upstream Kiro/Anthropic, usage accounting.
- route or API surface: `/cc`, `/ha`, `/v1`, `/dfcache`, admin/runtime config, health check.
- normalized error: remove timestamps, UUIDs, request IDs, account IDs, IPs, durations, token counts, and one-off hashes.
- failure mode: usage anomaly, upstream/local route failure, external pool failure, payload/tool/schema diagnostics, scheduler/capacity/rate-limit failure, runtime config mismatch, startup crash, schema mismatch, slow migration, gateway block, stream parse error, OOM, disk pressure, auth failure.

Keep separate folders when:

- the remediation would differ;
- the component differs even if the user-visible message is similar;
- one issue is only a symptom of another and needs distinct evidence.

Merge folders when:

- only request IDs/timestamps/accounts differ;
- log wording differs slightly but the same code path and root cause are clear.

## Read-Only Database Rules

- Use PostgreSQL metadata first: schema columns, indexes, table sizes, recent activity, locks, and migration/version tables.
- Do not run broad `SELECT *` or unbounded aggregation on large tables such as `usage_records`.
- For row samples, require a time predicate or specific request IDs plus `LIMIT`.
- Use a read-only transaction and short timeout for every SQL batch:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
-- bounded SELECTs only
COMMIT;
```

If a query times out, record that as evidence and move on. Do not retry with a heavier query unless the user explicitly approves.

## Evidence Packaging

Default package behavior:

- Include `README.md`, `commands.md`, `manifest.json`, `summary/`, `problems/`, and `redacted/`.
- Exclude `raw/` from the default archive.
- Include hashes and sizes for raw files in `manifest.json` so local integrity can be checked without exposing contents.
- Create a separate raw archive only when the user explicitly requests it and confirms the risk.

Run:

```bash
python3 .codex/skills/kiro-prod-evidence-audit/scripts/package_evidence.py --root <evidence-root>
```

Use `--include-raw` only after explicit user approval.

## Stop Conditions

Stop and report a blocker when:

- the only available auth path requires exposing a password in command arguments, evidence, reports, archives, or persistent files;
- a requested command would modify production;
- evidence collection would require a full table scan, full disk scan, or unbounded log dump;
- the next planned step is broad container log collection before checking `usage_records`, Redis usage snapshots, runtime tables, and tool-format debug file indexes;
- the server shows active instability where additional diagnostics could worsen load;
- the user asks to remediate instead of only diagnose, because this skill is read-only.
