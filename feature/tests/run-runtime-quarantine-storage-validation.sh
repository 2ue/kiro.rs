#!/usr/bin/env bash

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
[[ "$PWD" == "$repo_root" ]] || {
  printf 'run this validator from the repository root\n' >&2
  exit 64
}

postgres_url="${KIRO_RS_TEST_POSTGRES_URL:-}"
redis_url="${KIRO_RS_TEST_REDIS_URL:-}"
postgres_isolated="${KIRO_RS_TEST_POSTGRES_ISOLATED:-0}"
redis_isolated="${KIRO_RS_TEST_REDIS_ISOLATED:-0}"
allow_non_loopback="${KIRO_RS_ALLOW_NON_LOOPBACK_STORAGE_TESTS:-0}"
outer_rounds="${KIRO_RUNTIME_QUARANTINE_STORAGE_OUTER_ROUNDS:-3}"
scope="${KIRO_RUNTIME_QUARANTINE_STORAGE_SCOPE:-runtime-quarantine-storage-real}"

[[ -n "$postgres_url" ]] || {
  printf 'KIRO_RS_TEST_POSTGRES_URL is required; no storage test was run\n' >&2
  exit 64
}
[[ -n "$redis_url" ]] || {
  printf 'KIRO_RS_TEST_REDIS_URL is required; no storage test was run\n' >&2
  exit 64
}
[[ "$postgres_isolated" == "1" ]] || {
  printf 'KIRO_RS_TEST_POSTGRES_ISOLATED=1 is required\n' >&2
  exit 64
}
[[ "$redis_isolated" == "1" ]] || {
  printf 'KIRO_RS_TEST_REDIS_ISOLATED=1 is required\n' >&2
  exit 64
}
[[ "$outer_rounds" =~ ^[1-9][0-9]*$ ]] && (( outer_rounds <= 5 )) || {
  printf 'KIRO_RUNTIME_QUARANTINE_STORAGE_OUTER_ROUNDS must be between 1 and 5\n' >&2
  exit 64
}
[[ "$scope" =~ ^[a-z0-9][a-z0-9._-]{0,63}$ ]] || {
  printf 'KIRO_RUNTIME_QUARANTINE_STORAGE_SCOPE has an invalid format\n' >&2
  exit 64
}

KIRO_RS_TEST_POSTGRES_URL="$postgres_url" \
KIRO_RS_TEST_REDIS_URL="$redis_url" \
KIRO_RS_ALLOW_NON_LOOPBACK_STORAGE_TESTS="$allow_non_loopback" \
node <<'NODE'
const net = require('node:net');

const targets = [
  {
    name: 'KIRO_RS_TEST_POSTGRES_URL',
    raw: process.env.KIRO_RS_TEST_POSTGRES_URL,
    protocols: new Set(['postgres:', 'postgresql:']),
    defaultPort: 5432,
  },
  {
    name: 'KIRO_RS_TEST_REDIS_URL',
    raw: process.env.KIRO_RS_TEST_REDIS_URL,
    protocols: new Set(['redis:', 'rediss:']),
    defaultPort: 6379,
  },
];

function parseTarget(target) {
  let parsed;
  try {
    parsed = new URL(target.raw);
  } catch {
    throw new Error(`${target.name} is not a valid URL`);
  }
  if (!target.protocols.has(parsed.protocol)) {
    throw new Error(`${target.name} has an unsupported scheme`);
  }
  const hostname = parsed.hostname.toLowerCase();
  const networkHostname = hostname.startsWith('[') && hostname.endsWith(']')
    ? hostname.slice(1, -1)
    : hostname;
  const loopback = networkHostname === '127.0.0.1'
    || networkHostname === '::1'
    || networkHostname === 'localhost';
  if (!loopback && process.env.KIRO_RS_ALLOW_NON_LOOPBACK_STORAGE_TESTS !== '1') {
    throw new Error(`${target.name} requires an explicit non-loopback opt-in`);
  }
  const port = parsed.port === '' ? target.defaultPort : Number(parsed.port);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`${target.name} has an invalid port`);
  }
  if (port === 9022) {
    throw new Error(`${target.name} cannot use protected port 9022`);
  }
  return { name: target.name, hostname: networkHostname, port };
}

function probe(target) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({ host: target.hostname, port: target.port });
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error(`${target.name} TCP prerequisite is unreachable`));
    }, 2000);
    socket.once('connect', () => {
      clearTimeout(timer);
      socket.destroy();
      resolve();
    });
    socket.once('error', () => {
      clearTimeout(timer);
      reject(new Error(`${target.name} TCP prerequisite is unreachable`));
    });
  });
}

(async () => {
  try {
    const parsed = targets.map(parseTarget);
    for (const target of parsed) {
      await probe(target);
    }
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(64);
  }
})();
NODE

KIRO_RS_TEST_POSTGRES_URL="$postgres_url" \
KIRO_RS_TEST_REDIS_URL="$redis_url" \
KIRO_RS_REQUIRE_STORAGE_TESTS=1 \
KIRO_RUNTIME_QUARANTINE_STORAGE_OUTER_ROUNDS="$outer_rounds" \
feature/tests/run-cargo-scoped.sh "$scope" -- \
  env RUSTUP_TOOLCHAIN=1.92.0 \
  KIRO_RS_TEST_POSTGRES_URL="$postgres_url" \
  KIRO_RS_TEST_REDIS_URL="$redis_url" \
  KIRO_RS_REQUIRE_STORAGE_TESTS=1 \
  KIRO_RUNTIME_QUARANTINE_STORAGE_OUTER_ROUNDS="$outer_rounds" \
  bash -lc '
    set -euo pipefail
    cargo fmt --all -- --check
    git diff --check
    for ((round = 1; round <= KIRO_RUNTIME_QUARANTINE_STORAGE_OUTER_ROUNDS; round += 1)); do
      printf "runtime-quarantine-storage outer_round=%s\n" "$round"
      cargo test kiro::token_manager::manager::tests::postgres_pool_pressure_backlogs_non_terminal_success_without_quarantine_for_five_rounds -- --exact --nocapture --test-threads=1
      cargo test kiro::token_manager::manager::tests::postgres_pending_runtime_mutations_replay_in_order_and_unquarantine -- --exact --nocapture --test-threads=1
      cargo test kiro::token_manager::manager::tests::postgres_reset_generation_fences_pending_failure_and_disable_replay -- --exact --nocapture --test-threads=1
      cargo test kiro::token_manager::manager::tests::finite_redis_dispatch_queue_lease_deadline_does_not_move_after_renew_interval -- --exact --nocapture --test-threads=1
      cargo test kiro::token_manager::manager::tests::redis_dispatch_queue_waiter_fails_closed_after_coordination_degrades -- --exact --nocapture --test-threads=1
      cargo test kiro::token_manager::manager::tests::redis_dispatch_queue_cancelled_waiter_releases_local_and_remote_lease -- --exact --nocapture --test-threads=1
    done
  '
