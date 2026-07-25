#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

KIRO_RS_BINARY="${KIRO_RS_BINARY:-}"
if [[ -z "$KIRO_RS_BINARY" || ! -x "$KIRO_RS_BINARY" ]]; then
  echo "KIRO_RS_BINARY must point to an executable frozen kiro-rs binary" >&2
  exit 2
fi

POSTGRES_CONTAINER="${KIRO_LOCAL_POSTGRES_CONTAINER:-kiro-rs-postgres-local}"
REDIS_URL_BARE="${KIRO_CLI_SUITE_REDIS_URL_BARE:-redis://127.0.0.1:26379/14}"
REDIS_URL_LONG="${KIRO_CLI_SUITE_REDIS_URL_LONG:-redis://127.0.0.1:26379/14}"
REDIS_URL_THINKING="${KIRO_CLI_SUITE_REDIS_URL_THINKING:-redis://127.0.0.1:26379/13}"
POSTGRES_HOST="${KIRO_CLI_SUITE_POSTGRES_HOST:-127.0.0.1}"
POSTGRES_PORT="${KIRO_CLI_SUITE_POSTGRES_PORT:-25432}"
POSTGRES_USER="${KIRO_CLI_SUITE_POSTGRES_USER:-kiro_rs}"
POSTGRES_DATABASE="${KIRO_CLI_SUITE_POSTGRES_DATABASE:-postgres}"
POSTGRES_SSLMODE="${KIRO_CLI_SUITE_POSTGRES_SSLMODE:-disable}"
CLAUDE_BINARY="${KIRO_CLAUDE_BINARY:-claude}"
SUITE_ONLY="${KIRO_CLI_SUITE_ONLY:-all}"

ARTIFACT_ROOT="${KIRO_VALIDATION_ARTIFACT_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/kiro-cli-artifacts.XXXXXX")}"
mkdir -p "$ARTIFACT_ROOT"

random_hex() {
  od -An -N2 -tx1 /dev/urandom | tr -d ' \n'
}

SUFFIX="$(date -u +%H%M%S)_$$_$(random_hex)"
BARE_DB="kiro_bare_invoke_${SUFFIX}"
LONG_DB="kiro_long_session_${SUFFIX}"
OWNER="own$(date -u +%H%M%S)$(random_hex)"
THINK_CLI_DB="kiro_thinking_wire_${OWNER}_cli"
THINK_IDE_DB="kiro_thinking_wire_${OWNER}_ide"
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
  -e PGUSER="\${PGUSER:-kiro_rs}" \\
  -e PGPASSWORD="\${PGPASSWORD:-}" \\
  -e PGAPPNAME="\${PGAPPNAME:-kiro-validation}" \\
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

psql_admin -c "COMMENT ON DATABASE $THINK_CLI_DB IS 'kiro-thinking-wire-owner:${OWNER}:cli';" >/dev/null
psql_admin -c "COMMENT ON DATABASE $THINK_IDE_DB IS 'kiro-thinking-wire-owner:${OWNER}:ide';" >/dev/null

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
echo "CLI_SUITE_BINARY_SHA256=$(shasum -a 256 "$KIRO_RS_BINARY" | awk '{print $1}')"
if [[ "$SUITE_ONLY" == "all" || "$SUITE_ONLY" == "bare" ]]; then
  echo "[1/3] bare-invoke Claude CLI start $(date -u +%FT%TZ)"
  KIRO_RS_BINARY="$KIRO_RS_BINARY" \
  KIRO_VALIDATION_ARTIFACT_DIR="$ARTIFACT_ROOT" \
  KIRO_BARE_INVOKE_POSTGRES_URL="$BARE_URL" \
  KIRO_BARE_INVOKE_REDIS_URL="$REDIS_URL_BARE" \
  KIRO_CLAUDE_BINARY="$CLAUDE_BINARY" \
  node feature/tests/bare-invoke-claude-cli.mjs
  echo "[1/3] bare-invoke Claude CLI done $(date -u +%FT%TZ)"
fi

if [[ "$SUITE_ONLY" == "all" || "$SUITE_ONLY" == "long" ]]; then
  echo "[2/3] long-session Claude CLI start $(date -u +%FT%TZ)"
  KIRO_RS_BINARY="$KIRO_RS_BINARY" \
  KIRO_VALIDATION_ARTIFACT_DIR="$ARTIFACT_ROOT" \
  KIRO_LONG_SESSION_POSTGRES_URL="$LONG_URL" \
  KIRO_LONG_SESSION_REDIS_URL="$REDIS_URL_LONG" \
  KIRO_CLAUDE_BINARY="$CLAUDE_BINARY" \
  KIRO_LONG_SESSION_ROUNDS="${KIRO_LONG_SESSION_ROUNDS:-5}" \
  KIRO_LONG_SESSION_TOOL_CYCLES="${KIRO_LONG_SESSION_TOOL_CYCLES:-20}" \
  KIRO_VALIDATION_PROGRESS="${KIRO_VALIDATION_PROGRESS:-1}" \
  node feature/tests/claude-cli-long-session-continue.mjs
  echo "[2/3] long-session Claude CLI done $(date -u +%FT%TZ)"
fi

if [[ "$SUITE_ONLY" == "all" || "$SUITE_ONLY" == "thinking" ]]; then
  echo "[3/3] thinking-wire Claude CLI start $(date -u +%FT%TZ)"
  KIRO_RS_BINARY="$KIRO_RS_BINARY" \
  KIRO_VALIDATION_ARTIFACT_DIR="$ARTIFACT_ROOT" \
  KIRO_THINKING_WIRE_DATABASE_OWNER="$OWNER" \
  KIRO_THINKING_WIRE_CLI_POSTGRES_URL="$THINK_CLI_URL" \
  KIRO_THINKING_WIRE_IDE_POSTGRES_URL="$THINK_IDE_URL" \
  KIRO_THINKING_WIRE_REDIS_URL="$REDIS_URL_THINKING" \
  KIRO_PSQL_BINARY="$PSQL_WRAPPER" \
  KIRO_CLAUDE_BINARY="$CLAUDE_BINARY" \
  KIRO_VALIDATION_PROGRESS="${KIRO_VALIDATION_PROGRESS:-1}" \
  node feature/tests/thinking-effort-kiro-wire.mjs
  echo "[3/3] thinking-wire Claude CLI done $(date -u +%FT%TZ)"
fi

echo "CLI_SUITE_RESULT=pass"
