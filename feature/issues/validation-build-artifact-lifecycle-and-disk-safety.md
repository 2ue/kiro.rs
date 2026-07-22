# Validation Build Artifact Lifecycle And Disk Safety

Status: `reproduced-defect / original-and-latest-cleanup-complete / lifecycle-matrix-pass / current-inventory-pass / final-release-inventory-open`

Severity: P1

Last updated: 2026-07-19

## 问题与影响

本轮多次 Rust 聚焦测试持续复用默认 `target/debug`，同时部分支线使用独立 `CARGO_TARGET_DIR`。测试完成后没有统一的构建产物 owner、保留理由、大小预算和清理动作，最终当前仓库 `target/` 约占 50 GiB，其中默认 `target/debug` 约 43 GiB。数据卷可用空间下降到 446 MiB，达到 100% 显示，后续编译、数据库和普通桌面进程都可能因 `ENOSPC` 失败。

这不是业务网关的运行时内存泄漏，也不是“多个支线并发构建”造成的持久占用。**直接且必要的根因是构建结束后没有删除可再生产物，导致默认 target、独立 target、增量缓存和历史测试二进制跨批累积。** 支线数量、并行构建和 dirty tree 变化只会提高瞬时高水位或加快累积速度：完全串行地反复构建而不清理同样会填满磁盘；多支线各自构建但每批完成后删除，则不会形成这次的跨批线性增长。

2026-07-16 的定点清理删除了可再生的默认 debug/release、scheduler/auxiliary 隔离构建、旧固定二进制缓存和两个登记 worktree 内的 Cargo target。仓库根 `target/` 从约 50 GiB 降到 383 MiB，数据卷可用空间从 446 MiB 恢复到 54 GiB。小型脱敏报告和两个干净的登记 worktree 源码被保留。

## 根因

### 直接根因

1. 聚焦命令没有统一 wrapper，成功、失败、中断和代理取消后都依赖人工记得清理。
2. 发布门禁没有把“本批 target 已删除、路径不存在、空间已复核”作为完成条件，构建成功被误当成整个验证任务完成。
3. 测试证据与构建缓存没有分层。小型报告需要保留，但整个 `deps/build/incremental` 被错误地长期保留。

### 放大因素，但不是根因

1. 默认 `target/debug` 同时被测试、检查和编辑器使用，无法从目录名判断 owner 或保留理由。
2. `CARGO_INCREMENTAL` 未在验证任务中关闭；大 dirty tree 反复变化会保留多批不可复用或低复用对象。
3. 多支线或重复构建提高增长速度和瞬时空间峰值，但既不是产生长期残留的必要条件，也不足以解释测试结束后的持续占用。

## 复现方案

只读复现，不创建新构建：

```bash
df -h /System/Volumes/Data
du -sh target target/debug target/release
du -sh target/*
ps -axo pid,ppid,etime,state,command | rg '/cargo (test|check|build)|/rustc'
```

修复前现场关键值：

- 数据卷可用空间：`446 MiB`；
- 根 `target/debug`：约 `43 GiB`；
- 根 `target/release`：约 `2.5 GiB`；
- scheduler 独立 target：约 `842 MiB`；
- auxiliary 独立 target：约 `592 MiB`；
- 两个登记 worktree 内的 Cargo target：合计约 `3.3 GiB`。

## 选定优化方案

- 支线可以使用独立构建目录，但必须有明确 scope，并在该支线脱敏结果落盘后删除；并发不是禁止项，未清理的无期限积累才是缺陷。
- 新增 `feature/tests/run-cargo-scoped.sh`。它为一次逻辑测试批次创建唯一的 `target/.validation-build-<scope>.*`，设置 `CARGO_INCREMENTAL=0`，并在 success、failure、signal 下统一清理。
- wrapper 使用 Git common dir 下的小型 reservation state，让同仓库多个 worktree 共用一个原子准入临界区。默认每个活跃批次预留 12 GiB、保留 20 GiB 空闲下限；只有 `available >= floor + active reservations + requested` 才能进入构建。并发批次不被一刀切禁止，但不会因为同时读取旧 free-space 快照而超额准入。
- reservation 记录 owner PID、进程起始身份、创建时间、预留 KiB、文件系统身份、scope、reservation ID 和 scoped target。PID 存活但起始身份不一致视为 stale；PID 存活但身份暂时不可读取时保守视为 active。
- shell 无法捕获 `SIGKILL`、执行器崩溃或主机掉电。wrapper 因此把 owner PID/进程起始身份写入唯一目录；后续任一批次启动时先回收 owner 已消失的 stale 目录，发布前再显式执行 `--reap-stale`。活跃支线目录不会被并发回收，发现活跃或删除失败都会阻断最终清理门禁。
- wrapper 还在 command `exec` 前原子发布 command PID、start 和独立 PGID。wrapper owner 即使被 SIGKILL，只要该 PGID 任一成员仍活，local reaper 与 reservation reaper 都把容量视为 active 并保留 target；进程组完整退出后才允许删除。metadata 处于 `.command-starting` 或字段不一致时返回 unknown/fail closed。
- cleanup 入口移除 EXIT trap 后立即忽略 INT/TERM/HUP，使 cleanup 不可重入且不会被第二次普通信号打断。它先 TERM、有限等待并必要时 KILL 自己的 command PGID，再证明 target marker/reservation ID 后删除。SIGKILL 仍无法捕获，由 command owner/stale 协议处理。
- 默认 preflight 要求至少 `20 GiB` 可用空间；默认单批构建目录预算 `12 GiB`。阈值只能通过明确环境变量调整并记录原因。
- 单个逻辑批次应在 wrapper 内完成多轮测试，避免每轮重建依赖。测试报告写入独立的小型 report 目录。
- 最终冻结候选若需要保留，只复制实际二进制和 manifest/SHA-256；不保留整套 `deps/build/incremental`。
- 每个支线 handoff 必须记录清理前构建目录大小、清理后目录不存在、清理后可用空间。发布前扫描所有 `.validation-build-*`，任何残留都阻断。
- “Cargo 命令退出 0”不等于批次通过；wrapper 的清理日志必须为 `removed=true`，且独立残留扫描为零。失败测试和被中断测试遵守同一要求。
- 不删除无法证明归属于本轮的目录或进程。登记 worktree 用 Git 元数据识别，只删除其中明确可再生的 Cargo target，不直接破坏 worktree。
- 新增只读 `feature/tests/inventory-build-artifacts.mjs`。它盘点当前 repo/default target、登记 worktree target、reservation target、显式 `CARGO_TARGET_DIR`、有界 private-temp target，以及由活进程 cwd/executable/txt 反推出的 target；报告只使用路径 digest/受控 locator、PID 和归一分类，不输出完整命令或原始路径。
- inventory 对默认 temp root 只进入 `kiro/cargo/validation` 已知容器；显式 `--temp-root` 才执行有 20k-entry 预算的递归。扫描截断、目录不可读、`ps`/`lsof`/`/proc` 检查不完整都会 fail closed，而不是给出假阴性 pass。
- Docker 只读 inventory 不调用 Docker prune，也不删除任何目录；Docker 清理必须是单独人工确认的动作，并只删除可证明安全的资源。

### Inventory 分类与发布判定

| 分类/状态 | 含义 | 自动动作 | 发布判定 |
| --- | --- | --- | --- |
| `scoped-active` | reservation 与 owner 身份一致且 owner 存活 | 无 | 阻断，等待本批 cleanup |
| `scoped-stale` | 强 owner marker 存在但 owner 已消失 | inventory 只报告；由 wrapper reaper 回收 | 阻断 |
| `scoped-*-unreserved` | target marker 与 reservation 不一致/缺失 | 不删除 | 阻断并人工复核 |
| `unmanaged-repo/worktree/explicit-cargo-target` | 已知位置存在未纳入 scoped 生命周期的 Cargo target | 不删除 | 阻断 |
| `unknown-private-temp-cargo-target` | private-temp 中存在 Cargo marker，但无法证明 owner | 不删除 | 阻断 |
| target process | 参数、相对 cwd 路径、cwd、executable 或 txt fd 指向 target | 只输出 PID/分类 | 阻断 |
| incomplete/truncated | 进程或 temp 检查没有完整结束 | 无 | fail closed 阻断 |

典型用法：

```bash
feature/tests/run-cargo-scoped.sh pg-usage -- \
  bash -lc 'cargo +1.92.0 test <filter-1> && cargo +1.92.0 test <filter-2>'
```

## 验证与证据

- 清理后 `du -sh target` 为 `383 MiB`，`df` 显示数据卷可用空间 `54 GiB`。
- 清理过程中只终止了正在写入待删目录的可再生 Cargo 子任务，没有关闭 VS Code/rust-analyzer 主进程，也没有触碰 `127.0.0.1:9022`。
- `run-cargo-scoped.sh` 的成功、失败和 TERM 三类动态 gate 已通过：退出码分别为 `0`、原业务退出码 `23` 和 signal 退出码 `143`；三类都删除唯一 build dir 和 reservation。TERM 首轮曾稳定抓到“signal 落在 reservation temp/rename 窗口”的单 reservation 残留，修复为 cleanup 按 exact reservation ID 重新发现 final/temp record 后复测通过。
- 该 smoke 证明清理与命令结果解耦：即使构建/测试失败或被中断，可再生 target 也必须删除；它不依赖是否存在并发支线。
- 执行器强制终止的定点测试证明 trap 可能完全没有执行，并留下空的 validation build 目录；该反例促成 owner-aware stale reaper。修复后的验收还必须证明：下一次普通 invocation 和显式 `--reap-stale` 都能删除 owner 已消失的目录，同时保留 owner 仍存活的并行目录。
- owner/reaper 定点矩阵通过：active owner 返回 `75` 且 target/reservation 原样保留；stale owner 返回 `0` 且二者删除；缺 marker 的同名未知目录返回 `73` 且 sentinel 原样保留。
- 原子准入先以“两批允许、第三批拒绝”通过；20 路同时竞争在容量只允许 4 批时，结果严格为 admitted `4`、rejected `16`、unexpected `0`，峰值 reservation/target 均为 `4`，收尾 residual target/reservation 均为 `0`。首轮压力还发现 local reaper 与另一 wrapper cleanup 的 TOCTOU 误报，修复为路径复核已消失即视为安全、marker 写入窗口内 live PID 只保留不删除。
- command-owner 增强后重新执行完整 shell 矩阵：success `0`、业务 failure `23`、前台 INT `130`、TERM `143`、HUP `129` 均清理；cleanup 内连续 HUP/TERM/INT 的双信号夹具 `3/3`；20-way owner-only 为 `16 x 75 + 4 x 143`；故意在第 5 个 rejected target 尚清理时对全部仍可证明 owner 的 PID 施压，reservation 峰值仍为 4，最终 target/reservation/command marker 全零。
- wrapper 被 SIGKILL、command PGID 继续运行的矩阵 `3/3`：首次 reaper 返回 `75` 并保留，测试 owner 终止进程组后第二次返回 `0` 并删除。`.command-starting` 不完整 metadata 矩阵 `3/3` 先返回 `73` 保留，解除测试 marker 后正常回收。短命令 `true` 为 `30/30`，命令退出后遗留同组后台 child PID 为 `3/3` 被 wrapper 终止。
- 当前稳定 wrapper SHA-256 为 `2a6f219857197c702d7e4c5f89fb1b66467789c0d51781a9dc728327065c431f`。上述新增验证只使用 shell 临时夹具，没有运行 Cargo；另一 owner 仍有 active scoped target 时没有执行 root target `du`、reap 或删除。
- 上述关键退出路径已固化为 `feature/tests/run-cargo-scoped-lifecycle.test.mjs`：success、业务 failure、HUP/INT/TERM、wrapper SIGKILL 后 command-PGID 延迟回收、unknown owner fail-closed 各固定执行 3 轮。夹具只使用独占临时 target/state，不调用 Cargo，也不把一次性人工命令当成后续回归门禁。
- 2026-07-17 重新执行该固定门禁为 `21/21` 通过；测试后 `kiro-build-lifecycle-*` 临时目录、测试进程、scoped target 和 reservation 均为 `0`。同日 `body-payload-identity` 真实 Cargo 冷批次全部退出 `0`，wrapper 报告 `size_kib=2410696 removed=true reservation_released=true`，随后独立复核 scoped target/reservation/wrapper/command/Cargo/rustc 均为 `0`。加上此前 `cargo check`、literal-tool 和完整 stream 批次，真实 Cargo 清理要求已有多批证据；最终发布前 root/private-temp inventory 仍是独立开放门禁。
- 另一独立支线的 5 轮串行加 4 轮并发、每轮写入 16 MiB 的 shell fixture 共 `9/9` 报告 `removed=true`、`reservation_released=true`，最终 target/reservation 均为 `0`。它进一步证明并发只改变峰值，未清理才产生跨批线性增长；这仍不替代真实 Cargo 冷构建门禁。
- inventory clean gate、active/stale/worktree/private 分类矩阵、相对 `./target/release/...` 进程、源码 worktree 排除、检查能力缺失 fail-closed 各完成 3 轮。所有轮次的 fixture stat 快照前后一致，临时目录/进程为 `0`，报告未出现完整命令、fixture 路径或测试敏感标记。
- 默认 temp 盘点修复前达到 `20136` entries 并截断；改为 bounded-known-prefixes 后实测 `2954` entries、`truncated=false`，连同约 1.9 GiB 根 target 的 `du` 和全进程 cwd/txt 检查 wall time 约 `3.5s`。该工具不在请求热路径，只在本地/发布门禁运行。
- 实际相对路径启动的 PID `994` 已由 txt/executable 证据识别为 `kiro-runtime`；另有 16 个 cwd 位于 `target/claude-cli-tests` 的运行进程被保守报告。报告只显示 PID、target ID 和分类，不回显命令或路径。
- 2026-07-17 的实际 inventory 因根 unmanaged target 和 target 引用进程返回 fail；这是正确的 NO-GO 证据，不是待“清洗掉”的测试失败。未知/活跃资产必须由 owner 收尾，inventory 本身不删除。
- 最终只读复核：scoped target `0`、reservation `0`，根 unmanaged target `2146380 KiB`，target process `20`，总 blocker `21`；process inspection complete，temp entries `2956`、unreadable `0`、truncated `false`，可用磁盘 `48054528 KiB`。`docker system df` 在 5 秒内未返回，报告 `timed-out/manual-only`，没有执行 prune。inventory 退出 `75`，发布继续 NO-GO。
- 6 个 runtime/CLI runner 的默认 `target/debug/kiro-rs` 回退和根 `target/<report>` 输出已移除。它们现在共同使用 `runtime-validation-paths.mjs`，强制 `KIRO_RS_BINARY` 与 `KIRO_VALIDATION_ARTIFACT_DIR` 为仓库外绝对真实路径，并拒绝 lexical/symlink 回指仓库、路径缺失及 file/directory 类型错误。当前 Node 合同把 `bare-invoke-claude-cli`、request-api-key multi-instance、scheduler fairness、strict local-first、AWS region lifecycle 和 thinking wire 纳入同一静态门禁；正常、缺失/相对/类型错误、仓库内/symlink 矩阵各 5 轮，并新增禁止 `listenerSnapshot(9022)` / `listeningPids(9022)` / 9022 PID 快照的 no-probe 断言。2026-07-18 扩展 Node 合同集合为 `85/85`，覆盖 build wrapper lifecycle、runtime paths、thinking wire/capture signal、bare invoke signal 和 load target 合同。这只关闭源码路径与保护端口绕过，不替代冻结 binary 的完整 runner 运行。
- 2026-07-21 追加 runner 子进程环境隔离合同：新增 `feature/tests/validation-child-env.mjs`，真实 validation runner 的服务、Claude、proxy 和 scoped-Cargo 子进程只继承 `PATH`、temp/locale/user、`HOME`、`VOLTA_HOME`、`CARGO_HOME`、`RUSTUP_HOME` 等执行基础变量，并由调用点显式传入需要的 runtime 变量。`runtime-validation-paths.test.mjs` 当前 11/11 通过，证明 child env 不继承 `DATABASE_URL`、`REDIS_URL`、Anthropic/OpenAI key、`KIRO_API_KEY`、`KIRO_RS_TEST_REDIS_URL` 或任意未显式传入的 `KIRO_*`；所有非测试 `feature/tests/*.mjs` validation runner 均无 `...process.env`。这防止验证脚本被调用方 shell 的 PG/Redis/secret 覆盖污染，但不替代动态产品验证。
- 两个 Node load runner 原先在漏传地址/key 时默认请求 `127.0.0.1:9022`。现统一要求显式 base URL 与 API key，默认只允许 loopback，三种 loopback spelling 的 `9022` 均无条件拒绝，非 loopback 还需显式 remote opt-in。5 组 Node 合同通过，其中正常、缺参/坏协议/remote、受保护端口矩阵各 5 轮；负载文档也不再包含 `cargo run`、根 target report 或 `--base-url ...:9022` 示例，而是使用 scoped build 复制的仓库外冻结 `kiro_loadtest` 和 owned artifact root。
- 2026-07-17 最新一次 owner 复核后，只删除了本轮和已确认空闲的可再生构建路径；保留 PID `994` 使用的配置目录与旧 Claude tmux cwd。根 `target/` 从 `1,051,800 KiB` 降至约 `5.4 MiB`，数据卷可用约 `49-50 GiB`。随后 inventory 为 `targets=0 / reservations=0 / target_processes=0 / blockers=0`、release-gate pass；Cargo/rustc/loadtest PID 为 0。该旧复核曾额外比较 9022 PID；按当前 no-probe 合同，它不能再作为 release 隔离证据。根因结论仍成立：每批结束未清理会造成跨批线性增长，分支并发只加速累积。
- 后续至少三个真实 Rust 测试批次必须使用 wrapper，并证明没有 `.validation-build-*` 残留、`target/` 未出现跨批线性增长。
- 2026-07-18 默认 release admission 轻量复核执行 `feature/tests/run-cargo-scoped.sh release-admission-lightcheck-20260718 -- true`，未运行 Cargo。结果为 `admitted=false available_kib=28588348 floor_kib=20971520 requested_kib=12582912`、退出 75；wrapper 随后清理自身 `size_kib=16` 临时 target，复核根 `target=0 KiB`。当前不能跑默认 release C0 的直接原因是整盘可用空间低于门禁要求，不是本轮 scoped target 未清理。
- 2026-07-18 后续复核确认当时“需要 30G”的说法不是 Cargo 的实际构建体积需求，而是 release wrapper 的保守准入策略：默认单批 reservation 约 `12 GiB`，同时保留约 `20 GiB` 空闲下限，用于防止 release 构建、并发验证或编辑器干扰把磁盘打满。实际本轮 scoped debug/test target 多次约 `1.68-1.70 GiB`，此前 release C0 target 约 `2.49 GiB`。开发验证可显式用 `KIRO_VALIDATION_RESERVE_KIB=6291456` 的 6 GiB reservation；最终 release C0 仍使用默认保守门禁。
- 同日 Docker 盘点显示真正的大头在 Docker，而不是当前仓库 Cargo target：`Images 48.73GB / reclaimable 23.7GB`、`Containers 2.49GB / reclaimable 2.491GB`、`Local Volumes 15.97GB / reclaimable 13.66GB`、`Build Cache 34.3MB`。按用户确认执行了单独 Docker 清理：删除当前仓库上一轮遗留的 `kiro-rs-scheduler-gate-*` 测试容器及其匿名卷，执行 dangling image prune 和 builder cache prune；没有 prune 命名 volumes，没有删除有 tag 的项目镜像，没有停止 `kiro-rs-tool`、用户本地 `kiro-rs-postgres-local`/`kiro-rs-redis-local`，也没有触碰其他同名项目正在使用的容器。文件系统可用空间从约 `28 GiB` 恢复到约 `39-41 GiB`。
- 复核还发现另一个本机同名项目 `/Users/yuanfeijie/Desktop/project/kiro.rs` 正在运行 `target/release/kiro-rs`，并使用 `kiro-rs-scheduler-gate-*-a1` PostgreSQL/Redis 容器。按“即使其他相同项目在本地运行也不要互相干扰”的要求，该容器组未清理。
- 本轮多次 scoped Cargo cleanup 后，根 `target/` 仍被编辑器/rust-analyzer 的当前仓库 `cargo check --workspace --manifest-path /Users/yuanfeijie/Desktop/procode/kiro.rs/Cargo.toml` 重建到约 `544-709 MiB`。这不是 wrapper 残留；执行验证期间仅终止当前仓库 root `cargo check` 子进程并在无 `lsof +D target` 引用时删除根 `target`。未杀编辑器主进程，未影响其他项目。
- 2026-07-19 三个真实 Cargo storage 批次均通过 wrapper 完成清理：token-refresh Redis、Redis usage writer、external dispatch + runtime quarantine storage suite 的 scoped target 均约 `1.69 GiB`，并各自 `removed=true / reservation_released=true`。随后 inventory 发现旧 `kiro_cli_repro` Claude CLI/MCP tmux 会话仍以 `target/claude-cli-tests/...` 为 cwd；该 19 天前验证残留已关闭。未杀正在运行的 `kiro-rs` 服务；只在确认无进程引用后删除 root `target/debug`、`target/flycheck0` 和 `.rustc_info.json`。删除 `/tmp` 原始日志后，编辑器/flycheck 又重建约 `710 MiB` 可再生产物，复核无引用后再次删除。最终 `du -sh target -> 0B`，`inventory-build-artifacts --gate` 为 `targets=0 reservations=0 target_processes=0 blockers=0`，磁盘可用约 `84 GiB`。完整记录见 [2026-07-19 storage/artifact evidence](../evidence/storage-integration-and-artifact-gate-20260719.md)。
- 后续 frozen loadtest 构建批次继续遵守同一规则：`frozen-loadtest-20260719-r5` 通过 wrapper 构建并复制仓库外二进制后，wrapper 清理 `size_kib=751344 removed=true reservation_released=true`；两个 focused loadtest Rust test scope 也分别 `removed=true / reservation_released=true`。当前根 `target/` 约 710 MiB，`inventory-build-artifacts --gate` 返回 fail 的直接原因是用户已有 PID `84264` 正在运行 `./target/release/kiro-rs -c config.json --credentials credentials.json` 并引用 repo target。该 target/process 不属于验证残留，未经用户明确授权不得停止或删除；最终 release inventory 因此仍保持 open。
- 2026-07-21 低产物补证后，根 `target/` 再次由 rust-analyzer/flycheck 重建为约 `710 MiB`，实际大头为 `target/debug` 和 `target/flycheck0`。复核 `lsof +D target/debug` 与 `lsof +D target/flycheck0` 无引用后，仅删除这两个可再生产物和 `target/.rustc_info.json`，没有停止用户服务或删除不可证明归属资产。复核 `find . -maxdepth 3 -type d -name target` 无输出，`node feature/tests/inventory-build-artifacts.mjs --gate` 为 `targets=0 reservations=0 target_processes=0 blockers=0`，文件系统可用约 `71-73 GiB`。Docker 只读 inventory 超时，按规则仍是 `manual-only` 提示，没有执行 Docker 清理。该结果只证明当前时点零残留；后续 frozen CLI/load/UI/upgrade 或编辑器重建后仍必须重跑 inventory，不能把这次 pass 当作最终 release inventory。
- 2026-07-21/22 C0d final-candidate scoped batch 进一步证明 release 构建不需要 30G 常驻 target：`cargo fmt --check + cargo test --all-targets + cargo build --release --bins` 在同一 scoped target 内完成，最大清理前体积 `2446284 KiB`，wrapper 退出时 `removed=true reservation_released=true`，可用空间约 `82943832 KiB`。随后 inventory 首次因编辑器/flycheck 重建的可见 `target/debug`（约 `916M`）和 PID `84264` 仍运行历史 `./target/release/kiro-rs` 9022 服务而 fail closed；复核后只删除无引用的 `target/debug`、`target/flycheck0` 和 `.rustc_info.json`，没有停止 PID 84264，也没有删除它的运行时资源。最终 `target absent after cleanup`，`inventory-build-artifacts --gate` 为 `targets=0 reservations=0 target_processes=0 blockers=0`。这再次说明发布门禁需要“每批清理 + 最终 inventory”，而不是长期保留根 target；也说明用户服务引用的资产必须独立 owner 处理，不能由验证脚本强删。证据见 [C0d static/CLI/load/UI](../evidence/final-candidate-c0d-static-cli-load-ui-20260721.md)。

## 残余风险与回滚

- 禁用 incremental 和批次后清理会增加下一批冷编译时间；这是用可预测磁盘占用换取重复编译缓存。应把相关测试合并为一个逻辑批次，而不是取消清理。
- wrapper 只能管理它自己创建的目录，不能控制编辑器或人工直接运行到默认 `target/debug` 的构建；发布前仍需独立盘点根 `target/`。当前根 `target/debug` 可被正在运行的 rust-analyzer 重新生成，不能误记为支线清理失败，也不能在 owner 活跃时反复删除导致重建抖动。
- 6 个 Node runtime/CLI runner 的源码路径绕过与 9022 listener 探测已关闭，但完整 runtime gate 尚未在最终冻结 release binary 上重跑。调用方仍必须拥有并最终删除 `KIRO_VALIDATION_ARTIFACT_DIR`；runner 只删除本次唯一 runtime 子目录并保留小型报告供脱敏汇总，不能把报告目录当永久缓存。
- load runner 的 target/port fail-closed 只防止误操作；当前 frozen L1 smoke 已通过，但仍不证明 L3/L4/L5 的性能、资源回落或异常恢复。最终仍须用同一冻结候选实际运行剩余负载/混沌矩阵，并在提取脱敏摘要与哈希后删除 load artifact root。
- uncatchable termination 后的清理是 eventual：由下一次 wrapper invocation 或发布前 `--reap-stale` 完成。若没有任何后续动作，shell 本身无法保证立即删除；因此最终发布门禁必须单独执行 reaper 和零残留扫描。
- command PGID 可覆盖 Cargo/rustc 的正常进程树，但主动创建新 session/PGID 的恶意或非常规子进程可能逃离该组。runner 合同禁止 detached build work；最终 inventory 的 cwd/executable/txt/参数检查继续作为独立兜底。无法证明归属时不自动 kill 或删目录。
- 命令运行期间若单次构建异常超过剩余空间，事后 trap 来不及预防 `ENOSPC`。20 GiB preflight 和 12 GiB预算是当前仓库经验阈值；若实际 clean all-target/release 构建超过预算，必须先记录并调整，不得静默放宽。
- 回滚 wrapper 不会改变业务二进制，但会重新暴露无 owner 构建缓存累积风险；因此只能在有等价自动清理机制时替换。
