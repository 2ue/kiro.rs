# Evidence Skill Local Validation

Date: 2026-07-16

Scope: repository-owned dependency-free skill validation and synthetic-secret package test. No production host was contacted.

## Validator

```text
python3 .codex/skills/kiro-prod-evidence-audit/scripts/quick_validate.py \
  .codex/skills/kiro-prod-evidence-audit
Skill is valid (dependency-free repository validator).
```

The system skill-creator validator was also invoked and failed before reading the skill because its runtime lacks the undeclared `yaml` module (`ModuleNotFoundError`). This is retained as an environment limitation, not reported as a skill validation failure.

## Synthetic Fixture

The isolated fixture contained fake examples of API key, Bearer token, JWT, email, AWS access key, PostgreSQL DSN, Redis DSN, generic HTTPS URL credentials, refresh token, request body content, and encoded bytes. Packaging used:

```text
SOURCE_DATE_EPOCH=1784131200 python3 \
  .codex/skills/kiro-prod-evidence-audit/scripts/package_evidence.py \
  --root tmp/evidence-skill-validation-fixture
```

An initial archive scan failed because quoted JSON Authorization keys bypassed the text regex. JSON/JSONL key-aware scrubbing was added, then all three rounds were rerun from the same fixture.

## Final Result

All three final rounds produced the same hashes:

```text
manifest ba2d451d441d52b392cd917ddc06f2f7ff59e6f79913244852eb49e6897167a6
archive  971d9ce93d7aaa242a2d93f6d9eb2e32477ac9b7bfd834dd573a55f573af7b23
```

The archive contained six members: `README.md`, `commands.md`, `manifest.json`, two redacted files, and the synthetic inventory. It contained no `raw/` member. A byte scan found none of the fixture secrets and found every expected redaction marker.

This evidence proves the local synthetic contract only. It does not authorize or substitute for a production audit.
