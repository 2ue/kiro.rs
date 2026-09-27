# 闲置账号后台 Token 主动刷新

Status: `Implemented; release verification complete`

Last reviewed: 2026-09-28 Asia/Shanghai

## 目标

为长期没有被请求选中的 OAuth/外部 IdP 账号增加服务端后台主动刷新，使
refresh token 轮换、access token 和真实 `expires_at` 能在账号再次被调度前
持久化到 PgSQL。该功能只分析和实现当前 Rust 服务，不修改生产机器。

## 当前事实

- 请求热路径在 `MultiTokenManager::try_ensure_token...` 中只使用 5 分钟安全窗口；
  没有请求就不会进入该路径。
- Admin 强制刷新是显式调用，鉴权失败恢复只在真实推理/MCP 请求遇到无效
  access token 后触发，导入验证只覆盖导入时刻。
- `spawn_stats_flush_worker` 只刷统计、运行态 mutation 和 lease 清理，不会扫描
  账号或发送 refresh 请求。
- `is_token_expired` 对缺失或无法解析的 `expires_at` fail-closed；API Key 账号
  不需要 refresh。
- 现有 refresh 流程已包含本地 refresh lock、Redis 分布式协调、PgSQL 字段 CAS、
  refresh token rotation 持久化、真实 `expires_in`、失败 wave/退避、RPM 和
  auxiliary 并发准入；后台 worker 必须复用这些机制。

## 上游对照

`Kiro-account-manager` 在主进程每分钟扫描账号，默认提前约 10 分钟刷新，跳过
禁用、无 refresh token 和 API Key 账号，并将刷新后的 access token、轮换后的
refresh token 和真实过期时间写回持久化存储。该项目曾经出现“只更新 access token、
硬编码 1 小时、窗口最小化后定时器节流、刷新成功但未写入 IDE 磁盘”等问题。
本服务的设计保留其“服务端调度、提前量、失败隔离”经验，但不复制桌面 IDE 专属
的当前账号 timer。

## 目标设计

### 配置

新增运行时配置：

- `tokenRefreshBackgroundEnabled`：默认 `true`；
- `tokenRefreshBackgroundIntervalSecs`：默认 `60`，允许 `10..=3600`；
- `tokenRefreshBackgroundLeadSecs`：默认 `600`（10 分钟），允许 `60..=3600`。

配置通过文件/PgSQL、Admin API 和两套 UI 暴露。worker 每轮读取当前快照，运行时
关闭或调整间隔/提前量无需重启。

### 刷新语义

- worker 只处理未禁用、非 API Key、存在 refresh token 的账号；
- `expires_at` 缺失、无法解析或进入主动刷新窗口时尝试刷新；
- 普通请求仍保持 5 分钟窗口，避免短 Token 在请求突发下重复刷新；
- 获取本地 lock 后必须再次读取并按同一提前量判断，避免重复发送；
- 发送、轮换、CAS、通知和失败 wave 全部复用现有按需刷新流程；
- 每轮按账号逐个处理，单账号失败只记录并继续后续账号，不快速无限重试；
- invalid_grant/credential auth 等既有健康处理继续生效，但不删除凭据；
- API Key、禁用账号和无 refresh token 账号不产生上游 refresh 请求。

### 生命周期与资源

- worker 由主服务启动，使用有界 `JoinHandle` 和显式 shutdown handle；
- 关闭服务时先停止 worker，再进行统计最终刷盘；
- worker 不创建无界任务，不绕过 auxiliary 并发/RPM 限制；
- 一轮 panic 或单个账号错误不能终止 worker；
- 动态配置关闭后当前正在执行的单次刷新允许完成，下一轮不再发送。

## 验收矩阵

1. 默认配置、JSON round-trip、边界校验和 Admin/UI 字段一致。
2. 窗口外账号不刷新；窗口内、已过期和缺失 `expires_at` 账号刷新。
3. API Key、禁用、无 refresh token 账号跳过。
4. 同一轮/连续多轮成功后不重复刷新；本地并发和 Redis 多实例仍 singleflight。
5. refresh token rotation、真实 `expires_in`、profile/scopes 和 PgSQL CAS 持久化。
6. 单账号网络、429、invalid_grant、协议和持久化失败不会杀死 worker；下一轮可恢复。
7. 动态关闭/调整 interval/lead 生效，shutdown 无残留 task、socket 或 permit。
8. 指定 `127.0.0.1:19023` 实例闲置账号确实产生 refresh，请求热路径、`/cc/v1`
   和真实 Claude Code CLI 不回归。
9. C0/C1/C2、L1 fake-upstream 和低并发资源检查通过后才能发版；发布后必须
   核对 GitHub Actions quality、架构镜像和 manifest。

## 风险与回滚

- 主要风险是启动时大量账号同时进入窗口造成 refresh burst；由逐账号处理、
  token refresh RPM、auxiliary 并发和可调配置共同限流。
- 多实例重复扫描由既有 Redis refresh lease 和 PgSQL CAS 去重；任何 authority
  不一致都 fail-closed，不覆盖较新的字段。
- 回滚优先关闭 `tokenRefreshBackgroundEnabled`，再回退二进制；不删除凭据和
  refresh token 数据，不覆盖已有 Git tag。

## 进度

- [x] 现状与 `kiro-account-manager` 对照
- [x] 计划、配置和验收边界形成
- [x] Rust worker、主动刷新提前量和生命周期
- [x] Admin/UI 配置
- [x] 单元、并发、真实实例和 CLI 回归
- [ ] 发布与发布后核验（本地发版门禁已完成；远端 workflow 需在 tag 推送后核对）

## 已实现行为

- `MultiTokenManager` 新增独立后台 worker，默认每 60 秒扫描一次，主动刷新窗口
  默认是剩余 600 秒；请求热路径仍保持原来的 5 分钟窗口。
- worker 只扫描未禁用、非 API Key 且存在非空 refresh token 的账号；缺失或无法
  解析 `expires_at` 也会进入尝试路径，避免把未知过期状态误当成健康。
- 刷新前后均复用现有 refresh lock、Redis 分布式 lease、PgSQL 字段级 CAS、refresh
  token rotation、真实 `expires_in`、RPM/auxiliary 准入和失败 wave；单账号失败
  只累计为本轮失败并继续后续账号。
- worker 可通过 Admin API/UI 动态关闭或调整间隔/提前量；关闭不取消已开始的单次
  refresh，但下一轮不再发送。
- 服务 shutdown 先停止后台 refresh worker，再停止统计 flush worker；等待有界，
  超时会 abort 并记录最后一轮统计。

## 验证证据（2026-09-28）

- scoped `cargo fmt --check`、`cargo check --all-targets --offline`、release build
  通过；冻结候选二进制 SHA-256：
  `04e13c0a04cad1ca00138f6ef4b6d0652837883eb768d6ce13174d0f8a97245e`。
- focused worker tests：`4 passed / 0 failed`，覆盖 rotation/真实 `expires_in`、
  窗口判断、API Key/禁用/缺 refresh token 跳过、单账号失败隔离和 bounded shutdown。
- default Rust all-targets：`2191 passed / 0 failed / 6 ignored`；`kiro_loadtest`
  `31 passed / 0 failed`。no-default all-targets 同样通过。
- `ui` check/build 与 `admin-ui` build 通过；`git diff --check` 通过；artifact
  inventory：`targets=0 reservations=0 target_processes=0 blockers=0`。
- 指定 `127.0.0.1:19023` 已替换为本候选二进制（PID `82072`，配置仍为
  `tmp/thinking-budget-local/config.json`），`/healthz`、`/readyz` 均成功。
  Admin GET 读取到 `enabled=true / interval=60 / lead=600`；真实动态关闭、读取
  `enabled=false`、再恢复 `true/60/600` 均 HTTP 200。
- 指定实例日志已出现后台真实扫描：
  `scanned=3 attempted=3 refreshed=3 skipped=0 failed=0`，且此前也观测到
  `scanned=3 attempted=1 refreshed=1` 与 `attempted=2 refreshed=2`，证明闲置账号
  不依赖请求热路径即可刷新。
- C1 `/cc/v1/messages` 非流式和流式均 HTTP 200，文本正确，SSE 含
  `message_start/content_block_delta/message_delta/message_stop`，最终 usage 非零。
  Claude Code CLI `2.1.280` 隔离 `HOME/CLAUDE_CONFIG_DIR` 的 `--print` 通过，结果
  文本正确、最终 usage 非零、未泄漏内部调度术语。
- 低频资源观察跨越一个扫描周期：RSS `37760 KiB -> 26320 KiB`，打开文件
  `43 -> 42`，ESTABLISHED `15 -> 14`，未见增长或残留监听。

## 验证限制

- 当前环境 Rust 为 `1.98.0`，仓库 Clippy baseline 脚本固定要求 `1.92.0`；
  直接 `-D warnings` 会暴露仓库既有告警，不能作为本次新增告警的有效基线结果。
  未修改无关代码，也未更新 baseline；应在 CI 的 Rust `1.92.0` 环境完成最终
  baseline 检查。
- 未对生产 `152.53.243.159:59137` 做任何修改、重启、压测或配置写入；本次真实
  worker 证据仅来自项目指定的本地验证实例。
