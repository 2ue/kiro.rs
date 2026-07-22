# 2026-07-23 最终发布门禁复测证据

Date: 2026-07-23

Status: `release-gate-pass / publish-pending`

## 范围

本证据文件记录当前 v0.0.109 dirty-tree 候选在发版前的最终统一候选门禁结果。它补充而不替代 [2026-07-22 当前工作树回归复测证据](final-regression-rerun-20260722.md)：2026-07-22 文件覆盖真实 Claude Code CLI 长会话、thinking/output_config、body/reasoning、scheduler/Redis/external takeover 等用户点名问题；本文件覆盖最后一轮统一 Rust C0、非 Cargo 合同、文档链接、构建产物库存和磁盘安全门禁。

执行边界：

- 未执行 Docker 动态验证；这是用户本轮明确要求的豁免项，不能记为 Docker pass。
- 未停止、重启或压测既有 `127.0.0.1:9022` 服务。
- 未读取或暂存 `kiro_idc_users*.txt`。
- 所有 Cargo 命令均通过 `feature/tests/run-cargo-scoped.sh` 执行。
- 最终二进制仅复制到仓库外冻结候选目录；scoped Cargo target 已由 wrapper 删除。

## 当前源码身份

- Git HEAD at validation start: `401473ca1649997bdeccf4468e3add1bdb187248` (`401473c`)。
- Working tree: dirty, current remediation/release candidate。
- Rust toolchain: `cargo +1.92.0`。

## 冻结候选二进制

```text
925525419cd48b460217df2568891a40287da0c44d2bf921a38b103c047775ee  kiro-rs
90babda7388aa93854cbbdb81c132cc436c07f46b0ea22973531b0a7ffb3aff1  kiro_loadtest
```

仓库外候选目录：

```text
/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-final-candidate-20260723.6kcM3J
```

## Rust C0 / release build gate

执行方式：

```text
feature/tests/run-cargo-scoped.sh final-c0-release-20260723-r4 -- bash -lc '
  cargo +1.92.0 fmt --all -- --check
  cargo +1.92.0 test --all-targets
  cargo +1.92.0 build --release --bins
  install -m 755 "$CARGO_TARGET_DIR/release/kiro-rs" "$KIRO_FROZEN_BINARY"
  install -m 755 "$CARGO_TARGET_DIR/release/kiro_loadtest" "$KIRO_FROZEN_LOADTEST"
'
```

结果：

- `cargo +1.92.0 fmt --all -- --check`: pass。
- `cargo +1.92.0 test --all-targets`: pass。
  - main binary test tree: `1750 passed / 0 failed / 6 ignored`。
  - `kiro_loadtest`: `31 passed / 0 failed`。
- `cargo +1.92.0 build --release --bins`: pass。
- Release build elapsed inside Cargo log: `6m54s`。
- Scoped target cleanup: `validation-build-cleanup scope=final-c0-release-20260723-r4 size_kib=2516216 available_kib=86414424 removed=true reservation_released=true`。
- Cargo log SHA-256: `4f7faabce7cfcfbdb12ed508d39abe9732527d01a1cf7dd9c51a7786e7f7911d`。

Red/green note:

- Earlier r2 full run exposed one timing-sensitive test failure in `anthropic::body_processing::tests::slow_remote_fetch_is_cancelled_and_followed_by_normal_recovery` because the test used a deadline short enough to expire before the fake server necessarily received `/slow`.
- The test fixture was made deterministic by adding a `/slow-body` endpoint that returns headers before delaying the body. Focused exact rerun passed, r3 then failed only on rustfmt, and r4 passed the full C0/release gate above.
- Product runtime behavior was not changed for this red/green; the change makes cancellation recovery evidence deterministic.

## 非 Cargo 合同与文档门禁

```text
node feature/tests/check-feature-docs.mjs
```

Result:

```text
PASS: 47 issue documents satisfy the section contract; 115 relative links resolve.
```

```text
node --test feature/tests/*.test.mjs
```

Result:

```text
tests=283
pass=261
fail=0
cancelled=0
skipped=22
todo=0
duration_ms=60753.573958
```

The 22 skipped tests are explicit live-fixture opt-ins or safety-contract skips; they are not counted as product passes.

```text
git diff --check
```

Result: pass.

## 构建产物与磁盘门禁

Initial final inventory immediately after C0 r4 still found a regenerated root `target/`:

```text
build-artifact-inventory version=2 mode=read-only targets=1 reservations=0 target_processes=1 blockers=2
target id=d61e6fde19e5 location=<repo>/target classification=unmanaged-repo-cargo-target size_kib=940528
target-process target_id=d61e6fde19e5 pid=84264 classification=kiro-runtime
release-gate result=fail
```

Inspection showed the root target contained only reproducible local artifacts:

```text
target/.rustc_info.json
target/flycheck0/...
target/debug/...
```

No Cargo/rustc validation process was running. The existing `127.0.0.1:9022` process was not stopped. The repository root `target/` tree was deleted as disposable build output, then inventory passed:

```text
build-artifact-inventory version=2 mode=read-only targets=0 reservations=0 target_processes=0 blockers=0
process-inspection complete=true ps=complete open_files=lsof-cwd-txt
temp-scan roots=1 entries=4169 unreadable=0 truncated=false strategy=bounded-known-prefixes
docker status=inspected-read-only cleanup=manual-only hint=docker-system-df-and-builder-prune-require-manual-review
release-gate result=pass
```

Docker inventory timing out remains a read-only inventory limitation and does not imply Docker cleanup or Docker validation. Docker dynamic validation remains explicitly waived for this release by user instruction.

## 发布判定

当前候选满足本轮已执行的发布前门禁：

- Rust scoped C0/release build gate: pass。
- Final binary hashes recorded: pass。
- Feature docs/link contract: pass。
- Node source/runner/protocol/scheduler/prompt/thinking/fault-domain contracts: pass。
- `git diff --check`: pass。
- Build artifact inventory: pass after deleting only disposable repository `target/` output。

发布前仍需执行标准 Git 发布步骤：读取发布流程约束、检查远端/tag、提交当前工作、按项目版本规则创建下一个 tag 并推送。

## 明示限制

- 本文件不是 Docker pass。
- 本文件不是生产 9022 验证报告。
- 本文件不替代 2026-07-22 的真实 Claude CLI 和 scheduler/Redis/external takeover 复测；需要读二者合并判断当前候选。
- 未提供或执行需要外部真实 Kiro upstream 凭据的新增高压用例；当前用户要求的主要真实交互证据来自隔离 Claude Code CLI 对临时候选服务的真实调用。
