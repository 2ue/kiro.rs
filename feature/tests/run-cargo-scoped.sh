#!/usr/bin/env bash

set -euo pipefail
umask 077

usage() {
  cat <<'EOF'
Usage: feature/tests/run-cargo-scoped.sh <scope> -- <command> [args...]
       feature/tests/run-cargo-scoped.sh --reap-stale

Runs one validation command with an isolated, disposable Cargo target directory.
Every invocation reserves disk capacity atomically across Git worktrees, disables
incremental compilation, and removes its owned target after success, failure, or
a handled signal. After uncatchable owner termination, a later invocation keeps
the target while any recorded command-process-group member remains alive and
reclaims it only after the complete group exits.

Defaults:
  KIRO_VALIDATION_RESERVE_KIB=12582912   # 12 GiB per active batch
  KIRO_VALIDATION_MIN_FREE_KIB=20971520  # preserve a 20 GiB floor
  KIRO_VALIDATION_MAX_BUILD_KIB=12582912 # fail if one target exceeds 12 GiB
EOF
}

die() {
  printf '%s\n' "$1" >&2
  exit "${2:-70}"
}

read_first_line() {
  local file="$1"
  local value=""
  if [[ -f "$file" ]]; then
    IFS= read -r value <"$file" || true
  fi
  printf '%s' "$value"
}

process_start_identity() {
  local pid="$1"
  ps -p "$pid" -o lstart= 2>/dev/null | awk '{$1=$1};1'
}

owner_is_active() {
  local owner_pid="$1"
  local recorded_start="$2"
  local current_start=""

  [[ "$owner_pid" =~ ^[0-9]+$ ]] || return 1
  kill -0 "$owner_pid" 2>/dev/null || return 1

  current_start="$(process_start_identity "$owner_pid" || true)"
  # A live but temporarily uninspectable process is treated as active. This
  # favors leaving a stale record over deleting another process's artifacts.
  if [[ -z "$recorded_start" || -z "$current_start" ]]; then
    return 0
  fi
  [[ "$recorded_start" == "$current_start" ]]
}

# Return 0 when the owned validation command or any member of its process
# group is still alive, 1 when command metadata is complete and inactive, and
# 2 when metadata is incomplete or inconsistent. Callers must fail closed on 2.
owned_command_state() {
  local build_path="$1"
  local metadata_dir="$build_path/.command-owner"
  local command_pid command_start command_pgid current_start

  if [[ -e "$build_path/.command-starting" && ! -d "$metadata_dir" ]]; then
    return 2
  fi
  if [[ ! -e "$metadata_dir" ]]; then
    return 1
  fi
  [[ -d "$metadata_dir" && ! -L "$metadata_dir" ]] || return 2

  command_pid="$(read_first_line "$metadata_dir/pid")"
  command_start="$(read_first_line "$metadata_dir/start")"
  command_pgid="$(read_first_line "$metadata_dir/pgid")"
  [[ "$command_pid" =~ ^[0-9]+$ && "$command_pgid" =~ ^[0-9]+$ ]] || return 2
  (( command_pid > 1 && command_pgid > 1 )) || return 2
  [[ "$command_pid" == "$command_pgid" && -n "$command_start" ]] || return 2

  if kill -0 -- "-$command_pgid" 2>/dev/null; then
    if kill -0 "$command_pid" 2>/dev/null; then
      current_start="$(process_start_identity "$command_pid" || true)"
      if [[ -n "$current_start" && "$current_start" != "$command_start" ]]; then
        return 2
      fi
    fi
    return 0
  fi

  if kill -0 "$command_pid" 2>/dev/null; then
    current_start="$(process_start_identity "$command_pid" || true)"
    if [[ -z "$current_start" || "$current_start" == "$command_start" ]]; then
      return 0
    fi
    return 2
  fi
  return 1
}

repo_root="$(git rev-parse --show-toplevel)"
target_root="${KIRO_VALIDATION_TARGET_ROOT:-$repo_root/target}"
mkdir -p "$target_root"
target_root="$(cd "$target_root" && pwd -P)"

git_common_dir="$(git rev-parse --git-common-dir)"
if [[ "$git_common_dir" != /* ]]; then
  git_common_dir="$repo_root/$git_common_dir"
fi
git_common_dir="$(cd "$git_common_dir" && pwd -P)"
state_dir="${KIRO_VALIDATION_STATE_DIR:-$git_common_dir/kiro-validation-build-state}"
mkdir -p "$state_dir"
state_dir="$(cd "$state_dir" && pwd -P)"

owner_pid="$$"
owner_start="$(process_start_identity "$owner_pid" || true)"
[[ -n "$owner_start" ]] || die "could not determine validation owner start identity" 70
created_epoch="$(date +%s)"

if filesystem_id="$(stat -f '%d' "$target_root" 2>/dev/null)"; then
  :
elif filesystem_id="$(stat -c '%d' "$target_root" 2>/dev/null)"; then
  :
else
  die "could not determine validation target filesystem identity" 70
fi

lock_file="$state_dir/.mutation.lock"
lock_owner_file="$state_dir/.mutation-lock-owner"
lock_backend=""
lock_held=0
lock_timeout_secs="${KIRO_VALIDATION_LOCK_TIMEOUT_SECS:-15}"
[[ "$lock_timeout_secs" =~ ^[1-9][0-9]*$ ]] || \
  die "KIRO_VALIDATION_LOCK_TIMEOUT_SECS must be a positive integer" 64

release_state_lock() {
  (( lock_held == 1 )) || return 0

  if [[ "$lock_backend" == "flock" ]]; then
    rm -f -- "$lock_owner_file"
    flock -u 9 || true
    exec 9>&-
  elif [[ "$lock_backend" == "shlock" ]]; then
    local recorded_pid recorded_start
    recorded_pid="$(read_first_line "$lock_file")"
    recorded_start="$(read_first_line "$lock_owner_file")"
    if [[ "$recorded_pid" == "$owner_pid" && "$recorded_start" == "$owner_start" ]]; then
      rm -f -- "$lock_owner_file"
      rm -f -- "$lock_file"
    else
      printf 'validation reservation lock ownership changed; refusing unlock\n' >&2
      lock_held=0
      return 1
    fi
  fi
  lock_held=0
}

acquire_state_lock() {
  local waited=0
  local holder_start=""

  if command -v flock >/dev/null 2>&1; then
    lock_backend="flock"
    exec 9>"$lock_file"
    if ! flock -w "$lock_timeout_secs" 9; then
      exec 9>&-
      printf 'validation reservation lock timed out after %ss\n' \
        "$lock_timeout_secs" >&2
      return 1
    fi
    printf '%s\n%s\n%s\n' "$owner_pid" "$owner_start" "$created_epoch" \
      >"$lock_owner_file"
    lock_held=1
    return 0
  fi

  if ! command -v shlock >/dev/null 2>&1; then
    printf 'validation reservation requires flock or shlock for atomic admission\n' >&2
    return 1
  fi

  lock_backend="shlock"
  while (( waited < lock_timeout_secs * 10 )); do
    if shlock -f "$lock_file" -p "$owner_pid"; then
      printf '%s\n' "$owner_start" >"$lock_owner_file"
      lock_held=1
      return 0
    fi

    # shlock atomically reclaims dead PIDs. If a PID has been reused, do not
    # remove the lock by pathname: that could race a new owner. Fail closed so
    # an operator can inspect the tiny state file without risking live data.
    holder_start="$(read_first_line "$lock_owner_file")"
    if [[ -n "$holder_start" ]]; then
      local holder_pid current_holder_start
      holder_pid="$(read_first_line "$lock_file")"
      current_holder_start="$(process_start_identity "$holder_pid" || true)"
      if [[ -n "$current_holder_start" && "$holder_start" != "$current_holder_start" ]]; then
        printf 'validation reservation lock PID was reused; refusing unsafe reclaim\n' >&2
        return 1
      fi
    fi
    sleep 0.1
    ((waited += 1))
  done

  printf 'validation reservation lock timed out after %ss\n' \
    "$lock_timeout_secs" >&2
  return 1
}

available_kib_now() {
  if [[ "${KIRO_VALIDATION_TEST_MODE:-0}" == "1" && \
        -n "${KIRO_VALIDATION_TEST_AVAILABLE_KIB:-}" ]]; then
    printf '%s\n' "$KIRO_VALIDATION_TEST_AVAILABLE_KIB"
    return 0
  fi
  df -Pk "$target_root" | awk 'END {print $4}'
}

safe_remove_owned_target() {
  local build_path="$1"
  local expected_pid="$2"
  local expected_start="$3"
  local expected_reservation_id="$4"
  local build_name marker_pid marker_start marker_reservation command_state

  [[ -n "$build_path" ]] || return 1
  if [[ ! -e "$build_path" ]]; then
    return 0
  fi
  [[ -d "$build_path" && ! -L "$build_path" ]] || return 1

  build_name="$(basename "$build_path")"
  [[ "$build_name" == .validation-build-* ]] || return 1
  [[ "$build_name" == *".pid-$expected_pid."* ]] || return 1

  marker_pid="$(read_first_line "$build_path/.owner-pid")"
  marker_start="$(read_first_line "$build_path/.owner-start")"
  marker_reservation="$(read_first_line "$build_path/.owner-reservation-id")"
  [[ "$marker_pid" == "$expected_pid" ]] || return 1
  [[ -n "$expected_start" && "$marker_start" == "$expected_start" ]] || return 1
  if [[ -n "$expected_reservation_id" ]]; then
    [[ "$marker_reservation" == "$expected_reservation_id" ]] || return 1
  fi

  if owned_command_state "$build_path"; then
    return 1
  else
    command_state=$?
  fi
  (( command_state == 1 )) || return 1

  rm -rf -- "$build_path"
  [[ ! -e "$build_path" ]]
}

local_reap_active=0
local_reap_removed=0
local_reap_failed=0

reap_local_stale_builds() {
  local build_path build_name marker_pid marker_start marker_reservation build_kib command_state

  local_reap_active=0
  local_reap_removed=0
  local_reap_failed=0
  shopt -s nullglob
  for build_path in "$target_root"/.validation-build-*; do
    if [[ ! -e "$build_path" ]]; then
      continue
    fi
    [[ -d "$build_path" && ! -L "$build_path" ]] || {
      ((local_reap_failed += 1))
      continue
    }
    build_name="$(basename "$build_path")"
    marker_pid="$(read_first_line "$build_path/.owner-pid")"
    marker_start="$(read_first_line "$build_path/.owner-start")"
    marker_reservation="$(read_first_line "$build_path/.owner-reservation-id")"

    if [[ ! "$marker_pid" =~ ^[0-9]+$ || -z "$marker_start" || \
          "$build_name" != *".pid-$marker_pid."* ]]; then
      # Another live wrapper may have completed its owned cleanup between the
      # glob and marker reads. A path that is now absent is already safe.
      if [[ ! -e "$build_path" ]]; then
        continue
      fi
      # mktemp makes the directory before writing its owner markers. During
      # that tiny window, the PID embedded by this wrapper is enough to prove
      # that deletion is unsafe; preserve it as active until full identity is
      # visible. A dead PID with incomplete markers remains unknown and blocks.
      if [[ "$build_name" =~ \.pid-([0-9]+)\. ]] && \
         kill -0 "${BASH_REMATCH[1]}" 2>/dev/null; then
        ((local_reap_active += 1))
        continue
      fi
      printf 'validation-build-reap classification=unknown-owned-target removed=false\n' >&2
      ((local_reap_failed += 1))
      continue
    fi
    if owner_is_active "$marker_pid" "$marker_start"; then
      ((local_reap_active += 1))
      continue
    fi

    if owned_command_state "$build_path"; then
      ((local_reap_active += 1))
      continue
    else
      command_state=$?
    fi
    if (( command_state != 1 )); then
      printf 'validation-build-reap classification=unknown-command-owner removed=false\n' >&2
      ((local_reap_failed += 1))
      continue
    fi

    build_kib="$(du -sk "$build_path" 2>/dev/null | awk '{print $1}' || true)"
    build_kib="${build_kib:-0}"
    if safe_remove_owned_target "$build_path" "$marker_pid" "$marker_start" \
      "$marker_reservation"; then
      ((local_reap_removed += 1))
      printf 'validation-build-reap classification=stale-owned-target size_kib=%s removed=true\n' \
        "$build_kib" >&2
    else
      ((local_reap_failed += 1))
      printf 'validation-build-reap classification=stale-owned-target size_kib=%s removed=false\n' \
        "$build_kib" >&2
    fi
  done
  shopt -u nullglob
}

reservation_reap_active=0
reservation_reap_removed=0
reservation_reap_failed=0
active_reserved_kib=0

reap_reservations_locked() {
  local record record_name record_pid record_start record_created record_reserved
  local record_fs record_target record_scope record_id command_state

  (( lock_held == 1 )) || return 1
  reservation_reap_active=0
  reservation_reap_removed=0
  reservation_reap_failed=0
  active_reserved_kib=0

  shopt -s nullglob
  for record in "$state_dir"/.reservation-*; do
    [[ -d "$record" && ! -L "$record" ]] || {
      ((reservation_reap_failed += 1))
      continue
    }
    record_name="$(basename "$record")"
    [[ "$record_name" != .reservation-tmp-* ]] || {
      ((reservation_reap_failed += 1))
      continue
    }

    record_pid="$(read_first_line "$record/owner_pid")"
    record_start="$(read_first_line "$record/owner_start")"
    record_created="$(read_first_line "$record/created_epoch")"
    record_reserved="$(read_first_line "$record/reserved_kib")"
    record_fs="$(read_first_line "$record/filesystem_id")"
    record_target="$(read_first_line "$record/target_dir")"
    record_scope="$(read_first_line "$record/scope")"
    record_id="$(read_first_line "$record/reservation_id")"

    if [[ ! "$record_pid" =~ ^[0-9]+$ || -z "$record_start" || \
          ! "$record_created" =~ ^[0-9]+$ || \
          ! "$record_reserved" =~ ^[0-9]+$ || -z "$record_fs" || \
          -z "$record_target" || \
          ! "$record_scope" =~ ^[a-z0-9][a-z0-9._-]{0,63}$ || \
          ! "$record_id" =~ ^[a-zA-Z0-9._-]+$ || \
          "$record_name" != ".reservation-$record_id" ]]; then
      printf 'validation-reservation-reap classification=malformed removed=false\n' >&2
      ((reservation_reap_failed += 1))
      continue
    fi

    if owner_is_active "$record_pid" "$record_start"; then
      ((reservation_reap_active += 1))
      if [[ "$record_fs" == "$filesystem_id" ]]; then
        ((active_reserved_kib += record_reserved))
      fi
      continue
    fi


    if [[ -d "$record_target" && ! -L "$record_target" ]]; then
      if owned_command_state "$record_target"; then
        ((reservation_reap_active += 1))
        if [[ "$record_fs" == "$filesystem_id" ]]; then
          ((active_reserved_kib += record_reserved))
        fi
        continue
      else
        command_state=$?
      fi
      if (( command_state != 1 )); then
        printf 'validation-reservation-reap classification=unknown-command-owner removed=false\n' >&2
        ((reservation_reap_failed += 1))
        continue
      fi
    fi

    if safe_remove_owned_target "$record_target" "$record_pid" "$record_start" \
      "$record_id" && rm -rf -- "$record" && [[ ! -e "$record" ]]; then
      ((reservation_reap_removed += 1))
      printf 'validation-reservation-reap classification=stale-owned scope=%s reserved_kib=%s removed=true\n' \
        "$record_scope" "$record_reserved" >&2
    else
      ((reservation_reap_failed += 1))
      printf 'validation-reservation-reap classification=stale-owned scope=%s reserved_kib=%s removed=false\n' \
        "$record_scope" "$record_reserved" >&2
    fi
  done
  shopt -u nullglob
}

remove_own_reservation_locked() {
  (( lock_held == 1 )) || return 1
  [[ -n "${reservation_dir:-}" && -d "$reservation_dir" && \
     ! -L "$reservation_dir" ]] || return 1
  [[ "$(read_first_line "$reservation_dir/reservation_id")" == "$reservation_id" ]] || \
    return 1
  rm -rf -- "$reservation_dir"
  [[ ! -e "$reservation_dir" ]]
}

if [[ $# -eq 1 && "$1" == "--reap-stale" ]]; then
  reap_local_stale_builds
  if ! acquire_state_lock; then
    exit 75
  fi
  reap_reservations_locked || reservation_reap_failed=1
  release_state_lock || reservation_reap_failed=1
  printf 'validation-build-reap-summary active=%s removed=%s failed=%s reservation_active=%s reservation_removed=%s reservation_failed=%s\n' \
    "$local_reap_active" "$local_reap_removed" "$local_reap_failed" \
    "$reservation_reap_active" "$reservation_reap_removed" \
    "$reservation_reap_failed" >&2
  if (( local_reap_failed + reservation_reap_failed > 0 )); then
    exit 73
  fi
  if (( local_reap_active + reservation_reap_active > 0 )); then
    exit 75
  fi
  exit 0
fi

if [[ $# -lt 3 || "$2" != "--" ]]; then
  usage >&2
  exit 64
fi

scope="$1"
shift 2
if [[ ! "$scope" =~ ^[a-z0-9][a-z0-9._-]{0,63}$ ]]; then
  printf 'invalid validation scope: %s\n' "$scope" >&2
  exit 64
fi

min_free_kib="${KIRO_VALIDATION_MIN_FREE_KIB:-20971520}"
reserve_kib="${KIRO_VALIDATION_RESERVE_KIB:-12582912}"
max_build_kib="${KIRO_VALIDATION_MAX_BUILD_KIB:-12582912}"
for threshold in "$min_free_kib" "$reserve_kib" "$max_build_kib"; do
  [[ "$threshold" =~ ^[0-9]+$ ]] || \
    die "validation disk thresholds must be non-negative KiB integers" 64
done
(( reserve_kib > 0 )) || die "KIRO_VALIDATION_RESERVE_KIB must be greater than zero" 64

if [[ "${KIRO_VALIDATION_TEST_MODE:-0}" == "1" && \
      -n "${KIRO_VALIDATION_TEST_AVAILABLE_KIB:-}" && \
      ! "$KIRO_VALIDATION_TEST_AVAILABLE_KIB" =~ ^[0-9]+$ ]]; then
  die "KIRO_VALIDATION_TEST_AVAILABLE_KIB must be a non-negative KiB integer" 64
fi

reap_local_stale_builds
if (( local_reap_failed > 0 )); then
  printf 'refusing validation scope %s: local stale target cleanup failed\n' \
    "$scope" >&2
  exit 73
fi

reservation_id="$created_epoch-$owner_pid-${RANDOM}${RANDOM}"
build_dir=""
reservation_dir="$state_dir/.reservation-$reservation_id"
record_tmp=""
reservation_created=0
command_pid=0
command_pgid=0
received_signal_status=0
cleanup_started=0

command_tree_is_active() {
  if (( command_pgid > 1 )) && kill -0 -- "-$command_pgid" 2>/dev/null; then
    return 0
  fi
  if (( command_pid > 1 )) && kill -0 "$command_pid" 2>/dev/null; then
    return 0
  fi
  return 1
}

signal_command_tree() {
  local signal_name="$1"
  if (( command_pgid > 1 )); then
    kill -"$signal_name" -- "-$command_pgid" 2>/dev/null || true
  elif (( command_pid > 1 )); then
    kill -"$signal_name" "$command_pid" 2>/dev/null || true
  fi
}

stop_command_tree() {
  local attempt
  command_tree_is_active || return 0
  signal_command_tree TERM
  for attempt in $(seq 1 50); do
    command_tree_is_active || break
    sleep 0.1
  done
  if command_tree_is_active; then
    signal_command_tree KILL
  fi
  if (( command_pid > 1 )); then
    wait "$command_pid" 2>/dev/null || true
  fi
}

cleanup() {
  local command_status=$?
  local build_kib=0
  local removed=true
  local remaining_kib="unknown"
  local reservation_released=false

  (( cleanup_started == 0 )) || exit "$command_status"
  cleanup_started=1
  trap - EXIT
  trap '' INT TERM HUP

  stop_command_tree
  if ! command_tree_is_active && [[ -n "$build_dir" && -d "$build_dir" ]]; then
    rm -rf -- "$build_dir"/.command-owner-tmp-* 2>/dev/null || true
    rm -f -- "$build_dir/.command-starting" 2>/dev/null || true
  fi

  if [[ -n "$build_dir" && -d "$build_dir" ]]; then
    build_kib="$(du -sk "$build_dir" 2>/dev/null | awk '{print $1}' || true)"
    build_kib="${build_kib:-0}"
    if ! safe_remove_owned_target "$build_dir" "$owner_pid" "$owner_start" \
      "$reservation_id"; then
      removed=false
      command_status=73
      printf 'validation build cleanup refused: ownership could not be proven scope=%s\n' \
        "$scope" >&2
    fi
  fi
  if [[ -n "$build_dir" && -e "$build_dir" ]]; then
    removed=false
    command_status=73
  fi

  # A signal can arrive between the atomic rename and the shell assignment
  # that marks it complete. Re-discover only this invocation's exact record.
  if (( reservation_created == 0 )) && [[ -d "$reservation_dir" && \
       ! -L "$reservation_dir" ]] && \
       [[ "$(read_first_line "$reservation_dir/reservation_id")" == "$reservation_id" ]]; then
    reservation_created=1
  fi

  if [[ -n "$record_tmp" && -d "$record_tmp" && ! -L "$record_tmp" && \
        "$(basename "$record_tmp")" == .reservation-tmp-"$reservation_id".* ]]; then
    rm -rf -- "$record_tmp" || command_status=73
  fi
  record_tmp=""

  if (( reservation_created == 1 )); then
    if (( lock_held == 0 )) && ! acquire_state_lock; then
      command_status=73
    fi
    if (( lock_held == 1 )); then
      if [[ "$removed" == true ]]; then
        if ! remove_own_reservation_locked; then
          printf 'validation reservation release failed scope=%s\n' "$scope" >&2
          command_status=73
        else
          reservation_created=0
          reservation_released=true
        fi
      else
        printf 'validation reservation retained because owned target remains scope=%s\n' \
          "$scope" >&2
      fi
      release_state_lock || command_status=73
    fi
  elif (( lock_held == 1 )); then
    release_state_lock || command_status=73
  fi

  if (( reservation_created == 0 )) && [[ ! -e "$reservation_dir" ]]; then
    reservation_released=true
  fi

  remaining_kib="$(available_kib_now 2>/dev/null || true)"
  remaining_kib="${remaining_kib:-unknown}"
  printf 'validation-build-cleanup scope=%s size_kib=%s available_kib=%s removed=%s reservation_released=%s\n' \
    "$scope" "$build_kib" "$remaining_kib" "$removed" \
    "$reservation_released" >&2

  if [[ "$build_kib" =~ ^[0-9]+$ ]] && (( build_kib > max_build_kib )); then
    printf 'validation scope %s exceeded build budget: size=%s KiB max=%s KiB\n' \
      "$scope" "$build_kib" "$max_build_kib" >&2
    if (( command_status == 0 )); then
      command_status=74
    fi
  fi
  exit "$command_status"
}

forward_signal() {
  local signal_name="$1"
  local signal_status="$2"
  local attempt

  received_signal_status="$signal_status"
  trap '' INT TERM HUP
  if (( command_pid == 0 )); then
    exit "$signal_status"
  fi

  signal_command_tree "$signal_name"
  for attempt in $(seq 1 50); do
    command_tree_is_active || break
    sleep 0.1
  done
  if command_tree_is_active; then
    signal_command_tree KILL
  fi
}

trap cleanup EXIT
trap 'forward_signal INT 130' INT
trap 'forward_signal TERM 143' TERM
trap 'forward_signal HUP 129' HUP

build_dir="$(mktemp -d "$target_root/.validation-build-$scope.pid-$owner_pid.XXXXXX")"
printf '%s\n' "$owner_pid" >"$build_dir/.owner-pid"
printf '%s\n' "$owner_start" >"$build_dir/.owner-start"
printf '%s\n' "$reservation_id" >"$build_dir/.owner-reservation-id"
printf '%s\n' "$created_epoch" >"$build_dir/.owner-created-epoch"

if ! acquire_state_lock; then
  exit 75
fi
reap_reservations_locked || reservation_reap_failed=1
if (( reservation_reap_failed > 0 )); then
  printf 'refusing validation scope %s: reservation cleanup failed\n' "$scope" >&2
  exit 73
fi

available_kib="$(available_kib_now || true)"
if [[ -z "$available_kib" || ! "$available_kib" =~ ^[0-9]+$ ]]; then
  printf 'could not determine available disk space for validation target\n' >&2
  exit 70
fi
required_kib=$((min_free_kib + active_reserved_kib + reserve_kib))
if (( available_kib < required_kib )); then
  printf 'validation-build-admission scope=%s admitted=false available_kib=%s floor_kib=%s active_reserved_kib=%s requested_kib=%s\n' \
    "$scope" "$available_kib" "$min_free_kib" "$active_reserved_kib" \
    "$reserve_kib" >&2
  exit 75
fi

record_tmp="$(mktemp -d "$state_dir/.reservation-tmp-$reservation_id.XXXXXX")"
printf '%s\n' 1 >"$record_tmp/record_version"
printf '%s\n' "$reservation_id" >"$record_tmp/reservation_id"
printf '%s\n' "$owner_pid" >"$record_tmp/owner_pid"
printf '%s\n' "$owner_start" >"$record_tmp/owner_start"
printf '%s\n' "$created_epoch" >"$record_tmp/created_epoch"
printf '%s\n' "$reserve_kib" >"$record_tmp/reserved_kib"
printf '%s\n' "$filesystem_id" >"$record_tmp/filesystem_id"
printf '%s\n' "$build_dir" >"$record_tmp/target_dir"
printf '%s\n' "$scope" >"$record_tmp/scope"
mv "$record_tmp" "$reservation_dir"
record_tmp=""
reservation_created=1
release_state_lock

export CARGO_TARGET_DIR="$build_dir"
export CARGO_INCREMENTAL=0
printf 'validation-build-admission scope=%s admitted=true available_kib=%s floor_kib=%s active_reserved_kib=%s requested_kib=%s incremental=0\n' \
  "$scope" "$available_kib" "$min_free_kib" "$active_reserved_kib" \
  "$reserve_kib" >&2

# Job control gives the validation command its own process group so a handled
# signal can stop Cargo and its compiler children before the target is removed.
# The child publishes its own PID/start/PGID before exec. If this owner is
# uncatchably terminated, a later reaper can still detect a surviving process
# group and must preserve the target until every member exits.
printf '%s\n' "$created_epoch" >"$build_dir/.command-starting"
set -m
/bin/bash -euo pipefail -c '
  build_dir="$1"
  shift
  child_pid="$$"
  child_start="$(ps -p "$child_pid" -o lstart= 2>/dev/null | awk '\''{$1=$1};1'\'')"
  child_pgid="$(ps -p "$child_pid" -o pgid= 2>/dev/null | tr -d " ")"
  if [[ -z "$child_start" || ! "$child_pgid" =~ ^[0-9]+$ || \
        "$child_pgid" != "$child_pid" ]]; then
    printf "validation command process-group identity is unavailable\n" >&2
    exit 70
  fi
  command_metadata_tmp="$build_dir/.command-owner-tmp-$child_pid"
  mkdir "$command_metadata_tmp"
  printf "%s\n" "$child_pid" >"$command_metadata_tmp/pid"
  printf "%s\n" "$child_start" >"$command_metadata_tmp/start"
  printf "%s\n" "$child_pgid" >"$command_metadata_tmp/pgid"
  mv "$command_metadata_tmp" "$build_dir/.command-owner"
  rm -f -- "$build_dir/.command-starting"
  exec "$@"
' validation-command "$build_dir" "$@" &
command_pid=$!
command_pgid="$(ps -p "$command_pid" -o pgid= 2>/dev/null | tr -d ' ' || true)"
set +m

if [[ ! "$command_pgid" =~ ^[0-9]+$ || "$command_pgid" != "$command_pid" ]]; then
  printf 'validation command did not receive an isolated process group\n' >&2
  stop_command_tree
  exit 70
fi

set +e
wait "$command_pid"
command_status=$?
set -e
command_pid=0

# A validation command must not leave detached work in its owned process
# group. cleanup() will terminate any surviving descendants before removal.

if (( received_signal_status > 0 )); then
  exit "$received_signal_status"
fi
exit "$command_status"
