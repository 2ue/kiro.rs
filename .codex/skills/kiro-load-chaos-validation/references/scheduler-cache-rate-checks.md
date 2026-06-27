# Scheduler Cache Rate Checks

## Scheduler

Verify:

- eligible accounts are considered before returning capacity errors.
- account concurrency limits are respected.
- global concurrency limits are respected.
- priority ordering matches configured semantics.
- balanced mode distributes across available accounts.
- cooldown removes bad accounts temporarily and recovery restores them.
- external accounts are normalized as accounts in public errors.

Evidence:

- usage records for selected account ids.
- call trace reasons for skipped accounts.
- status distribution by scenario.
- request ids for failures.

## Per-Account RPM

Use low values in a temp environment.

Pass criteria:

- requests over the per-account RPM are delayed or rejected according to configured behavior.
- other eligible accounts can still serve traffic.
- after the RPM window passes, the account can serve again.
- public error does not expose credential/fallback/internal-pool terms.

## dfcache And High Cache

Verify:

- existing built-in routes still behave unchanged.
- configured `/dfcache/*` route works.
- missing `/dfcache/*` route fails fast.
- the path prefix cannot be changed by client input.
- usage fields match route policy.
- cache creation/read fields do not stay all zero when high-cache reporting should apply.

## Payload And Usage Hot Paths

Check high CPU or latency regressions when:

- tools array is large.
- history contains large tool_result blocks.
- payload guard trims history.
- malformed tool schema needs normalization.
- usage detail recording includes error metadata.

Pass criteria:

- normal small requests do not pay large-history diagnostic cost.
- error details are retained internally but normalized publicly.
- usage logging does not block downstream streaming.

