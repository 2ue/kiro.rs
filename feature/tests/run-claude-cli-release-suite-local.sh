#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

ACCOUNT_RUNTIME_BINARY="${ACCOUNT_RUNTIME_BINARY:-}"
if [[ -z "$ACCOUNT_RUNTIME_BINARY" || ! -x "$ACCOUNT_RUNTIME_BINARY" ]]; then
  echo "ACCOUNT_RUNTIME_BINARY must point to an executable frozen account runtime binary" >&2
  exit 2
fi

POSTGRES_CONTAINER="${ACCOUNT_RUNTIME_LOCAL_POSTGRES_CONTAINER:-account-runtime-postgres-local}"
REDIS_URL_BARE="${ACCOUNT_RUNTIME_CLI_SUITE_REDIS_URL_BARE:-redis://127.0.0.1:26379/14}"
REDIS_URL_LONG="${ACCOUNT_RUNTIME_CLI_SUITE_REDIS_URL_LONG:-redis://127.0.0.1:26379/14}"
REDIS_URL_THINKING="${ACCOUNT_RUNTIME_CLI_SUITE_REDIS_URL_THINKING:-redis://127.0.0.1:26379/13}"
POSTGRES_HOST="${ACCOUNT_RUNTIME_CLI_SUITE_POSTGRES_HOST:-127.0.0.1}"
POSTGRES_PORT="${ACCOUNT_RUNTIME_CLI_SUITE_POSTGRES_PORT:-25432}"
POSTGRES_USER="${ACCOUNT_RUNTIME_CLI_SUITE_POSTGRES_USER:-account_runtime}"
POSTGRES_DATABASE="${ACCOUNT_RUNTIME_CLI_SUITE_POSTGRES_DATABASE:-postgres}"
POSTGRES_SSLMODE="${ACCOUNT_RUNTIME_CLI_SUITE_POSTGRES_SSLMODE:-disable}"
CLAUDE_BINARY="${ACCOUNT_RUNTIME_CLAUDE_BINARY:-claude}"
SUITE_ONLY="${ACCOUNT_RUNTIME_CLI_SUITE_ONLY:-all}"

ARTIFACT_ROOT="${ACCOUNT_RUNTIME_VALIDATION_ARTIFACT_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/account-runtime-cli-artifacts.XXXXXX")}"
mkdir -p "$ARTIFACT_ROOT"

random_hex() {
  od -An -N2 -tx1 /dev/urandom | tr -d ' \n'
}

SUFFIX="$(date -u +%H%M%S)_$$_$(random_hex)"
BARE_DB="account_runtime_bare_invoke_${SUFFIX}"
LONG_DB="account_runtime_long_session_${SUFFIX}"
OWNER="own$(date -u +%H%M%S)$(random_hex)"
THINK_CLI_DB="account_runtime_thinking_wire_${OWNER}_cli"
THINK_IDE_DB="account_runtime_thinking_wire_${OWNER}_ide"
PSQL_WRAPPER="$ARTIFACT_ROOT/psql-docker-local"

POSTGRES_PASSWORD="$(
  docker inspect "$POSTGRES_CONTAINER" --format '{{range .Config.Env}}{{println .}}{{end}}' \
    | awk -F= '$1=="POSTGRES_PASSWORD"{print $2; exit}'
)"
if [[ -z "$POSTGRES_PASSWORD" ]]; then
  echo "failed to read local PostgreSQL test container password" >&2
  exit 2
fi

cat > "$PSQL_WRAPPER" <<WRAP
#!/usr/bin/env bash
set -euo pipefail
exec docker exec -i \\
  -e PGDATABASE="\${PGDATABASE:-postgres}" \\
  -e PGUSER="\${PGUSER:-account_runtime}" \\
  -e PGPASSWORD="\${PGPASSWORD:-}" \\
  -e PGAPPNAME="\${PGAPPNAME:-account-runtime-validation}" \\
  -e PGCONNECT_TIMEOUT="\${PGCONNECT_TIMEOUT:-5}" \\
  -e PGSSLMODE="\${PGSSLMODE:-disable}" \\
  "$POSTGRES_CONTAINER" psql -h 127.0.0.1 -p 5432 "\$@"
WRAP
chmod 700 "$PSQL_WRAPPER"

psql_admin() {
  docker exec "$POSTGRES_CONTAINER" psql \
    -U "$POSTGRES_USER" \
    -d "$POSTGRES_DATABASE" \
    -v ON_ERROR_STOP=1 \
    "$@"
}

cleanup() {
  set +e
  for db in "$BARE_DB" "$LONG_DB" "$THINK_CLI_DB" "$THINK_IDE_DB"; do
    docker exec "$POSTGRES_CONTAINER" psql \
      -U "$POSTGRES_USER" \
      -d "$POSTGRES_DATABASE" \
      -v ON_ERROR_STOP=0 \
      -Atqc "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '$db' AND pid <> pg_backend_pid();" \
      >/dev/null 2>&1
    docker exec "$POSTGRES_CONTAINER" psql \
      -U "$POSTGRES_USER" \
      -d "$POSTGRES_DATABASE" \
      -v ON_ERROR_STOP=0 \
      -c "DROP DATABASE IF EXISTS $db;" \
      >/dev/null 2>&1
  done
}
trap cleanup EXIT

for db in "$BARE_DB" "$LONG_DB" "$THINK_CLI_DB" "$THINK_IDE_DB"; do
  psql_admin -c "CREATE DATABASE $db;" >/dev/null
done

psql_admin -c "COMMENT ON DATABASE $THINK_CLI_DB IS 'account-runtime-thinking-wire-owner:${OWNER}:cli';" >/dev/null
psql_admin -c "COMMENT ON DATABASE $THINK_IDE_DB IS 'account-runtime-thinking-wire-owner:${OWNER}:ide';" >/dev/null

url_for_db() {
  local db="$1"
  printf 'postgresql://%s:%s@%s:%s/%s?sslmode=%s' \
    "$POSTGRES_USER" \
    "$POSTGRES_PASSWORD" \
    "$POSTGRES_HOST" \
    "$POSTGRES_PORT" \
    "$db" \
    "$POSTGRES_SSLMODE"
}

BARE_URL="$(url_for_db "$BARE_DB")"
LONG_URL="$(url_for_db "$LONG_DB")"
THINK_CLI_URL="$(url_for_db "$THINK_CLI_DB")"
THINK_IDE_URL="$(url_for_db "$THINK_IDE_DB")"

echo "CLI_SUITE_ARTIFACT_ROOT=$ARTIFACT_ROOT"
echo "CLI_SUITE_OWNER=$OWNER"
echo "CLI_SUITE_BINARY_SHA256=$(shasum -a 256 "$ACCOUNT_RUNTIME_BINARY" | awk '{print $1}')"
if [[ "$SUITE_ONLY" == "all" || "$SUITE_ONLY" == "bare" ]]; then
  echo "[1/3] bare-invoke Claude CLI start $(date -u +%FT%TZ)"
  ACCOUNT_RUNTIME_BINARY="$ACCOUNT_RUNTIME_BINARY" \
  ACCOUNT_RUNTIME_VALIDATION_ARTIFACT_DIR="$ARTIFACT_ROOT" \
  ACCOUNT_RUNTIME_BARE_INVOKE_POSTGRES_URL="$BARE_URL" \
  ACCOUNT_RUNTIME_BARE_INVOKE_REDIS_URL="$REDIS_URL_BARE" \
  ACCOUNT_RUNTIME_CLAUDE_BINARY="$CLAUDE_BINARY" \
  node feature/tests/bare-invoke-claude-cli.mjs
  echo "[1/3] bare-invoke Claude CLI done $(date -u +%FT%TZ)"
fi

if [[ "$SUITE_ONLY" == "all" || "$SUITE_ONLY" == "long" ]]; then
  echo "[2/3] long-session Claude CLI start $(date -u +%FT%TZ)"
  ACCOUNT_RUNTIME_BINARY="$ACCOUNT_RUNTIME_BINARY" \
  ACCOUNT_RUNTIME_VALIDATION_ARTIFACT_DIR="$ARTIFACT_ROOT" \
  ACCOUNT_RUNTIME_LONG_SESSION_POSTGRES_URL="$LONG_URL" \
  ACCOUNT_RUNTIME_LONG_SESSION_REDIS_URL="$REDIS_URL_LONG" \
  ACCOUNT_RUNTIME_CLAUDE_BINARY="$CLAUDE_BINARY" \
  ACCOUNT_RUNTIME_LONG_SESSION_ROUNDS="${ACCOUNT_RUNTIME_LONG_SESSION_ROUNDS:-5}" \
  ACCOUNT_RUNTIME_LONG_SESSION_TOOL_CYCLES="${ACCOUNT_RUNTIME_LONG_SESSION_TOOL_CYCLES:-20}" \
  ACCOUNT_RUNTIME_VALIDATION_PROGRESS="${ACCOUNT_RUNTIME_VALIDATION_PROGRESS:-1}" \
  node feature/tests/claude-cli-long-session-continue.mjs
  echo "[2/3] long-session Claude CLI done $(date -u +%FT%TZ)"
fi

if [[ "$SUITE_ONLY" == "all" || "$SUITE_ONLY" == "thinking" ]]; then
  echo "[3/3] thinking-wire Claude CLI start $(date -u +%FT%TZ)"
  ACCOUNT_RUNTIME_BINARY="$ACCOUNT_RUNTIME_BINARY" \
  ACCOUNT_RUNTIME_VALIDATION_ARTIFACT_DIR="$ARTIFACT_ROOT" \
  ACCOUNT_RUNTIME_THINKING_WIRE_DATABASE_OWNER="$OWNER" \
  ACCOUNT_RUNTIME_THINKING_WIRE_CLI_POSTGRES_URL="$THINK_CLI_URL" \
  ACCOUNT_RUNTIME_THINKING_WIRE_IDE_POSTGRES_URL="$THINK_IDE_URL" \
  ACCOUNT_RUNTIME_THINKING_WIRE_REDIS_URL="$REDIS_URL_THINKING" \
  ACCOUNT_RUNTIME_PSQL_BINARY="$PSQL_WRAPPER" \
  ACCOUNT_RUNTIME_CLAUDE_BINARY="$CLAUDE_BINARY" \
  ACCOUNT_RUNTIME_VALIDATION_PROGRESS="${ACCOUNT_RUNTIME_VALIDATION_PROGRESS:-1}" \
  node feature/tests/thinking-effort-account-wire.mjs
  echo "[3/3] thinking-wire Claude CLI done $(date -u +%FT%TZ)"
fi

echo "CLI_SUITE_RESULT=pass"
