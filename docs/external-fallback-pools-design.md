# 外部备用号池接入与故障转移设计文档

更新时间：2026-06-07

状态：已完成第一版实现，本文档同时记录原始需求、设计方案和当前落地状态。

## 0. 当前落地状态（2026-06-07）

第一版已在当前项目中落地，核心实现文件如下：

- 外部池模型、调度、透传、重试、自动禁用、usage 投影：`src/external_pool.rs`。
- Anthropic 入口接入直连/fallback：`src/anthropic/handlers.rs`。
- 外部池 Admin API：`src/admin/router.rs`、`src/admin/handlers.rs`、`src/admin/service.rs`。
- 外部池配置结构：`src/model/config.rs`。
- 外部池持久化：`src/storage/postgres.rs`。
- 外部池跨实例并发 lease、cooldown 辅助状态：`src/storage/redis_cache.rs`。
- 旧版管理后台入口和配置页：`admin-ui/src/components/external-pools-panel.tsx`。
- 新版 Daisy 管理后台入口和配置页：`admin-ui-daisy/src/components/ExternalPoolsPanel.tsx`。
- Usage 路由/外部池链路展示：`admin-ui/src/components/usage-records-panel.tsx`、`admin-ui-daisy/src/components/UsagePanel.tsx`。

当前第一版明确生效的能力：

1. `externalPoolsEnabled=false` 时完全关闭外部池，不改变本地凭据调度行为。
2. 显式直连策略支持模型规则、路径规则、本地维护开关，命中后记录为 `external_direct_policy`。
3. 本地优先；只有本地容量 fail-fast、无可用凭据、瞬态错误耗尽、可选的不支持模型等场景才会 fallback。
4. 本地容量预检不是长期缓存状态，而是在外部池可用时对本地调度执行 fail-fast acquire；本地可调度时仍走本地凭据。
5. 不引入 `localPoolFallbackGraceMs`，不会等待一段时间后再 fallback。
6. 外部池请求保持原始 Anthropic body 透传，不执行本地 Kiro payload guard、模型映射、profileArn 注入、machineId 逻辑。
7. 外部池响应默认严格透传；单池 `usageProjectionMode=current_path_policy` 时才按当前路径的 `reportedUsage` 策略改写 usage/cache 上报。
8. 外部池非 2xx 响应先用于换池、冷却、自动禁用判断；如果最终需要把该错误返回给下游，必须透传最后一个外部上游的 status/body/主要响应 header，不能包装成本网关的错误 envelope。
9. 外部池按优先级和当前 in-flight 占比选择；相同优先级和占用率的候选随机分散。
10. 外部池并发使用 Redis lease，单池并发和外部池全局并发都能跨实例生效；流式请求会持有 lease 直到 stream 结束或客户端断开。
11. 外部池错误不会在同一个池重复重试；可重试错误会立即排除当前池并尝试下一个外部池，受 `externalPoolRetryMaxAttempts` 限制。
12. 本地 400、请求体过长、context full、improper request、tool schema invalid、JSON invalid、tool_use/tool_result 问题不会 fallback 到外部池。
13. 外部池自动禁用和人工 `enabled` 分离；自动禁用状态持久化在 Postgres，cooldown/in-flight 存 Redis。
14. Usage record 会记录本地/外部路由类型、路由子类型、fallback/direct 原因、本地尝试链路、外部池尝试链路、最终外部池、是否应用 usage 投影。
15. 旧版和新版管理后台都有独立 `备用池` 入口，支持策略配置、外部池新增/编辑/启停/删除/测试/清除自动禁用。

当前第一版保留但尚未生效的配置：

1. `externalPoolMaxQueuedRequests`：保留字段。当前实现不做外部池排队等待，因为用户明确不需要 fallback grace 或等待队列；外部池无槽位时直接视为不可调度。
2. `localPoolCircuitEnabled`、`localPoolCircuitWindowSecs`、`localPoolCircuitOpenAfterFailures`、`localPoolCircuitRequireDistinctCredentials`、`localPoolCircuitOpenSecs`、`localPoolCircuitHalfOpenMaxProbes`：保留字段。当前实现未启用本地池 circuit breaker，默认 `localPoolCircuitEnabled=false`。本地状态预检依赖实际 fail-fast acquire 和 Redis/本地调度状态，不依赖 circuit。

当前第一版已知观测限制：

1. 外部池流式响应在拿到上游响应头后即返回下游，并记录外部池尝试成功；并发 lease 会随 stream 结束或连接关闭释放。若上游在响应头之后发生流式读取错误，该错误会传播给下游连接，但当前不会二次回写 usage record 为失败。该限制只影响 usage 观测精度，不影响外部池并发释放、换池决策或本地凭据调度。

数据保护与备份硬约束：

1. 本地 Kiro 凭据和外部备用号池配置都属于生产核心数据，后续任何升级、发版、迁移、默认配置填充、模型映射规则生成，都不能覆盖、删除或重建这两类数据。
2. 当前存储边界是三套独立数据：运行配置写入 `runtime_config`，本地凭据写入 `credentials`，外部备用号池写入 `external_upstream_pools`。修改 `runtime_config` 时不能隐式改写 `credentials` 或 `external_upstream_pools`。
3. 后续代码变更必须保持凭据保存的非破坏性语义：`save_credentials()` 只能 upsert 传入凭据，不能因为当前进程内存快照缺少某些 ID 就软删除或覆盖数据库里的其他凭据。
4. 外部池更新必须保持局部更新语义：编辑某个外部池只能更新该 `id` 对应行；空 `apiKey` 更新请求必须表示保留原 key，不能把密钥写空。
5. 发版、部署、迁移前必须备份 `credentials`、`external_upstream_pools`、`runtime_config`。如果使用 PgSQL，最低要求是导出这三张表；如果使用 docker volume，还需要保留对应数据库 volume 快照。
6. 管理后台“填充默认规则”只能填充或更新模型映射规则，不允许清空凭据列表、不允许清空外部池列表、不允许重置外部池启用状态、自动禁用状态、并发配置和密钥。
7. 若后续需要引入新的初始化逻辑，必须采用“数据库已有数据优先”的策略：只有目标表为空时才允许从文件或默认值 bootstrap；数据库已有数据时，文件配置只能作为缺失字段补默认，不能作为覆盖源。

第一版验证结果：

- `cargo test --locked --no-default-features`：439 个 Rust 测试通过。
- `pnpm --dir admin-ui build`：通过。
- `pnpm --dir admin-ui-daisy build`：通过。
- `node tools/check-admin-ui-api-parity.mjs`：通过，两个前端 API 覆盖一致。

## 1. 背景

当前 `kiro.rs` 主要通过本地维护的 Kiro 凭据池承接下游 `/v1/messages`、`/cc/v1/messages`、`/ha/v1/messages`、`/na/v1/messages` 请求。系统会在本地凭据池内执行模型映射、Anthropic 请求到 Kiro 请求的转换、payload guard、缓存用量上报模拟、账号并发控制、冷却、失败重试、粘性调度和 usage 记录。

现有本地池在以下情况下可能无法继续承接流量：

- 所有可用凭据并发槽位都满。
- 全局调度并发满。
- 所有可用凭据处于上游临时冷却、RPM 限流或本地不可调度状态。
- 凭据被禁用、额度耗尽、403/401、suspended、risk controlled。
- 本地池对某个模型没有可调度凭据。
- 本地池多次换号后仍遇到 429、408、5xx、网络错误、协议瞬态错误。

用户希望在这类情况下增加“外部备用号池”能力：当本地凭据池明确不能承接请求时，把请求转发到一个或多个外部号池。外部号池由 `baseUrl + key` 配置，独立调度、独立并发、独立状态，优先消耗本地凭据额度，仅在本地池不能承接时消耗外部号池额度。

## 2. 需求总览

### 2.1 核心需求

1. 新增外部备用号池能力。
2. 外部号池独立配置，不混入现有 Kiro 凭据。
3. 外部号池可配置多个，每个号池包含 `baseUrl`、`key`、认证方式、启用状态、优先级、并发上限等。
4. 请求优先走当前系统本地 Kiro 凭据池。
5. 备用池必须有全局总开关，只有总开关开启时才允许 fallback 到备用池。
6. 备用池还需要按场景配置 fallback 开关，例如本地容量不足、本地无可用凭据、本地瞬态错误耗尽、本地不支持模型。
7. 只有本地池明确负载不了、不可用、或本地瞬态错误耗尽，且对应 fallback 开关开启时，才进入外部备用号池。
8. 不增加 `localPoolFallbackGraceMs` 这类“等待多少毫秒再 fallback”的开关。
9. 本地池如果已经明显不可调度，不应该让请求一直等待本地凭据释放。
10. 需要支持两层“直达外部池”能力：
    - 显式策略直连外部池：管理员通过维护模式、模型、路径、下游 key、租户等规则明确要求请求直接走外部池。该路径不是 fallback，必须记录为 `external_direct_policy`。
    - 本地状态预检 fallback：系统仍然本地优先，但每次请求先检查本地池当前状态；如果本地池已明确不可承接，则不真实请求本地上游，直接 fallback 到外部池。该路径必须记录为 `external_fallback_preflight`。
11. 本地状态预检必须每次请求执行，或通过 Redis/本地调度状态做原子 try-acquire；不能依赖一个长期缓存的“本地是否可用”布尔值。
12. 外部号池调度要独立，不集成本地全局并发。
13. 外部号池之间按优先级和并发占用率平均分配。
14. 每个外部号池条目也必须支持单独启用/禁用，禁用后不参与调度。
15. 某个外部号池失败后，当前请求必须马上尝试另一个外部号池，不在同一个外部号池上重复重试。
16. 本地凭据遇到某些错误时，也应在当前请求内马上切换到另一个凭据，不要在同一个凭据上反复重试。
17. 本地凭据和外部号池都需要最大尝试上限。
18. 外部号池默认应尽量透传，不改变请求和响应。
19. 需要增加开关，允许对外部号池响应“给下游上报缓存”，即按当前系统的路径配置对 usage/cache 字段做整形。
20. 外部号池需要自动禁用策略：某些明确代表外部池自身不可用的错误，可以按配置把外部池自动排除出后续调度。
21. 自动禁用必须和人工启用/禁用分开，避免系统禁用覆盖管理员配置意图。
22. 显式直连策略、本地状态预检、fallback 判断、外部池调度、外部池调用、外部池自动禁用、用量上报投影必须抽象为独立模块，方便通过开关组合控制。
23. 需要独立 Admin UI 管理入口，例如独立 tab：`备用号池` 或 `外部号池`。
24. 需要 usage 记录能看清请求是走本地凭据还是走外部号池，以及是本地成功、预检 fallback、本地失败后 fallback，还是策略直连外部池。

### 2.2 明确不需要的能力

1. 不需要外部号池参与 Kiro 凭据 token 刷新。
2. 不需要外部号池参与 machineId 生成。
3. 不需要外部号池注入 profileArn。
4. 不需要外部号池进入本地凭据余额查询。
5. 不需要外部号池接入当前 Kiro endpoint transform。
6. 不需要为了 fallback 新增一个“本地池等待时间”配置。
7. 默认不对外部号池请求执行本地 Kiro payload guard。
8. 默认不修改外部号池响应 usage，除非显式开启“用量上报投影”。
9. 默认不自动禁用外部号池，除非显式开启自动禁用策略。

## 2.3 开关模型与抽象边界

外部备用号池必须由多个开关共同控制，不能做成“只要配置了外部池就无条件 fallback”。推荐分为四层开关：

### 2.3.1 全局总开关

字段建议：

```json
{
  "externalPoolsEnabled": false
}
```

语义：

- `false`：完全关闭外部备用号池能力。所有请求只走本地 Kiro 凭据池，保持现有行为。
- `true`：允许在满足场景开关和外部池可用条件时 fallback 到外部备用号池。

该开关必须优先于所有其他外部池配置。即使配置了外部池条目，只要总开关关闭，也不能 fallback。

### 2.3.2 fallback 场景开关

字段建议：

```json
{
  "fallbackOnLocalCapacityExhausted": true,
  "fallbackOnNoAvailableCredentials": true,
  "fallbackOnLocalTransientExhausted": true,
  "fallbackOnUnsupportedModel": false
}
```

语义：

- 本地容量不足只有在 `fallbackOnLocalCapacityExhausted=true` 时才进入外部池。
- 本地无可用凭据只有在 `fallbackOnNoAvailableCredentials=true` 时才进入外部池。
- 本地瞬态错误耗尽只有在 `fallbackOnLocalTransientExhausted=true` 时才进入外部池。
- 本地不支持模型只有在 `fallbackOnUnsupportedModel=true` 时才进入外部池。

这几类开关必须走统一的 fallback 判定模块，不允许在 handler 或 provider 中散落硬编码判断。

### 2.3.3 单个外部池启用/禁用

每个外部池条目必须有独立 `enabled` 字段：

```json
{
  "enabled": true
}
```

语义：

- `enabled=false` 的外部池不参与调度。
- 禁用外部池不删除配置和历史记录。
- 禁用外部池后，其 in-flight 已开始请求不强制中断；只影响新请求调度。
- 管理后台必须支持单池启用/禁用。

### 2.3.4 用量上报投影开关

每个外部池应独立配置：

```json
{
  "usageProjectionMode": "pass_through"
}
```

语义：

- `pass_through`：严格透传外部池响应，不修改 usage。
- `current_path_policy`：请求仍透传，但响应 usage/cache 上报字段按当前系统路径策略整形，用于给下游上报缓存。

该开关必须独立于 fallback 开关。也就是说，是否 fallback 和 fallback 后是否修改 usage 是两件事。

### 2.3.5 外部池自动禁用开关

外部池需要支持“某些错误自动禁用”的策略，但该策略不能和人工 `enabled` 混在一起。

推荐拆成两类状态：

```json
{
  "enabled": true,
  "autoDisabled": false,
  "autoDisabledReason": null,
  "autoDisabledUntil": null
}
```

语义：

- `enabled`：管理员人工配置。`false` 表示管理员主动停用该外部池。
- `autoDisabled`：系统根据错误策略自动停用。只影响调度，不代表管理员删除或手动关闭。
- `autoDisabledReason`：记录自动禁用原因，例如 `auth_error`、`security_lock`、`quota_exhausted`。
- `autoDisabledUntil`：自动恢复时间。`null` 表示需要管理员手动解除；有时间则到期后自动恢复参与调度。

全局策略建议：

```json
{
  "externalPoolAutoDisableEnabled": false,
  "externalPoolAutoDisableOnAuthError": true,
  "externalPoolAutoDisableOnSecurityLock": true,
  "externalPoolAutoDisableOnQuotaExhausted": false,
  "externalPoolAutoDisableOnMisconfiguredEndpoint": false,
  "externalPoolAutoDisableFailureThreshold": 1,
  "externalPoolAutoDisableWindowSecs": 60,
  "externalPoolAutoDisableDurationSecs": 0
}
```

语义：

- `externalPoolAutoDisableEnabled=false`：完全不自动禁用外部池，只记录错误、进入冷却、当前请求换池。
- `externalPoolAutoDisableOnAuthError=true`：401、403 且错误语义明确为 key 无效、未授权、权限不足时可自动禁用。
- `externalPoolAutoDisableOnSecurityLock=true`：错误语义明确为账号 suspended、risk controlled、locked、security precaution 时可自动禁用。
- `externalPoolAutoDisableOnQuotaExhausted=false`：额度耗尽默认不自动禁用，因为有些外部池额度会按周期恢复；开启后可自动禁用。
- `externalPoolAutoDisableOnMisconfiguredEndpoint=false`：baseUrl、路径、认证方式明显配置错误时默认不自动禁用，避免临时网络或外部池升级误伤。
- `externalPoolAutoDisableFailureThreshold=1`：满足自动禁用条件的连续失败次数阈值。认证、安全锁这类确定性错误建议 1 次即可。
- `externalPoolAutoDisableWindowSecs=60`：自动禁用失败计数统计窗口。只有同一外部池、同一错误原因在该窗口内累计到阈值，才会触发自动禁用。该字段独立于本地池熔断保留字段。
- `externalPoolAutoDisableDurationSecs=0`：`0` 表示自动禁用后不自动恢复，需要管理员手动解除；大于 0 表示禁用到指定秒数后自动恢复。

调度过滤必须同时满足：

```text
enabled == true
autoDisabled == false 或 autoDisabledUntil 已过期
未删除
未冷却
并发未满
当前请求未排除
```

自动禁用不是 retry 决策本身。某个外部池触发自动禁用后，当前请求仍应把该池加入 `excluded_pool_ids` 并立即尝试下一个外部池。

### 2.3.6 必须抽象为独立模块的逻辑

为了便于开关控制和后续维护，以下逻辑必须拆成独立模块或独立服务对象：

1. `FallbackPolicy`
   - 输入本地失败类型、全局开关、场景开关。
   - 输出是否允许 fallback，以及 fallbackReason。
   - 禁止在 handler 中直接用字符串判断是否 fallback。

2. `ExternalPoolManager`
   - 管理外部池配置、状态、并发、冷却、调度、单池启停。
   - 不依赖本地 Kiro credentials。

3. `ExternalPoolClient`
   - 负责原始请求透传、header 清洗、认证替换、stream/non-stream 转发。
   - 不参与本地 Kiro payload 转换。

4. `ExternalPoolRetryPolicy`
   - 决定外部池哪些错误可以换池。
   - 确保同一次请求不会在同一个外部池上重复重试。

5. `ExternalPoolAutoDisablePolicy`
   - 决定外部池哪些错误应自动禁用。
   - 输出 `autoDisabledReason`、`autoDisabledUntil`、是否需要管理员手动解除。
   - 和 `ExternalPoolRetryPolicy` 分离：是否换池、是否禁用是两个决策。

6. `CredentialRetrySwitchPolicy`
   - 决定本地凭据哪些错误要立即换号。
   - 不改变现有调度基础逻辑，只控制当前请求内是否把失败凭据加入 `excluded_ids`。

7. `UsageProjection`
   - 负责外部池响应 usage/cache 上报整形。
   - 默认不启用，只有 `usageProjectionMode=current_path_policy` 时执行。

该拆分的目标是：外部备用号池能力可以独立启停、独立测试、独立观测，并且不污染现有 Kiro 凭据调度核心逻辑。

## 3. 当前代码链路分析

### 3.1 请求入口

Anthropic 兼容 API 入口在：

- `src/anthropic/router.rs`
- `src/anthropic/handlers.rs`

当前主要入口函数：

- `post_messages`
- `post_messages_real_cache_usage`
- `post_messages_ha`
- `post_messages_cc`
- `post_messages_inner`

当前路由：

- `POST /v1/messages`
- `POST /na/v1/messages`
- `POST /ha/v1/messages`
- `POST /cc/v1/messages`

`post_messages_inner` 当前流程大致为：

1. 读取 `Json<MessagesRequest>`。
2. 获取 `KiroProvider`。
3. 应用 runtime config。
4. 处理 thinking 模型名后缀。
5. materialize 远程多模态资源。
6. 检查 web_search。
7. 解析模型映射。
8. Anthropic 请求转换成 Kiro request。
9. 构造 `KiroRequest`。
10. payload guard / payload shaping。
11. token 估算。
12. 构造 usage context。
13. 调用 `handle_stream_request` 或 `handle_non_stream_request`。
14. 通过 `KiroProvider` 调用上游。
15. 解析响应并转换成下游 Anthropic 兼容响应。
16. 写 usage 记录。

### 3.2 本地凭据调度

本地凭据调度主要在：

- `src/kiro/provider.rs`
- `src/kiro/token_manager.rs`

关键方法：

- `KiroProvider::call_api_with_retry`
- `KiroProvider::call_api_stream_with_request_id`
- `KiroProvider::call_api_with_context_with_request_id`
- `MultiTokenManager::acquire_context_for_session`
- `MultiTokenManager::record_session_soft_failure`
- `MultiTokenManager::report_transient_failure_kind`

当前本地池已支持：

- 多凭据故障转移。
- 模型过滤。
- 会话粘性。
- sticky 满时 fallback 到其他本地凭据。
- 单凭据并发限制。
- 全局并发限制。
- 等待队列限制。
- 瞬态错误冷却。
- 429 / 408 / 5xx 处理。
- 401 / 403 处理。
- 402 quota exhausted 处理。
- risk control / suspended 处理。
- token 强刷。
- usage attempts 链路记录。

### 3.3 当前调度等待问题

现有 `acquire_context_for_session` 在某些容量不足场景会等待，例如：

- 本地健康凭据都被并发占满。
- 全局并发已满。
- RPM 限流短时间内可恢复。

这对纯本地池是合理的，但启用外部备用号池后，容量不足场景应该立即进入外部池，而不是一直等待本地池释放。因此需要新增一个“不等待容量”的调度路径，而不是新增“等 N 毫秒再 fallback”的开关。

推荐新增：

```rust
enum AcquireMode {
    WaitNormally,
    FailFastOnCapacity,
}
```

或新增方法：

```rust
try_acquire_context_for_session(...)
```

启用外部备用号池时，主请求链路使用 `FailFastOnCapacity`。未启用外部备用号池时，保持当前 `WaitNormally` 行为，以避免影响现有部署。

### 3.4 当前错误分类问题

当前很多 fallback 判断可通过错误字符串识别，例如：

- `所有凭据已用尽`
- `所有凭据均已禁用`
- `没有支持当前模型的可用凭据`
- `所有可用凭据均处于上游临时冷却`
- `凭据调度排队等待超时`
- `本地限流`
- `retry-after`
- `429`

这不适合长期作为外部备用池触发依据。需要结构化本地失败类型。

推荐新增：

```rust
enum LocalPoolFailureKind {
    CapacityExhausted,
    NoAvailableCredentials,
    AllCandidatesCoolingDown,
    LocalTransientExhausted,
    UnsupportedModel,
    BadRequest,
    PayloadTooLong,
    ContextWindowFull,
    ImproperlyFormed,
    ClientError,
    Unknown,
}
```

并让 `KiroProvider` 返回结构化错误：

```rust
struct KiroProviderError {
    kind: LocalPoolFailureKind,
    message: String,
    attempts: Vec<KiroCredentialAttempt>,
    retry_after: Option<Duration>,
}
```

## 4. 外部备用号池语义

### 4.1 外部号池是什么

外部号池是一个独立上游，通常是 Anthropic-compatible API 服务，例如：

- 另一个 `kiro.rs` 网关。
- 另一个 Claude/Anthropic 兼容网关。
- 其他兼容 `/v1/messages` 的号池系统。

外部号池不是本地 Kiro 凭据，不能进入本地 credentials 表，也不能参与本地 Kiro 凭据调度。

### 4.2 默认“透传”的定义

默认透传模式必须满足：

1. 使用原始下游请求 body。
2. 不执行 Anthropic -> Kiro payload 转换。
3. 不注入 machineId。
4. 不注入 profileArn。
5. 不执行 Kiro endpoint transform。
6. 不执行 Kiro payload guard。
7. 不修改 request 中的 model、messages、tools、system、metadata 等字段。
8. 不修改 response body。
9. 只替换认证 header。
10. 只清理 hop-by-hop header。
11. 外部池返回非 2xx 时，网关仍可读取响应用于错误分类、冷却、换池和自动禁用；但最终返给下游的错误响应必须保持外部上游原始 status/body/content-type 等主要 header，不能改写成本网关自己的错误 JSON。

唯一允许的响应改写例外是“用量上报投影”。该能力必须由单池 `usageProjectionMode=current_path_policy` 显式开启，并且只允许改写响应里的 usage/cache 上报字段，不能改写正文内容、tool 调用、文本块、stop_reason、id、model 等其他字段。请求侧始终保持原始 body 透传。

这意味着现有 handler 不能只使用 `Json<MessagesRequest>`，因为解析后再序列化不能称为严格透传。必须保留 raw body。

推荐入口结构：

```rust
async fn post_messages_raw(
    state: AppState,
    headers: HeaderMap,
    body: Bytes,
    endpoint: &'static str,
) -> Response {
    let payload: MessagesRequest = serde_json::from_slice(&body)?;
    call_local_first_or_external_fallback(state, headers, body, payload, endpoint).await
}
```

入口处理顺序必须保证：

1. 先保留 `raw_body`。
2. 只做基础 JSON 解析、客户端认证、路径合法性等最小校验。
3. 显式直连外部池命中时，直接使用 `raw_body` 转发，不进入 Anthropic -> Kiro 转换。
4. 本地状态预检 fallback 命中时，直接使用 `raw_body` 转发，不进入 Anthropic -> Kiro 转换。
5. 只有确定要走本地 Kiro 凭据时，才执行 Kiro payload 转换、payload guard、machineId/profileArn 注入等本地专用逻辑。
6. 本地转换错误、payload guard 错误、tool schema 错误不应 fallback 到外部池，除非该请求一开始就由显式直连策略命中。

这能避免两个问题：

- 外部池路径被本地 Kiro 转换污染，破坏严格透传。
- 本地请求自身错误被误判成本地池不可承接，从而错误消耗外部池。

### 4.3 路径透传

默认 `preservePath = true`：

- `/v1/messages` -> `{baseUrl}/v1/messages`
- `/cc/v1/messages` -> `{baseUrl}/cc/v1/messages`
- `/ha/v1/messages` -> `{baseUrl}/ha/v1/messages`
- `/na/v1/messages` -> `{baseUrl}/na/v1/messages`

如果外部号池只支持标准 `/v1/messages`，后续可以支持：

```json
{
  "pathRewrite": "standard_v1"
}
```

但第一版默认不改路径。

## 5. fallback 触发规则

fallback 判定必须统一经过 `FallbackPolicy`。外部池总开关、场景开关和本地失败类型必须同时满足，才允许进入外部备用号池。

### 5.0 路由类型和“直达外部池”语义

外部池参与请求路由时，必须先区分路由类型。不能只用“是否走了外部池”一个字段描述，否则无法判断该请求是本地不可承接后的 fallback，还是管理员策略强制外发。

推荐固定路由类型：

```text
local_success
local_error_no_fallback
external_fallback_preflight
external_fallback_after_local_attempts
external_direct_policy
external_error
```

语义：

- `local_success`
  - 请求走本地 Kiro 凭据并成功。
- `local_error_no_fallback`
  - 请求走本地 Kiro 凭据或本地转换流程失败，且错误不允许 fallback。
- `external_fallback_preflight`
  - 系统策略仍然是本地优先，但本地池预检已经明确当前不可承接，所以没有真实请求本地上游，直接转外部池。
  - 这仍然是 fallback，因为路由原因来自本地池不可承接。
- `external_fallback_after_local_attempts`
  - 已经尝试过一个或多个本地凭据，本地可重试错误耗尽后转外部池。
- `external_direct_policy`
  - 管理员显式策略要求直连外部池，不是 fallback。
  - 典型原因：维护模式、指定模型强制外部池、指定路径强制外部池、指定下游 key/租户强制外部池、灰度验证外部池。
- `external_error`
  - 请求已经进入外部池链路，但外部池全部失败或错误不可重试。

这里的“直达外部池”有两层支持：

1. 显式策略直连外部池。
   - 由 `DirectExternalPolicy` 决定。
   - 不检查本地池是否还有可用凭据。
   - 不记录 `fallback_reason`，而记录 `direct_policy_reason`。
   - 日志和 usage 中必须展示为 `external_direct_policy`。

2. 本地状态预检后的 fallback 直达外部池。
   - 由 `LocalPoolPreflight` + `FallbackPolicy` 决定。
   - 先检查本地池状态；如果本地池已经明确不可承接，则不请求本地上游，直接进入外部池。
   - 必须记录 `fallback_reason`，例如 `capacity_exhausted`、`no_available_credentials`、`all_candidates_cooling_down`。
   - 日志和 usage 中必须展示为 `external_fallback_preflight`，并记录 `local_attempted=false`。

### 5.0.1 显式直连外部池策略

显式直连外部池是管理员主动配置的路由策略，不是错误兜底。

建议配置：

```json
{
  "externalDirectPolicyEnabled": false,
  "directExternalOnLocalMaintenance": false,
  "directExternalModelRules": [],
  "directExternalPathRules": [],
  "directExternalConsumerKeyRules": [],
  "directExternalTenantRules": []
}
```

推荐第一版只实现最小能力：

- `directExternalOnLocalMaintenance`
  - 本地池维护模式。开启后所有允许的请求直接走外部池。
- `directExternalModelRules`
  - 指定模型直接走外部池。
- `directExternalPathRules`
  - 指定路径直接走外部池，例如只让 `/cc/v1/messages` 走外部池。

后续可扩展：

- 按下游 API key。
- 按租户。
- 按用户组。
- 按请求 header。

显式直连必须满足：

1. `externalPoolsEnabled=true`。
2. `externalDirectPolicyEnabled=true`。
3. 至少命中一个直连规则。
4. 至少存在一个可调度外部池。

显式直连不应该绕过基础请求校验。JSON 解析失败、客户端 key 不合法、路径不合法等错误仍然应在本地直接返回。

如果显式直连规则命中，但没有可调度外部池，第一版推荐直接返回明确错误，而不是自动回落本地：

```text
External direct policy matched, but no external pools are available
```

原因：

- 显式直连是管理员明确路由意图，静默回落本地会破坏维护模式、灰度规则或隔离规则。
- 如果需要回落本地，应后续增加独立配置 `directExternalFallbackToLocal`，默认仍应为 `false`。
- 该错误应记录 `route_subtype=external_direct_policy`、`external_pool_available=false`、`local_attempted=false`。

### 5.0.2 本地状态预检 fallback

本地状态预检 fallback 的目标是避免每个请求都先打一次必然失败的本地凭据，再进入外部池。

预检不应该是一个长期缓存的 `local_pool_available=true/false`。推荐实现为：

```text
每次请求读取本地池候选状态
+ 读取 Redis 中的跨实例运行态
+ 必要时执行原子 try-acquire / fail-fast acquire
=> 生成 LocalPoolPreflightResult
```

推荐结构：

```rust
struct LocalPoolPreflightResult {
    decision: LocalPreflightDecision,
    failure_kind: Option<LocalPoolFailureKind>,
    candidates_total: usize,
    candidates_enabled: usize,
    candidates_model_supported: usize,
    candidates_available_now: usize,
    global_in_flight: usize,
    global_max_concurrent: Option<usize>,
    queue_depth: usize,
    queue_limit: Option<usize>,
    blocked_by_cooldown: usize,
    blocked_by_concurrency: usize,
    blocked_by_quota: usize,
    blocked_by_auth_or_risk: usize,
    blocked_by_circuit: bool,
    state_source: Vec<LocalPreflightStateSource>,
}
```

推荐决策枚举：

```rust
enum LocalPreflightDecision {
    UseLocal,
    FallbackPreflight(LocalPoolFailureKind),
    DirectExternalPolicy(String),
    RejectWithoutFallback(LocalPoolFailureKind),
}
```

### 5.0.3 本地状态从哪里来

预检状态来源必须分层，避免单实例内存状态和多实例状态不一致：

1. PgSQL / 配置源
   - 凭据是否人工启用。
   - 凭据支持的模型。
   - 凭据是否被标记 quota exhausted。
   - 凭据是否被标记 suspended、risk controlled、unauthorized。
   - 运行时配置，例如全局并发、单凭据并发、fallback 开关。

2. Redis 跨实例运行态
   - 全局 in-flight lease。
   - 单凭据 in-flight lease。
   - 本地等待队列深度。
   - RPM / rate-limit 窗口。
   - cooldown_until。
   - credential-level circuit 状态。
   - local-pool/model/path-level circuit 状态。
   - 最近失败计数和 half-open 探针状态。

3. 本实例内存态
   - 当前进程内尚未同步到 Redis 的短期统计。
   - 本地 token manager 的即时候选排序。
   - 会话 sticky 绑定。
   - 正在执行但尚未释放的本地任务句柄。

4. 原子获取结果
   - 对并发槽位、全局槽位这类会发生竞争的状态，最终不能只靠读取判断。
   - 推荐使用 `try_acquire_context_for_session` 或 `AcquireMode::FailFastOnCapacity` 做原子占位。
   - 如果读取阶段看起来可用，但原子占位失败，应把失败归类为 `CapacityExhausted`，再由 `FallbackPolicy` 决定是否预检 fallback。
   - 如果原子占位成功，预检结果必须携带 reservation / lease，后续本地调用必须复用该 lease，不能再次 acquire。
   - 如果后续因为请求转换失败、客户端断开、或策略改走外部池而不再使用该 lease，必须立即释放，避免本地并发槽位泄漏。

状态准确性原则：

- 能从 Redis 获取的跨实例状态，不应只依赖本地内存。
- 能用原子 lease 判断的容量状态，不应只依赖一次普通读取。
- 如果状态不确定，默认倾向尝试本地，而不是直接消耗外部池，除非本地池 circuit 已打开或显式直连策略命中。
- 预检结果必须写入日志，包括状态来源和阻塞原因，方便解释为什么没有尝试本地凭据。
- 当 `externalPoolsEnabled=false`，或没有任何可调度外部池时，本地预检不能启用 fail-fast 行为，必须保持现有本地等待/排队语义。
- `localPoolPreflightEnabled=true` 只表示允许生成预检决策，不表示一定 fail-fast。是否 fail-fast 还必须由 `externalPoolsEnabled`、fallback 场景开关、外部池可用性共同决定。
- 预检不能把本地转换错误、payload guard 错误、tool schema 错误包装成容量问题。请求自身错误必须走 `RejectWithoutFallback` 或本地错误返回。

### 5.0.4 预检 fallback 触发条件

以下状态可以在没有真实请求本地上游的情况下直接进入 `external_fallback_preflight`：

- 本地全局并发满。
- 本地等待队列满。
- 所有支持该模型的本地候选凭据单账号并发满。
- 没有任何启用的本地凭据。
- 没有任何支持当前模型的本地凭据，并且 `fallbackOnUnsupportedModel=true`。
- 所有候选凭据都 quota exhausted。
- 所有候选凭据都 suspended、risk controlled、unauthorized。
- 所有候选凭据都处于 cooldown，且 cooldown 尚未到期。
- local-pool/model/path-level circuit 处于 open。

以下状态不能直接预检 fallback：

- sticky 凭据不可用，但其他本地凭据可用。
- 某个凭据 429，但其他本地凭据可用。
- 某个凭据 403，但其他本地凭据可用。
- 某个凭据网络错误，但其他本地凭据可用。
- 只是最近出现过错误，但当前仍有健康可调度凭据。
- 请求本身 JSON 不合法。
- 客户端 API key 不合法。
- 请求明显会产生 400。
- 外部池总开关关闭。
- 外部池全部禁用、自动禁用、冷却或并发满。

正确策略是：

```text
sticky 可用 => 优先 sticky
sticky 不可用但其他本地凭据可用 => 尝试其他本地凭据
所有本地候选都不可用 => 预检 fallback 到外部池
```

统一判定伪代码：

```rust
fn should_fallback(kind: LocalPoolFailureKind, config: &ExternalPoolConfig) -> bool {
    if !config.external_pools_enabled {
        return false;
    }

    match kind {
        LocalPoolFailureKind::CapacityExhausted => {
            config.fallback_on_local_capacity_exhausted
        }
        LocalPoolFailureKind::NoAvailableCredentials
        | LocalPoolFailureKind::AllCandidatesCoolingDown => {
            config.fallback_on_no_available_credentials
        }
        LocalPoolFailureKind::LocalTransientExhausted => {
            config.fallback_on_local_transient_exhausted
        }
        LocalPoolFailureKind::UnsupportedModel => {
            config.fallback_on_unsupported_model
        }
        LocalPoolFailureKind::BadRequest
        | LocalPoolFailureKind::PayloadTooLong
        | LocalPoolFailureKind::ContextWindowFull
        | LocalPoolFailureKind::ImproperlyFormed
        | LocalPoolFailureKind::ClientError => false,
        LocalPoolFailureKind::Unknown => false,
    }
}
```

`FallbackPolicy` 还应输出明确的 `fallbackReason`，用于日志、usage 记录和前端详情页展示。

### 5.1 应触发外部备用号池

以下本地最终失败在外部池总开关开启、且对应场景开关开启时，应触发外部备用号池：

#### 本地容量不足

- 所有可用凭据并发满。
- 全局调度并发满。
- 等待队列满。
- 本地没有可立即调度的 slot。

对应 `LocalPoolFailureKind::CapacityExhausted`。

还要求：

```json
{
  "externalPoolsEnabled": true,
  "fallbackOnLocalCapacityExhausted": true
}
```

#### 本地无可用凭据

- 所有凭据禁用。
- 所有凭据额度耗尽。
- 所有凭据 suspended / unauthorized / risk controlled。
- 没有支持当前模型的可用凭据。

对应 `LocalPoolFailureKind::NoAvailableCredentials`。

还要求：

```json
{
  "externalPoolsEnabled": true,
  "fallbackOnNoAvailableCredentials": true
}
```

#### 本地候选全部冷却

- 所有候选都处于 429 冷却。
- 所有候选都处于 5xx / network / protocol 冷却。
- 所有候选都因上游 Retry-After 暂不可用。

对应 `LocalPoolFailureKind::AllCandidatesCoolingDown`。

还要求：

```json
{
  "externalPoolsEnabled": true,
  "fallbackOnNoAvailableCredentials": true
}
```

#### 本地瞬态错误耗尽

- 本地换号尝试后仍然 429。
- 本地换号尝试后仍然 408 / 5xx。
- 本地换号尝试后仍然网络错误。
- 本地换号尝试后仍然非 eventstream / 协议瞬态错误。

对应 `LocalPoolFailureKind::LocalTransientExhausted`。

还要求：

```json
{
  "externalPoolsEnabled": true,
  "fallbackOnLocalTransientExhausted": true
}
```

### 5.2 默认不触发外部备用号池

以下错误不应触发外部备用号池：

- 400 Bad Request。
- `Input is too long`。
- `CONTENT_LENGTH_EXCEEDS_THRESHOLD`。
- `Context window is full`。
- `Improperly formed request`。
- tool schema 不合法。
- tool_use/tool_result 不匹配。
- 多模态 source 不合法。
- 请求 JSON 解析失败。
- 客户端 API key 错误。
- 本地转换阶段明确判断请求不合法。

这些错误说明请求本身有问题，fallback 到外部池会隐藏真实问题并可能产生额外成本。

### 5.3 unsupported model

是否因本地不支持模型而 fallback 到外部池应作为独立开关，默认关闭。

推荐配置：

```json
{
  "fallbackOnUnsupportedModel": false
}
```

原因：本地模型映射失败可能代表请求模型名错误，也可能代表外部池确实支持该模型。默认关闭更安全，需要时再打开。

## 6. 本地凭据“失败后立即换号”策略

### 6.1 需求

本地凭据池内，某些错误不能在同一个凭据上反复重试。当前请求内遇到这些错误后，应立即把当前凭据加入 `excluded_ids`，下一次尝试必须换其他凭据。

### 6.2 推荐配置

新增本地重试切换策略：

```json
{
  "credentialRetrySwitchPolicy": {
    "maxAttempts": 0,
    "switchOnRateLimit": true,
    "switchOnServerError": true,
    "switchOnNetworkError": true,
    "switchOnProtocolError": true,
    "switchOnStreamError": true,
    "switchOnAuthError": true,
    "switchOnQuotaExhausted": true,
    "switchOnRiskControl": true,
    "retrySameAfterTokenRefresh": true
  }
}
```

字段说明：

- `maxAttempts`
  - `0` 表示自动，按当前凭据数量覆盖一轮。
  - `>0` 表示单次请求最多尝试多少个本地凭据。
- `switchOnRateLimit`
  - 429 后当前请求内立即排除当前凭据。
- `switchOnServerError`
  - 408 / 5xx 后当前请求内立即排除当前凭据。
- `switchOnNetworkError`
  - 请求发送失败后当前请求内立即排除当前凭据。
- `switchOnProtocolError`
  - 非 eventstream 或协议异常后当前请求内立即排除当前凭据。
- `switchOnStreamError`
  - 流式读取失败后当前请求内记录软失败；如果尚未向下游输出，可换号，否则不能重放。
- `switchOnAuthError`
  - 401 / 403 认证类错误后换号。
- `switchOnQuotaExhausted`
  - 402 quota exhausted 后立即换号。
- `switchOnRiskControl`
  - suspended / risk controlled 后立即换号。
- `retrySameAfterTokenRefresh`
  - token 失效场景允许同一凭据强刷 token 后再试一次。

### 6.3 错误处理建议

#### 429

1. 当前凭据进入 rate-limit cooldown。
2. 当前请求内加入 `excluded_ids`。
3. 立即尝试其他凭据。
4. 没有其他本地凭据时进入外部备用号池。

#### 408 / 5xx

1. 当前凭据进入 server transient cooldown。
2. 当前请求内加入 `excluded_ids`。
3. 立即尝试其他凭据。
4. 本地候选耗尽后进入外部备用号池。

#### 网络错误

1. 当前凭据或其代理进入 network cooldown。
2. 当前请求内加入 `excluded_ids`。
3. 立即尝试其他凭据。

#### 非 eventstream / 协议异常

1. 当前凭据进入 protocol cooldown。
2. 当前请求内加入 `excluded_ids`。
3. 立即尝试其他凭据。

#### 401 / 403

1. 如果 token 可刷新，允许一次强制刷新。
2. 如果强刷后仍失败，当前凭据进入 auth cooldown 或禁用。
3. 当前请求内加入 `excluded_ids`。
4. 立即尝试其他凭据。

#### 402 quota exhausted

1. 标记额度耗尽或禁用。
2. 当前请求内加入 `excluded_ids`。
3. 立即尝试其他凭据。

#### 400

1. 不换号。
2. 不 fallback 外部池。
3. 直接返回请求错误。

## 7. 外部号池调度策略

### 7.1 独立并发体系

外部备用号池不参与本地调度并发。需要独立维护：

- 外部池全局并发。
- 单外部池并发。
- 外部池等待队列。
- 外部池 cooldown。
- 外部池失败统计。

如果线上多实例部署，外部池 in-flight 必须使用 Redis lease，否则多个实例会分别认为外部池未满，从而打爆外部池。

外部池调度前必须同时满足：

1. 全局开关 `externalPoolsEnabled=true`。
2. 本地失败类型通过 `FallbackPolicy` 判定允许 fallback。
3. 至少存在一个 `enabled=true` 的外部池。
4. 该外部池未被软删除。
5. 该外部池未被系统自动禁用，或自动禁用已到期。
6. 该外部池不在 cooldown。
7. 该外部池当前 in-flight 未达到 `maxConcurrentRequests`。
8. 该外部池未在当前请求的 `excluded_pool_ids` 中。

### 7.2 单个外部池配置

建议结构：

```json
{
  "id": 1,
  "name": "pool-a",
  "baseUrl": "https://example.com",
  "apiKey": "sk-xxx",
  "authType": "bearer",
  "enabled": true,
  "priority": 10,
  "maxConcurrentRequests": 20,
  "usageProjectionMode": "pass_through",
  "autoDisablePolicy": "inherit",
  "preservePath": true,
  "notes": ""
}
```

字段说明：

- `name`
  - 管理后台展示名称。
- `baseUrl`
  - 外部号池基础地址。
- `apiKey`
  - 外部号池认证 key，前端只脱敏展示。
- `authType`
  - `bearer` 或 `x_api_key`。
- `enabled`
  - 是否参与调度。
  - 可以在后台单独切换。
  - 关闭后不删除配置，不影响历史 usage 记录。
  - 关闭后只影响新请求，不强制中断正在转发的请求。
- `priority`
  - 数值越小优先级越高。
- `maxConcurrentRequests`
  - 该外部池最大并发，必须配置，建议默认 `10`。
- `usageProjectionMode`
  - `pass_through` 或 `current_path_policy`。
- `autoDisablePolicy`
  - `inherit`、`disabled` 或 `enabled`。
  - `inherit` 表示使用全局外部池自动禁用策略。
  - `disabled` 表示该池永不被系统自动禁用，只会冷却和记录错误。
  - `enabled` 表示该池强制参与自动禁用策略，可以覆盖全局 `externalPoolAutoDisableEnabled=false`。这只影响自动禁用判断，不影响外部池总开关 `externalPoolsEnabled`。
  - `externalPoolsEnabled=false` 仍是外部池能力总开关；单池 `enabled` 或 `autoDisablePolicy=enabled` 都不能绕过外部池总开关。
- `preservePath`
  - 默认 `true`，保持当前请求路径。
- `notes`
  - 备注。

### 7.3 全局配置

建议结构：

```json
{
  "externalPoolsEnabled": true,
  "externalPoolGlobalMaxConcurrentRequests": 0,
  "externalPoolMaxQueuedRequests": 0,
  "externalPoolRetryMaxAttempts": 0,
  "externalDirectPolicyEnabled": false,
  "directExternalOnLocalMaintenance": false,
  "directExternalModelRules": [],
  "directExternalPathRules": [],
  "fallbackOnLocalCapacityExhausted": true,
  "fallbackOnNoAvailableCredentials": true,
  "fallbackOnLocalTransientExhausted": true,
  "fallbackOnUnsupportedModel": false,
  "localPoolPreflightEnabled": true,
  "localPoolCircuitEnabled": false,
  "localPoolCircuitWindowSecs": 60,
  "localPoolCircuitOpenAfterFailures": 3,
  "localPoolCircuitRequireDistinctCredentials": 2,
  "localPoolCircuitOpenSecs": 30,
  "localPoolCircuitHalfOpenMaxProbes": 1,
  "externalPoolAutoDisableEnabled": false,
  "externalPoolAutoDisableOnAuthError": true,
  "externalPoolAutoDisableOnSecurityLock": true,
  "externalPoolAutoDisableOnQuotaExhausted": false,
  "externalPoolAutoDisableOnMisconfiguredEndpoint": false,
  "externalPoolAutoDisableFailureThreshold": 1,
  "externalPoolAutoDisableWindowSecs": 60,
  "externalPoolAutoDisableDurationSecs": 0
}
```

字段说明：

- `externalPoolsEnabled`
  - 总开关，默认 `false`。
- `externalPoolGlobalMaxConcurrentRequests`
  - 外部池整体并发，`0` 表示不限。
- `externalPoolMaxQueuedRequests`
  - 第一版保留字段，当前不生效。
  - 当前实现不做外部池等待队列；外部池无并发槽位时直接视为不可调度并尝试其他外部池或返回外部池不可用。
  - 保留该字段是为了以后如果明确需要外部池排队，可以不破坏配置结构。
- `externalPoolRetryMaxAttempts`
  - `0` 表示自动覆盖一轮所有 enabled 外部池。
- `externalDirectPolicyEnabled`
  - 显式直连外部池策略总开关，默认 `false`。
  - 关闭时，模型/路径/维护模式等直连规则全部不生效。
- `directExternalOnLocalMaintenance`
  - 本地池维护模式。开启后允许请求直接走外部池，记录为 `external_direct_policy`。
- `directExternalModelRules`
  - 指定模型直接走外部池的规则列表。
- `directExternalPathRules`
  - 指定路径直接走外部池的规则列表。
- `fallbackOnLocalCapacityExhausted`
  - 本地容量不足时 fallback。
- `fallbackOnNoAvailableCredentials`
  - 本地无可用凭据时 fallback。
- `fallbackOnLocalTransientExhausted`
  - 本地瞬态错误耗尽时 fallback。
- `fallbackOnUnsupportedModel`
  - 本地不支持模型时 fallback，默认关闭。
- `localPoolPreflightEnabled`
  - 是否启用本地状态预检，默认 `true`。
  - 关闭后只在真实本地调用失败后 fallback，不做 `external_fallback_preflight`。
- `localPoolCircuitEnabled`
  - 第一版保留字段，默认 `false`，当前不生效。
  - 当前本地状态预检依赖实际 fail-fast acquire 和调度器/Redis 运行态，不依赖 circuit breaker。
- `localPoolCircuitWindowSecs`
  - 本地池失败统计窗口，建议默认 `60s`。
- `localPoolCircuitOpenAfterFailures`
  - 统计窗口内触发 circuit open 的失败次数，建议默认 `3`。
- `localPoolCircuitRequireDistinctCredentials`
  - 触发本地池 circuit 至少需要涉及多少个不同本地凭据，建议默认 `2`，避免单个坏账号影响整个池。
- `localPoolCircuitOpenSecs`
  - circuit 打开持续时间，建议默认 `30s`。
- `localPoolCircuitHalfOpenMaxProbes`
  - half-open 阶段允许多少个请求试探本地池，建议默认 `1`。

本地池 circuit 只能统计本地池级可重试失败，例如 429、408、5xx、网络错误、协议错误、认证/风控类凭据不可用。以下错误不能计入 circuit：

- 400 Bad Request。
- `Input is too long`。
- `CONTENT_LENGTH_EXCEEDS_THRESHOLD`。
- `Context window is full`。
- `Improperly formed request`。
- tool schema invalid。
- tool_use/tool_result 不匹配。
- JSON parse error。
- 客户端 API key 错误。

否则会因为用户请求自身问题错误打开本地池 circuit，导致正常请求被错误导向外部池。
- `externalPoolAutoDisableEnabled`
  - 外部池自动禁用总开关，默认 `false`。
- `externalPoolAutoDisableOnAuthError`
  - 外部池 401/403 且语义明确为认证/权限问题时是否自动禁用。
- `externalPoolAutoDisableOnSecurityLock`
  - 外部池返回 suspended、risk controlled、locked、security precaution 等账号安全锁定语义时是否自动禁用。
- `externalPoolAutoDisableOnQuotaExhausted`
  - 外部池返回 quota exhausted、payment required、insufficient credits 等额度耗尽语义时是否自动禁用，默认关闭。
- `externalPoolAutoDisableOnMisconfiguredEndpoint`
  - 外部池 baseUrl、路径、认证方式明显配置错误时是否自动禁用，默认关闭。
- `externalPoolAutoDisableFailureThreshold`
  - 满足自动禁用错误条件的连续失败次数阈值，默认 `1`。
- `externalPoolAutoDisableWindowSecs`
  - 自动禁用失败计数统计窗口，默认 `60s`。该字段独立于本地池 circuit 保留字段。
- `externalPoolAutoDisableDurationSecs`
  - 自动禁用持续时间。`0` 表示直到管理员手动解除。

### 7.4 选择算法

外部池选择步骤：

1. 过滤不可用池：
   - disabled。
   - auto disabled 且未到恢复时间。
   - deleted。
   - cooldown。
   - in-flight >= maxConcurrentRequests。
   - 当前请求内已失败的 pool id。
2. 按 `priority` 分组。
3. 选择最小 priority 的可用分组。
4. 同 priority 内按占用率选择：

```text
load = inFlight / maxConcurrentRequests
```

选择 load 最低的池。

如果 load 相同，使用 round-robin 或近期选中次数打散。

该策略效果：

- 优先级可控。
- 同优先级内按照并发容量平均分配。
- 大池自然承接更多请求。
- 某个池满了或冷却时立即跳过。

### 7.5 外部池会话粘性

建议支持轻量粘性，但不能等待粘性池：

1. 如果 conversationId 上次使用外部池 A，且 A 当前可用，优先 A。
2. 如果 A 并发满、冷却、禁用、当前请求内已失败，立即选择其他外部池。
3. 不为外部池粘性等待。

## 8. 外部池错误处理

### 8.1 外部池失败后立即换池

当前请求内，如果外部池 A 出现可重试池级错误，必须马上尝试外部池 B，不在 A 上重复重试。

需要维护：

```rust
let mut excluded_pool_ids = HashSet::new();
```

每次可重试错误后：

1. 标记外部池状态。
2. 根据 `ExternalPoolAutoDisablePolicy` 判断是否需要自动禁用。
3. 加入 `excluded_pool_ids`。
4. 释放 in-flight lease。
5. 选择下一个外部池。

### 8.2 外部池错误分类

外部池错误处理需要同时回答三个问题：

1. 当前请求是否可以换另一个外部池。
2. 当前外部池是否需要进入冷却。
3. 当前外部池是否需要自动禁用。

这三个问题必须分开实现。不能因为一个错误可重试就自动禁用，也不能因为自动禁用关闭就阻止当前请求换池。

#### 429

- 当前外部池进入 rate-limit cooldown。
- 如果有 `Retry-After`，使用 `Retry-After`。
- 没有则使用默认 `externalPoolRateLimitCooldownSecs`，建议默认 `30s`。
- 当前请求立即换下一个外部池。
- 不自动禁用。429 通常代表限流或并发压力，不代表外部池配置失效。

#### 408 / 5xx

- 当前外部池进入 server cooldown。
- 建议默认 `10s`。
- 当前请求立即换下一个外部池。
- 默认不自动禁用。只有后续明确增加“连续 N 次 5xx 自动禁用”策略时才可禁用，第一版不建议开启。

#### 网络错误

- 当前外部池进入 network cooldown。
- 建议默认 `10s`。
- 当前请求立即换下一个外部池。
- 默认不自动禁用。网络错误可能是链路抖动、DNS、TLS、外部池重启，不应直接永久排除。

#### 401 / 403

- 当前外部池 key 可能无效。
- 标记 auth error。
- 如果 `externalPoolAutoDisableEnabled=true` 且 `externalPoolAutoDisableOnAuthError=true`，并且单池 `autoDisablePolicy` 未关闭，自动禁用。
- 当前请求立即换下一个外部池。

需要根据错误体进一步区分：

- 明确认证失败：`invalid api key`、`unauthorized`、`permission denied`、`not authorized`、`forbidden`，可归类为 `auth_error`。
- 明确账号安全锁定：`suspended`、`risk controlled`、`locked`、`security precaution`，可归类为 `security_lock`。
- 如果 403 来自请求模型无权限，且下游请求本身可能选择了错误模型，应记录为池级失败并换池，但是否自动禁用需要谨慎。第一版建议只在错误文案明确指向账号/key 状态时自动禁用。

#### 402 / quota exhausted

- 当前外部池额度可能耗尽。
- 当前请求立即换下一个外部池。
- 如果 `externalPoolAutoDisableEnabled=true` 且 `externalPoolAutoDisableOnQuotaExhausted=true`，并且错误体明确包含 quota exhausted、insufficient credits、payment required 等语义，可自动禁用或禁用到管理员配置的恢复时间。
- 默认不自动禁用。原因是部分外部池额度可能按日、按小时或账期恢复，永久禁用会导致后续可用额度无法自动利用。

#### 400

- 不换池。
- 直接返回。
- 该错误大概率是请求本身不合法。
- 不冷却。
- 不自动禁用。

典型错误：

- `Input is too long`。
- `CONTENT_LENGTH_EXCEEDS_THRESHOLD`。
- `Context window is full`。
- `Improperly formed request`。
- JSON schema invalid。
- tool_use/tool_result 不匹配。

这些错误必须透传给下游，不能换其他外部池，也不能禁用外部池。

#### 协议错误 / 非法响应

- 如果尚未向客户端输出任何有效事件，当前请求可换下一个外部池。
- 当前外部池进入 protocol cooldown，建议默认 `10s`。
- 默认不自动禁用。
- 如果错误明确是配置错误，例如 baseUrl 返回 HTML 登录页、路径 404、认证方式不匹配，并且 `externalPoolAutoDisableOnMisconfiguredEndpoint=true`，可按 `misconfigured_endpoint` 自动禁用。

#### stream 中途失败

- 如果尚未向客户端输出任何有效事件，理论上可以换池重放。
- 如果已经向客户端输出事件，不能换池，只能返回 stream error 并记录。
- 这是流式协议限制，无法绕过。
- 默认不自动禁用，除非错误同时满足认证、安全锁定、额度耗尽或明显配置错误等自动禁用条件。

### 8.3 自动禁用决策矩阵

推荐第一版矩阵：

| 错误类型 | 当前请求换池 | 冷却 | 自动禁用默认 | 可配置自动禁用 | 原因 |
| --- | --- | --- | --- | --- | --- |
| 429 | 是 | rate-limit cooldown | 否 | 不建议 | 限流通常可恢复 |
| 408 / 5xx | 是 | server cooldown | 否 | 后续可扩展 | 服务端瞬态错误 |
| 网络错误 | 是 | network cooldown | 否 | 后续可扩展 | 链路可能抖动 |
| 401 明确 key 无效 | 是 | 可选短冷却 | 否，除非总开关开启 | 是 | key/权限问题通常持续存在 |
| 403 明确未授权 | 是 | 可选短冷却 | 否，除非总开关开启 | 是 | key/权限问题通常持续存在 |
| suspended / locked / risk controlled | 是 | 可选短冷却 | 否，除非总开关开启 | 是 | 账号状态异常通常持续存在 |
| 402 / quota exhausted | 是 | 可选额度冷却 | 否 | 是 | 额度可能周期恢复 |
| baseUrl/path/authType 明显配置错误 | 是 | protocol cooldown | 否 | 是 | 配置错误需要人工处理 |
| 400 请求非法 | 否 | 否 | 否 | 否 | 请求自身问题 |
| stream 已输出后失败 | 否 | 按错误类型 | 否 | 仅满足确定性条件时 | 已无法安全重放 |

自动禁用触发后必须写入：

- `autoDisabled=true`。
- `autoDisabledReason`。
- `autoDisabledAt`。
- `autoDisabledUntil`。
- `autoDisabledLastError`。
- 最近一次外部池 attempt 记录。

如果 `externalPoolAutoDisableDurationSecs=0`，需要管理员在 Admin UI 手动解除自动禁用。

如果 `externalPoolAutoDisableDurationSecs>0`，到期后外部池可重新参与调度，但应保留最近自动禁用原因供页面展示。

### 8.4 外部池最大尝试上限

配置：

```json
{
  "externalPoolRetryMaxAttempts": 0
}
```

语义：

- `0` 表示自动，最多覆盖一轮所有 enabled 外部池。
- `>0` 表示单次请求最多尝试多少个外部池。

如果外部池全部不可用，返回明确错误：

```text
No available external fallback pools
```

并记录 usage failure。

## 9. 用量上报投影

### 9.1 需求

外部号池默认完全透传，不修改响应。但用户需要一个开关，允许外部号池响应按当前系统配置给下游上报缓存，也就是对 response usage/cache 上报字段做整形。

该能力需要显式开启，因为开启后不再是严格 byte-level 透传。

### 9.2 建议命名

中文名称：

```text
用量上报投影
```

英文配置：

```json
{
  "usageProjectionMode": "pass_through"
}
```

可选值：

- `pass_through`
  - 默认值。
  - 不修改外部池响应 usage。
  - 最符合“不要改变任何东西”。
- `current_path_policy`
  - 请求仍透传。
  - 响应 usage/cache 上报字段按当前系统 `reportedUsage` 配置整形。
  - 路径策略按当前 endpoint 区分 `/v1`、`/cc`、`/ha`、`/na`。

### 9.3 UI 文案要求

后台页面必须明确提示：

```text
开启“用量上报投影”后，请求仍透传到外部号池，但响应中的 usage/cache 上报字段会按当前系统路径策略调整。若要求严格响应透传，请保持关闭。
```

### 9.4 模块拆分

现有 `ReportedUsageConfig` 在 `src/model/config.rs`，但实际响应 usage 整形逻辑与 handler/usage context 绑定较深。建议抽出独立模块：

```text
src/anthropic/usage_projection.rs
```

输入：

- endpoint path。
- 原始 `MessagesRequest`。
- 外部池原始响应 usage。
- `ReportedUsageConfig`。
- `PromptCacheTracker`。
- model。
- conversationId。
- input token estimate。

输出：

- projected usage。
- usage source。
- 是否修改了响应。

### 9.5 非流式处理

非流式响应：

1. 读取外部池 JSON 响应 body。
2. 如果 `usageProjectionMode=pass_through`，直接返回。
3. 如果 `usageProjectionMode=current_path_policy`：
   - 解析 JSON。
   - 根据当前路径策略计算 usage。
   - 替换 response body 中的 `usage` 字段。
   - 返回修改后的 body。

### 9.6 流式处理

流式响应：

1. 如果 `usageProjectionMode=pass_through`，直接 pipe SSE。
2. 如果 `usageProjectionMode=current_path_policy`：
   - 解析 SSE event。
   - 找到包含 usage 的事件。
   - 按路径策略重写 usage。
   - 其他事件不改变。

注意：流式 usage 投影不等于严格 byte-level 透传，应在 UI 和日志里标明。

## 10. 数据模型

### 10.1 PgSQL 表：external_upstream_pools

建议新增：

```sql
CREATE TABLE IF NOT EXISTS external_upstream_pools (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT NOT NULL,
    auth_type TEXT NOT NULL DEFAULT 'bearer',
    enabled BOOLEAN NOT NULL DEFAULT true,
    priority INTEGER NOT NULL DEFAULT 100,
    max_concurrent_requests INTEGER NOT NULL DEFAULT 10,
    usage_projection_mode TEXT NOT NULL DEFAULT 'pass_through',
    auto_disable_policy TEXT NOT NULL DEFAULT 'inherit',
    auto_disabled BOOLEAN NOT NULL DEFAULT false,
    auto_disabled_reason TEXT,
    auto_disabled_at TIMESTAMPTZ,
    auto_disabled_until TIMESTAMPTZ,
    auto_disabled_last_error TEXT,
    preserve_path BOOLEAN NOT NULL DEFAULT true,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_external_upstream_pools_active_priority
    ON external_upstream_pools (priority ASC, id ASC)
    WHERE deleted_at IS NULL AND enabled = true;

CREATE INDEX IF NOT EXISTS idx_external_upstream_pools_auto_disabled
    ON external_upstream_pools (auto_disabled, auto_disabled_until)
    WHERE deleted_at IS NULL;
```

字段说明：

- `enabled` 是管理员人工启用/禁用。
- `auto_disabled` 是系统自动禁用。
- 调度时必须同时检查 `enabled=true` 和 `auto_disabled=false`，或者 `auto_disabled_until` 已到期。
- 自动禁用状态建议持久化在 PgSQL，而不是只存在 Redis。否则多实例部署时，一个实例自动禁用了外部池，另一个实例仍可能继续调度该池。
- cooldown、in-flight lease 这类高频运行态仍建议放 Redis。

### 10.2 运行状态

第一版 Redis 运行状态使用以下 key：

```text
external_pool:inflight:lease_sequence
external_pool:inflight:{id}:last_seen
external_pool:inflight:{id}:acquired
external_pool:inflight:{id}:kind
external_pool:global:inflight:last_seen
external_pool:global:inflight:acquired
external_pool:global:inflight:kind
external_pool:{id}:cooldown
external_pool:{id}:auto_disable_failures:{reason}
```

说明：

- `last_seen/acquired/kind` 是 Redis lease 的三组状态，单池和全局各一份。
- 单池 `maxConcurrentRequests` 和全局 `externalPoolGlobalMaxConcurrentRequests` 都通过 Redis Lua 原子判断与占用。
- 流式外部池请求会把 lease 持有到 stream 结束或客户端断开，并在请求存活期间周期性续租，避免长流式请求被过期清理后错误释放并发槽。
- cooldown 写入 `external_pool:{id}:cooldown`，同时保留进程内 fallback 状态；多实例以 Redis 为准。
- 自动禁用失败计数使用 `external_pool:{id}:auto_disable_failures:{reason}`，TTL 使用 `externalPoolAutoDisableWindowSecs`，自动禁用最终状态写入 PgSQL。
- 第一版没有实现外部池 session 粘性绑定；外部池之间按优先级和 in-flight 占比均衡。

如果需要页面展示长期统计，可以定期或请求结束时写 PgSQL 观测字段，但并发 lease 不建议使用 PgSQL 字段实现。

### 10.3 usage_records 扩展

第一版已新增 usage record 字段：

- `route_kind`
  - `local_credential`
  - `external_pool`
- `external_pool_id`
- `external_pool_name`
- `fallback_reason`
- `local_attempts`
- `external_attempts`
- `usage_projection_applied`

用途：

- 看到请求最终走本地还是外部池。
- 看到 fallback 原因。
- 看到本地和外部池尝试链路。
- 看到是否修改了 usage 上报。

## 11. Admin API

第一版已新增接口：

```text
GET    /api/admin/external-pools
POST   /api/admin/external-pools
PUT    /api/admin/external-pools/:id
DELETE /api/admin/external-pools/:id
POST   /api/admin/external-pools/:id/enabled
POST   /api/admin/external-pools/:id/test
POST   /api/admin/external-pools/:id/auto-disabled/clear
GET    /api/admin/external-pools/status
```

外部池全局策略通过现有 runtime config 接口读写，即运行时配置对象里的 `externalPools` 字段；第一版没有单独提供 `/config/external-pools` 路由。

API 要求：

- `apiKey` 返回时必须脱敏。
- 创建/更新时允许修改 key。
- 删除建议软删除。
- `test` 应测试 `/v1/messages` 或 `/v1/models`，具体取决于外部池兼容能力。
- `status` 返回每个池的 in-flight、cooldown、是否可调度、跳过原因。
- `auto-disabled/clear` 只解除系统自动禁用，不改变管理员 `enabled` 配置。

## 12. Admin UI

### 12.1 新增 tab

新增独立 tab：

```text
备用号池
```

不要放入 `凭据` tab，因为外部号池不是本地 Kiro 凭据。

### 12.2 顶部全局设置

字段：

- 启用备用号池。
- 启用显式直连外部池策略。
- 本地池维护模式直连外部池。
- 模型直连外部池规则。
- 路径直连外部池规则。
- 本地容量不足时 fallback。
- 本地无可用凭据时 fallback。
- 本地瞬态错误耗尽时 fallback。
- 本地不支持模型时 fallback。
- 外部池全局最大并发。
- 外部池最大尝试次数。
- 外部池最大等待队列。
- 启用外部池自动禁用策略。
- 401/403 认证/权限错误是否自动禁用外部池。
- suspended / risk controlled / locked 是否自动禁用外部池。
- quota exhausted 是否自动禁用外部池。
- baseUrl/path/authType 明显配置错误是否自动禁用外部池。
- 自动禁用连续失败阈值。
- 自动禁用恢复时间，`0` 表示需要手动解除。

### 12.3 号池列表

列表字段：

- 名称。
- baseUrl。
- key 脱敏。
- authType。
- enabled。
- autoDisabled 状态。
- autoDisabledReason。
- autoDisabledUntil。
- priority。
- maxConcurrentRequests。
- 当前 in-flight。
- cooldown。
- 最近错误。
- 最近成功。
- 成功次数。
- 失败次数。
- 用量上报投影模式。

操作：

- 新增。
- 编辑。
- 启用/禁用。
- 测试。
- 清冷却。
- 解除自动禁用。
- 删除。

### 12.4 新增/编辑表单

字段：

- name。
- baseUrl。
- apiKey。
- authType。
- enabled。
- priority。
- maxConcurrentRequests。
- usageProjectionMode。
- autoDisablePolicy。
- preservePath。
- notes。

`usageProjectionMode` 文案：

- `严格透传`
  - 对应 `pass_through`。
- `按当前路径策略上报缓存`
  - 对应 `current_path_policy`。

`autoDisablePolicy` 文案：

- `继承全局策略`
  - 对应 `inherit`。
- `该池不自动禁用`
  - 对应 `disabled`。
- `该池允许自动禁用`
  - 对应 `enabled`，可以覆盖全局 `externalPoolAutoDisableEnabled=false`，但不能绕过外部池总开关 `externalPoolsEnabled=false`。

页面状态展示建议：

- `人工停用`：`enabled=false`。
- `系统自动禁用`：`enabled=true` 且 `autoDisabled=true` 且未到恢复时间。
- `冷却中`：未禁用但 cooldown 未结束。
- `可调度`：人工启用、未自动禁用、未冷却、并发未满。

这几个状态必须分开展示。不能只显示一个笼统的“不可用”，否则无法判断是管理员手动停用、系统自动停用、还是临时冷却。

## 13. 请求流程设计

### 13.1 总体流程

伪代码：

```rust
let raw_body = body.clone();
let payload = parse_messages_request(&raw_body)?;

validate_client_auth_and_basic_request(&payload, &headers)?;

if direct_external_policy.matches(&payload, endpoint_path, &headers) {
    return external_pool_manager
        .forward_with_failover(
            raw_body,
            original_headers,
            endpoint_path,
            payload,
            usage_projection_config,
            RouteSubtype::ExternalDirectPolicy,
            RouteReason::DirectPolicy(direct_external_policy.reason()),
        )
        .await;
}

let preflight = local_pool_preflight.evaluate(
    &payload,
    endpoint_path,
    &headers,
    LocalPreflightMode::FailFastOnlyWhenExternalFallbackIsActuallyPossible,
).await;

if let LocalPreflightDecision::FallbackPreflight(kind) = preflight.decision {
    if fallback_policy.allow(kind) {
        return external_pool_manager
            .forward_with_failover(
                raw_body,
                original_headers,
                endpoint_path,
                payload,
                usage_projection_config,
                RouteSubtype::ExternalFallbackPreflight,
                RouteReason::Fallback(kind),
            )
            .await;
    }
}

let local_result = kiro_provider
    .call_api_with_policy(
        converted_kiro_body,
        LocalDispatchMode::UseExistingPreflightReservationOrWaitNormally(preflight.reservation),
        RetrySwitchPolicy::exclude_failed_credential_immediately(),
    )
    .await;

match local_result {
    Ok(response) => local_response(response),

    Err(err) if fallback_policy.allow(err.kind) => {
        external_pool_manager
            .forward_with_failover(
                raw_body,
                original_headers,
                endpoint_path,
                payload,
                usage_projection_config,
                RouteSubtype::ExternalFallbackAfterLocalAttempts,
                RouteReason::Fallback(err.kind),
            )
            .await
    }

    Err(err) => map_provider_error(err),
}
```

流程说明：

1. 基础校验失败不进入外部池。
2. 显式直连策略命中时，直接走外部池，记录 `external_direct_policy`。
3. 本地状态预检明确不可承接时，直接走外部池，记录 `external_fallback_preflight`。
4. 本地预检认为可用时，才调用本地凭据。
5. 本地调用失败且错误允许 fallback 时，走外部池，记录 `external_fallback_after_local_attempts`。
6. 本地调用失败且错误不允许 fallback 时，直接返回本地错误，记录 `local_error_no_fallback`。
7. 如果预检阶段已经 acquire 本地 lease，本地调用必须复用该 lease；任意提前返回路径都必须释放 lease。
8. 如果外部池关闭或没有可调度外部池，预检不能让本地调度 fail-fast，必须回到现有本地等待/排队行为。

### 13.2 外部池 failover

伪代码：

```rust
let mut excluded_pool_ids = HashSet::new();
let max_attempts = configured_or_enabled_pool_count();

for attempt in 0..max_attempts {
    let pool = select_pool(excluded_pool_ids)?;
    let lease = acquire_pool_lease(pool)?;

    let result = external_client.forward(pool, raw_body, headers, endpoint_path).await;

    match result {
        Ok(resp) => {
            record_pool_success(pool);
            return maybe_project_usage(resp, pool.usage_projection_mode);
        }

        Err(err) if err.is_retryable_pool_error() => {
            record_pool_failure(pool, err);
            mark_pool_cooldown_if_needed(pool, err);
            auto_disable_pool_if_policy_matches(pool, err);
            excluded_pool_ids.insert(pool.id);
            release_pool_lease(lease);
            continue;
        }

        Err(err) => {
            release_pool_lease(lease);
            return map_external_pool_error(err);
        }
    }
}

return no_available_external_pool_error();
```

## 14. Header 转发规则

### 14.1 必须替换的 header

不能把当前系统请求 key 转发给外部池。

需要删除：

- 当前系统下游 `x-api-key`。
- 当前系统下游 `Authorization`。

再根据外部池配置写入：

- `Authorization: Bearer {apiKey}`
- 或 `x-api-key: {apiKey}`

### 14.2 应保留的 header

建议保留：

- `content-type`
- `accept`
- `user-agent`
- `anthropic-version`
- `anthropic-beta`
- `x-request-id`

### 14.3 必须删除的 hop-by-hop header

必须删除：

- `host`
- `connection`
- `content-length`
- `transfer-encoding`
- `keep-alive`
- `proxy-authenticate`
- `proxy-authorization`
- `te`
- `trailer`
- `upgrade`

## 15. 流式响应限制

流式请求有一个硬限制：

- 如果还没有向客户端输出任何有效 SSE event，外部池失败后可以尝试换池重放。
- 如果已经向客户端输出 event，就不能再换池重放，因为客户端已经收到部分响应。

因此流式外部池实现需要维护：

```rust
has_sent_downstream_event: bool
```

如果 `false`，可 retry next pool。

如果 `true`，只能转发错误事件或结束流，并记录失败。

## 16. 可观测性

### 16.1 日志

需要记录：

- 本地池是否成功。
- 路由类型：`local_success`、`external_fallback_preflight`、`external_fallback_after_local_attempts`、`external_direct_policy` 等。
- 是否真实尝试过本地凭据：`local_attempted`。
- 本地预检决策和状态来源。
- 本地池失败类型。
- fallback 是否触发。
- fallback 原因。
- 显式直连原因。
- 外部池尝试链路。
- 外部池最终命中。
- 外部池错误。
- 外部池自动禁用决策。
- 外部池被跳过原因：人工停用、系统自动禁用、冷却、并发满、当前请求已排除。
- usageProjection 是否启用。

示例：

```text
local_pool_preflight decision=fallback_preflight kind=CapacityExhausted request_id=...
routing_decision route_subtype=external_fallback_preflight local_attempted=false reason=CapacityExhausted state_source=redis_lease,postgres_credentials
external_pool_fallback_started reason=CapacityExhausted stage=preflight pools=3
external_pool_attempt pool_id=1 status=429 action=retry_next
external_pool_attempt pool_id=2 status=403 action=retry_next
external_pool_auto_disable pool_id=2 reason=auth_error until=manual
external_pool_attempt pool_id=3 status=200 action=success
```

### 16.2 Usage 详情页

第一版 Usage 详情页已展示：

- 路由类型：本地凭据 / 外部备用号池。
- 路由子类型：本地成功 / 预检 fallback / 本地失败后 fallback / 策略直连。
- 本地是否真实尝试。
- 本地预检结果和阻塞原因。
- fallback 原因。
- 显式直连原因。
- 本地凭据尝试链路。
- 外部池尝试链路。
- 最终命中的外部池。
- 外部池是否因为策略被自动禁用。
- 外部池跳过原因。
- 是否应用用量上报投影。

## 17. 配置默认值建议

```json
{
  "externalPoolsEnabled": false,
  "externalPoolGlobalMaxConcurrentRequests": 0,
  "externalPoolMaxQueuedRequests": 0,
  "externalPoolRetryMaxAttempts": 0,
  "externalDirectPolicyEnabled": false,
  "directExternalOnLocalMaintenance": false,
  "directExternalModelRules": [],
  "directExternalPathRules": [],
  "fallbackOnLocalCapacityExhausted": true,
  "fallbackOnNoAvailableCredentials": true,
  "fallbackOnLocalTransientExhausted": true,
  "fallbackOnUnsupportedModel": false,
  "localPoolPreflightEnabled": true,
  "localPoolCircuitEnabled": false,
  "localPoolCircuitWindowSecs": 60,
  "localPoolCircuitOpenAfterFailures": 3,
  "localPoolCircuitRequireDistinctCredentials": 2,
  "localPoolCircuitOpenSecs": 30,
  "localPoolCircuitHalfOpenMaxProbes": 1,
  "externalPoolAutoDisableEnabled": false,
  "externalPoolAutoDisableOnAuthError": true,
  "externalPoolAutoDisableOnSecurityLock": true,
  "externalPoolAutoDisableOnQuotaExhausted": false,
  "externalPoolAutoDisableOnMisconfiguredEndpoint": false,
  "externalPoolAutoDisableFailureThreshold": 1,
  "externalPoolAutoDisableWindowSecs": 60,
  "externalPoolAutoDisableDurationSecs": 0,
  "externalPoolRateLimitCooldownSecs": 30,
  "externalPoolServerErrorCooldownSecs": 10,
  "externalPoolNetworkErrorCooldownSecs": 10,
  "externalPoolProtocolErrorCooldownSecs": 10,
  "credentialRetrySwitchPolicy": {
    "maxAttempts": 0,
    "switchOnRateLimit": true,
    "switchOnServerError": true,
    "switchOnNetworkError": true,
    "switchOnProtocolError": true,
    "switchOnStreamError": true,
    "switchOnAuthError": true,
    "switchOnQuotaExhausted": true,
    "switchOnRiskControl": true,
    "retrySameAfterTokenRefresh": true
  }
}
```

单个外部池默认：

```json
{
  "enabled": true,
  "priority": 100,
  "maxConcurrentRequests": 10,
  "authType": "bearer",
  "usageProjectionMode": "pass_through",
  "autoDisablePolicy": "inherit",
  "preservePath": true
}
```

## 18. 实施步骤

### 阶段 1：基础模型和管理能力

1. 新增外部池配置类型。
2. 新增 PgSQL 表 `external_upstream_pools`。
3. 新增 storage CRUD。
4. 新增 Admin API。
5. 新增 Admin UI 独立 tab。
6. 支持创建、编辑、启用/禁用、删除、测试。
7. 新增外部池全局配置，包括总开关和各场景 fallback 开关。
8. 新增显式直连外部池策略配置，包括维护模式、模型规则、路径规则。

### 阶段 2：外部池调度器

1. 新增 `FallbackPolicy`，集中判断本地失败是否允许 fallback。
2. 新增 `DirectExternalPolicy`，集中判断显式直连外部池规则。
3. 新增 `LocalPoolPreflight`，集中计算本地状态预检结果。
4. 新增 `ExternalPoolManager`。
5. 实现外部池 in-flight lease。
6. 实现优先级 + 并发占用率调度。
7. 实现单池启用/禁用过滤。
8. 实现外部池 cooldown。
9. 实现当前请求内 `excluded_pool_ids`。
10. 支持外部池最大尝试次数。
11. 实现外部池自动禁用策略和手动解除自动禁用。

### 阶段 3：本地池结构化错误和 fail-fast

1. 新增 `LocalPoolFailureKind`。
2. `KiroProvider` 返回结构化错误。
3. 新增本地 acquire fail-fast 模式。
4. 启用外部池时，本地容量不足不等待。
5. 增加本地凭据失败后当前请求内立即排除策略。
6. 新增 `CredentialRetrySwitchPolicy`，不要把切换规则散落在 provider 分支里。

### 阶段 4：请求透传

1. handler 改为保留 raw body。
2. 本地 Kiro 流程继续使用 parse 后的 `MessagesRequest`。
3. 外部池 fallback 使用 raw body。
4. 实现 header 清洗和认证替换。
5. 实现 stream / non-stream 外部池转发。

### 阶段 5：用量上报投影

1. 抽出 `usage_projection` 模块。
2. 支持 `pass_through`。
3. 支持 `current_path_policy`。
4. 非流式 response usage 重写。
5. 流式 SSE usage 重写。
6. usage 记录标记 `usageProjectionApplied`。

### 阶段 6：usage 记录和观测

1. 扩展 usage record。
2. 记录 route kind。
3. 记录 fallback reason。
4. 记录 local attempts。
5. 记录 external attempts。
6. Usage 详情页展示完整链路。

## 19. 测试计划

### 19.1 本地池优先

- 本地有可用凭据时，不调用外部池。
- 本地 sticky 凭据可用时，走 sticky。
- 本地 sticky 凭据并发满，但其他本地凭据可用时，走其他本地凭据，不走外部池。

### 19.2 本地容量不足

- 所有本地凭据并发满时，立即走外部池。
- 本地全局并发满时，立即走外部池。
- 本地队列满时，立即走外部池。
- 不等待某个本地账号释放。
- 上述情况应记录 `route_subtype=external_fallback_preflight`。
- 上述情况应记录 `local_attempted=false`。
- 上述情况应记录本地预检阻塞原因和状态来源。

### 19.3 本地不可用

- 所有凭据禁用时，走外部池。
- 所有凭据额度耗尽时，走外部池。
- 所有凭据 suspended 时，走外部池。
- 没有支持当前模型的可用凭据时，在开关开启时走外部池。
- local-pool/model/path-level circuit open 时，预检阶段走外部池。
- circuit half-open 时，只允许配置数量的探针请求尝试本地，其他请求仍走预检 fallback。

### 19.4 本地错误不应 fallback

- 本地 400 不走外部池。
- payload too long 不走外部池。
- context window full 不走外部池。
- improperly formed 不走外部池。
- tool schema invalid 不走外部池。
- JSON parse error 不走外部池。
- 状态不确定时不应直接预检 fallback，应尝试本地或进入原子 try-acquire 判断。

### 19.4.1 显式直连外部池

- `externalDirectPolicyEnabled=false` 时，维护模式、模型规则、路径规则都不生效。
- `externalDirectPolicyEnabled=true` 且维护模式开启时，请求直接走外部池。
- 模型直连规则命中时，请求直接走外部池。
- 路径直连规则命中时，请求直接走外部池。
- 显式直连应记录 `route_subtype=external_direct_policy`。
- 显式直连应记录 `local_attempted=false`。
- 显式直连应记录 `direct_policy_reason`。
- 显式直连不应记录 `fallback_reason`。
- 显式直连仍应执行基础请求校验，JSON parse error 和客户端 API key 错误不能进入外部池。

### 19.4.2 本地预检状态

- 预检应读取 PgSQL/配置中的凭据启用状态、模型支持、quota/auth/risk 标记。
- 第一版预检通过真实 fail-fast acquire 读取当前调度状态，不使用长期缓存的“本地是否可用”布尔值。
- 如果后续启用本地池 circuit breaker，预检才需要额外读取 Redis 中的 circuit 状态；当前 circuit 相关字段为保留字段。
- 容量判断应通过原子 try-acquire 或 fail-fast acquire 验证，不能只靠普通读取。
- 多实例下，一个实例设置的 cooldown/circuit 应被其他实例预检读取到。
- sticky 凭据满但其他本地凭据可用时，不应预检 fallback，应选择其他本地凭据。
- 单个凭据错误但其他本地凭据可用时，不应预检 fallback。

### 19.5 本地凭据立即换号

- 429 后当前请求不再打同一凭据。
- 5xx 后当前请求不再打同一凭据。
- 网络错误后当前请求不再打同一凭据。
- 协议错误后当前请求不再打同一凭据。
- 401/403 token refresh 后仍失败时换号。
- 402 quota exhausted 后换号。

### 19.6 外部池调度

- 多个外部池同 priority 时按并发占用率分配。
- 外部池 A 满时选 B。
- 外部池 A 冷却时选 B。
- 外部池 A 429 时当前请求立即选 B。
- 外部池 A 5xx 时当前请求立即选 B。
- 外部池 A 网络错误时当前请求立即选 B。
- 外部池 A 400 时不换 B，直接返回。
- 外部池 A 401/403 在自动禁用策略关闭时只记录错误、冷却或换 B，不自动禁用。
- 外部池 A 401/403 在自动禁用策略开启且错误明确为认证/权限问题时，自动禁用 A 并立即选 B。
- 外部池 A suspended/risk controlled/locked 在策略开启时自动禁用 A 并立即选 B。
- 外部池 A quota exhausted 默认不自动禁用；开启 quota 自动禁用后才禁用。
- 外部池 A 429/5xx/network 默认只冷却和换 B，不自动禁用。
- 外部池 A 400 请求非法时不换 B、不冷却、不自动禁用。
- 外部池全部不可用时返回明确错误。

### 19.7 流式响应

- 外部池 stream pass-through 正常。
- 外部池 stream 在未输出前失败可换池。
- 外部池 stream 在已输出后失败不重放。
- stream usageProjectionMode=pass_through 不改响应。
- stream usageProjectionMode=current_path_policy 只改 usage。

### 19.8 非流式响应

- non-stream pass-through 不改响应。
- non-stream current_path_policy 按路径改 usage。
- 外部池返回 400 不 fallback。
- 外部池返回 429 fallback 到其他池。

### 19.9 Admin UI

- 新增外部池成功。
- 修改 key 成功且列表脱敏。
- 禁用后不参与调度。
- 自动禁用后不参与调度。
- 解除自动禁用后，如果人工 `enabled=true` 且未冷却，应重新参与调度。
- 人工 `enabled=false` 时，解除自动禁用也不能让该池参与调度。
- 删除后不参与调度。
- 测试按钮可用。
- 清冷却可用。
- 解除自动禁用按钮可用。
- 状态 in-flight、cooldown、最近错误展示正确。
- 人工停用、系统自动禁用、冷却中、可调度状态展示互不混淆。

## 20. 风险和约束

### 20.1 严格透传与用量上报投影冲突

`usageProjectionMode=current_path_policy` 会修改响应 usage/cache 上报字段，因此不再是严格响应透传。必须在 UI 和日志中明确标记。

### 20.2 流式响应不能任意重放

一旦已经向客户端输出 SSE event，就不能换外部池重放。实现时必须跟踪是否已经发送下游事件。

### 20.3 多实例并发必须使用 Redis

外部池并发如果只存在内存里，多实例部署会超发。建议第一版就使用 Redis lease。

### 20.4 不能 fallback 请求自身错误

400、payload too long、context window full、tool schema invalid 这类问题必须直接返回，不能 fallback。

### 20.5 本地池 fail-fast 不能影响未启用外部池的部署

新增本地 fail-fast acquire 应只在外部池启用并且 fallback 条件允许时使用。外部池关闭时必须保持当前本地调度行为。

## 21. 推荐最终行为摘要

最终行为应满足：

1. 默认只使用本地 Kiro 凭据池。
2. 启用外部备用号池后，本地池仍然优先。
3. 本地池可调度时，不调用外部池。
4. 本地池明确不可承接时，立即调用外部池，不新增等待时间开关。
5. 外部池有两层直达能力：显式策略直连外部池，以及本地状态预检 fallback 直达外部池。
6. 显式策略直连外部池不是 fallback，必须记录为 `external_direct_policy`，并记录 `direct_policy_reason`。
7. 本地状态预检 fallback 没有真实请求本地上游，但仍然是 fallback，必须记录为 `external_fallback_preflight`，并记录 `fallback_reason` 和 `local_attempted=false`。
8. 本地状态预检必须每次请求计算，或通过 Redis/本地调度器做原子 fail-fast acquire；不能依赖长期缓存的本地可用状态。
9. 状态不确定时默认尝试本地，除非显式直连策略命中或本地池 circuit 已明确打开。
10. 本地凭据遇到 429/5xx/network/protocol/quota/risk/auth 等可切换错误时，当前请求内立即换号。
11. 外部池遇到 429/5xx/network/auth 等池级错误时，当前请求内立即换池。
12. 400 和请求自身错误不触发本地换号，也不触发外部池 fallback。
13. 外部池默认严格透传原始 Anthropic 请求。
14. 外部池默认不修改响应。
15. 只有显式开启“用量上报投影”时，才按当前系统路径策略修改 response usage/cache 字段；请求 body 仍保持透传。
16. 外部池自动禁用默认关闭；开启后只对明确代表外部池自身不可用的错误生效，例如认证失败、安全锁定、可选额度耗尽、可选明显配置错误。
17. 429、5xx、网络错误默认只冷却和换池，不自动禁用。
18. 人工 `enabled=false` 和系统 `autoDisabled=true` 必须分开展示、分开处理。
19. usage 记录必须清楚展示本地尝试链路、fallback 原因、显式直连原因、预检状态来源、外部池尝试链路、最终路由、跳过原因、自动禁用决策和用量上报投影状态。

## 22. 需要后续确认的问题

以下问题在实现前最好确认：

1. 外部号池是否全部保证 Anthropic-compatible？
2. 外部号池是否都支持 `/cc/v1/messages`、`/ha/v1/messages`、`/na/v1/messages`，还是只支持 `/v1/messages`？
3. 外部池自动禁用总开关是否默认关闭？
4. 外部池 401/403 是否在开启自动禁用策略后自动禁用？
5. 外部池 quota exhausted 是否要自动禁用，还是只做冷却？
6. 自动禁用是否永久直到管理员解除，还是按秒数自动恢复？
7. 外部池 key 是否需要支持 Bearer 和 x-api-key 之外的认证方式？
8. usageProjectionMode 是否按单个外部池配置即可，还是还需要全局默认值？
9. unsupported model fallback 是否要默认开启？
10. 外部池是否需要按模型筛选，例如某个池只承接 sonnet？
11. 外部池是否需要按路径筛选，例如只承接 `/cc`？

当前推荐答案：

- 外部池默认 Anthropic-compatible。
- 默认 preserve path。
- 自动禁用总开关默认关闭。
- 开启自动禁用后，明确认证/权限错误和安全锁定可自动禁用。
- quota exhausted 默认不自动禁用，只冷却或换池；用户显式开启后才自动禁用。
- 自动禁用默认永久直到管理员解除，即 `externalPoolAutoDisableDurationSecs=0`。
- usageProjectionMode 按单个外部池配置。
- unsupported model fallback 默认关闭。
- 第一版不做按模型/路径筛选，后续可扩展。
