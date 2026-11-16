# 任务 s2b：管理层去 Kiro（阶段 2 第二步）

## 目标

`src/admin/` 不再依赖 `src/kiro/`：删除全部 Kiro 账号相关端点与服务，`AdminService` 不再持有 `token_manager` 与 `kiro_provider`。

## 设计依据

`docs/claude-pool-refactor/08-管理API与控制台.md` 第 1 节、`01-现状盘点与去留清单.md` 第 2.5 节。

## 允许改动的文件

`src/admin/**`、`src/main.rs`（只改 AdminService 装配）、`src/storage/account_events.rs`（删 local 分支）、`src/storage/credential_inventory.rs`（整文件删）、`src/storage/kiro_api_keys.rs`（整文件删）、`src/storage/mod.rs`。

## 删除

- 端点（`admin/router.rs`）：`/credentials*` 全部、`/inventory*`、`/proxy-resources*`、`/subscription-model-lists*`、`/config/kiro-version/*`、`/config/load-balancing`、`/public-pool`、`/api/public/pool`、`/route-secret`、`/model-capabilities/sync`、`/usage-dashboard/accounts`、`/usage-dashboard/overview/local`。
- 文件：`admin/inventory.rs`、`admin/public_pool.rs`。
- `admin/service.rs`：对应的方法、`token_manager` / `kiro_provider` 字段与 `AdminServiceDependencies` 同名字段、`credential_imports_in_flight`、Kiro 余额缓存 `balance:*`、admin 缓存里的 credentials 相关 key。保留外部池、运行配置、路由场景、密钥、usage、审计、诊断、模型价格/能力（手工录入与 LiteLLM 价格同步保留）。
- `admin/types.rs`：Kiro 凭据相关类型；`RuntimeConfigResponse` / `UpdateRuntimeConfigRequest` 里五个 Kiro 必填字段（`credentialRpm`、`credentialTransientCooldownSecs`、`credentialMaxCooldownSecs`、`credentialWarmupRequests`、`compressionEnabled`）本任务暂留（Config 字段还在），下一任务随 Config 一起删。
- `admin/route_scenarios.rs`：`local`、`local_accounts` 及其 preview/apply 逻辑。
- `admin/service_tests.rs`：删只测已删功能的测试，保留其余。

## 自检

```
cargo fmt --all
cargo build -q --bin kiro-rs
grep -rn "crate::kiro\|KiroProvider\|MultiTokenManager\|token_manager" src/admin   # 应为空
cargo test --bin kiro-rs -q -- golden_usage
cargo test --bin kiro-rs -q 2>&1 | tail -30
git status --short
```
