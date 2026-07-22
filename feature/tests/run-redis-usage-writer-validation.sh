#!/usr/bin/env bash

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
[[ "$PWD" == "$repo_root" ]] || {
  printf 'run this validator from the repository root\n' >&2
  exit 64
}

redis_url="${KIRO_RS_TEST_REDIS_URL:-}"
isolated="${KIRO_RS_TEST_REDIS_ISOLATED:-0}"
allow_non_loopback="${KIRO_RS_ALLOW_NON_LOOPBACK_STORAGE_TESTS:-0}"
outer_rounds="${KIRO_REDIS_USAGE_OUTER_ROUNDS:-3}"
scope="${KIRO_REDIS_USAGE_SCOPE:-redis-usage-writer-real}"

[[ -n "$redis_url" ]] || {
  printf 'KIRO_RS_TEST_REDIS_URL is required; no Redis test was run\n' >&2
  exit 64
}
[[ "$isolated" == "1" ]] || {
  printf 'KIRO_RS_TEST_REDIS_ISOLATED=1 is required; refusing an unconfirmed Redis target\n' >&2
  exit 64
}
[[ "$outer_rounds" =~ ^[1-9][0-9]*$ ]] && (( outer_rounds <= 10 )) || {
  printf 'KIRO_REDIS_USAGE_OUTER_ROUNDS must be between 1 and 10\n' >&2
  exit 64
}
[[ "$scope" =~ ^[a-z0-9][a-z0-9._-]{0,63}$ ]] || {
  printf 'KIRO_REDIS_USAGE_SCOPE has an invalid format\n' >&2
  exit 64
}

KIRO_RS_ALLOW_NON_LOOPBACK_STORAGE_TESTS="$allow_non_loopback" \
KIRO_RS_TEST_REDIS_URL="$redis_url" \
node <<'NODE'
const raw = process.env.KIRO_RS_TEST_REDIS_URL;
let parsed;
try {
  parsed = new URL(raw);
} catch {
  process.stderr.write('KIRO_RS_TEST_REDIS_URL is not a valid URL\n');
  process.exit(64);
}
if (!['redis:', 'rediss:'].includes(parsed.protocol)) {
  process.stderr.write('KIRO_RS_TEST_REDIS_URL must use redis:// or rediss://\n');
  process.exit(64);
}
const hostname = parsed.hostname.toLowerCase();
const loopback = hostname === '127.0.0.1'
  || hostname === '::1'
  || hostname === '[::1]'
  || hostname === 'localhost';
if (!loopback && process.env.KIRO_RS_ALLOW_NON_LOOPBACK_STORAGE_TESTS !== '1') {
  process.stderr.write(
    'non-loopback Redis requires KIRO_RS_ALLOW_NON_LOOPBACK_STORAGE_TESTS=1\n',
  );
  process.exit(64);
}
if (parsed.port === '9022') {
  process.stderr.write('port 9022 is protected and cannot be used by this validator\n');
  process.exit(64);
}
NODE

KIRO_RS_TEST_REDIS_URL="$redis_url" \
KIRO_RS_REQUIRE_STORAGE_TESTS=1 \
KIRO_REDIS_USAGE_OUTER_ROUNDS="$outer_rounds" \
feature/tests/run-cargo-scoped.sh "$scope" -- \
  env RUSTUP_TOOLCHAIN=1.92.0 \
  KIRO_RS_TEST_REDIS_URL="$redis_url" \
  KIRO_RS_REQUIRE_STORAGE_TESTS=1 \
  KIRO_REDIS_USAGE_OUTER_ROUNDS="$outer_rounds" \
  bash -lc '
    set -euo pipefail
    cargo fmt --all -- --check
    git diff --check
    for ((round = 1; round <= KIRO_REDIS_USAGE_OUTER_ROUNDS; round += 1)); do
      printf "redis-usage-writer outer_round=%s\n" "$round"
      cargo test storage::redis_cache::tests::redis_usage_summary_cache_read_cardinality_is_hard_capped_for_five_rounds -- --exact --nocapture --test-threads=1
      cargo test storage::redis_cache::tests::redis_usage_summary_partial_command_error_never_sets_seen_for_five_rounds -- --exact --nocapture --test-threads=1
    done
    cargo test storage::redis_cache::tests::redis_usage_writer_burst_keeps_scheduler_latency_bounded_for_three_rounds -- --exact --nocapture --test-threads=1
  '
