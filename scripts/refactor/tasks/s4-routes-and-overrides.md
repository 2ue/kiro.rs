# 任务 s4：路由定义收口与账号覆盖（阶段 4）

## 目标

落实设计 `docs/claude-pool-refactor/03-领域模型与配置分层.md` 第 4、5 节与 `11-实施说明与关键逻辑契约.md` 第 3.1、3.2、3.3、3.11、3.12 节：

1. 把五张按路径前缀的 `pathOverrides` 表（`cachePolicy.pathOverrides`、`reportedUsage.pathOverrides`、`promptInjection.pathOverrides`、`routeFeatures.pathOverrides`）加 `definedCacheRoutes` 收口为一个 `routes[]` 列表。保留最长前缀匹配和五个内置前缀，现网客户端不改地址。
2. 路由新增 `accountRule`（原全局 `externalPoolRouteMode/Rules` 的路由侧）和 `modelRule`（路由级模型白/黑名单）。
3. 账号新增 `tags`、`proxyUrl`、`costMultiplier`、`overrides: AccountOverrides`（超时、流模式、uplift 的 `Option` 覆盖）。
4. 模型目录新增 `listed` 字段替代已删的 `claudeModelsOnly`；`/v1/models` 按路由取交集。
5. 写旧结构 → 新结构的配置迁移函数，覆盖现网脱敏配置。
6. 调度默认值已在阶段 2 调整（等待 60、队列 64、wait 模式），本任务补上对应的 table-driven 测试。

**命名不改**（`external_pool*` 等阶段 5 统一改）。golden 与 usage 投影测试必须通过。

## 先读

- 设计 03 章第 4、5 节；11 章第 3.1、3.2、3.3、3.12 节；06 章第 1、3 节。
- 阶段 3 已完成：路由功能集 `RouteFeatureSet` 在 `src/model/prompt_injection/mod.rs:~470`，含 8 个功能；`RouteFeatureOverride` 用 `Option<Option<T>>` 三态；`routeFeatures.path_overrides` 仍是按前缀的表，本任务一并收口进 `routes[]`。
- 现有代码：`src/model/config.rs`（`CachePolicyConfig` ~2933、`ReportedUsageConfig` ~893、`resolve_cache_policy_for_path` ~3243、`external_pool_route_allowed` ~3964、`normalize_route_rule` ~5143、`route_rule_matches` ~5173、`normalize_defined_cache_routes` ~5134、`RequestRuntimeConfig::cache_policy_for_path` ~5303）、`src/model/prompt_injection/mod.rs`（`PromptInjectionConfig.path_overrides` ~137、`RouteFeaturesConfig` ~484）、`src/anthropic/router.rs:308-341`（五组前缀注册）、`src/anthropic/handlers.rs`（`route_features_for`、`injections_for`、`cache_policy_for_path`、dfcache 404）、`src/external_pool.rs`（`ExternalPool` 结构 ~580、候选过滤 `select_pool_for_route` 与路由双向放行、`supported_models` 变体匹配）、`src/storage/postgres.rs`（`external_upstream_pools` 表与 `EXTERNAL_POOL_SELECT_SQL`、`model_capabilities` 表）、`src/admin/route_scenarios.rs`。

## 允许改动的文件

`src/model/**`、`src/anthropic/**`、`src/external_pool.rs`、`src/external_pool/**`、`src/storage/**`、`src/admin/**`、`src/main.rs`、`config.example.json`、`tests/`。函数体不得改动的受保护文件同前（`cache.rs`、`prompt_cache*.rs`、`bounded_shaping.rs`、`kiro_plus_shaping.rs`、`kiro_rs_tool_plus.rs`、`usage_projection.rs`、`project_usage_value` 链）。

## A. 路由定义

`src/model/routes.rs`（新建）：

```rust
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RouteConfig {
    pub prefix: String,              // 归一化后："" 表示根（/v1）；"/cc"；"/dfcache/foo"
    pub name: String,
    #[serde(default = "default_true")] pub enabled: bool,
    #[serde(default)] pub cache_policy: CacheRoutePolicyPatch,          // 原 cachePolicy.pathOverrides[prefix]
    #[serde(default)] pub reported_usage: Option<ReportedUsagePathPolicy>, // 原 reportedUsage.pathOverrides[prefix]
    #[serde(default)] pub features: RouteFeatureOverride,                 // 原 routeFeatures.pathOverrides[prefix]（阶段 3 已改为功能开关集）
    #[serde(default)] pub prompt_injection: BTreeMap<InjectionId, ItemOverride>, // 原 promptInjection.pathOverrides[prefix]
    #[serde(default)] pub account_rule: RouteRule,   // {mode, rules}：rules 是账号 id 字符串或 "*"
    #[serde(default)] pub model_rule: RouteRule,     // {mode, rules}：rules 支持尾部 * 通配
}
pub struct RouteRule { pub mode: ExternalPoolRouteMode /* allow_all|allow_list|deny_list */, pub rules: Vec<String> }
pub struct RoutesConfig { pub routes: Vec<RouteConfig> }
```

- 前缀归一化沿用 `normalize_defined_cache_route`（11 章 3.1 节第 1 条）；保存时拒绝重复前缀；根路由用 `""`。
- 内置路由 `""`、`/cc`、`/ha`、`/na` 不可删除（校验拒绝），可改策略。`with_builtin_path_defaults` 的语义改为"缺哪条内置路由就补哪条"，默认 cacheType 不变（前三 current_high_cache，/na no_cache）；`reportedUsage` 对 `/cc`、`/ha` 的内置默认（`config.rs:1410-1434` 对应处）作为这两条路由的默认 `reported_usage`。
- `RoutesConfig::compile() -> CompiledRoutes`：按前缀长度降序排好，预先对每条算好 `ResolvedCacheRoutePolicy`、`ResolvedInjections`、`RouteFeatureSet`。`CompiledRoutes::match_endpoint(endpoint) -> Option<&CompiledRoute>`：最长前缀；根路由兜底。`RequestRuntimeConfig` 持有 `CompiledRoutes`，`cache_policy_for_path` / `route_features_for` / `injections_for` 改为查它。
- `Config` 新增 `routes: RoutesConfig`；删除 `cache_policy.path_overrides`、`reported_usage.path_overrides`、`prompt_injection.path_overrides`、`route_features.path_overrides`、`defined_cache_routes`、`external_pools.external_pool_route_mode/rules`（迁入各路由的 `account_rule`：原全局规则是"路由能否用外部池"，现在没有本地池，等价于每条路由的 `account_rule`；迁移时若原规则对某前缀不放行，则该路由 `account_rule = deny_list ["*"]`，等价于禁用，保存时提示）。
- axum 注册：五组保持不动（`/v1`、`/na/v1`、`/cc/v1`、`/ha/v1`、`/{route}/v1`）。`/{route}/v1` 组内从 `CompiledRoutes` 查前缀 `/{route}`（含 `/dfcache/{name}` 形式，即 `{route}` 捕获的是第一段；查不到 → 404 `not_found_error`，沿用现有 dfcache 404 文案）。注意现有 `/dfcache/{route}` 是两段前缀，新的 `/{route}` 是一段；为兼容现网 `/dfcache/foo` 地址，`CompiledRoutes::match_endpoint` 对 endpoint 做完整路径匹配即可（`/dfcache/foo/v1/messages` 匹配前缀 `/dfcache/foo`），axum 只需把 `/dfcache/{route}/v1/*` 与 `/{route}/v1/*` 都挂到同一 handler。
- 路由 `enabled=false` → 404。

## B. 账号与路由双向规则、模型规则

- 候选过滤（`external_pool.rs` `select_pool_for_route*`）：现有"全局 `external_pool_route_allowed(endpoint)` + 账号 `route_rules` 放行"改为"路由 `account_rule` 对账号 id 放行 + 账号 `route_rules` 对 endpoint 放行"。三态语义按 11 章 3.2 节（`allow_list` 空 = 不放行；`deny_list` 空 = 全放行）。
- 模型放行：路由 `model_rule` 用与 `supported_models` 相同的变体展开（`external_pool.rs` 的 `normalize_external_pool_*` 系列），规则值支持尾部 `*` 对归一化 base 做前缀匹配。先过 `model_rule` 再过账号 `supported_models`。
- 候选为空的原因区分：全部被路由/模型规则拒绝 → 404 `not_found_error`（model not found 文案）；全部冷却/并发满 → 进入现有排队。

## C. 账号新增字段

`ExternalPool` 新增：`tags: Vec<String>`、`proxy_url: Option<String>`（支持 `http://`、`https://`、`socks5://`、`socks5h://`，可带用户名密码；`http_client.rs` 已有 `ProxyConfig`，按 proxy_url 缓存 reqwest client，与现有 `wire_profile`/`tls_profile` 四个预建 client 组合；为空时用全局 `proxy_url`，再空直连）、`cost_multiplier: f64`（默认 1.0，只用于 usage 记录新增字段 `account_cost_usd = raw_cost × multiplier`，不影响整形）、`overrides: AccountOverrides`：

```rust
#[derive(Default, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountOverrides {
    pub request_timeout_secs: Option<u64>,
    pub stream_request_timeout_secs: Option<u64>,
    pub stream_idle_timeout_secs: Option<u64>,
    pub stream_first_output_timeout_secs: Option<u64>,
    pub usage_projection_uplift_percent: Option<u32>,
    pub usage_projection_cost_floor_enabled: Option<bool>,
    pub usage_projection_cost_floor_margin_percent: Option<u32>,
    pub usage_projection_output_uplift_min_tokens: Option<i32>,
    pub usage_projection_output_uplift_percent: Option<u32>,
}
```

`external_pool.rs` 中读取上述全局值的地方改为 `pool.overrides.x.unwrap_or(config.external_pool_x)`（现有 `stream_response_mode` / `auto_disable_policy` / `pre_output_stream_retry_mode` 已是这个模式，照搬）。`usage_projection::build_context` 的入参由调用方传入已合并后的 uplift 值，**不改 `build_context` 函数体**。

`external_upstream_pools` 表加列 `tags TEXT[] DEFAULT '{}'`、`proxy_url TEXT`、`cost_multiplier DOUBLE PRECISION DEFAULT 1`、`overrides JSONB DEFAULT '{}'`（新 schema 直接写进 CREATE TABLE，不写 ALTER）。`usage_records` 加 `account_cost_usd DOUBLE PRECISION DEFAULT 0`。admin 的创建/更新请求类型与校验同步（proxy_url 格式校验、cost_multiplier > 0、overrides 数值范围沿用全局同名项的校验）。`usage_projection_mode` 默认值改为 `current_path_policy`（设计 03 章 2.3 节）。

## D. 模型目录 `listed`

- `model_capabilities` 表加 `listed BOOLEAN NOT NULL DEFAULT true`；`Model` 结构加 `listed`；seed 文件每条加 `"listed": true`。
- `/v1/models`、`/v1/models/{id}`：只返回 `listed=true` 且该路由 `model_rule` 放行、且至少一个该路由可用账号（enabled 且 `account_rule`/`route_rules` 双向放行）的 `supported_models` 为空或命中的模型。
- admin：`/model-capabilities/manual` 支持设置 `listed`；新增 `POST /external-pools/{id}/models/promote`：把账号 `supported_models` 中不在目录的模型按默认能力加入目录（`listed=false`，source=`account_discovery`）。

## E. 配置迁移

`src/model/config.rs` 的加载路径：`runtime_config` JSON 若含 `cachePolicy.pathOverrides` 或 `definedCacheRoutes` 或 `reportedUsage.pathOverrides` 或 `promptInjection.pathOverrides` 或 `routeFeatures.pathOverrides` 或 `externalPools.externalPoolRouteMode`，执行 `migrate_path_overrides_to_routes()`：把同一前缀的各表条目合并成一条 `RouteConfig`；`definedCacheRoutes` 每项生成 `/dfcache/{name}` 路由；`externalPoolRouteMode/Rules` 按上面的等价规则进各路由 `account_rule`；迁移后删除旧键；`runtimeConfigMigrationVersion` 从 1 升到 2。写单测覆盖：五张表各有不同前缀集合时的并集、同一前缀多表合并、dfcache、空配置、重复执行幂等。

`config.example.json` 更新为 `routes[]` 形式（内置四条）。

## F. 测试

- `src/model/routes.rs` 单测：前缀归一化（11 章 3.1 节 5 条）、最长前缀匹配、根路由兜底、`/dfcache/foo` 匹配、重复前缀拒绝、内置路由不可删。
- 双向规则与三态 table-driven 测试（`allow_all`/`allow_list` 空/非空/`deny_list` 空/非空 × 账号侧同样三态）。
- `model_rule` 通配与变体展开测试。
- `AccountOverrides` 合并测试：None 继承、Some 覆盖，对每个字段。
- 调度默认值测试：`ExternalPoolsConfig::default()` 的 `external_pool_dispatch_max_wait_secs == 60`、`external_pool_max_queued_requests == 64`、`external_pool_capacity_mode == Wait`、`external_pools_enabled == true`。
- `/v1/models` 交集测试：两条路由挂不同 `model_rule`，同一账号 `supported_models`，列表不同。
- 现有 `external_pool/tests.rs` 中用 `route_rules`/`external_pool_route_mode` 的测试按新结构适配，断言不变。

## 自检

```
cargo fmt --all
cargo build -q --bins
grep -rn "path_overrides\|defined_cache_routes\|external_pool_route_mode\|external_pool_route_rules" src --include=*.rs | grep -v "migrate\|legacy\|tests.rs"   # 应为空（迁移函数内允许）
cargo test --bin kiro-rs -q -- golden_usage
cargo test --bin kiro-rs -q -- external_pool::tests::usage_projection
cargo test --bin kiro-rs -q -- model::routes
cargo test --bin kiro-rs -q 2>&1 | tail -30
```
