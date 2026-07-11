#!/usr/bin/env bash
set -euo pipefail

base_sha="${1:-}"
head_sha="${2:-HEAD}"

if [[ -z "$base_sha" || "$base_sha" =~ ^0+$ ]] || ! git cat-file -e "${base_sha}^{commit}" 2>/dev/null; then
  if git rev-parse --verify "${head_sha}^" >/dev/null 2>&1; then
    base_sha="${head_sha}^"
  else
    base_sha="$(git hash-object -t tree /dev/null)"
  fi
fi

git diff --check "$base_sha" "$head_sha"
