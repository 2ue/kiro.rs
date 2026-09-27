# kiro-rs 服务问题审计（v0.0.170）

- 审计日期：2026-09-24
- 代码基线：`main` 分支，HEAD `79d8de8`，`Cargo.toml` version = 0.0.170
- 审计方式：只读静态审计 + `cargo clippy` 实测，未修改任何代码
- 排序依据：业务影响，而非缺陷数量

---

## 结论摘要

| # | 问题 | 类别 | 严重度 | 修复成本 |
|---|---|---|---|---|
| 1 | 同步阻塞桥占用 Tokio 工作线程 | 性能/架构 | 高 | 高（架构改造） |
| 2 | Admin Key 轮换不生效于整个集群 | 安全 | 高 | 低 |
| 3 | Admin 认证锁中毒导致空 key 绕过 | 安全 | 中高（潜在） | 低 |
| 4 | 同一 Redis 操作维护同步/异步两份实现 | 可维护性 | 中 | 随 #1 消解 |
| 5 | 模块巨型化与 Provider 耦合 | 交付速度 | 中 | 高（已有计划） |
| 6 | Clippy 基线债务上限偏高 | 代码质量 | 低中 | 中（机械） |
| 7 | 双前端重复构建 | 构建成本 | 低 | 中（已有计划） |
| 8 | 仓库卫生问题 | 卫生 | 低 | 低 |

已验证**不是**问题的项见文末"排除清单"，避免后续重复排查。

---

## 1. 同步阻塞桥占用 Tokio 工作线程（吞吐天花板）

### 现状

服务里存在**三套独立的 sync-over-async 桥**，共 63 个调用点，在同步函数内用 `block_in_place` + `block_on` 执行 Postgres / Redis / HTTP：

| 桥函数 | 定义位置 | 调用点数 | 后备 runtime |
|---|---|---|---|
| `block_on_storage` | `src/kiro/token_manager/storage_task.rs:677` | 17（其中 16 个在 `manager.rs`） | 独立 2 线程 `kiro-storage-task`（同文件 :654） |
| `block_on_usage_store` | `src/anthropic/usage.rs:3628` | 14 | 独立 2 线程 `kiro-usage-store`（同文件 :3677） |
| `block_on_admin_store` | `src/admin/service.rs:9098` | 32 | **复用当前 runtime**（`handle.block_on`） |

补充一处同形状写法：`src/token.rs:122`，在同步函数 `count_all_tokens` 内 `Handle::current().block_on(call_remote_count_tokens(...))`。

`grep -rn "worker_threads(2)" src | grep -v tests` 精确命中 2 处，确认两个隔离 runtime 的线程数都硬编码为 2。

### 风险分析

风险点不在调用数量，而在**形状差异**：

- 前两套至少隔离到了专用 runtime，但 `worker_threads(2)` 意味着调度决策中的 Redis/PG 访问最多两路并行，第三个请求排队等待。
- 第三套（以及 `src/token.rs:122`）直接在**当前** runtime 上 `block_on`，这是能够把请求 runtime 拖到饥饿的写法。

更关键的是，这些调用点**落在请求热路径上**，不是后台任务：

```
src/kiro/token_manager/manager.rs
  :5414  读取 Redis 会话绑定          ← 每个带 session 的请求
  :5443  原子写入 Redis 会话绑定      ← 每个带 session 的请求
  :5495  按凭据删除 Redis 会话绑定
  :5538  删除 Redis 凭据会话绑定
  :5571  原子记录 Redis 会话软失败
  :5621  原子清理 Redis 会话软失败
  :3565  热态结果回写
  :4960  热态结果回写
  :9881  从 Redis 同步调度运行态
  :9918  从 Redis 同步指定凭据调度运行态
  :4670  清理 Redis 超时并发 lease
  :4791  清理 Redis 凭据并发占用
  :7913  保存凭据到 PgSQL
  :8136  原子更新凭据及运行态
```

代码本身已经意识到这点：两个桥都内置了 ≥100ms 的 `tracing::warn!` 自诊断。也就是说"慢"是已知事实，只是被当作可观测指标，而没有当作缺陷处理。

### 业务后果

并发爬升时调度被串行化，P99 延迟随凭据池规模恶化，且表现形式是"偶发卡顿"而非明确错误，归因困难。这是当前架构里唯一会**限制横向扩容收益**的结构性问题 —— 加机器不能线性换来吞吐。

### 修复方向

把 `manager.rs` 的调度决策路径改造为 `async fn`，让 Redis/PG 访问回到 await。

**不要**通过调大 `worker_threads(2)` 来缓解 —— 那只是把阻塞挪远一点，属于典型的临时绕过，不解决工作线程被占用的本质。

---

## 2. Admin Key 轮换不生效于整个集群（安全）

### 现状

`src/main.rs:1278-1377` 的 `spawn_redis_runtime_event_listener` 在配置变更通道消息和 60s `periodic_reload` 定时器上会刷新：

```rust
request_api_key_store.replace_keys(config.request_api_keys());
request_admission.update_config(config.request_admission);
```

但它的参数列表是 `(redis_store, token_manager, external_pool_manager, request_api_key_store, request_admission, health)` —— **根本没有 `AdminState`**，因此从不刷新 `admin_api_key`。

相关事实：

- `AdminState::new` 仅在 `src/main.rs:663` 调用一次
- `set_admin_api_key` 仅被 `src/admin/handlers.rs:1224` 调用
- `src/admin/service.rs:1493-1527` 的 `update_admin_api_key` 校验（非空、≥8 字符）、通过 `token_manager.update_runtime_config` 持久化、用 `mask_secret` 记审计 —— 单机语义是完整的

### 业务后果

多实例部署下，管理员轮换 admin key 时：生效范围只有处理该请求的那一台实例，**其余实例继续接受旧 key 直到重启**。即轮换不具备吊销能力。

对比之下，下游请求 API key 的轮换是正确广播的，这说明这是遗漏而非有意设计。

### 修复方向

把 `AdminState`（或一个可共享的 admin key 句柄）传入 Redis 配置监听器，在配置变更与定时重载时一并刷新。

---

## 3. Admin 认证锁中毒导致空 key 绕过（安全，潜在）

### 现状

`src/admin/middleware.rs:34-39`：

```rust
pub struct AdminState {
    admin_api_key: Arc<RwLock<String>>,   // std::sync::RwLock，全仓唯一一处
    pub service: Arc<AdminService>,
}

pub fn current_admin_api_key(&self) -> String {
    self.admin_api_key.read().map(|guard| guard.clone()).unwrap_or_default()
}
```

`admin_auth_middleware` 的判定：

```rust
Some(key) if auth::constant_time_eq(&key, &state.current_admin_api_key()) => next.run(request).await,
```

### 风险链

1. 持写锁期间发生 panic → 锁中毒（`std::sync::RwLock` 语义）
2. 此后 `current_admin_api_key()` 永久返回 `""`（被 `unwrap_or_default()` 吞掉）
3. `extract_api_key` 对"存在但为空"的 `x-api-key` 请求头返回 `Some("")`
4. `constant_time_eq("", "")` 为真 → **管理后台变为无认证开放**

值得注意的是 `src/main.rs:636-646` 已经专门防御过"空配置 key 绕过"，注释写着"安全检查：空字符串被视为未配置，防止空 key 绕过认证" —— 说明这个威胁模型是想到了的，但锁中毒路径绕过了那道闸。

另外全仓其他地方一律使用 `parking_lot`（无中毒概念），这里是唯一孤例。

### 修复方向

二者任一即可闭环，建议都做：

- 改用 `parking_lot::RwLock`，消除中毒语义
- 认证中间件对空 key 无条件拒绝（不进入比较）

---

## 4. 同一 Redis 操作维护同步/异步两份实现

`manager.rs` 中以下三组桥函数各自都有配对的 `await_*` 版本，且各自带独立的熔断器与超时配置：

- `block_on_scheduler_redis_affinity` / `await_scheduler_redis_affinity`
- `block_on_scheduler_redis_hot_outcome` / `await_...`
- `block_on_scheduler_redis_state_sync` / `await_...`

同一语义两条状态机、两套熔断状态。后续只改一边就会产生"同步路径已修、异步路径仍旧"的静默分歧，而这种分歧在测试里很难覆盖到。

这是问题 #1 的技术债衍生物，解决 #1 时会自然消解，不建议单独投入。

---

## 5. 模块巨型化与 Provider 耦合（交付速度）

### 文件体量

| 文件 | 行数 | 备注 |
|---|---|---|
| `src/external_pool/tests.rs` | 19,901 | 测试 |
| `src/storage/postgres.rs` | 19,276 | 生产区约止于 12,482 行 |
| `src/external_pool.rs` | 15,850 | 240 个函数 |
| `src/kiro/provider.rs` | 13,630 | 264 个函数 |
| `src/kiro/token_manager/manager.rs` | 13,419 | |
| `src/kiro/token_manager/manager_tests.rs` | 13,298 | 测试 |
| `src/storage/redis_cache.rs` | 11,912 | 生产区约止于 7,521 行 |
| `src/anthropic/handlers.rs` | 11,245 | |
| `src/admin/service.rs` | 9,109 | |

全仓 243,360 行 Rust，其中 46,531 行在 `*tests*.rs`，382 个 `#[cfg(test)]` 块，7,076 个函数。

### 函数参数量

- `src/anthropic/handlers.rs:9538` —— **19 个参数**
- `src/anthropic/middleware.rs:271`、`src/kiro/provider.rs:8959` —— 各 15 个
- `src/anthropic/handlers.rs:1605`、`src/storage/redis_cache.rs:3666` —— 各 13 个
- `src/anthropic/handlers.rs:6025`、`admin_snapshot.rs:437` —— 各 12 个
- 全仓 34 处 `too_many_arguments`

### 耦合现状（与仓库自有诊断一致）

`docs/refactor-plan/01-现状与不可回归不变量.md` 记录的三点经本次审计验证全部成立：

1. `AnthropicRouterDependencies` 同时持有 Kiro 与外部池的具体管理器 —— `src/main.rs:620-632` 并列传入 `kiro_provider` 和 `external_pool_manager`
2. `ExternalPoolManager` 直接引用 Kiro 侧的 `anthropic::cache`、`prompt_cache`、`model_processing`，并借 Kiro storage task 写状态
3. `RuntimeConfig` 把 Kiro 请求体转换、外部路由、缓存策略、上报用量混为一个整体，导致 UI 只能整体保存，无法字段级更新

### 该计划自身的风险

`docs/refactor-plan/` 目录**整体处于 untracked 状态**（未提交），且文档内写的是 v0.0.169 而 `Cargo.toml` 已到 0.0.170。计划正在漂移，这是它当前最现实的风险 —— 在动手重构前应先提交并对齐版本，否则会退化成摆设。

---

## 6. Clippy 基线债务上限偏高

- `scripts/ci/clippy-baseline.json` 允许 **849 条告警 / 127 个 lint-文件桶**，锁定 rustc 1.92.0
- `--bin kiro-rs` 实测 **286 条生产告警**
- 棘轮机制（`scripts/ci/check-clippy-baseline.mjs`）本身设计正确：数量下降时 CI 会失败并要求在专门的 lint 清理变更中用 `--update` 下调基线

问题只是天花板偏高。主要构成：

| lint | 数量 | 性质 |
|---|---|---|
| `collapsible_if` | 158 | 纯机械 |
| `too_many_arguments` | 34 | 与 #5 同源 |
| `derivable_impls` | 22 | 机械 |
| `result_large_err` | 7 | **有真实开销**，错误类型 ≥128/160/232 字节 |
| `type_complexity` | 5 | |
| `single_match` | 4 | |
| `needless_return` / `redundant_closure` | 3 / 3 | |

按文件集中度：`admin/service.rs` 41、`external_pool.rs` 29、`model/config.rs` 24、`redis_cache.rs` 23、`anthropic/handlers.rs` 20、`kiro/provider.rs` 19、`anthropic/usage.rs` 18、`anthropic/stream.rs` 17、`manager.rs` 15。

建议：`result_large_err` 优先（有性能含义），`collapsible_if` 可在某次独立的 lint 清理变更里批量处理并下调基线。

---

## 7. 双前端重复构建

`ui/` 与 `admin-ui/` 在每次 CI 和每次发布中都完整构建（pnpm install + build）并通过 `rust-embed` 嵌入二进制。

重构计划已将 `ui/` 定为基线、`admin-ui/` 标为分阶段退役。在退役完成之前，这是每次发布固定多付的构建时间与二进制体积成本。

---

## 8. 仓库卫生问题（低危）

- `.kilo/` 占 23MB，**0 个 tracked 文件**，且注册了一个陈旧 git worktree：`.kilo/worktrees/soapy-clipper`，detached HEAD `35ce08e`（`git worktree list` 可见），内含 `scripts/ci/` 的完整副本
- `feature/` 目录有 **264 个 tracked 文件**（audits / evidence / issues / releases / tests / todo）提交进了仓库
- 根目录散落 `CLOUDFLARE_TIMEOUT_FIX.md`、`STREAMING_KEEPALIVE.md`，应归入 `docs/`
- 两个未跟踪的 `.DS_Store`（根目录与 `docs/`）
- CI 测试串行执行（`cargo test ... -- --test-threads=1`），随测试量增长会成为反馈速度瓶颈

---

## 排除清单：已验证不是问题

记录在此以免后续重复排查。

### 生产代码的 `unwrap()` 基本为零

早期粗略计数得到的数百次 `unwrap()`/`expect()` **全部落在文件内联的 `#[cfg(test)]` 区域**。按生产区行范围精确统计：

- `src/storage/postgres.rs` 第 1–12482 行 → **0**
- `src/storage/redis_cache.rs` 第 1–7521 行 → **0**
- `src/request_admission.rs` 第 1–1120 行 → **0**
- `src/anthropic/converter.rs` 第 1–635 行 → 1

### 缓存有界

- `prompt_cache.rs`：单账号 200 条、全局 20,000 条、24h TTL、256MB 估算上限，具备 `prune_expired_locked`
- `files.rs`：单文件 50MB、最多 128 文件、总量 256MB、并发上传 2

### 凭据保护到位

- `kiro-credentials-2026-09-16T12-11-07-416Z.json`（248KB 真实数据）权限 0600，被 `.gitignore:16`（`/kiro-credentials-*.json`）忽略
- `git ls-files` 中无任何凭据 / env / secret 文件，仅有 `.example` 变体
- `tmp/` 已正确忽略（`.gitignore:23`）

### 下游请求认证是干净的

`src/common/auth.rs`（175 行，全文核对）：

- `RequestApiKeyIdentity` 只存 SHA-256 的 `[u8; 32]`，自定义 `Debug` 仅打印 `stable_id`，不泄露密钥
- `constant_time_eq` 使用 `subtle::ConstantTimeEq::ct_eq`
- `RequestApiKeyStore` 使用 `parking_lot::RwLock<HashSet<[u8; 32]>>`，无锁中毒风险
- 请求路径上仅一次 SHA-256 + 一次内存查找，零 DB/Redis 访问

### 其他

- 优雅关闭存在（`src/main.rs:1021 / 1038 / 1067`）
- 非测试代码仅 1 处 `unbounded_channel`
- `cargo clippy --all-targets --all-features` 退出码 0
- CI（`.github/workflows/build.yaml`）覆盖完整：`check-diff.sh`、双前端构建、`check-frontend-contracts.mjs`、`cargo fmt --check`、clippy 基线校验、两轮 `cargo test`（含 `--no-default-features`）、`cargo check`、Postgres 集成测试、`cargo build --release --locked`

---

## 建议处理顺序

1. **问题 #2、#3** —— 小改动高收益，预计一两小时可闭环，安全语义立即修正。优先。
2. **问题 #1** —— 真正的架构工作。把调度决策路径改为 `async fn`。注意不要用调大线程数的方式绕过。
3. **提交并对齐 `docs/refactor-plan/` 到 0.0.170** —— 在动 #5、#7 之前必须先做，否则计划继续漂移。
4. **问题 #5、#7** —— 跟随已有重构计划分阶段推进。
5. **问题 #6** —— 在独立的 lint 清理变更中处理，`result_large_err` 先行。
6. **问题 #8** —— 随手清理，不需要专门排期。
