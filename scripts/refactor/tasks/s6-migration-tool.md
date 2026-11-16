# 任务 s6：从旧 kiro-rs 迁移的导出/导入工具（阶段 6）

## 目标

落实设计 `docs/claude-pool-refactor/07-存储与迁移.md` 第 4 节。两个子命令：

1. **导出**：`claude-pool export-legacy --from-postgres <旧库URL> --out <file.json>`。它直接连旧 kiro-rs 的 PostgreSQL（表名 `external_upstream_pools`、`runtime_config`、`route_scenarios`、`model_pricing`、`model_capabilities`），用 SQL 原样读出，不依赖旧 Rust 类型。这是**唯一允许出现旧表名与旧字段名字符串**的地方。
2. **导入**：`claude-pool import-legacy <file.json> [--apply]`。默认 dry-run 输出人类可读的差异报告（中文）；`--apply` 时在单事务里**全量覆盖**目标库的账号、运行配置、路由场景、模型价格、手工模型，然后发 `events:runtime_config_changed` 与 `events:account_data_changed`。

设计依据：07 章 4.1/4.2/4.3；memory 约定“配置迁移走导出导入、按自然键匹配、导入全量覆盖、机器本地字段不导出”。

## 先读

- `docs/claude-pool-refactor/07-存储与迁移.md` 第 4 节（字段映射表与行为差异提示）
- `docs/claude-pool-refactor/01-现状盘点与去留清单.md` 第 3.1 节（要丢弃的 Kiro 字段清单）、第 6 节（改名表）
- `src/model/arg.rs`（现有 `Command::Maintenance` 子命令模式，照它加两个新子命令）
- `src/model/config.rs`：当前 `Config` 结构、`migrate_path_overrides_to_routes`（阶段 4 产物）、`AccountsConfig`（阶段 5 改名后）
- `src/account/mod.rs`（阶段 5 改名后）的 `Account` 结构与 `CreateAccountRequest`
- `src/storage/postgres.rs` 的 `accounts` 表、`save_runtime_config_returning_version`、路由场景/模型价格/模型能力的写入函数

## 允许改动的文件

新建 `src/migration/mod.rs`、`src/migration/legacy_export.rs`、`src/migration/legacy_import.rs`、`src/migration/report.rs`；`src/model/arg.rs`、`src/main.rs`（接子命令）；`src/storage/postgres.rs`（只允许新增“单事务全量覆盖”的写入函数，不改现有函数）；`tests/migration/`（fixture 与测试）；`docs/claude-pool-refactor/07-存储与迁移.md` 第 4.1 节（把旧侧子命令改为由新二进制直连旧库导出，并把“`feat/config-package` 已提供配置包”这句删掉，因为核实后 main 上没有通用配置包功能）。

## 导出包格式

```jsonc
{
  "formatVersion": 1,
  "exportedAt": "2026-11-17T00:00:00Z",
  "exportedFrom": { "kind": "kiro-rs-postgres", "schemaFingerprint": "<sha256 of information_schema columns of the 5 tables>" },
  "accounts": [ { /* external_upstream_pools 每行的全部列，列名原样，deleted_at 非空的行跳过 */ } ],
  "runtimeConfig": { /* runtime_config.config 原样 JSON（id='default' 那一行）*/ },
  "routeScenarios": [ { "name", "description", "content" } ],
  "modelPricing": [ { 全部列 } ],
  "modelCapabilities": [ { 全部列，只导 source='manual' 的 } ]
}
```

导出不做任何转换，`api_key` 明文导出（加 `--redact-secrets` 时替换为 `"<redacted>"`，此时导入会拒绝 `--apply`）。

## 导入映射（dry-run 报告必须逐项列出）

| 旧 | 新 | 规则 |
|---|---|---|
| `accounts[].{name, base_url, api_key, auth_type, header_profile, append_beta_query, header_overrides, wire_profile, tls_profile, enabled, priority, max_concurrent_requests, usage_projection_mode, stream_response_mode, auto_disable_policy, pre_output_stream_retry_mode, normalize_model_version_dots, model_mapping_mode, model_mapping_require_match, model_mapping_rules, supported_models, route_mode, route_rules, notes}` | `accounts` 同名字段 | 直接搬；`name` 是自然键，目标库已有同名账号则覆盖 |
| `accounts[].{request_body_mode, raw_model_mode, preserve_path, skip_non_stream_usage_projection, auto_disabled*, revision, id}` | 丢弃 | 报告列出每个账号被丢弃的字段与原值 |
| `accounts[].model_mapping_mode == "processed_mapping"` | 改为 `passthrough_mapping` | 报告高亮：该模式已删除 |
| `accounts[].usage_projection_mode` | 保留旧值 | 不改为新默认 |
| `runtimeConfig.externalPools.externalPoolXxx` | `accounts.xxx` | 去前缀；01 章 3.1 节里 `externalPools` 内要删的本地相关字段（`externalDirectPolicy*`、`directExternal*`、`localPool*`、`fallbackOn*`、`externalPoolLocalRescue*`、`localPoolCircuit*`、`externalPoolMaxInputTokens`、`externalPoolTransientFailureCooldownThreshold`）丢弃并列出 |
| `runtimeConfig.{cachePolicy.pathOverrides, reportedUsage.pathOverrides, promptInjection.pathOverrides, routeFeatures.pathOverrides, definedCacheRoutes, externalPools.externalPoolRouteMode/Rules}` | `routes[]` | 调用阶段 4 的 `migrate_path_overrides_to_routes` 同一函数，不重复实现 |
| `runtimeConfig.cachePolicy.{kiroRsTool, kiroRsToolPlus}`、各路由补丁里的同名键、`cacheType: kiro_rs_tool(_plus)` | `prefixTool`、`prefixToolPlus`、`prefix_tool(_plus)` | 改名 |
| `runtimeConfig.cachePolicy.*.cachePoint`、`cacheTtl.local` | 丢弃 | 列出 |
| `runtimeConfig.promptInjection.defaults[x].externalPool` | 丢弃 | 若原 `externalPool=false` 且 `enabled=true`，报告**高亮**：该注入项导入后会对账号生效 |
| `runtimeConfig.{missingMaxTokens, localRequestCompat, payloadGuard*, payloadTooLongHandling, payloadShaping, imageProcessing}` | `routeFeatures.defaults.*`（阶段 3 产物） | 原全局开启的值搬入 defaults；报告高亮：这些现在是默认关闭的路由功能，请确认哪些路由要开 |
| `runtimeConfig` 里 01 章 3.1 节的全部 Kiro 顶层字段 | 丢弃 | 列出（按字段名归组，不逐值） |
| `runtimeConfig.{apiKey, apiKeys, requestApiKeyPolicies, adminApiKey}` | 同名 | 搬；报告提示 adminApiKey 会被覆盖 |
| `runtimeConfig.{postgres, redis, observabilityRedis, host, port}` | 丢弃 | 机器本地字段永不导入 |
| `routeScenarios[].content.{local, localAccounts}` | 丢弃 | 其余搬，`name` 自然键 |
| `modelPricing`、`modelCapabilities` | 同名表 | `model` 自然键；capabilities 加 `listed=true` |

导入后的 `Config` 必须通过现有 `validate()`；校验失败则报告并拒绝 `--apply`。

## 行为差异提示（报告末尾固定输出，07 章 4.3 节）

1. 所有流量走账号池；列出账号池并发总量（各账号 `max_concurrent_requests` 之和）。
2. 原本只对本地生效的注入项现在对账号生效（逐项列出）。
3. 请求体不再规范化：原 `request_body_mode=normalized` 的账号列表。
4. `capacityMode`、`dispatchMaxWaitSecs`、`maxQueuedRequests` 默认值变化（仅当旧配置未显式设置时提示）。
5. 原全局路由功能现在默认关闭（逐项列出原开启的）。

## 测试

- `tests/migration/fixtures/legacy-sample.json`：手工构造一份覆盖所有映射行的旧导出包（含 2 个账号，其中一个 `normalized` + `processed_mapping`；runtimeConfig 含 5 张 pathOverrides、definedCacheRoutes、若干 Kiro 字段、`externalPool=false` 的注入项、开启的 `payloadGuardEnabled`）。
- 单测：dry-run 报告包含每一条映射/丢弃/高亮；`--apply` 产出的 `Config` 通过 `validate()`；幂等（同一包导入两次结果一致）；`--redact-secrets` 的包拒绝 `--apply`。
- 集成测试（有 `CLAUDE_POOL_TEST_POSTGRES_URL` 时）：建临时 schema → 写旧表结构与样本 → 导出 → 导入到另一个临时 schema → 断言账号数、路由数、配置字段。

## 自检

```
cargo fmt --all
cargo build -q --bins
cargo run -q --bin claude-pool -- import-legacy tests/migration/fixtures/legacy-sample.json | head -80   # 看报告格式
cargo test --bin claude-pool -q -- migration
cargo test --bin claude-pool -q -- golden_usage
cargo test --bin claude-pool -q 2>&1 | tail -30
grep -rn "external_upstream_pools\|kiroRsTool\|externalPool" src --include=*.rs | grep -v "src/migration/" | grep -v tests   # 应为空
```
