#!/usr/bin/env bash
# 普通版本升级只拉取并重建应用服务，依赖服务保持运行。
# 不要使用 `docker compose pull && docker compose up -d`：它会一起更新并重建
# Redis，导致外部池协调器进入 35 秒恢复窗口，期间请求可能返回
# external_pool_coordinator_unavailable。
# KIRO_R10_SKIP_PULL 和 KIRO_R10_TEST_HOOK_AFTER_UP 仅供集成测试使用，正常升级不要设置。
# 用法：scripts/deploy/upgrade-app.sh [-f FILE] [-p NAME] [-s SERVICE] [-v VERSION]
#       [--dry-run] [--wait-secs N]
# 示例：./scripts/deploy/upgrade-app.sh -v 0.0.19

set -euo pipefail

usage() {
  cat <<'EOF'
用法：upgrade-app.sh [-f FILE] [-p NAME] [-s SERVICE] [-v VERSION] [--dry-run] [--wait-secs N]

选项：
  -f FILE       Compose 文件；默认使用 KIRO_RS_COMPOSE_FILE，或查找
                docker-compose.yml、docker-compose.database.yml
  -p NAME       Compose project 名；默认使用 KIRO_RS_COMPOSE_PROJECT
  -s SERVICE    应用服务名；默认自动识别 kiro-rs、kiro-rs-app 或 app
  -v VERSION    本次使用的 KIRO_RS_VERSION，不改写 .env
  --dry-run     只显示升级命令
  --wait-secs N 健康检查等待上限，默认 120 秒
  -h            显示帮助
EOF
}

fail() {
  local code=$1
  shift
  printf '错误：%s\n' "$*" >&2
  exit "$code"
}

print_command() {
  printf '+ ' >&2
  printf '%q ' "$@" >&2
  printf '\n' >&2
}

run_docker() {
  print_command "$@"
  "$@"
}

run_compose() {
  run_docker "${compose[@]}" "$@"
}

compose_file=${KIRO_RS_COMPOSE_FILE:-}
project_name=${KIRO_RS_COMPOSE_PROJECT:-}
app_service=${KIRO_RS_APP_SERVICE:-}
version=
dry_run=false
wait_secs=120

while (($#)); do
  case "$1" in
    -f)
      (($# >= 2)) || fail 2 '参数 -f 缺少文件名'
      compose_file=$2
      shift 2
      ;;
    -p)
      (($# >= 2)) || fail 2 '参数 -p 缺少 project 名'
      project_name=$2
      shift 2
      ;;
    -s)
      (($# >= 2)) || fail 2 '参数 -s 缺少服务名'
      app_service=$2
      shift 2
      ;;
    -v)
      (($# >= 2)) || fail 2 '参数 -v 缺少版本号'
      version=$2
      shift 2
      ;;
    --wait-secs)
      (($# >= 2)) || fail 2 '参数 --wait-secs 缺少秒数'
      wait_secs=$2
      shift 2
      ;;
    --dry-run)
      dry_run=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail 2 "未知参数：$1"
      ;;
  esac
done

[[ $wait_secs =~ ^[0-9]+$ ]] || fail 2 '--wait-secs 必须是非负整数'
if [[ -n $version ]]; then
  export KIRO_RS_VERSION=$version
fi

if ! run_docker docker compose version >/dev/null; then
  fail 2 'docker compose 不可用'
fi

if [[ -z $compose_file ]]; then
  for candidate in docker-compose.yml docker-compose.database.yml; do
    if [[ -f $candidate ]]; then
      compose_file=$candidate
      break
    fi
  done
fi
[[ -n $compose_file && -f $compose_file ]] || fail 2 'Compose 文件不存在，请用 -f 指定'

compose=(docker compose -f "$compose_file")
if [[ -n $project_name ]]; then
  compose+=(-p "$project_name")
fi

services_output=$(run_compose config --services) || fail 2 '无法读取 Compose 服务列表'
services=()
while IFS= read -r service; do
  [[ -n $service ]] && services+=("$service")
done <<< "$services_output"

if [[ -n $app_service ]]; then
  found=false
  for service in "${services[@]+"${services[@]}"}"; do
    if [[ $service == "$app_service" ]]; then
      found=true
      break
    fi
  done
  [[ $found == true ]] || fail 2 "应用服务 '$app_service' 不存在"
else
  matches=()
  match_count=0
  for service in "${services[@]+"${services[@]}"}"; do
    case "$service" in
      kiro-rs|kiro-rs-app|app)
        matches+=("$service")
        match_count=$((match_count + 1))
        ;;
    esac
  done
  [[ $match_count -eq 1 ]] || fail 2 '无法唯一识别应用服务，请用 -s 指定'
  app_service=${matches[0]}
fi

if ! run_compose config -q; then
  fail 2 'Compose 配置校验失败'
fi

if [[ $dry_run == true ]]; then
  if [[ ${KIRO_R10_SKIP_PULL:-} != 1 && ${KIRO_R10_SKIP_PULL:-} != true ]]; then
    print_command "${compose[@]}" pull "$app_service"
  fi
  print_command "${compose[@]}" up -d --no-deps "$app_service"
  printf 'dry-run：跳过容器状态记录、升级执行、健康检查和依赖校验。\n'
  exit 0
fi

dependency_services=()
dependency_ids_before=()
for service in "${services[@]+"${services[@]}"}"; do
  [[ $service == "$app_service" ]] && continue
  dependency_services+=("$service")
  dependency_id=$(run_compose ps -q "$service") || fail 2 "无法读取依赖服务 '$service' 的容器 ID"
  dependency_ids_before+=("$dependency_id")
done

# KIRO_R10_SKIP_PULL 仅供集成测试跳过应用镜像拉取，正常升级不要设置。
if [[ ${KIRO_R10_SKIP_PULL:-} == 1 || ${KIRO_R10_SKIP_PULL:-} == true ]]; then
  printf '测试模式：跳过应用镜像拉取。\n'
else
  run_compose pull "$app_service"
fi
run_compose up -d --no-deps "$app_service"

if [[ -n ${KIRO_R10_TEST_HOOK_AFTER_UP:-} ]]; then
  print_command bash -c "$KIRO_R10_TEST_HOOK_AFTER_UP"
  bash -c "$KIRO_R10_TEST_HOOK_AFTER_UP"
fi

app_container_id=$(run_compose ps -q "$app_service")
start_time=$SECONDS
no_health_running_since=-1
while :; do
  health_status=
  container_status=
  if [[ -n $app_container_id ]]; then
    health_status=$(run_docker docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$app_container_id" || true)
    if [[ $health_status == none ]]; then
      container_status=$(run_docker docker inspect -f '{{.State.Status}}' "$app_container_id" || true)
    fi
  fi

  if [[ $health_status == healthy ]]; then
    break
  elif [[ $health_status == none && $container_status == running ]]; then
    if ((no_health_running_since < 0)); then
      no_health_running_since=$SECONDS
    elif ((SECONDS - no_health_running_since >= 5)); then
      break
    fi
  else
    no_health_running_since=-1
  fi

  if ((SECONDS - start_time >= wait_secs)); then
    run_compose logs --tail 50 "$app_service" || true
    fail 3 "应用服务 '$app_service' 在 ${wait_secs} 秒内未就绪"
  fi
  sleep 1
  app_container_id=$(run_compose ps -q "$app_service")
done

dependencies_changed=()
dependencies_changed_count=0
dependency_index=0
for service in "${dependency_services[@]+"${dependency_services[@]}"}"; do
  current_id=$(run_compose ps -q "$service") || fail 2 "无法读取依赖服务 '$service' 的容器 ID"
  dependency_id_before=${dependency_ids_before[$dependency_index]}
  if [[ $current_id != "$dependency_id_before" ]]; then
    dependencies_changed+=("$service: ${dependency_id_before:-(无)} -> ${current_id:-(无)}")
    dependencies_changed_count=$((dependencies_changed_count + 1))
  fi
  dependency_index=$((dependency_index + 1))
done
if ((dependencies_changed_count > 0)); then
  printf '\n========== WARN：依赖服务被重建 =========\n' >&2
  printf '%s\n' "${dependencies_changed[@]+"${dependencies_changed[@]}"}" >&2
  printf 'Redis 重启会触发外部池协调器 35 秒恢复窗口。\n' >&2
  exit 4
fi

image_id=$(run_docker docker inspect -f '{{.Image}}' "$app_container_id")
version_label=$(run_docker docker image inspect -f '{{if .Config.Labels}}{{index .Config.Labels "org.opencontainers.image.version"}}{{else}}<none>{{end}}' "$image_id")
revision_label=$(run_docker docker image inspect -f '{{if .Config.Labels}}{{index .Config.Labels "org.opencontainers.image.revision"}}{{else}}<none>{{end}}' "$image_id")
started_at=$(run_docker docker inspect -f '{{.State.StartedAt}}' "$app_container_id")
printf '升级完成：服务=%s 镜像版本=%s 镜像修订=%s 启动时间=%s\n' \
  "$app_service" "$version_label" "$revision_label" "$started_at"
