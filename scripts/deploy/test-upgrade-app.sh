#!/usr/bin/env bash
# 在本机 Docker 上验证 upgrade-app.sh 只重建应用服务，不重建依赖服务。
# 测试使用 redis:8-alpine 占位镜像，不拉取或构建 kiro-rs 镜像。

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../.." && pwd)
upgrade_script=$repo_root/scripts/deploy/upgrade-app.sh
tmp_parent=${TMPDIR:-/tmp}
tmp_dir=$(mktemp -d "$tmp_parent/kiro-r10-upgrade.XXXXXX")

projects=()
compose_files=()
project_count=0
failures=0

cleanup() {
  local i
  for ((i = project_count - 1; i >= 0; i--)); do
    docker compose -p "${projects[$i]}" -f "${compose_files[$i]}" down -v --remove-orphans >/dev/null 2>&1 || true
  done
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

write_fixture() {
  local file=$1
  local app_service=$2
  local app_healthcheck=${3:-yes}
  cat >"$file" <<EOF
services:
  redis:
    image: redis:8-alpine
    pull_policy: never
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 1s
      timeout: 1s
      retries: 10
  postgres:
    image: redis:8-alpine
    pull_policy: never
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 1s
      timeout: 1s
      retries: 10
  $app_service:
    image: redis:8-alpine
    pull_policy: never
    command: ["sh", "-c", "echo upgrade-app-log-marker; sleep infinity"]
    environment:
      KIRO_R10_HEALTH: "\${KIRO_R10_HEALTH:-pass}"
    labels:
      com.kiro-r10.revision: "\${KIRO_R10_REV:-initial}"
      com.kiro-r10.requested-version: "\${KIRO_RS_VERSION:-unset}"
EOF
  if [[ $app_healthcheck == yes ]]; then
    cat >>"$file" <<'EOF'
    healthcheck:
      test: ["CMD-SHELL", "test \"$$KIRO_R10_HEALTH\" = pass"]
      interval: 1s
      timeout: 1s
      retries: 2
EOF
  fi
  cat >>"$file" <<'EOF'
    depends_on:
      redis:
        condition: service_healthy
      postgres:
        condition: service_healthy
EOF
}

new_case() {
  local name=$1
  local app_service=${2:-kiro-rs}
  local app_healthcheck=${3:-yes}
  local case_dir="$tmp_dir/$name"
  local project="kiro-r10-test-${name}-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
  local file="$case_dir/compose.yml"
  mkdir -p "$case_dir"
  write_fixture "$file" "$app_service" "$app_healthcheck"
  projects+=("$project")
  compose_files+=("$file")
  project_count=$((project_count + 1))
  CASE_PROJECT=$project
  CASE_FILE=$file
}

new_single_app_case() {
  local name=$1
  local case_dir="$tmp_dir/$name"
  local project="kiro-r10-test-${name}-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
  local file="$case_dir/compose.yml"
  mkdir -p "$case_dir"
  cat >"$file" <<'EOF'
services:
  kiro-rs:
    image: redis:8-alpine
    pull_policy: never
    command: ["sh", "-c", "echo upgrade-app-log-marker; sleep infinity"]
    labels:
      com.kiro-r10.revision: "${KIRO_R10_REV:-initial}"
    healthcheck:
      test: ["CMD", "true"]
      interval: 1s
      timeout: 1s
      retries: 2
EOF
  projects+=("$project")
  compose_files+=("$file")
  project_count=$((project_count + 1))
  CASE_PROJECT=$project
  CASE_FILE=$file
}

compose_for_case() {
  docker compose -p "$CASE_PROJECT" -f "$CASE_FILE" "$@"
}

wait_healthy() {
  local service=$1
  local id status
  local deadline=$((SECONDS + 30))
  while ((SECONDS < deadline)); do
    id=$(compose_for_case ps -q "$service")
    if [[ -n $id ]]; then
      status=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$id" 2>/dev/null || true)
      [[ $status == healthy ]] && return 0
    fi
    sleep 1
  done
  return 1
}

run_capture() {
  local output=$1
  shift
  set +e
  "$@" >"$output" 2>&1
  local status=$?
  set -e
  return "$status"
}

assertion_failures=0

assert_equal() {
  local expected=$1 actual=$2 description=$3
  if [[ $expected != "$actual" ]]; then
    printf '断言失败：%s（期望=%s，实际=%s）\n' "$description" "$expected" "$actual" >&2
    assertion_failures=$((assertion_failures + 1))
  fi
}

assert_different() {
  local left=$1 right=$2 description=$3
  if [[ $left == "$right" ]]; then
    printf '断言失败：%s（两值相同：%s）\n' "$description" "$left" >&2
    assertion_failures=$((assertion_failures + 1))
  fi
}

assert_contains() {
  local file=$1 needle=$2 description=$3
  if ! grep -Fq -- "$needle" "$file"; then
    printf '断言失败：%s（未找到 %s）\n' "$description" "$needle" >&2
    assertion_failures=$((assertion_failures + 1))
  fi
}

assert_healthy() {
  local service=$1
  if ! wait_healthy "$service"; then
    printf '断言失败：服务 %s 未能在 30 秒内就绪\n' "$service" >&2
    assertion_failures=$((assertion_failures + 1))
  fi
}

case_single_service_no_dependencies() {
  new_single_app_case single-app-no-dependencies
  local output="$tmp_dir/single-app-no-dependencies.out"
  export KIRO_R10_REV=before
  compose_for_case up -d >/dev/null
  assert_healthy kiro-rs
  local app_before app_after
  app_before=$(compose_for_case ps -q kiro-rs)

  run_capture "$output" env KIRO_R10_SKIP_PULL=1 KIRO_R10_REV=after \
    "$upgrade_script" -f "$CASE_FILE" -p "$CASE_PROJECT"
  local status=$?
  app_after=$(compose_for_case ps -q kiro-rs)
  assert_equal 0 "$status" '仅含应用服务且无依赖时退出码为 0'
  assert_different "$app_before" "$app_after" '无依赖时应用容器被重建'
  assert_contains "$output" '升级完成：服务=kiro-rs' '无依赖时打印升级结果'
}

case_only_rebuild_app() {
  new_case only-rebuild
  local output="$tmp_dir/only-rebuild.out"
  export KIRO_R10_REV=before
  compose_for_case up -d >/dev/null
  assert_healthy redis
  assert_healthy postgres
  assert_healthy kiro-rs
  local redis_before postgres_before app_before
  redis_before=$(compose_for_case ps -q redis)
  postgres_before=$(compose_for_case ps -q postgres)
  app_before=$(compose_for_case ps -q kiro-rs)

  run_capture "$output" env KIRO_R10_SKIP_PULL=1 KIRO_R10_REV=after \
    "$upgrade_script" -f "$CASE_FILE" -p "$CASE_PROJECT"
  local status=$?
  local redis_after postgres_after app_after
  redis_after=$(compose_for_case ps -q redis)
  postgres_after=$(compose_for_case ps -q postgres)
  app_after=$(compose_for_case ps -q kiro-rs)
  assert_equal 0 "$status" '只重建应用的退出码'
  assert_equal "$redis_before" "$redis_after" 'Redis 容器 ID 不变'
  assert_equal "$postgres_before" "$postgres_after" 'Postgres 容器 ID 不变'
  assert_different "$app_before" "$app_after" 'kiro-rs 容器被重建'
  assert_contains "$output" '升级完成：服务=kiro-rs' '成功结果包含应用服务'
  assert_contains "$output" '镜像版本=' '成功结果包含镜像版本 label'
  assert_contains "$output" '镜像修订=' '成功结果包含镜像 revision label'
  assert_contains "$output" '启动时间=' '成功结果包含容器启动时间'
}

case_service_detection() {
  new_case service-detection foo
  local output="$tmp_dir/service-detection.out"
  export KIRO_R10_REV=before
  compose_for_case up -d >/dev/null
  assert_healthy redis
  assert_healthy postgres
  assert_healthy foo

  run_capture "$output" env KIRO_RS_COMPOSE_FILE="$CASE_FILE" \
    KIRO_RS_COMPOSE_PROJECT="$CASE_PROJECT" KIRO_R10_SKIP_PULL=1 \
    KIRO_R10_REV=after "$upgrade_script"
  local status=$?
  assert_equal 2 "$status" '没有匹配服务时退出码为 2'
  assert_contains "$output" '请用 -s 指定' '服务识别错误提示 -s'

  run_capture "$output" env KIRO_R10_SKIP_PULL=1 KIRO_R10_REV=after \
    "$upgrade_script" -f "$CASE_FILE" -p "$CASE_PROJECT" -s foo -v test-version
  status=$?
  assert_equal 0 "$status" '-s foo 指定服务后成功'
  local app_id requested_version
  app_id=$(compose_for_case ps -q foo)
  requested_version=$(docker inspect -f '{{index .Config.Labels "com.kiro-r10.requested-version"}}' "$app_id")
  assert_equal test-version "$requested_version" '-v 只在本次命令中设置 KIRO_RS_VERSION'

  cat >>"$CASE_FILE" <<'EOF'
  app:
    image: redis:8-alpine
    pull_policy: never
    command: ["sh", "-c", "sleep infinity"]
  kiro-rs-app:
    image: redis:8-alpine
    pull_policy: never
    command: ["sh", "-c", "sleep infinity"]
EOF
  run_capture "$output" env KIRO_R10_SKIP_PULL=1 \
    "$upgrade_script" -f "$CASE_FILE" -p "$CASE_PROJECT"
  status=$?
  assert_equal 2 "$status" '多个候选服务时退出码为 2'
  assert_contains "$output" '请用 -s 指定' '多候选错误提示 -s'

  new_case no-health kiro-rs no
  local case_dir="$tmp_dir/no-health"
  mv "$CASE_FILE" "$case_dir/docker-compose.yml"
  CASE_FILE="$case_dir/docker-compose.yml"
  compose_files[$((project_count - 1))]=$CASE_FILE
  compose_for_case up -d >/dev/null
  assert_healthy redis
  assert_healthy postgres
  local started_at=$SECONDS elapsed
  run_capture "$output" env KIRO_RS_COMPOSE_PROJECT="$CASE_PROJECT" \
    KIRO_RS_APP_SERVICE=kiro-rs KIRO_R10_SKIP_PULL=1 \
    bash -c 'cd "$1" && exec "$2" --wait-secs 10' _ "$case_dir" "$upgrade_script"
  status=$?
  elapsed=$((SECONDS - started_at))
  assert_equal 0 "$status" '没有 healthcheck 时运行容器就绪'
  if ((elapsed < 5)); then
    printf '断言失败：无 healthcheck 分支至少等待 5 秒（实际=%s）\n' "$elapsed" >&2
    assertion_failures=$((assertion_failures + 1))
  fi
}

case_dry_run() {
  new_case dry-run
  local output="$tmp_dir/dry-run.out"
  export KIRO_R10_REV=before
  compose_for_case up -d >/dev/null
  assert_healthy redis
  assert_healthy postgres
  assert_healthy kiro-rs
  local redis_before postgres_before app_before
  redis_before=$(compose_for_case ps -q redis)
  postgres_before=$(compose_for_case ps -q postgres)
  app_before=$(compose_for_case ps -q kiro-rs)

  run_capture "$output" env KIRO_R10_REV=after \
    "$upgrade_script" -f "$CASE_FILE" -p "$CASE_PROJECT" --dry-run
  local status=$?
  local redis_after postgres_after app_after
  redis_after=$(compose_for_case ps -q redis)
  postgres_after=$(compose_for_case ps -q postgres)
  app_after=$(compose_for_case ps -q kiro-rs)
  assert_equal 0 "$status" 'dry-run 退出码'
  assert_equal "$redis_before" "$redis_after" 'dry-run Redis 容器 ID 不变'
  assert_equal "$postgres_before" "$postgres_after" 'dry-run Postgres 容器 ID 不变'
  assert_equal "$app_before" "$app_after" 'dry-run 应用容器 ID 不变'
  assert_contains "$output" 'pull kiro-rs' 'dry-run 回显 pull 命令'
  assert_contains "$output" 'up -d --no-deps' 'dry-run 输出升级命令'
}

case_health_timeout() {
  new_case health-timeout
  local output="$tmp_dir/health-timeout.out"
  export KIRO_R10_HEALTH=pass KIRO_R10_REV=before
  compose_for_case up -d >/dev/null
  assert_healthy redis
  assert_healthy postgres
  assert_healthy kiro-rs

  run_capture "$output" env KIRO_R10_SKIP_PULL=1 KIRO_R10_HEALTH=fail KIRO_R10_REV=after \
    "$upgrade_script" -f "$CASE_FILE" -p "$CASE_PROJECT" --wait-secs 5
  local status=$?
  assert_equal 3 "$status" '健康检查超时退出码为 3'
  assert_contains "$output" 'logs --tail 50 kiro-rs' '超时输出最近日志命令'
  assert_contains "$output" 'upgrade-app-log-marker' '超时输出应用日志片段'
}

case_dependency_guard() {
  new_case dependency-guard
  local output="$tmp_dir/dependency-guard.out"
  export KIRO_R10_REV=before
  compose_for_case up -d >/dev/null
  assert_healthy redis
  assert_healthy postgres
  assert_healthy kiro-rs
  local hook
  hook="docker compose -p '$CASE_PROJECT' -f '$CASE_FILE' up -d --force-recreate redis"

  run_capture "$output" env KIRO_R10_SKIP_PULL=1 KIRO_R10_REV=after \
    KIRO_R10_TEST_HOOK_AFTER_UP="$hook" \
    "$upgrade_script" -f "$CASE_FILE" -p "$CASE_PROJECT"
  local status=$?
  assert_equal 4 "$status" '依赖服务重建防御校验退出码为 4'
  assert_contains "$output" '依赖服务被重建' '防御校验输出警告'
  assert_contains "$output" 'Redis 重启会触发外部池协调器 35 秒恢复窗口' '防御校验包含恢复窗口提示'
}

run_case() {
  local name=$1 function_name=$2
  local assertions_before=$assertion_failures
  printf '用例 %s ... ' "$name"
  local status=0
  "$function_name" || status=$?
  if ((status == 0 && assertion_failures == assertions_before)); then
    printf 'PASS\n'
  else
    printf 'FAIL\n'
    failures=$((failures + 1))
  fi
}

run_case '无依赖单服务' case_single_service_no_dependencies
run_case '只重建应用' case_only_rebuild_app
run_case '服务识别' case_service_detection
run_case 'dry-run' case_dry_run
run_case '健康超时' case_health_timeout
run_case '防御校验' case_dependency_guard

cleanup
trap - EXIT
leftovers=$(docker ps -a --filter name=kiro-r10-test- --format '{{.Names}}')
if [[ -n $leftovers ]]; then
  printf 'FAIL：仍有测试容器：%s\n' "$leftovers" >&2
  failures=$((failures + 1))
fi

if ((failures)); then
  printf '测试失败：%d 个用例\n' "$failures" >&2
  exit 1
fi
printf '全部 6 个用例 PASS\n'
