# Lightweight Contract Regression 2026-07-21

Role: 记录本轮轻量合同主体不启动 Docker、不运行 Cargo、不启动真实服务的回归结果；后续 RedisStore role guard 的 scoped Cargo check 单独列为补证。

Status: `contract-pass / release-gate-still-no-go`

## 目的

本轮在推进 E05 non-Docker runner 后，追加执行一组低产物合同，确认文档、UI 字段、prompt 开关、runner 路径、安全信号和构建 wrapper 清理没有回归。该批不替代动态服务、真实 Claude Code CLI/native upstream、UI browser/build、旧版本升级或 final release gate。

## 执行约束

- 未启动 Docker。
- 轻量合同主体未运行 Cargo；RedisStore role guard 后续补证单独通过 `run-cargo-scoped.sh` 执行 `cargo +1.92.0 check --bin kiro-rs`，并立即清理 scoped target。
- 未启动 kiro.rs 服务。
- 未触碰 `127.0.0.1:9022`。
- 未读取或暂存 `kiro_idc_users*.txt`。
- `run-cargo-scoped-lifecycle.test.mjs` 只测试 wrapper 清理协议，不调用 Cargo；产生的临时 scoped target 已清理。

## 结果

### 1. 文档、UI 和 prompt 合同

命令：

```bash
node feature/tests/check-feature-docs.mjs
node feature/tests/cost-format-contract.mjs
node feature/tests/mcp-attempt-channel-contract.mjs
node feature/tests/request-api-key-id-contract.mjs
node feature/tests/prompt-control-independence.mjs
node feature/tests/prompt-default-parity.mjs
```

结果：

- feature issue 文档结构：47/47 issue documents pass；首次记录为 102 relative links resolve；本轮补证后复跑为 104 relative links resolve。
- cost format：`ui + admin-ui` PASS。
- MCP attempt channel：两套 UI PASS。
- request API key ID：`ui + admin-ui` PASS。
- prompt control independence：2 UI surfaces PASS。
- prompt default parity：Rust、UI、Admin UI PASS，且未发现 internal transcript fingerprints。

### 2. Runner input/signal safety contracts

命令：

```bash
node --test \
  feature/tests/e03-real-two-process-scheduler.contract.test.mjs \
  feature/tests/run-token-refresh-cluster-validation.contract.test.mjs \
  feature/tests/run-multi-instance-redis-coordination-validation.contract.test.mjs

node --test \
  feature/tests/run-scheduler-redis-chaos-validation.contract.test.mjs \
  feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs

node --test \
  feature/tests/thinking-effort-kiro-wire-contract.test.mjs \
  feature/tests/thinking-effort-kiro-wire-signal.test.mjs

node --test \
  feature/tests/thinking-effort-claude-cli-capture-signal.test.mjs \
  feature/tests/bare-invoke-claude-cli-signal.test.mjs
```

结果：

- E03 + token-refresh cluster + multi-instance coordination contracts：71 tests total；70 pass、1 skipped。
- 修改旧测试标题后单独复跑 E03 contract：52/52 pass。
- Scheduler Redis chaos + business/observability fault-domain contracts：初始 65 tests total，44 pass、21 skipped；业务/观测 Redis 生产源码合同补证后，合批为 70 tests total，49 pass、21 skipped、0 failed；RedisStore production role guard 合同补证后，合批为 71 tests total，50 pass、21 skipped、0 failed；追加主/观测 Redis 路径隔离合同后，合批为 74 tests total，53 pass、21 skipped、0 failed。
- Thinking effort Kiro wire contract/signal：45/45 pass。
- Claude CLI thinking capture signal + bare invoke signal：5/5 pass。

Skip 说明：

- skipped cases 均为需要显式 live Redis URL、caller-confirmed non-empty Redis DB 或 live signal URL 的 opt-in 场景。
- skipped cases 未被计为产品 pass；它们只是本轮无 live fixture 时的明确不执行。

### 3. Scoped build wrapper lifecycle

命令：

```bash
node --test feature/tests/run-cargo-scoped-lifecycle.test.mjs
```

结果：

- 21/21 pass。
- 覆盖 success、business failure、HUP/INT/TERM、SIGKILL owner stale reap、unknown owner preserve/block。
- 该测试没有调用 Cargo。

### 4. Disk / inventory

命令：

```bash
df -h .
du -sh target
node feature/tests/inventory-build-artifacts.mjs --gate
```

结果：

- Data volume available：约 `77 GiB`。
- root `target/`：初始约 `709 MiB`。
- 初始 inventory 为预期 fail：
  - `targets=1`。
  - `reservations=0`。
  - `target_processes=1`。
- blocker 为 `<repo>/target`，约 `725148-725612 KiB`，当时由 PID `84264` 的 `kiro-runtime` 引用。

判断：

- 本轮轻量合同没有新增 scoped target/reservation 残留。
- 该时点 release inventory 不能通过；后续只删除无引用的可再生产物后，inventory 已在第 5 节补证为 pass。
- 第二次 inventory 的 Docker read-only inspection 超时；按当前规则这仍是 manual-only hint，不授权自动清理 Docker。

### 5. Runner child environment isolation and post-cleanup rerun

命令：

```bash
node feature/tests/check-feature-docs.mjs

node --test \
  feature/tests/runtime-validation-paths.test.mjs \
  feature/tests/frozen-load-chaos-runner.contract.test.mjs \
  feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs \
  feature/tests/scheduler-fairness-sticky-race.contract.test.mjs \
  feature/tests/aws-api-key-region-lifecycle.contract.test.mjs \
  feature/tests/request-api-key-admission-multi-instance.contract.test.mjs \
  feature/tests/strict-local-first-routing.contract.test.mjs \
  feature/tests/run-token-refresh-cluster-validation.contract.test.mjs \
  feature/tests/run-multi-instance-redis-coordination-validation.contract.test.mjs

node --check feature/tests/validation-child-env.mjs \
  && node --check feature/tests/bare-invoke-claude-cli.mjs \
  && node --check feature/tests/claude-cli-long-session-continue.mjs \
  && node --check feature/tests/thinking-effort-claude-cli-capture.mjs \
  && node --check feature/tests/external-takeover-scheduler-degraded-nondocker.mjs \
  && node --check feature/tests/e03-real-two-process-scheduler.mjs \
  && node --check feature/tests/scheduler-fairness-sticky-race.mjs \
  && node --check feature/tests/run-redis-fault-domain-product-validation.mjs

rg -n "\.\.\.process\.env" feature/tests/*.mjs scripts/loadtest/*.mjs
node feature/tests/inventory-build-artifacts.mjs --gate
```

结果：

- feature docs：47/47 issue documents pass；104 relative links resolve。
- non-Docker runner/path contract batch：69 tests total；68 pass、1 skipped、0 fail。skip 为显式 live nonempty Redis opt-in。
- `runtime-validation-paths.test.mjs` 在该批中为 11/11，覆盖 runner 子进程白名单环境和所有非测试 validation runner 无 `...process.env`。
- 8 个更新 runner/helper 的 `node --check` 通过。
- `rg "...process.env"` 只剩 `.test.mjs` fixture launcher；非测试 validation runner 无匹配。
- 清理无引用的可再生 `target/debug`、`target/flycheck0` 和 `target/.rustc_info.json` 后，`find . -maxdepth 3 -type d -name target` 无输出。
- inventory：`targets=0 reservations=0 target_processes=0 blockers=0`，release-gate result=pass。Docker 只读盘点超时，仍为 `manual-only` hint，未执行 Docker 清理。
- RedisStore role guard 后 scoped `cargo +1.92.0 check --bin kiro-rs` 通过，wrapper cleanup `size_kib=446876 removed=true reservation_released=true`。该 Cargo 验证使用独立 scoped target，结束后又清理了无引用 root `target/debug`/`target/flycheck0`，inventory 仍为 pass。
- 磁盘可用空间复核约 `71-73 GiB`。

判断：

- 本轮补证没有新增 scoped target/reservation 残留。
- 当前文件系统 inventory 已从此前的 root target/PID 阻断恢复为 pass；这只是当前时点的零残留证据，不替代最终冻结候选后的 release inventory。
- 运行中的用户服务未被停止；未触碰 `127.0.0.1:9022`，未读取或暂存 `kiro_idc_users*.txt`。

### 6. Continuation rerun: protocol, runner and signal contracts

命令：

```bash
node feature/tests/cost-format-contract.mjs \
  && node feature/tests/mcp-attempt-channel-contract.mjs \
  && node feature/tests/request-api-key-id-contract.mjs \
  && node feature/tests/prompt-control-independence.mjs \
  && node feature/tests/prompt-default-parity.mjs

node --test feature/tests/protocol-contamination-source-contract.test.mjs

node --test feature/tests/protocol-marker-inventory-source-contract.test.mjs

node --test \
  feature/tests/protocol-contamination-source-contract.test.mjs \
  feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs

node --test \
  feature/tests/runtime-validation-paths.test.mjs \
  feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs \
  feature/tests/scheduler-fairness-sticky-race.contract.test.mjs \
  feature/tests/strict-local-first-routing.contract.test.mjs \
  feature/tests/aws-api-key-region-lifecycle.contract.test.mjs \
  feature/tests/request-api-key-admission-multi-instance.contract.test.mjs \
  feature/tests/frozen-load-chaos-runner.contract.test.mjs

node --test \
  feature/tests/e03-real-two-process-scheduler.contract.test.mjs \
  feature/tests/run-token-refresh-cluster-validation.contract.test.mjs \
  feature/tests/run-multi-instance-redis-coordination-validation.contract.test.mjs \
  feature/tests/run-scheduler-redis-chaos-validation.contract.test.mjs \
  feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs

node --test \
  feature/tests/thinking-effort-kiro-wire-contract.test.mjs \
  feature/tests/thinking-effort-kiro-wire-signal.test.mjs \
  feature/tests/thinking-effort-claude-cli-capture-signal.test.mjs \
  feature/tests/bare-invoke-claude-cli-signal.test.mjs
```

结果：

- UI/prompt 字段合同：cost format、MCP attempt channel、request API key ID、prompt control independence、prompt default parity 均 PASS。
- Protocol contamination source contract：10 tests total；10 pass、0 skipped、0 failed。该合同锁定不信任任意 `Hashxxxxxxxx`、raw marker-free body 不 DOM parse/serialize、assistant 清理不改 user/tool data、signed/redacted thinking 原子 fail closed、strict request 不 raw-external bypass，以及 stream/non-stream/external 污染后不发送 success terminal。
- Protocol marker inventory source contract：4 tests total；4 pass、0 skipped、0 failed。该合同锁定生产源码中内部 transcript marker 的允许位置、inert dot placeholder、无 bare `<invoke>` 生产字面量、无旧 output placeholder 生产生成点，以及 invalid tool_result repair 不 textify rejected content。
- Protocol contamination + Redis fault-domain 合批：56 tests total；47 pass、9 explicit live-signal skips、0 failed。9 个 skip 继承自 Redis fault-domain live signal fixture，不是 protocol-contamination skip。
- non-Docker runner/path 合批：49 tests total；49 pass、0 skipped、0 failed。
- E03/token-refresh/multi-instance/scheduler/fault-domain 合批：146 tests total；124 pass、22 explicit fixture skips、0 failed。
- thinking/Claude signal 合批：首次 30 秒工具超时只截断正在通过的长信号矩阵；以 120 秒上限复跑完整通过，50 tests total；50 pass、0 skipped、0 failed。
- 复跑后无匹配的 `thinking-effort`、`bare-invoke`、`kiro-wire`、`claude-cli-capture`、`validation-build`、`redis-chaos-proxy` 或 `node --test` 残留进程。
- 只清理了 5 个明确本轮测试前缀且无打开文件的小型临时目录：`thinking-wire-signal_race-*`、`kiro-cost-format-*`、`kiro-request-api-key-id-*`、`kiro-redis-fault-domain-[0-9]*-*`；总量约 4 KiB，未触碰不确定历史证据目录。
- 复核 build artifact inventory：`targets=0 reservations=0 target_processes=0 blockers=0`。

边界：

- 本节仍是合同/信号/静态路径验证，不替代真实 Claude Code CLI native upstream、主动/被动 thinking 长会话、search/image/MCP/agent、UI browser/build、upgrade、two-instance fault/fallback 或 final frozen release gate。

### 7. Post-documentation link and inventory recheck

命令：

```bash
node feature/tests/check-feature-docs.mjs

git diff --check -- \
  feature/tests/protocol-contamination-source-contract.test.mjs \
  feature/issues/protocol-transcript-and-tool-history-leak.md \
  feature/evidence/protocol-contamination-source-contract-20260721.md \
  feature/evidence/README.md \
  feature/final-report.md \
  feature/implementation-status.md \
  feature/tests/reverification-matrix.md \
  docs/plantree/plans/runtime-correctness-and-release-gates/implementation-status.md \
  docs/plantree/plans/runtime-correctness-and-release-gates/history/evidence-index.md

node feature/tests/inventory-build-artifacts.mjs --gate
```

结果：

- feature docs：47/47 issue documents pass；106 relative links resolve。
- `git diff --check`：通过。
- 首次 inventory 复核为 fail：`targets=1 reservations=0 target_processes=1 blockers=2`，root `target/` 约 `710 MiB`，PID `84264` 为已存在的 `./target/release/kiro-rs -c config.json --credentials credentials.json`，并有 `target/local-verify/kiro-rs-9022.log` 写入句柄。
- 按用户磁盘清理要求，只检查并删除无引用的可再生产物：`target/debug` 约 `709 MiB`、`target/flycheck0` 约 `1.1 MiB`、`target/.rustc_info.json`。`lsof` 未发现这些路径有打开引用。
- 未停止或 kill PID `84264`，未清理不确定用户服务资产。
- 清理后 `target/` 为 `0B`，inventory 重新通过：`targets=0 reservations=0 target_processes=0 blockers=0`。
- 磁盘可用空间约 `69 GiB`。

边界：

- 该 cleanup 只是当前工作区 build artifact hygiene，不是最终 release inventory。
- Docker 只读盘点仍超时并保持 `manual-only` hint；本轮未执行 Docker 清理。

### 8. Marker inventory contract and post-run target cleanup

命令：

```bash
node --test \
  feature/tests/protocol-contamination-source-contract.test.mjs \
  feature/tests/protocol-marker-inventory-source-contract.test.mjs

node feature/tests/check-feature-docs.mjs
git diff --check
node feature/tests/inventory-build-artifacts.mjs --gate
df -h .
find . -maxdepth 3 -type d -name target -print
```

结果：

- Protocol contamination + marker inventory source contracts 合批：14 tests total；14 pass、0 skipped、0 failed。
- Feature docs：47/47 issue documents pass；108 relative links resolve。
- `git diff --check`：通过。
- 第一次 inventory 复核又发现 root `target/` 被 rust-analyzer/flycheck 类任务重建：`targets=1 reservations=0 target_processes=1 blockers=2`，目录约 `710 MiB`。PID `84264` 仍是既有 `./target/release/kiro-rs -c config.json --credentials credentials.json` 服务；当前可见 `target/` 只剩 `debug`、`flycheck0` 和 `.rustc_info.json`，`lsof +D target` 对这些当前目录项无打开引用。
- 只删除无引用、可再生的 `target/debug`、`target/flycheck0`、`target/.rustc_info.json` 并尝试删除空 `target/`；未停止 PID `84264`，未删除不确定用户服务资产。
- 清理后 inventory 重新通过：`targets=0 reservations=0 target_processes=0 blockers=0`，release-gate result=pass。
- 磁盘可用空间约 `69 GiB`。Docker 只读盘点本次超时，仍是 `manual-only` hint，未执行 Docker 清理。

边界：

- 该 cleanup 仍只是当前工作区 build artifact hygiene，不是最终冻结候选后的 release inventory。

## 结论

本证据支持以下结论：

- E05 runner 改造后，相邻低产物合同未回归。
- 文档、UI cost/request-key/MCP 字段、prompt 控制独立性仍满足合同。
- 多个 runtime runner 的输入拒绝、no protected-9022 probe、signal cleanup 和 temp cleanup 合同仍成立。
- scoped wrapper 的清理协议仍成立。

本证据不支持以下结论：

- 不能声明发版通过。
- 不能声明 E05 动态全矩阵通过。
- 不能声明 external takeover dynamic、E01/E02 dynamic、真实 upstream/CLI、多能力 native、UI browser/build、upgrade 或 final inventory 已通过。

发布状态保持 `NO-GO`。
