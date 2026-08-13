# cachePoint 与缓存归一化实施方案

## 适用范围

本方案处理 high-cache 路由、本地 prompt cache usage 模拟、真实 Kiro `cachePoint` 试验、volatile id 归一化、缓存内存边界和管理端统计。

## 来源项目与学习点

- `kiroxy/internal/reqconv/cache_points.go`：把 Anthropic `cache_control` 映射为 Kiro `cachePoint`。
- 本地 `kiro2api/internal/reqconv/cache_points.go`：cachePoint 结构可参考。
- `pluto2sun/kiro2api/src/anthropic/true_cache.rs`：volatile id normalization 值得学习，但 full response cache 不适合作为默认能力。
- `Kiro-Go/proxy/cache_tracker.go` 和 `Kiro-account-manager/src/main/proxy/promptCacheTracker.ts`：per-account cache entry cap 思路适合补齐内存边界。

## 当前项目现状

当前项目已经有：

- `/cc`、`/ha`、`/na` 高缓存路由。
- `/dfcache/*` 安全自定义高缓存路由。
- 本地 prompt cache tracker。
- usage 模拟和上报策略。
- route-level cache policy。

当前不足：

- 本地 high-cache 更偏 usage 模拟，不等于真实发送 Kiro cachePoint。
- cache tracker 的内存边界需要更直观地展示。
- volatile id 会影响 fingerprint 稳定性。

## 目标

- 保持现有高缓存路由行为不变。
- 增加默认关闭的真实 `cachePoint` 试验能力。
- 增加 fingerprint normalization，降低无意义 cache miss。
- 为缓存条目设置清晰上限和 TTL。
- 管理端显示 cache entry 数、命中趋势、内存估算。

## 非目标

- 不默认启用 full response cache。
- 不缓存完整响应内容。
- 不改变 `/dfcache/*` 路由前缀。
- 不允许管理端修改 `/dfcache/` 固定前缀。
- 不把 cachePoint 失败直接暴露给下游。

## 涉及文件

- `src/anthropic/prompt_cache.rs`
- `src/anthropic/cache.rs`
- `src/anthropic/converter.rs`
- `src/model/config.rs`
- `src/kiro/provider.rs`
- 管理端设置和路由配置页面

## 新增数据结构

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheFingerprint {
    pub route_key: String,
    pub account_scope: Option<String>,
    pub model: String,
    pub normalized_body_sha256: String,
    pub cache_control_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachePointPlan {
    pub enabled: bool,
    pub insert_tool_cache_point: bool,
    pub inserted_count: usize,
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCacheStats {
    pub route_key: String,
    pub entry_count: usize,
    pub max_entries: usize,
    pub estimated_bytes: u64,
    pub hit_count_1h: u64,
    pub miss_count_1h: u64,
    pub eviction_count_1h: u64,
}
```

## cachePoint 策略

默认配置：

```rust
pub kiro_cache_point_enabled: bool, // 默认 false
pub kiro_cache_point_tools_only: bool, // 默认 true
pub kiro_cache_point_record_plan: bool, // 默认 true
```

规则：

- 第一阶段只处理 tools 上的 `cache_control`。
- 不处理 system message 的广泛 cachePoint 注入。
- 不处理所有 message block 的自动 cachePoint。
- 插入 cachePoint 前必须确认 Kiro 上游真实接受。
- 如果上游返回 body invalid，必须自动关闭本次请求的 cachePoint 重试一次，且记录 diagnostics。

## Fingerprint normalization 规则

只对本地 fingerprint 生效，不得修改发送给上游的真实请求体。

允许归一化：

- tool_use id。
- request id。
- message id。
- 临时 UUID。
- 时间戳类 volatile 字段。

不得归一化：

- 用户文本。
- system prompt 文本。
- tool name。
- tool input 语义字段。
- model。
- temperature、max_tokens 等影响输出的参数。

## 缓存边界

新增配置：

```rust
pub prompt_cache_max_entries_per_account: usize, // 默认 200
pub prompt_cache_max_entries_global: usize, // 默认 20000
pub prompt_cache_entry_ttl_secs: u64, // 默认 86400
pub prompt_cache_estimated_bytes_limit: u64, // 默认 256MB
```

淘汰规则：

1. 过期优先。
2. 超过 per-account 上限时按 LRU 淘汰。
3. 超过 global 上限时按 LRU 淘汰。
4. 超过估算内存上限时按 LRU 淘汰。

## `/dfcache/*` 安全规则

必须保持：

- 只有配置过的 `/dfcache/{name}` 路由生效。
- 未配置路由直接返回 404。
- 管理端不能修改固定前缀 `/dfcache/`。
- 即使前端被绕过提交了其他前缀，后端也必须拒绝。
- route name 必须只允许安全字符，例如 `[a-zA-Z0-9_-]`。

## 实施步骤

1. 增加 cache fingerprint normalization，默认只用于统计。
2. 增加 cache entry 上限和 TTL。
3. 管理端展示 stats。
4. 增加 cachePoint plan，但默认 `enabled=false`。
5. 使用 fake server 验证 body 结构。
6. 使用真实上游小流量验证 cachePoint。
7. 成功后允许管理员按路由开启。

## 测试方案

新增测试：

- `cache_fingerprint_normalizes_volatile_tool_use_ids`
- `cache_fingerprint_does_not_change_user_text`
- `prompt_cache_enforces_per_account_entry_limit`
- `prompt_cache_enforces_global_entry_limit`
- `prompt_cache_evicts_expired_entries_first`
- `dfcache_unconfigured_route_is_rejected`
- `dfcache_prefix_cannot_be_overridden`
- `cache_point_disabled_by_default`
- `cache_point_tools_only_inserts_expected_marker`
- `cache_point_invalid_body_fallback_records_diagnostics`

真实测试：

- `/cc/v1/messages` 普通高缓存。
- `/dfcache/cc/v1/messages` 已配置路由。
- 未配置 `/dfcache/aa/v1/messages`。
- tools cache_control 请求。
- 长会话 repeated prompt。

## 验收标准

- 默认行为与现有 high-cache 一致。
- cache entry 数有上限。
- volatile id 不再导致明显重复 entry。
- `cachePoint` 默认关闭。
- 开启 `cachePoint` 后如果上游不接受，请求仍可回退完成。
- 管理端能看到缓存占用和淘汰数量。

## 风险与回滚

风险：

- cachePoint body 不被 Kiro 接受。
- normalization 过度导致错误命中。
- entry 上限过低降低命中率。

规避：

- cachePoint 默认关闭。
- normalization 只处理明确 volatile 字段。
- 不做 full response cache。

回滚：

- 关闭 `kiro_cache_point_enabled`。
- 关闭 normalization。
- 调大 entry 上限。

## 不得做的事项

- 不得默认启用 full response cache。
- 不得缓存完整响应文本。
- 不得修改发送给上游的用户内容。
- 不得允许未配置 `/dfcache/*` 路由生效。
- 不得允许管理端提交自定义前缀覆盖 `/dfcache/`。

## 后续可选扩展

后续可以研究 response cache，但必须单独设计一致性、隐私、清理、命中可解释和禁用策略。

