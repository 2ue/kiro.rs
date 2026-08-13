#!/usr/bin/env bash
set -euo pipefail

target="${1:-ui}"
api_target="${VITE_API_PROXY_TARGET:-http://127.0.0.1:9022}"

case "$target" in
  ui|new)
    dir="ui"
    url="http://127.0.0.1:9023/ui/runtime"
    ;;
  admin|old)
    dir="admin-ui"
    url="http://127.0.0.1:9025/admin/"
    ;;
  *)
    cat >&2 <<'USAGE'
Usage: bash scripts/dev-ui.sh [ui|admin]

ui       New UI on http://127.0.0.1:9023/ui/runtime
admin    Old Admin UI on http://127.0.0.1:9025/admin/

Set VITE_API_PROXY_TARGET to override the backend API target.
USAGE
    exit 2
    ;;
esac

echo "Starting $target frontend from $dir"
echo "Preview: $url"
echo "API proxy: $api_target"
echo "Debug backend UI entry redirects here when the backend is built with cargo run."

exec env VITE_API_PROXY_TARGET="$api_target" pnpm --dir "$dir" dev
