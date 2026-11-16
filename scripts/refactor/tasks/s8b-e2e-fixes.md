# 任务 s8b：端到端走查缺陷修复与阶段 8 收尾

## 背景

阶段 7 提交后我用隔离实例（mock Anthropic 上游 + 临时 PG/Redis）做了端到端走查，发现 2 个真实缺陷和若干 Kiro 残留。阶段 8 第一轮已提交（README/文档/Docker/CI），本任务是阶段 8 的收尾。

## 缺陷 A（严重）：请求侧 AppState 持有私有的 RuntimeConfigStore，管理端改配置对请求路径不生效

现象：`PUT /api/admin/routes` 把 `/cc` 路由 `enabled=false` 后，`POST /cc/v1/messages` 仍返回 200；下游密钥 `allowedRoutes=["/na"]` 后打 `/cc` 仍 200 而不是 403。但 `/routes/effective` 能看到改动，说明管理端的 store 已更新。

根因：`src/anthropic/middleware.rs:342` `AppState::new` 内部 `RuntimeConfigStore::new(initial, None)` 新建了一个**私有** store，`main.rs` 构造 `anthropic::create_router` 时（`src/main.rs:495`）通过 `AnthropicRouterDependencies` 并没有把主 store 传进去；`with_routes`/`with_accounts`/`with_*` 一系列方法都往这个私有 store 里 `install_local`。阶段 1 的设计要求（`docs/claude-pool-refactor/02` 第 1 节、`09` 阶段 1）是全系统只有一个 `RuntimeConfigStore`。阶段 4 时 `from_app_state` 还从 `state.runtime.snapshot()` 读 `compiled_routes`，于是请求侧永远看到启动时的路由。

修复（最小改动）：
1. `AnthropicRouterDependencies` 加字段 `runtime: Arc<RuntimeConfigStore>`；`main.rs` 传 `runtime.clone()`。
2. `AppState` 加 `pub fn with_runtime(mut self, runtime: Arc<RuntimeConfigStore>) -> Self`，`create_router` 在 `AppState::new` 后**第一步**调用它，再调其余 `with_*`。
3. 删除 `AppState` 上那些与 `Config` 重复的配置副本字段（`reported_usage`、`cache_policy`、`routes`、`model_mapping`、`prompt_injection`、`route_features`、`accounts`、`image_processing`、`payload_shaping`、`missing_max_tokens`、`payload_guard_*`、`model_resolution_mode`、`compat_profile`），以及对应的 `with_*` 方法里往私有 store `install_local` 的代码；`RequestRuntimeConfig::from_app_state` 改为完全从 `state.runtime.snapshot()` 构造（`from_config_with_defaults` 已存在，直接用它），`from_config_with_fallback` 的 fallback 分支删掉。`AppState` 只保留运行时对象（store、密钥索引、recorder、tracker、controller、catalog、account_manager）和 prompt_cache 的几个模拟参数（它们也应从 store 读，若改动面太大可暂留但要在汇报里说明）。
4. `AnthropicRouterConfig` 若因此变空则删除；`from_runtime_config` 删除；`create_router(dependencies)` 单参数。
5. 三处 router 测试（`router.rs:602/668/736`）与 `handlers/tests.rs`、`passthrough_tests.rs` 里的 `AppState::new(...)` 调用按新签名适配：测试需要自定义配置时用 `state.runtime.install_local(|c| ...)`。
6. 新增回归测试 `admin_route_update_is_visible_to_request_path`：构造共享 store，经 store `update` 禁用 `/cc`，再经 anthropic router 发请求，断言 404；以及 `allowed_routes` 经 store 更新后请求返回 403。

## 缺陷 B（中）：账号 JSON 省略空数组导致前端崩溃

现象：`/ui/routes/_root` 编辑页白屏 `Cannot read properties of undefined (reading 'some')`，来自 `ui/src/features/routes/route-utils.ts:44` 的 `account.routeRules.some(...)`。根因：`src/account/mod.rs:622/626/590` 对 `supported_models`、`route_rules`、`header_overrides` 标了 `skip_serializing_if = "Vec::is_empty"/"HashMap::is_empty"`，空集合不出现在 JSON 里，而前端类型 `ui/src/types/accounts.ts:192/211/213` 声明为必填。

修复：删除 `Account` 结构上这三个 `skip_serializing_if`（保留 `default`），让空数组/空对象照常序列化为 `[]`/`{}`。检查 `src/admin/config_transfer.rs` 的导出 round-trip 测试仍通过（空数组出现在导出包里是正确的）。前端 `route-utils.ts:44` 与 `route-scenarios/account-rules.ts:7-15` 同时加 `?? []` 兜底（防御，不是主修复）。

## 残留 C：账号列表响应键名 `pools`

`GET /api/admin/accounts` 返回 `{"pools": [...]}`，`GET /api/admin/accounts/status` 每项是 `{"pool": {...}, ...}`。改为 `{"accounts": [...]}` 与 `{"account": {...}, ...}`：
- 后端：`src/admin/handlers.rs:493` 的 `json!({ "pools": pools })` → `"accounts"`；`src/account/mod.rs:836` `AccountStatus.pool` → `account`（字段改名，serde 自动变 camelCase）；`src/account/mod.rs:2161/2261` 诊断 JSON 里的 `"pool"` → `"account"`；`src/admin/service.rs:1276/1288` 随字段名改。
- 前端：`ui/src/types/accounts.ts:220/224/248` 与 7 个使用文件（`grep -rln '\.pools\b\|pools:\|\.pool\b\|pool: Account' ui/src`）同步；`account-utils.ts` 里 `poolFormFromPool`、`poolModelMappingSummary` 等函数名改为 `accountFormFromAccount`、`accountModelMappingSummary` 等。

## 残留 D：阶段 8 验收口径内的 Kiro 字样

| 文件 | 处理 |
|---|---|
| `ui/index.html` | `<title>` 改 `Claude Pool 控制台`；主题 localStorage 键 `kiro-console:theme` 改 `claude-pool-console:theme`（与 `ui/src/app/theme.tsx:5` 已有的键一致） |
| `ui/package.json` | `"name": "claude-pool-ui"` |
| `.env.example` | 内容是 Kiro 时期的保活示例，变量名也是旧的；**删除**（README 已覆盖环境变量说明） |
| `.vscode/settings.json` | `/tmp/kiro-rs-rust-analyzer-target` 是本机共享编译目录路径，**不改**（改了会导致全量重编）；把该文件加入 verify 的排除项 |
| `.codex/skills/kiro-*`（3 个目录，git 跟踪） | 是 Kiro 时期的 codex 技能，**删除整个 `.codex/skills/`** |
| `tests/migration/tests.rs:44/57` | 测试里引用旧配置键 `kiroRsTool` 与说明文字 `Kiro/旧顶层字段`，属于迁移测试的合法输入，**保留**；verify 排除 `tests/migration/` |
| `tests/migration/fixtures/legacy-sample.json` | 旧导出包样本，合法，保留并排除 |
| `ui/dist/index.html` | 构建产物，改完 `ui/index.html` 后 `pnpm --dir ui build` 重新生成即可（`ui/dist` 不在 git 跟踪内） |

## 文档

`docs/claude-pool-refactor/13-阶段8实施验收.md` 改名为 `13-实施验收记录.md`，在开头加"端到端走查（2026-11-17）"一节，记录：走查环境（mock 上游 39090、隔离 PG 库 `claude_pool_e2e`、Redis db 12、服务 19390、debug 二进制）、走查步骤与结果（创建账号 ✓、模型发现返回 3 个模型 ✓、/cc 非流式与流式 usage 按 current_high_cache 整形 ✓、/na 原样透传 ✓、usage 记录 raw/shaped/reported 三层齐全 ✓、`/routes` PUT 改 cacheType 后 `/routes/effective` 即时反映 ✓、导出 → dry-run 导入全部无变化 ✓、529 overloaded ✓）、发现的缺陷 A/B/C 及本任务的修复；末尾把"s8 整体未通过"改为按本任务完成后的实际结果写。

## 允许改动的文件

`src/anthropic/**`、`src/main.rs`、`src/account/mod.rs`（只改 C 与 B 列出的位置）、`src/admin/**`、`ui/**`（`ui/node_modules` 除外）、`.codex/`（删除）、`.env.example`（删除）、`docs/claude-pool-refactor/13-*.md`、`scripts/refactor/verify.sh`（只改 s8 分支的排除项：加 `--exclude-dir=.vscode --exclude-dir=migration`）。

受保护整形文件不得改动（同前）。

## 自检

```
cargo fmt --all
cargo build -q --bins
cargo test --bin claude-pool -q -- golden_usage
cargo test --bin claude-pool -q -- admin_route_update_is_visible_to_request_path
cargo test --bin claude-pool -q 2>&1 | tail -30
pnpm --dir ui check && pnpm --dir ui build
grep -rn '"pools"\|\.pool\b' src/admin src/account/mod.rs ui/src --include='*.rs' --include='*.ts' --include='*.tsx' | grep -v "pool_id\|pool_name\|postgres_store.pool()\|UsageOverviewPool\|\.pool()" | head   # 应为空
grep -rni "kiro" . --exclude-dir=.git --exclude-dir=target --exclude-dir=node_modules --exclude-dir=claude-pool-refactor --exclude-dir=refactor --exclude-dir=.vscode --exclude-dir=migration --exclude=config.toml 2>/dev/null | grep -v "src/migration/" | grep -v "^./ui/node_modules"   # 应为空
grep -n "skip_serializing_if" src/account/mod.rs | grep -c "Vec::is_empty\|HashMap::is_empty"   # 应为 0
```
