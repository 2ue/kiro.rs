#!/usr/bin/env bash

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
[[ "$PWD" == "$repo_root" ]] || {
  printf 'run this validator from the repository root\n' >&2
  exit 64
}

redis_url="${ACCOUNT_RUNTIME_TEST_REDIS_URL:-}"
isolated="${ACCOUNT_RUNTIME_TEST_REDIS_ISOLATED:-0}"
allow_non_loopback="${ACCOUNT_RUNTIME_ALLOW_NON_LOOPBACK_STORAGE_TESTS:-0}"
outer_rounds="${ACCOUNT_RUNTIME_TOKEN_REFRESH_REDIS_OUTER_ROUNDS:-3}"
scope="${ACCOUNT_RUNTIME_TOKEN_REFRESH_REDIS_SCOPE:-token-refresh-redis-real}"

[[ -n "$redis_url" ]] || {
  printf 'ACCOUNT_RUNTIME_TEST_REDIS_URL is required; no Redis test was run\n' >&2
  exit 64
}
[[ "$isolated" == "1" ]] || {
  printf 'ACCOUNT_RUNTIME_TEST_REDIS_ISOLATED=1 is required; refusing an unconfirmed Redis target\n' >&2
  exit 64
}
[[ "$outer_rounds" =~ ^[1-9][0-9]*$ ]] && (( outer_rounds <= 10 )) || {
  printf 'ACCOUNT_RUNTIME_TOKEN_REFRESH_REDIS_OUTER_ROUNDS must be between 1 and 10\n' >&2
  exit 64
}
[[ "$scope" =~ ^[a-z0-9][a-z0-9._-]{0,63}$ ]] || {
  printf 'ACCOUNT_RUNTIME_TOKEN_REFRESH_REDIS_SCOPE has an invalid format\n' >&2
  exit 64
}

ACCOUNT_RUNTIME_ALLOW_NON_LOOPBACK_STORAGE_TESTS="$allow_non_loopback" \
ACCOUNT_RUNTIME_TEST_REDIS_URL="$redis_url" \
node <<'NODE'
const raw = process.env.ACCOUNT_RUNTIME_TEST_REDIS_URL;
let parsed;
try {
  parsed = new URL(raw);
} catch {
  process.stderr.write('ACCOUNT_RUNTIME_TEST_REDIS_URL is not a valid URL\n');
  process.exit(64);
}
if (!['redis:', 'rediss:'].includes(parsed.protocol)) {
  process.stderr.write('ACCOUNT_RUNTIME_TEST_REDIS_URL must use redis:// or rediss://\n');
  process.exit(64);
}
const hostname = parsed.hostname.toLowerCase();
const loopback = hostname === '127.0.0.1'
  || hostname === '::1'
  || hostname === '[::1]'
  || hostname === 'localhost';
if (!loopback && process.env.ACCOUNT_RUNTIME_ALLOW_NON_LOOPBACK_STORAGE_TESTS !== '1') {
  process.stderr.write(
    'non-loopback Redis requires ACCOUNT_RUNTIME_ALLOW_NON_LOOPBACK_STORAGE_TESTS=1\n',
  );
  process.exit(64);
}
if (parsed.port === '9022') {
  process.stderr.write('port 9022 is protected and cannot be used by this validator\n');
  process.exit(64);
}
NODE

ACCOUNT_RUNTIME_TEST_REDIS_URL="$redis_url" \
ACCOUNT_RUNTIME_REQUIRE_STORAGE_TESTS=1 \
ACCOUNT_RUNTIME_TOKEN_REFRESH_REDIS_OUTER_ROUNDS="$outer_rounds" \
feature/tests/run-cargo-scoped.sh "$scope" -- \
  env RUSTUP_TOOLCHAIN=1.92.0 \
  ACCOUNT_RUNTIME_TEST_REDIS_URL="$redis_url" \
  ACCOUNT_RUNTIME_REQUIRE_STORAGE_TESTS=1 \
  ACCOUNT_RUNTIME_TOKEN_REFRESH_REDIS_OUTER_ROUNDS="$outer_rounds" \
  bash -lc '
    set -euo pipefail
    cargo fmt --all -- --check
    git diff --check
    cargo test local_upstream_impl::provider::tests::api_and_mcp_final_attempt_fixtures_do_not_start_oauth_refresh_for_five_rounds -- --exact --nocapture --test-threads=1
    for ((round = 1; round <= ACCOUNT_RUNTIME_TOKEN_REFRESH_REDIS_OUTER_ROUNDS; round += 1)); do
      printf "token-refresh-redis outer_round=%s\n" "$round"
      cargo test storage::redis_cache::tests::token_refresh_redis_concurrent_begin_elects_one_leader_for_five_rounds -- --exact --nocapture --test-threads=1
      cargo test storage::redis_cache::tests::token_refresh_redis_failure_replay_health_claim_and_identity_are_fenced_for_five_rounds -- --exact --nocapture --test-threads=1
      cargo test storage::redis_cache::tests::token_refresh_redis_stale_leader_cannot_overwrite_success_for_five_rounds -- --exact --nocapture --test-threads=1
      cargo test storage::redis_cache::tests::token_refresh_redis_cancel_before_send_allows_immediate_new_leader_for_five_rounds -- --exact --nocapture --test-threads=1
      cargo test storage::redis_cache::tests::token_refresh_redis_bucket_ttl_refill_and_version_switch_hold_for_five_rounds -- --exact --nocapture --test-threads=1
    done
  '
