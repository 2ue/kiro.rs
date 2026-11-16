# 任务 s1-runtime-store：剥离运行时配置持有者（阶段 1）

## 目标

新建独立的 `RuntimeConfigStore`，让运行时配置的读取、更新、从 PostgreSQL 重载、代次号全部由它负责；`MultiTokenManager` 与 `KiroProvider` 不再是配置的持有者，只保留对 store 的引用并转发。`handlers.rs`、`admin/service.rs`、`main.rs` 对配置的访问改为经 store。**这个阶段不删除任何 Kiro 功能**，全部现有单测必须保持通过。

## 允许改动的文件

- 新建 `src/model/runtime.rs`，并在 `src/model/mod.rs` 导出
- `src/kiro/token_manager/manager.rs`：把 `config: RwLock<Arc<Config>>` 与 `config_generation` 两个字段替换为 `runtime: Arc<RuntimeConfigStore>`；`runtime_config()`、`runtime_config_snapshot()`、`runtime_config_snapshot_with_generation()`、`install_runtime_config()`、`update_runtime_config()`、`reload_runtime_config_from_postgres()`、`update_load_balancing_mode_in_config()` 改为调用 store；其余 Kiro 侧的联动逻辑（warmup 重置、auxiliary 限额、rpm、cohort 失效、dispatch 通知、publish）保留在 manager 里，由一个 `apply_runtime_config_side_effects(previous, current)` 统一调用
- `src/kiro/token_manager/mod.rs` 与 manager 的构造函数：接收 `Arc<RuntimeConfigStore>`
- `src/kiro/provider.rs`：`runtime_config*` 三个方法改为转发 store（可以继续经 token_manager 转发，但 token_manager 内部必须是 store）
- `src/anthropic/middleware.rs`：`AppState` 新增 `runtime: Arc<RuntimeConfigStore>` 字段
- `src/anthropic/handlers.rs`：`request_runtime_config`（约 1626 行）改为从 `state.runtime` 取快照与代次，缓存 key 中的 `provider` 指针改为 store 指针；`live_runtime_config`（约 7891 行）、`request_image_processing_config`、dfcache 路由定义读取（约 7948 行）等所有 `state.kiro_provider.as_ref().map(|p| p.runtime_config_snapshot…)` 改为 `state.runtime`
- `src/admin/service.rs`：全部 35 处 `self.token_manager.runtime_config()` 改为 `self.runtime.runtime_config()`；`update_runtime_config` 相关调用改为 `self.runtime.update(...)`（Kiro 侧副作用由 store 的订阅回调触发，见下）
- `src/main.rs`：构造 store，注入 token_manager、AppState、AdminService；Redis 事件监听（约 1566-1660 行）里的 `token_manager.reload_runtime_config_from_postgres()` 与 `token_manager.runtime_config()` 改为 store
- 各处测试辅助构造（`handlers/tests.rs`、`admin/service_tests.rs`、`external_pool/tests.rs` 等）按新签名补字段，不改断言

## `RuntimeConfigStore` 契约

```rust
pub struct RuntimeConfigStore { /* RwLock<Arc<Config>>, AtomicU64 generation, Option<Arc<PostgresStore>>, listeners */ }

impl RuntimeConfigStore {
    pub fn new(initial: Config, postgres: Option<Arc<PostgresStore>>) -> Arc<Self>;
    pub fn snapshot(&self) -> Arc<Config>;
    pub fn snapshot_with_generation(&self) -> (Arc<Config>, u64);
    pub fn generation(&self) -> u64;
    /// 独立副本（现有 runtime_config() 语义）
    pub fn config(&self) -> Config;
    /// 修改 → 校验 → 写 PG（递增 version）→ 安装新快照 → 通知订阅者。返回保存后的 PG version。
    pub fn update(&self, mutate: impl FnOnce(&mut Config)) -> anyhow::Result<Option<i64>>;
    /// 从 PG 重载；没有 PG 或没有行返回 Ok(false)。成功安装后通知订阅者。
    pub fn reload_from_postgres(&self) -> anyhow::Result<bool>;
    /// 只换内存快照不写 PG（现有 update_load_balancing_mode_in_config 的语义）
    pub fn install_local(&self, mutate: impl FnOnce(&mut Config));
    /// 订阅配置变化：回调收到 (previous, current, reason)。用于 Kiro manager 的副作用与 external_pool 的策略失效。
    pub fn subscribe(&self, listener: Arc<dyn Fn(&Config, &Config, RuntimeConfigChangeReason) + Send + Sync>);
}
pub enum RuntimeConfigChangeReason { Updated, Reloaded, LocalInstall }
```

- 代次号沿用现有 `next_config_generation()` 的生成方式（搬到 runtime.rs）。
- 现有 `validate_token_refresh_admission_config(&updated)?` 校验保留：store 提供 `set_validator(Box<dyn Fn(&Config) -> anyhow::Result<()>>)`，由 Kiro manager 在构造时注册，这样阶段 2 删 Kiro 时只需不再注册。
- `block_on_credential_pgsql` 这种在同步上下文里等待 PG 的做法沿用（搬到 runtime.rs 或 common），保持调用方签名同步。
- Redis `publish_runtime_config_changed` 仍由 manager 在订阅回调里做（它需要 manager 的实例 id），不要搬进 store。

## 不要做的事

- 不删任何 Kiro 字段、函数、端点。
- 不改 `Config` 结构。
- 不改任何测试断言；测试只允许因构造函数签名变化而补参数。

## 自检

```
cargo fmt --all
cargo build -q --bin kiro-rs
grep -n "token_manager\.runtime_config\|provider\.runtime_config_snapshot" src/anthropic/handlers.rs src/anthropic/handlers/*.rs src/admin/*.rs src/main.rs   # 应为空
cargo test --bin kiro-rs -q 2>&1 | tail -30     # 全部通过（集成测试因无环境变量跳过属正常）
git status --short
```
