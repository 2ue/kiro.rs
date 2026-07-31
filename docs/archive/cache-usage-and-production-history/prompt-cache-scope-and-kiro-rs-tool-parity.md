# Prompt Cache Scope 开关 + 复现 Kiro-RS-Tool 缓存策略 实施方案

> 目标：(1) 给 prompt-cache 命中的 **scope（隔离维度）增加可配置开关**，支持"仅按 session 会话"或"按 client key"隔离；(2) 在当前 `kiro.rs` 上**通过配置 + 最小代码改动复现 `Kiro-RS-Tool` 的朴素缓存策略**，且**不影响现有策略**。
>
> **本文档力求自包含**：即使不读源码、不看对话记录，也能据此准确定位、无歧义地实现，不产生误解。

方案时间：2026-06-30。基于对 `kiro.rs` 与 `Kiro-RS-Tool` 两个项目缓存实现的逐文件核查。

---

## 0. 阅读前必须了解的背景（避免误判）

### 0.1 两个项目是什么

| 项目 | 路径 | 角色 |
|---|---|---|
| `kiro.rs` | `~/Desktop/procode/kiro.rs` | **本方案要改的项目**。Kiro→Anthropic 代理，缓存计费是一套四阶段流水线 |
| `Kiro-RS-Tool` | `~/Desktop/procode/Kiro-RS-Tool` | **参照项目**。同类代理，缓存策略是单文件、无配置的朴素实现 |

两者后端 API 形态相同。`Kiro-RS-Tool` 的缓存逻辑全部在单文件 `src/anthropic/cache_metering.rs`。

### 0.2 两套缓存策略的本质关系（关键认知）

**`Kiro-RS-Tool` 的缓存 = `kiro.rs` 缓存流水线"第一阶段"裸跑。** `kiro.rs` 在第一阶段（前缀链命中）之上又叠了四层整形。对齐 = 关掉这四层 + 对齐 scope + 解决持久化。

`kiro.rs` 的四阶段流水线：

| 阶段 | 作用 | 对应代码 | Kiro-RS-Tool 有无 |
|---|---|---|---|
| ① 前缀链指纹命中 | SHA256 累积指纹 + 内存表 + TTL，算出 creation/read | `src/anthropic/prompt_cache.rs` | ✅ 这就是它的全部 |
| ② 目标比例浮动 | `target_read_ratio` 在无命中数据时兜底 | `src/anthropic/cache.rs` | ❌ 无 |
| ③ token 放大 | `CacheAmplification`（scale 1.0~3.0、封顶、jitter） | `src/anthropic/cache.rs` | ❌ 无 |
| ④ 创建频次控制 | 抑制过频 cache_creation 上报 | `src/anthropic/prompt_cache_creation_control.rs` | ❌ 无 |
| ⑤ 下游上报采样 | `reportedUsage` 采样/裁剪对外数字 | `src/model/config.rs`（ReportedUsage*） | ❌ 无 |

> 注意 `effective_cache_ratio` 语义（`cache.rs` 已确认）：真实前缀命中算出的比例**优先**，`target_read_ratio` 只是没有命中数据时的兜底。所以默认就"反映真实命中"，②不需要特意关。

### 0.3 两套 scope（隔离维度）的差异

- `kiro.rs` 的 `PromptCacheScope`（`prompt_cache.rs:24`）是 **4 元组**：`(credential_id, conversation_id, model, route_namespace)`。含 `credential_id` → **同一会话切换上游账号不命中**。
- `Kiro-RS-Tool` 只按 **session 或 client-key id** 隔离（二选一作哈希链起头种子），**不绑上游账号** → 跨账号仍命中同一会话前缀。

本方案改动 1/3 就是让 `kiro.rs` 的 scope 可以切到"仅 session"或"按 client key"。

### 0.4 关键约束

- **不影响现有策略**：所有改动以"新增可选开关 + 默认保持现状"为准则。默认值是否翻转见 §4 决策点。
- 改动语言为后端 **Rust**；配置字段沿用现有 `serde(rename_all="camelCase")` 风格（Rust 蛇形 → JSON 驼峰）。
- 编译/测试：在 `kiro.rs/` 下 `cargo build` / `cargo test`（Rust 项目，与前端 node 无关）。
- 行号基于审计时点快照，实现时以实际代码为准（用文中给出的关键代码片段定位）。

---

## 1. 决定方案分期的前提发现

**client key 当前没有流进缓存上下文。** `src/anthropic/middleware.rs:304` 的 `auth_middleware` 只做存在性校验：

```rust
match auth::extract_api_key(&request) {
    Some(key) if state.request_api_keys.contains(&key) => { /* 放行，未保存 key */ }
    _ => /* 401 */
}
```

它**没有** `request.extensions_mut().insert(...)` 把 key/key-id 存进请求扩展，handlers 也没有任何读取 client key 的地方（grep `handlers.rs` 的 `Extension`/`key_id`/`client_key` 均为空）。

推论，直接决定工作量：

- **"仅 session" 维度** → conversation_id 数据已就绪（见 §2.3），**纯改 scope 即可**，工作量小。
- **"按 client key" 维度** → 必须先铺一段管道（中间件提取 key id → 存 extensions → 透传到 `RequestUsageContext`），才能进 scope。工作量大，列为第二阶段（§3 改动 3）。

---

## 2. 现有可照搬的范式与精确代码坐标

### 2.1 已有同款范式：频次控制的 scope 开关（直接照抄）

项目里**已经有**一个"scope 里要不要带 credential_id"的开关，就是频次控制用的。新增缓存命中的 scope 开关照它抄即可，无需从零设计。

`src/model/config.rs:688`：

```rust
/// 本地 prompt-cache creation 上报频次控制的状态维度。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheCreationControlScopeMode {
    /// 同一凭据、会话、模型独立控制，最贴近真实账号缓存隔离。
    CredentialConversationModel,
    /// 同一会话、模型共享控制，默认值。
    ConversationModel,
}
impl Default for PromptCacheCreationControlScopeMode {
    fn default() -> Self { Self::ConversationModel }
}
```

它如何作用于 key —— `src/anthropic/prompt_cache_creation_control.rs:24`：

```rust
fn from_scope(scope: &PromptCacheScope, mode: PromptCacheCreationControlScopeMode) -> Self {
    Self {
        credential_id: match mode {
            PromptCacheCreationControlScopeMode::CredentialConversationModel => Some(scope.credential_id),
            PromptCacheCreationControlScopeMode::ConversationModel => None,  // ← 不带账号
        },
        conversation_id: scope.conversation_id.clone(),
        model: scope.model.clone(),
        route_namespace: scope.route_namespace.clone(),
    }
}
```

**这正是要复制的模式**：用一个 enum mode 决定 key 里是否包含 `credential_id`。

### 2.2 缓存 scope 的定义与全部"生产构造点"

定义 —— `src/anthropic/prompt_cache.rs:24`：

```rust
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct PromptCacheScope {
    pub credential_id: u64,
    pub conversation_id: String,
    pub model: String,
    pub route_namespace: Option<String>,
}
```

scope 进入计算的入口（**内部逻辑不要改**）：`PromptCacheTracker::compute_with_bounds` 和 `update_with_bounds`（`prompt_cache.rs`，签名都接收 `scope: Option<PromptCacheScope>`）。它们用 `entries_by_scope.get_mut(&scope)` 按 scope 分桶。**只要在构造 scope 时改写字段，命中桶就变了，计算逻辑完全复用。**

**生产路径只有 2 处构造点**（其余都是 `#[cfg(test)]` 测试，不用动）：

| 文件:行 | 路径 | 现状 |
|---|---|---|
| `src/anthropic/handlers.rs:2873` | 本地账号路径 | `PromptCacheScope { credential_id, conversation_id, model, route_namespace }` |
| `src/external_pool.rs:4406` | 外部池路径 | 同上，`credential_id` 用 `external_pool_prompt_cache_credential_id(pool.id)`（池 id + 偏移） |

> `handlers.rs:1796` 的 `fn scope(&self)` 是另一个构造点，但它服务于同一条本地路径的 usage 上报，改动 1 落地时需一并核对（见 §3 改动 1 步骤 4）。

### 2.3 conversation_id（session）的来源

`src/anthropic/converter.rs` 的 `extract_stable_conversation_id(payload)`：先取 `metadata.user_id`；否则用 `system + tools + first_user_message` 的 SHA256 派生确定性 UUID。已在两条生产路径就绪（`handlers.rs:2785` 存入 `prompt_cache_scope_conversation_id`，`external_pool.rs:4406` 直接调用）。**这就是"仅 session"维度的数据基础，无需新增管道。**

### 2.4 缓存模拟配置结构（开关挂载点）

`src/model/config.rs:817` `CacheSimulationPolicy`（每条缓存路径一份，挂在 `cachePolicy.default` 和路径覆盖下）：

```rust
pub struct CacheSimulationPolicy {
    pub enabled: bool,                       // 总开关
    pub target_read_ratio: f64,              // ② 兜底比例
    pub token_scale: f64,                    // ③ 放大倍率，clamp(1.0,3.0)
    pub max_simulated_input_tokens: i32,     // ③ 封顶
    pub cap_jitter_min_tokens: i32,          // ③ 抖动
    pub cap_jitter_max_tokens: i32,          // ③ 抖动
    pub scale_min_input_tokens: i32,         // ③ 放大下限
}
```

配套有 `CacheSimulationPolicyPatch`（config.rs:898，路径覆盖用）、`normalized()`、`validate()`。**新增 scope 开关字段就加在这个结构体上**（§3 改动 1 步骤 2）。

---

## 3. 改动方案（逐步、可无歧义实现）

### 改动 1：缓存 scope 增加 mode 开关（"仅 session" 维度）— 第一阶段

目标：让缓存命中桶可以从"账号+会话+模型+路由"切到"仅会话+模型+路由"，复现 Kiro-RS-Tool 的跨账号命中。**照搬 §2.1 的频次控制范式。**

**步骤 1 — 新增枚举**（`src/model/config.rs`，紧挨 `PromptCacheCreationControlScopeMode`）：

```rust
/// 本地 prompt-cache 命中（tracker 分桶）的隔离维度。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheScopeMode {
    /// 现状：凭据 + 会话 + 模型 + 路由。同一会话跨账号不互相命中。
    CredentialConversationModel,
    /// 仅会话 + 模型 + 路由。跨账号共享同一会话前缀（Kiro-RS-Tool 式）。
    ConversationModel,
    /// 按 client key + 会话 + 模型 + 路由。需要改动 3 先铺管道，否则等同 ConversationModel。
    ClientKeyConversationModel,
}

impl Default for PromptCacheScopeMode {
    // 见 §4 决策点：保守=CredentialConversationModel，翻默认=ConversationModel
    fn default() -> Self { Self::CredentialConversationModel }
}
```

**步骤 2 — 挂到配置**（`CacheSimulationPolicy`，config.rs:817）：

```rust
pub struct CacheSimulationPolicy {
    // ...现有字段...
    #[serde(default)]
    pub scope_mode: PromptCacheScopeMode,   // 新增
}
```

同步处理：
- `Default for CacheSimulationPolicy`（config.rs:835）补 `scope_mode: PromptCacheScopeMode::default()`。
- `normalized()`（config.rs:850）透传 `scope_mode: self.scope_mode`。
- `CacheSimulationPolicyPatch`（config.rs:898）加 `#[serde(default)] pub scope_mode: Option<PromptCacheScopeMode>`，并在其 `apply_to`（config.rs:920 附近）补 `if let Some(v) = self.scope_mode { policy.scope_mode = v; }`。
- `validate()` 无需新增（枚举天然合法）。
- 检查 `is_empty`/相等判断类方法（patch 是否"空补丁"）是否需把 `scope_mode.is_none()` 纳入（config.rs:943 一带）。

**步骤 3 — 给 PromptCacheScope 加规范化方法**（`src/anthropic/prompt_cache.rs`，紧随结构体定义）：

```rust
/// 哨兵：ConversationModel 模式下所有账号归入同一桶。
pub const SCOPE_ANY_CREDENTIAL: u64 = 0;

impl PromptCacheScope {
    /// 按配置维度规范化分桶 key。不改 compute/update 内部逻辑。
    pub fn canonical(mut self, mode: PromptCacheScopeMode, client_key_seed: Option<u64>) -> Self {
        use crate::model::config::PromptCacheScopeMode::*;
        match mode {
            CredentialConversationModel => {}                       // 保持现状
            ConversationModel => { self.credential_id = SCOPE_ANY_CREDENTIAL; }
            ClientKeyConversationModel => {
                // 改动 3 落地前 client_key_seed 恒为 None → 退化为 ConversationModel
                self.credential_id = client_key_seed.unwrap_or(SCOPE_ANY_CREDENTIAL);
            }
        }
        self
    }
}
```

> 用 `credential_id` 字段承载 client key seed 是为了**不改 `PromptCacheScope` 的字段结构和 `Hash` 实现**，最小化爆炸半径。哨兵 0 需确认不会与真实 credential_id 撞（真实 id 从 1 起则安全；外部池用偏移量，也不会是 0 —— 实现时核对 `external_pool_prompt_cache_credential_id` 的偏移基值 > 0）。

**步骤 4 — 在 2 处生产构造点调用 `.canonical(...)`**：

- `src/anthropic/handlers.rs:2873`：构造 scope 后接 `.canonical(scope_mode, None)`。`scope_mode` 从该路径已解析的 `CacheSimulationPolicy` 取（这条路径上 `usage_context` 持有解析后的 policy；若当前未透传 scope_mode，需顺着 `prepare_usage_context`→`prepare_credential_usage_context` 把 `scope_mode` 加入 `RequestUsageContext`，与 `prompt_cache_target_read_ratio` 同源同路径，照它加一个字段即可）。
- `src/external_pool.rs:4406`（`external_prompt_cache_scope`）：同样接 `.canonical(scope_mode, None)`，`scope_mode` 从 `ExternalRouteRequest`/route policy 取（照 `prompt_cache_target_read_ratio` 在 `ExternalRouteRequest` 上的传递方式新增一个字段）。
- 复核 `handlers.rs:1796` 的 `fn scope(&self)`：若它也用于命中/写回，同样接 `.canonical(...)`，保证 compute 与 update 用**同一规范化 key**（否则写进 A 桶、读 B 桶，永不命中）。

**步骤 5 — 测试**：照 `prompt_cache.rs` 现有 `credential_and_conversation_are_isolated` 写一个反向用例：`ConversationModel` 模式下两个不同 `credential_id` 的 scope `.canonical()` 后应命中同一条缓存。

**对现有行为的影响**：默认 `CredentialConversationModel` 时 `canonical` 是恒等变换，**零行为变更**。

### 改动 2：默认值是否翻转（决策点，见 §4）

你的原话是"变为默认行为"。把 `Default` 设成 `ConversationModel` 会让**存量部署升级后**同一会话跨账号开始互相命中 —— 这与"不影响现有策略"冲突。处理见 §4，**默认建议保守，目标部署用配置 opt-in**。

### 改动 3：client key 维度（第二阶段，跨三文件穿线）

仅当需要 Kiro-RS-Tool 的"session 优先、否则 client key id"语义时做。纯增量穿线，不碰缓存计算逻辑。

1. **中间件**（`src/anthropic/middleware.rs:304` `auth_middleware`）：命中 key 后，把 key 的稳定标识（key id；若 store 只有明文 key，则取其 SHA256 的前 8 字节转 u64 作 seed）`request.extensions_mut().insert(ClientKeySeed(seed))`。需要在 `common::auth` 暴露"key→id/seed"的方法（`RequestApiKeyStore`，auth.rs:68）。
2. **handler 取出**：在 anthropic 请求入口从 `extensions` 取出 `ClientKeySeed`，写入 `RequestUsageContext` 新增字段 `client_key_seed: Option<u64>`（沿 `prompt_cache_scope_conversation_id` 同一条上下文线传递，handlers.rs:102 一带）。
3. **接入 canonical**：改动 1 步骤 4 调用处把 `None` 换成 `usage_context.client_key_seed`，并在配置里把 `scope_mode` 设为 `client_key_conversation_model`。

> 注意：client key 维度不绑会话时，单独用 client key 会让该 key 下**所有会话**挤进一桶，命中率虚高且互相污染。所以保留 `conversation_id` 作为主键、client key 仅替换 `credential_id` 槽位，是更稳妥的语义（与上面 canonical 写法一致）。

---

## 4. 决策点（实现前必须确认）

| 决策 | 选项 A（推荐） | 选项 B |
|---|---|---|
| **scope_mode 默认值** | `CredentialConversationModel`（零行为变更，目标部署配置 opt-in 切 `conversation_model`） | `ConversationModel`（符合"变默认"字面要求，但属行为变更，须 release note 标注并确认存量部署可接受） |
| **clientkey 维度时机** | 先只上 session（改动 1+2），验证后再做 | 一次性连 client key（改动 1+3）一起上 |

> 文档作者建议：A + 先上 session。理由：满足"在目标部署复现 Kiro-RS-Tool"的诉求，同时严格不触碰其他部署的现有缓存行为。若产品明确要求全局默认即 session 隔离，则选 B 并在变更说明里写明影响面。

---

## 5. 通过配置复现 Kiro-RS-Tool 缓存（零代码，配合改动 1）

把四层整形全调成中性，缓存即回归"真实前缀命中、不放大、不采样"。以下全部是 `config.json` 配置，作用在目标缓存路径（`cachePolicy.default` 或具体路径覆盖）：

| 目标 | 配置项（驼峰 JSON 路径） | 值 |
|---|---|---|
| scope 仅 session | `cachePolicy.default.simulation.scopeMode` | `"conversation_model"`（依赖改动 1） |
| 关 token 放大 | `cachePolicy.default.simulation.tokenScale` | `1.0` |
| 关封顶 | `cachePolicy.default.simulation.maxSimulatedInputTokens` | `0` |
| 关抖动 | `cachePolicy.default.simulation.capJitterMinTokens` / `capJitterMaxTokens` | `0` / `0` |
| 用真实命中比例 | `cachePolicy.default.simulation.targetReadRatio` | 保持默认即可（`effective_cache_ratio` 已优先于它） |
| 关创建频次控制 | `promptCacheCreationControl.enabled` | `false` |
| 上报不采样/裁剪 | `reportedUsage` 默认策略设为原始（`raw`）、清空 path 覆盖、`finalCacheReadMaxTokens` 放开（设为足够大或按字段策略改 `raw`） | — |
| TTL | 5m 默认 / 1h 上限 | 两边天然一致，无需配置 |

> `reportedUsage` 的精确字段名与默认策略以 `src/model/config.rs` 的 `ReportedUsage*` / `ReportedCacheUsagePolicy`（config.rs:489 一带，含 `finalCacheReadMaxTokens`、`cacheRead`、`cacheCreation` 字段策略）为准；目标是让 cache_read/cache_creation 走 `raw`（原样上报），不做 sample/preserve 整形。

### 5.1 配置无法复现、必须改代码的一项：持久化

- Kiro-RS-Tool：每分钟落盘 `cache_dir/cache_metering.json`，启动读回 → **重启保持命中**。
- kiro.rs：`PromptCacheTracker.entries` 是纯内存 `Mutex<HashMap<PromptCacheScope, HashMap<[u8;32], PromptCacheEntry>>>`（prompt_cache.rs:130），**重启清零、首轮必 miss**。

**没有对应配置开关。** 若需要"重启保持命中"，必须给 tracker 加 JSON 落盘 + 启动读回（可直接移植 Kiro-RS-Tool `cache_metering.rs` 的落盘段）。`PromptCacheScope` 需可序列化作 map key（它已 `derive` 了所需 trait，但作为 JSON map key 需转成字符串键或落成数组结构 —— 实现时注意）。本项独立于改动 1/3，按需再做。

---

## 6. 改动清单与验证

**改动 1（session 维度）触达文件**：
- `src/model/config.rs`：新增 `PromptCacheScopeMode` 枚举；`CacheSimulationPolicy` / `Patch` / `Default` / `normalized` / `apply_to` 加 `scope_mode`。
- `src/anthropic/prompt_cache.rs`：加 `SCOPE_ANY_CREDENTIAL` 常量与 `PromptCacheScope::canonical`；加反向命中测试。
- `src/anthropic/handlers.rs`：`RequestUsageContext` 加 `scope_mode` 字段并透传；scope 构造点（:2873、:1796）调用 `.canonical(...)`。
- `src/external_pool.rs`：`ExternalRouteRequest`/route 透传 `scope_mode`；`external_prompt_cache_scope`（:4406）调用 `.canonical(...)`。

**改动 3（client key 维度，第二阶段）额外触达**：
- `src/anthropic/middleware.rs`、`src/common/auth.rs`：提取并注入 `ClientKeySeed`。
- `src/anthropic/handlers.rs`：从 extensions 取出，写入 `RequestUsageContext.client_key_seed`，接入 canonical。

**验证**：
- `cargo build` 通过。
- `cargo test`（重点 `prompt_cache` 模块的隔离/命中用例 + 新增反向用例）。
- 手动回归：默认配置下行为与改动前一致（可对比同一组请求的 usage 输出）；开启 `conversation_model` 后，同会话不同账号应共享命中。
- 配置复现验证：按 §5 配好后，观察 usage 的 cache_read/cache_creation 是否回归"未放大、未采样"的原始量级。

---

## 7. 与其他文档的关系

- 本文档讲"缓存 scope 开关 + Kiro-RS-Tool 对齐"，是后端 Rust 改动。
- 已删除的历史前端功能审计与 [归档的 dashboard 增强方案](archive/ui-planning-2026-06-to-07/README.md) 与本文档无依赖关系；若 R8 重新采纳其中的“用量/缓存口径展示”候选，仍需与本文档 §5 的 reportedUsage 配置保持口径一致，并以当前 usage/cache 决策为准。
