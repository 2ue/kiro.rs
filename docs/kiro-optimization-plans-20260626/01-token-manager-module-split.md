# 账号调度模块拆分实施方案

## 适用范围

本方案只处理 `src/kiro/token_manager.rs` 的结构拆分，第一阶段不得改变调度行为、配置语义、错误返回、usage 记录、Redis key、接口签名和管理端数据格式。

当前 `src/kiro/token_manager.rs` 已经承担本地账号调度、RPM、并发 lease、冷却、session sticky、Redis 同步、刷新锁、管理态快照、队列等待、健康分、路由状态判断等职责。文件体量超过 1.1 万行，继续追加功能会提高误改概率。

## 来源项目与学习点

- `kirocc-prox/internal/pool/selector.go`：选择逻辑独立成 Selector，策略和运行态分开。
- `kirocc-prox/internal/pool/conductor_default.go`：调度编排层只负责拿账号、释放账号、处理 retry，不直接做请求转换。
- `kirocc-prox/internal/pool/runtime_state.go`：运行态字段集中管理，降低并发状态泄漏风险。
- `ndycode/kiro-rs/src/kiro/token_manager/*`：Rust 版本按模块拆分，比单文件更适合后续演进。

## 当前项目现状

必须保留的能力：

- `MultiTokenManager` 对外类型名和主要方法保持不变。
- 本地账号 RPM 限制、单账号并发限制、全局并发限制保持不变。
- session sticky、sticky 换号、sticky 回退保持不变。
- Redis lease、in-flight lease、summary cache、runtime sync 保持不变。
- `priority`、`balanced`、`health_balanced` 调度模式保持不变。
- `credential_rpm`、`credential_max_concurrent_requests` 等历史配置字段保持兼容。
- usage、call trace、管理端 snapshot 字段保持兼容。

当前主要问题：

- 调度策略、容量判断、Redis 操作、冷却判断混在同一层，难以独立测试。
- 新增调度失败原因、健康分解释、压测诊断时容易侵入热路径。
- 文件过大导致 review 难以判断变更是否改变行为。

## 目标

- 将单文件拆成清晰模块。
- 第一阶段只做搬迁和 re-export，不改变行为。
- 第二阶段再在清晰模块内追加结构化失败原因、健康分 breakdown 等能力。
- 新模块必须能单独写单元测试。

## 非目标

- 不重写调度算法。
- 不修改优先级含义。
- 不修改 Redis key。
- 不修改数据库结构。
- 不修改对外接口和管理端返回字段。
- 不引入新的 async runtime、actor 框架或全局锁模型。

## 目标目录结构

必须将 `src/kiro/token_manager.rs` 迁移为目录模块：

```text
src/kiro/token_manager/
  mod.rs
  types.rs
  manager.rs
  account_state.rs
  capacity.rs
  rpm.rs
  concurrency.rs
  cooldown.rs
  sticky.rs
  strategy.rs
  queue.rs
  redis_runtime.rs
  refresh.rs
  admin_snapshot.rs
  route_state.rs
  tests.rs
```

每个模块职责必须固定如下：

- `mod.rs`：只做模块声明、公开 re-export、兼容旧路径。
- `types.rs`：放置跨模块共享类型，例如 `AcquireMode`、`CallContext`、`LocalPoolRouteStateKind`、`LocalPoolRouteState`。
- `manager.rs`：放置 `MultiTokenManager` 主结构和对外方法编排。
- `account_state.rs`：放置单账号运行态、健康态、冷却态的纯数据结构。
- `capacity.rs`：放置“账号是否可接请求”的判断入口，不直接写 Redis。
- `rpm.rs`：放置 RPM window 计算和命中判断。
- `concurrency.rs`：放置本地并发和全局并发 lease 占用/释放。
- `cooldown.rs`：放置错误分类到冷却时间的映射。
- `sticky.rs`：放置 session binding、sticky 命中、sticky 回退。
- `strategy.rs`：放置排序、打分、随机 top-k、健康权重。
- `queue.rs`：放置 dispatch wait、队列长度、等待超时。
- `redis_runtime.rs`：放置 Redis key 读写、lease 同步、summary cache。
- `refresh.rs`：放置 token refresh、refresh lock、刷新失败处理。
- `admin_snapshot.rs`：放置管理端 snapshot、可用性统计、展示字段拼装。
- `route_state.rs`：放置 `/cc`、`/ha`、`/na`、`/dfcache/*` 路由级状态。
- `tests.rs`：只放模块内单元测试；跨模块集成测试放 `tests/` 目录。

## 公开 API 兼容要求

拆分后下列路径必须继续可用：

```rust
crate::kiro::token_manager::MultiTokenManager
crate::kiro::token_manager::AcquireMode
crate::kiro::token_manager::CallContext
crate::kiro::token_manager::LocalPoolRouteState
crate::kiro::token_manager::LocalPoolRouteStateKind
```

`mod.rs` 必须显式 re-export：

```rust
pub use manager::MultiTokenManager;
pub use types::{
    AcquireMode,
    CallContext,
    LocalPoolRouteState,
    LocalPoolRouteStateKind,
};
```

不得让其他模块直接依赖拆分后的私有模块路径，除非该模块被明确标记为 `pub(crate)` 并有稳定职责。

## 新增或调整的数据结构

第一阶段只允许新增内部结构，不得改变序列化字段。

建议新增：

```rust
pub(crate) struct AccountRuntimeView<'a> {
    pub account_id: u32,
    pub priority: i32,
    pub enabled: bool,
    pub model_supported: bool,
    pub in_flight: u32,
    pub max_concurrent: u32,
    pub rpm_used: u32,
    pub rpm_limit: u32,
    pub cooldown_until_ms: Option<i64>,
    pub health_score: f64,
}

pub(crate) struct CapacityDecision {
    pub eligible: bool,
    pub waitable: bool,
    pub reason_code: &'static str,
}
```

`CapacityDecision` 第一阶段只用于内部搬迁，不得写入对外响应。

## 配置与兼容策略

- 不新增配置。
- 所有配置读取必须仍通过现有 config 类型。
- 不改配置默认值。
- 不改配置文件序列化字段名。
- 不改环境变量读取逻辑。

## 实施步骤

1. 新建 `src/kiro/token_manager/` 目录。
2. 将原 `src/kiro/token_manager.rs` 改名为 `src/kiro/token_manager/manager.rs`。
3. 新建 `src/kiro/token_manager/mod.rs`，re-export 原对外类型。
4. 保持编译通过后，再按函数依赖从低风险模块开始搬迁：
   - 先搬纯类型到 `types.rs`。
   - 再搬纯计算函数到 `rpm.rs`、`cooldown.rs`、`strategy.rs`。
   - 再搬不改变 Redis key 的包装函数到 `redis_runtime.rs`。
   - 最后搬 sticky、queue、admin snapshot。
5. 每搬一个模块必须执行 `cargo test` 对应测试集合。
6. 每次搬迁不得同时修改逻辑表达式；只允许移动代码、调整可见性、补充 re-export。
7. 搬迁完成后再单独提交格式化改动。

## 测试方案

必须新增或保留以下测试：

- `token_manager_priority_order_is_unchanged`
- `token_manager_balanced_order_is_unchanged`
- `token_manager_health_balanced_order_is_unchanged`
- `token_manager_rpm_window_is_unchanged`
- `token_manager_concurrency_lease_releases_on_drop`
- `token_manager_sticky_binding_is_unchanged`
- `token_manager_route_state_for_builtin_routes_is_unchanged`
- `token_manager_route_state_for_dfcache_is_unchanged`
- `token_manager_admin_snapshot_schema_is_unchanged`

测试断言要求：

- 同一输入账号集合，拆分前后的选中账号 ID 必须一致。
- 同一请求结束路径，lease 释放次数必须一致。
- 同一 Redis mock，读写 key 必须一致。
- 同一配置，管理端 JSON 字段必须一致。

## 验收标准

- `cargo test` 通过。
- `cargo clippy --all-targets --all-features` 不新增 warning。
- `rg "pub struct MultiTokenManager" src/kiro/token_manager` 只能命中新的 manager 模块。
- 对外 API 编译路径不变。
- 功能日志中不新增对下游可见字段。
- 生产配置无需迁移即可启动。

## 风险与回滚

主要风险是移动过程中改变私有函数调用顺序。规避方式：

- 每个搬迁提交只移动一类函数。
- 复杂函数先保持在 `manager.rs`，等测试覆盖后再搬。
- 如果某一步出现行为差异，直接回滚该步搬迁，不影响前面已经稳定的模块。

回滚策略：

- 保留旧文件搬迁 commit 边界。
- 若线上发现调度异常，回滚到拆分前最后一个稳定 tag。
- 不允许在同一发布里同时做大拆分和调度算法变更。

## 不得做的事项

- 不得借拆分机会重写调度。
- 不得把 Redis key 改名。
- 不得把内部 `credential_*` 配置字段强行改名。
- 不得修改对外错误文案。
- 不得把模块拆分和 UI 改造混在同一个变更里。

## 后续可选扩展

拆分稳定后，再在 `capacity.rs` 中接入结构化失败原因，在 `strategy.rs` 中接入健康分 breakdown，在 `admin_snapshot.rs` 中输出可解释调度信息。

