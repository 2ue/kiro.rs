# Redis + PgSQL 状态模型全面分析

本文档分析当前代码在从本地文件持久化迁移到 Redis + PgSQL 后，哪些实现已经适合新架构，哪些地方还保留了文件时代的策略，以及后续应如何改造成更可靠的数据库/分布式状态模型。

分析范围不是抽样阅读，覆盖了当前仓库与状态、缓存、凭据、usage、admin 和部署相关的主要链路：

- 启动与 bootstrap：`src/main.rs`
- 配置模型：`src/model/config.rs`
- 凭据模型：`src/kiro/model/credentials.rs`
- PgSQL 存储：`src/storage/postgres.rs`
- Redis 存储：`src/storage/redis_cache.rs`
- 凭据调度与运行态：`src/kiro/token_manager.rs`
- 上游调用、重试、错误兜底：`src/kiro/provider.rs`
- Anthropic 路由与 usage 构建：`src/anthropic/router.rs`、`src/anthropic/handlers.rs`、`src/anthropic/cache.rs`、`src/anthropic/prompt_cache.rs`
- usage 记录与价格目录：`src/anthropic/usage.rs`、`src/anthropic/pricing.rs`
- Admin API 与 UI：`src/admin/*`、`admin-ui/src/*`
- 配置、文档与部署：`config.example.json`、`README.md`、`docker-compose.local-infra.yml`、`docker-compose.database.yml`

## 总体结论

当前迁移已经把运行依赖切换到 PgSQL + Redis：

- PgSQL 存放运行配置、凭据、凭据统计、凭据运行态、usage 记录、模型价格。
- Redis 存放余额缓存、会话绑定、临时冷却、rate limit、并发 lease、Token 刷新锁。
- 服务启动时 PgSQL/Redis 现在是必需依赖。
- `config.json` 和 `credentials.json` 只用于首次导入或 CLI 诊断。

但是，很多实现仍然沿用了本地文件时代的状态模型：

- `MultiTokenManager.entries` 仍像完整配置文件一样作为大量写操作的内存权威。
- 凭据、统计、运行态仍存在整份快照保存。
- 新凭据 ID、重复检测、删除语义仍依赖内存扫描。
- 配置和凭据缺少跨实例变更通知。
- Redis 调度状态已经方向正确，但部分操作还不是原子的，并且 Redis 连接被一个全局 mutex 串行化。

所以现在的架构状态可以概括为：

> 存储介质已经迁到 Redis + PgSQL，但状态权威模型还没有完全迁到 Redis + PgSQL。

更合理的长期模型应该是：

| 层级 | 应承担职责 | 当前状态 | 主要问题 |
| --- | --- | --- | --- |
| PgSQL | 长期权威数据：配置、凭据、凭据统计、手动禁用、usage、价格、审计 | 已接入 | 仍有整份快照覆盖、缺少 row-level 操作和版本 |
| Redis | 短期分布式调度状态：并发、冷却、限流、会话、刷新锁、余额缓存 | 已接入 | 部分操作非原子，连接串行，缺少 pub/sub 唤醒 |
| 进程内存 | 当前进程快照和请求内状态 | 仍承担很多权威写入 | 多实例和 DB 写失败时容易漂移 |
| 本地文件 | 首次 bootstrap、CLI 诊断 | 基本退出运行时 | 文档和注释还有少量旧语义 |

## 当前启动链路

`src/main.rs` 当前启动逻辑如下：

1. 从 `config.json` 加载 `file_config`。
2. 应用 `KIRO_RS_POSTGRES_URL` 和 `KIRO_RS_REDIS_URL` 环境变量覆盖。
3. 连接 PgSQL，必要时执行 schema 初始化。
4. 连接 Redis。
5. 如果 PgSQL 没有 `runtime_config`，从 `config.json` bootstrap。
6. 如果 PgSQL 没有凭据，从 `credentials.json` bootstrap。
7. 从 PgSQL 加载运行配置和凭据。
8. 如果存在 `KIRO_API_KEY` 环境变量，把 API Key 凭据插入 `credentials_list`。
9. 构建 `UsageRecorder`、`PromptCacheTracker`、`PricingCatalog`。
10. 从 PgSQL 加载模型价格，然后启动一次后台同步。
11. 构建 `MultiTokenManager`，并传入 PgSQL/Redis store。
12. 构建 provider、Anthropic 路由和 Admin 路由。

这个链路有几个值得注意的点：

- PgSQL 和 Redis 是启动硬依赖，符合用户之前“不需要兼容写文件”的方向。
- 文件只做首次导入，这是合理的。
- `KIRO_API_KEY` 环境变量插入的是无 ID 凭据，随后 `MultiTokenManager::new_with_stores` 会分配 ID 并触发 `persist_credentials()`，这会把环境变量注入的 API Key 持久化进 PgSQL。这个行为需要明确：如果期望环境变量只是临时覆盖，就不应该持久化；如果期望是自动导入，就需要文档写清楚。
- `load_runtime_config()` 从 PgSQL 读出的配置不会再应用环境变量覆盖。连接 PgSQL/Redis 用的是 `file_config`，但运行时配置来自 DB。这个做法基本合理，但如果后续要让端口、proxy、compat 等运行参数都以 DB 为准，就需要明确哪些配置是“启动前配置”，哪些是“运行时热配置”。

## PgSQL 当前职责和表结构

PgSQL schema 目前在 `src/storage/postgres.rs` 的 `SCHEMA_SQL` 中集中定义，启动时通过 `split(";")` 执行。

当前表：

| 表 | 当前用途 | 评价 |
| --- | --- | --- |
| `runtime_config` | 存一行 `id='default'` 的完整 `Config` JSONB | 可用，但缺少 version/updated_by/审计 |
| `credentials` | 存凭据 JSONB，同时冗余 `priority`、`disabled` | 迁移方便，但不适合长期精细更新 |
| `credential_stats` | `success_count`、`last_used_at` | 可用，但当前写入是整表快照覆盖 |
| `credential_runtime_state` | 失败计数、刷新失败计数、禁用原因、预热剩余次数 | 可用，但当前写入是整表快照覆盖 |
| `usage_records` | 请求级 usage、错误、价格估算、完整 JSON | 当前较合理，查询已经 SQL 下推 |
| `model_pricing` | 模型价格目录 | 当前较合理，但同步状态按每个 model 重复存储 |

已经做得比较好的地方：

- PgSQL 集成测试使用独立 schema，避免误清公共表。
- usage 查询已经从“取全量再过滤”改成 SQL 过滤、排序、分页。
- usage summary 和 credential cost summary 已经下推到 SQL 聚合。
- pricing 状态已经可从 PgSQL 恢复。

主要问题：

1. `migrate()` 用字符串 `split(";")` 执行 schema，缺少正式版本迁移。
2. `credentials` 仍以 JSONB 为主，缺少列化字段和唯一约束。
3. `save_credentials()`、`save_credential_stats()`、`save_credential_runtime_state()` 都是整份快照保存。
4. 历史版本的 `save_credentials()` 会把本次没有传入的未软删除 ID 软删除，这是文件覆盖模型，不适合多实例；当前代码已经改成只 upsert 传入行，不再由旧快照推导删除。
5. `usage_records.clear()` 使用 `TRUNCATE`，适合开发，不适合审计型生产。

## Redis 当前职责和 key 模型

Redis 现在用于短期运行态，方向是正确的。

当前 key 大致如下：

| key | 用途 | TTL |
| --- | --- | --- |
| `balance:{id}` | 后台余额查询缓存 | 300 秒 |
| `scheduler:session:{sha256(session_id)}` | 会话绑定凭据、软失败计数 | 6 小时 |
| `scheduler:sessions_by_credential:{credential_id}` | 凭据到 session 的反向索引 | 6 小时 |
| `scheduler:cooldown:{credential_id}` | 429/408/5xx 等瞬态错误冷却 | 冷却时长 |
| `scheduler:rate_limit:{credential_id}` | 单凭据本地 RPM 限速时间戳 | 到可用时间 |
| `scheduler:refresh_lock:{credential_id}` | 跨实例 Token 刷新锁 | 当前 120 秒 |
| `scheduler:inflight:lease_sequence` | 并发 lease 自增 ID | 无 TTL |
| `scheduler:inflight:{id}:last_seen` | 并发 lease 最近活跃时间 | lease TTL |
| `scheduler:inflight:{id}:acquired` | 并发 lease 初始占用时间 | lease TTL |
| `scheduler:inflight:{id}:kind` | lease 类型：api/stream/mcp/test | lease TTL |

已经做得比较好的地方：

- 并发 lease 用 Redis 共享，多实例能看到同一个凭据是否占满。
- 429/408/5xx 瞬态冷却放 Redis，不会因为单实例内存导致多实例继续打同一账号。
- session binding 放 Redis，多实例能保持会话粘性。
- refresh lock 放 Redis，避免多个实例同时刷新同一凭据。
- 并发 lease 有 `credentialInFlightLeaseMaxSecs` 自动回收，能缓解异常未释放。

主要问题：

1. `RedisStore` 用一个 `ConnectionManager` 包 `tokio::sync::Mutex`，所有 Redis 操作被串行化。高并发时 Redis 会成为单通道瓶颈。
2. `set_session_binding()`、`delete_session_binding()`、`record_session_soft_failure()` 都是多步操作，不是 Lua 原子脚本。中间失败可能导致 session key 和 reverse set 不一致。
3. `scheduler_state_for_credentials()` 对每个凭据逐个读 cooldown、rate limit、in-flight。凭据多时，调度前同步 Redis 的成本会比较高。
4. 当前唤醒是进程内 `Notify`。一个实例释放并发 lease 后，另一个实例上的等待请求不会立即被唤醒，只能靠 1 秒轮询恢复。可用，但不是最优。
5. refresh lock 获取失败时部分路径会 fail open，可能在 Redis 故障时出现多实例同时刷新。

## 凭据状态模型

当前 `MultiTokenManager` 的核心状态仍然是：

- `config: Mutex<Config>`
- `entries: Arc<Mutex<Vec<CredentialEntry>>>`
- `current_id: Mutex<u64>`
- `load_balancing_mode: Mutex<String>`
- `session_bindings: Mutex<HashMap<...>>`

其中 `entries` 仍然是大量逻辑的核心状态来源。它包含：

- `credentials`
- `failure_count`
- `refresh_failure_count`
- `disabled`
- `disabled_reason`
- `success_count`
- `last_used_at`
- `cooldown_until`
- `rate_limit_available_at`
- `in_flight_requests`
- `in_flight_leases`
- `warmup_remaining`

这些字段现在应该拆成三类：

| 字段 | 更合理的权威位置 | 当前位置 | 风险 |
| --- | --- | --- | --- |
| 凭据密钥、token、email、endpoint、priority、manual disabled | PgSQL | 内存 + PgSQL JSONB 快照 | DB 写失败或多实例覆盖 |
| success_count、last_used_at | PgSQL 原子计数 | 内存 + PgSQL 快照 | 多实例统计丢增量 |
| failure_count、refresh_failure_count、disabled_reason、warmup_remaining | PgSQL 或 Redis+PgSQL 分工 | 内存 + PgSQL 快照 | 多实例覆盖、语义混杂 |
| cooldown、rate limit、in-flight、session binding | Redis | Redis + 内存镜像 | 镜像只是展示/本地判断，需同步 |
| current_id | 进程内派生值 | 进程内 | 多实例各自选择不同 current_id，priority 模式可接受但要明确 |

### 凭据保存仍是文件时代的整份覆盖

`persist_credentials()` 会从当前 `entries` 生成完整凭据列表，然后调用 `PostgresStore::save_credentials()`。

历史版本的 `save_credentials()` 做三件事：

1. 查询当前未软删除凭据 ID。
2. 对传入凭据逐个 upsert。
3. 对 DB 里存在但传入列表没有的 ID 标记 `deleted_at=now()`。

这个逻辑和“把整个 credentials.json 覆盖写回磁盘”非常像。迁移到 PgSQL 后不应该继续这样做。

当前代码已经修正为非破坏性保存：`PostgresStore::save_credentials()` 只 upsert 传入凭据，不会删除 PgSQL 中其他未软删除凭据。明确删除只能由删除接口触发软删除。

主要风险：

- 实例 A 内存是旧列表，实例 B 新增了凭据，实例 A 后续一次 `persist_credentials()` 可能把 B 新增凭据软删除。
- 某个操作只是修改一个字段，却保存全量 JSONB，扩大了写入范围。
- 删除语义由“本次快照缺失”推导，而不是明确的 delete action。
- PgSQL 已经具备 row-level transaction，但当前没有利用。

建议改成：

- `insert_credential(...) -> id`
- `update_credential_token(id, token_fields)`
- `set_credential_disabled(id, disabled, reason)`
- `set_credential_priority(id, priority)`
- `set_credential_warmup(id, warmup_remaining)`
- `soft_delete_credential(id)`
- `update_subscription_title(id, title)`

Admin 操作应该变成：

1. 校验请求。
2. PgSQL transaction 写单行。
3. commit 成功。
4. 更新当前进程内存快照。
5. 发布 Redis `credentials_changed` 事件。
6. 其他实例 reload/reconcile。

### 新凭据 ID 仍由内存 max + 1 分配

`add_credential()` 当前用 `entries.iter().map(|e| e.id).max().unwrap_or(0) + 1` 分配 ID。

这个在单实例可以，在多实例下不可靠：

- 两个实例同时新增，会拿到同一个 ID。
- DB 里已有被当前实例没加载到的新 ID，也可能冲突。

建议：

- `credentials.id BIGSERIAL PRIMARY KEY`
- 新增凭据由 PgSQL 返回 ID。
- 如果必须兼容已有 ID，可以保留 `BIGINT`，新增 sequence，再把 sequence set 到当前 max(id)+1。

### 重复凭据检测仍是内存扫描

新增凭据时，当前用内存扫描 `refresh_token` 或 `kiro_api_key` 的 SHA-256 判断重复。

风险：

- 多实例并发新增绕过内存检查。
- 当前前端也依赖 snapshot 返回 hash 去重，但这只是 UI 层辅助。

建议：

- `credentials` 增加 `refresh_token_hash`、`api_key_hash`。
- 增加部分唯一索引；索引名里的 `active` 仅表示 `deleted_at IS NULL`，不表示 `disabled = false`：

```sql
CREATE UNIQUE INDEX uniq_credentials_refresh_token_hash_active
ON credentials(refresh_token_hash)
WHERE deleted_at IS NULL AND refresh_token_hash IS NOT NULL;

CREATE UNIQUE INDEX uniq_credentials_api_key_hash_active
ON credentials(api_key_hash)
WHERE deleted_at IS NULL AND api_key_hash IS NOT NULL;
```

### 凭据 JSONB 需要列化

现在 `credentials.data` 保存完整 `KiroCredentials`，只冗余了 `priority` 和 `disabled`。

这适合快速迁移，但长期问题明显：

- 不能靠 DB 约束校验 auth method、endpoint、hash 唯一。
- 局部更新困难，只能整体 JSONB 覆盖。
- 查询和导出都依赖反序列化整段 JSON。
- 敏感字段集中明文放在 JSONB 中，后续加密不方便。

建议逐步列化：

- `email`
- `auth_method`
- `priority`
- `disabled`
- `disabled_reason`
- `endpoint`
- `subscription_title`
- `region/auth_region/api_region`
- `machine_id`
- `refresh_token_hash`
- `api_key_hash`
- `access_token_expires_at`
- `created_at/updated_at/deleted_at`

敏感字段如 `access_token`、`refresh_token`、`kiro_api_key`、`client_secret`、代理密码，建议独立字段加密存储，或者放到单独 secret 表。

## 调度策略与 Redis 化后的改进空间

当前调度主路径在 `acquire_context_for_session()` 中：

1. 同步 Redis 调度状态。
2. 清理过期 in-flight lease。
3. 尝试会话绑定。
4. priority 模式尝试当前凭据。
5. balanced 模式按 success_count 和 warmup 选择。
6. 选中后获取并发 lease。
7. 确保 token 可用。
8. 成功后绑定会话并返回。

这套逻辑已经比文件时代更合理，特别是：

- 并发满会排队，而不是直接报错。
- 只有可用凭据都临时不可调度时才等待。
- 402 额度用尽会禁用并 fallback。
- 429/408/5xx 走瞬态冷却，不直接禁用。
- 单可用凭据遇到瞬态错误时不会把唯一凭据冷却掉。
- session soft failure 达阈值后只在有备选凭据时排除当前凭据。

但迁到 Redis 后还可以更进一步：

### 调度前全量同步 Redis 成本较高

`refresh_scheduler_state_from_redis()` 会对所有凭据拉取 Redis 状态，再写进内存。

凭据少时没问题，凭据多时每次调度都会有额外 Redis 压力。

改进方向：

- 对候选凭据按需拉取，而不是每次全量拉取。
- 用 Lua 或 pipeline 批量读取某批凭据的 cooldown/rate/in-flight。
- 本地缓存 Redis 调度状态极短时间，比如 100-300ms，降低高并发抖动。
- 使用 Redis pub/sub 在 release/cooldown clear 时唤醒其他实例，而不是 1 秒轮询。

### balanced 模式的 success_count 应改成 DB 原子计数

balanced 当前用 `success_count` 选择较少成功次数的凭据。这个字段现在来自内存，并定期整表保存到 PgSQL。

多实例下问题：

- 每个实例看到的 success_count 可能不同。
- 两个实例都认为同一个凭据 success_count 最少，容易集中打同一个账号。
- 快照保存可能覆盖别的实例增量。

更好的做法：

- 请求成功时 PgSQL 原子增量：

```sql
INSERT INTO credential_stats (credential_id, success_count, last_used_at)
VALUES ($1, 1, now())
ON CONFLICT (credential_id)
DO UPDATE SET
  success_count = credential_stats.success_count + 1,
  last_used_at = EXCLUDED.last_used_at,
  updated_at = now();
```

- balanced 选择时可用 Redis 做轻量 recent score，PgSQL 做长期统计。
- 如果需要跨实例实时均衡，可以用 Redis sorted set 保存最近调度分数，而不是依赖内存。

### 预热模式当前是合理方向，但状态更新方式仍需原子化

当前预热不是后台主动打请求，而是在真实业务流量中低概率参与调度。成功后扣 `warmup_remaining`，不伪造 `success_count`。这个方向是合理的。

但 `warmup_remaining` 当前属于内存字段，保存到 `credential_runtime_state` 时仍是整表快照。

建议：

- 预热剩余次数放 PgSQL 单行字段。
- 成功扣减用原子 SQL：

```sql
UPDATE credential_runtime_state
SET warmup_remaining = GREATEST(warmup_remaining - 1, 0),
    updated_at = now()
WHERE credential_id = $1;
```

- Admin 手动设置预热时只更新这一行。

## Token 刷新与跨实例一致性

当前 Token 刷新有两个层次：

- 本进程 `refresh_lock: TokioMutex<()>`
- Redis `scheduler:refresh_lock:{credential_id}`

优点：

- Redis 锁能防止多个实例同时刷新同一凭据。
- 如果发现其他实例在刷新，会轮询 PgSQL，等待新 token 写入。

问题：

1. 本进程 `refresh_lock` 是全局锁，不是按凭据锁。一个凭据刷新会阻塞同进程其他凭据刷新。
2. `reload_credentials_from_postgres()` 只更新本地已有 ID，不完整 reconcile 新增/删除。
3. 强制刷新时 Redis lock 获取失败会使用本进程锁继续刷新，Redis 故障时可能多实例并发刷新。
4. Token 刷新后仍通过 `persist_credentials()` 保存全量凭据快照。

建议：

- 本进程锁改为按 credential_id 的锁。
- Token 更新走 `update_credential_token(id, ...)` 单行 SQL。
- Redis lock 获取失败时按场景选择 fail closed。对自动刷新建议 fail closed，避免多实例重复刷新；对 Admin 手动刷新可以明确提示 Redis 锁不可用。
- `reload_credentials_from_postgres()` 改为完整 reconcile。

## 配置热更新与跨实例一致性

当前 `runtime_config` 已入 PgSQL，Admin 修改配置会写 PgSQL 并更新当前进程内存。

问题：

- 其他实例不会立即看到配置变化。
- `runtime_config` 缺少 version。
- 没有配置变更事件。
- `load_balancing_mode` 同时存在 `config.load_balancing_mode` 和 `load_balancing_mode: Mutex<String>` 两份状态，虽然当前有同步调用，但模型容易漂移。

建议：

1. `runtime_config` 表增加：

```sql
version BIGINT NOT NULL DEFAULT 1,
updated_by TEXT,
updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
```

2. Admin 更新配置时：

```sql
UPDATE runtime_config
SET config = $1,
    version = version + 1,
    updated_at = now()
WHERE id = 'default'
RETURNING version;
```

3. Redis publish：

```text
channel: kiro_rs:config_changed
payload: {"version":123}
```

4. 所有实例订阅后 reload config。
5. 同时保留定时 version check，防止 pub/sub 丢消息。

## usage 记录和统计

当前 usage 迁移是相对成功的部分：

- `UsageRecord` 字段完整。
- PgSQL 表列化了常用查询字段，同时保留 `data JSONB`。
- `query_page` 只返回 `has_next`，不需要 count，符合之前的分页要求。
- summary 和 credential cost summary 已经 SQL 聚合。

仍需注意的问题：

### 写入是 fire-and-forget

`UsageRecorder.record()` 会先写内存 ring buffer，再 `tokio::spawn` 写 PgSQL。

如果 usage 只是后台观察，这可以接受。如果 usage 用于计费、结算或严格审计，就不够可靠：

- 进程退出时异步任务可能丢。
- PgSQL 短暂失败只记录 warn，不重试。
- 内存 ring buffer 不是可靠队列。

建议：

- 使用 bounded channel + 单后台 writer。
- 写入失败做有限重试。
- shutdown 时 flush。
- 保留内存最近记录只是 UI 快速兜底，不作为可靠数据源。

### `ON CONFLICT` 只更新 data，不更新列化字段

`usage_records` 插入冲突时当前只更新 `data` 和 `updated_at`，不更新其他列化字段。正常 request_id 唯一时问题不大。但如果重放同一 ID 或补写记录，查询列和 JSONB 可能不一致。

建议：

- 要么 `id` 保证绝对不可重复，冲突直接忽略。
- 要么冲突时同步更新所有列化字段。

### 清空 usage 是 TRUNCATE

当前 Admin 清空 usage 会 `TRUNCATE TABLE usage_records`。

建议生产策略：

- 保留“清空全部”但加二次确认和审计。
- 增加按时间范围删除。
- 增加保留最近 N 天。
- 如果成本统计重要，默认不允许直接清空全部。

## 模型价格

当前模型价格逻辑：

- 启动时从 PgSQL 加载历史价格。
- 启动后异步同步 LiteLLM 价格。
- Admin 可手动同步。
- 同步失败不影响调度。
- usage record 记录 `estimated_cost_usd`、`pricing_available`、`pricing_model`。

这个方向合理。

可改进点：

1. `model_pricing` 同时保存每个模型价格和同步元信息；`source/source_url/last_synced_at/last_error` 在每行重复。建议拆成：
   - `model_pricing`
   - `model_pricing_sync_status`
2. 价格只做统计，不影响调度，这是对的。
3. 需要记录价格版本或同步时间到 usage record，避免未来价格变化后历史成本解释不清。当前 `pricing_model` 还不够。

## prompt cache 和 high-cache 模拟

当前 prompt cache tracker 仍是进程内：

- `PromptCacheTracker.entries: Mutex<HashMap<PromptCacheScope, HashMap<fingerprint, entry>>>`
- scope 包含 credential_id、conversation_id、model。
- 只影响本地 usage 模拟，不影响上游真实请求。

这个放内存是可以接受的，原因：

- 它不是安全/调度/凭据权威状态。
- 它只是模拟 Anthropic prompt cache 行为，用于下游 usage 上报。
- 跨实例共享会让模拟更一致，但不是必须。

但如果多实例背后挂同一个负载均衡，下游同一会话可能打到不同实例，那么本地 prompt cache tracker 会不连续，导致 usage 模拟抖动。

可选改进：

- 如果要求多实例 usage 模拟一致，可以把 prompt cache fingerprint 放 Redis，TTL 使用 5m/1h。
- 如果只是本地单实例或不要求严格一致，保持内存更简单。

注意：当前 `compute()` 命中后会刷新 `expires_at`，这和之前 foxfishc 分析里提到的官方 TTL 语义不完全一致。但用户之前明确 P0-1 reader 不动，因此这里只建议记录，不建议本轮改。

## Admin API 和 UI

Admin 现在主要读取 `token_manager.snapshot()`，而不是直接查 PgSQL。

这在单实例下简单有效，但多实例下有问题：

- A 实例 admin 改了凭据，B 实例 UI 看到的是 B 内存快照。
- 其他实例不会自动 reconcile。
- 凭据列表分页是内存分页，且仍返回 total/total_pages。
- usage 分页已经是 PgSQL SQL 分页，并且不依赖 total，这是较好的部分。

建议：

- 凭据列表可以继续从本地 snapshot 展示“当前实例调度视角”，但要明确这是当前实例状态。
- 如果后台需要全局权威视图，应从 PgSQL 查询凭据基础信息，再合并 Redis 调度状态和 PgSQL 统计。
- Admin 写操作改 DB-first 后，UI 返回的数据应来自 DB commit 后的新 snapshot。
- 增加 admin audit log，记录谁在什么时候禁用、删除、修改配置、导出凭据。

## 安全问题

迁移到 PgSQL 后，安全边界发生变化：

- `credentials.data` 内含 refresh token、access token、api key、client secret、代理密码。
- `export_credentials` 能导出完整敏感凭据。
- PgSQL 数据库成为核心敏感资产。

建议：

1. 敏感字段字段级加密。
2. 加密 key 从环境变量或 secret manager 注入，不入库。
3. hash 字段用于去重和展示，不暴露明文。
4. 导出凭据操作写 audit log。
5. 导出接口可选择隐藏 access token，只导出 refresh token/API key，或提供不同导出等级。
6. 避免日志打印完整上游错误中可能包含 token 或账号隐私信息。目前错误里会带 credential label，但要继续确保不会带 token。

## 多实例一致性风险清单

当前如果只跑单实例，风险相对可控。如果跑多实例，以下问题需要优先处理：

| 场景 | 当前风险 | 建议 |
| --- | --- | --- |
| 两个实例同时新增凭据 | 内存 max+1 可能重复 ID | DB sequence |
| 两个实例同时新增同一 token | 内存去重会竞态 | DB hash unique index |
| 实例 A 保存旧凭据快照 | 可能软删除实例 B 新增凭据 | row-level update，禁止全量覆盖 |
| 多实例成功计数 | success_count 快照覆盖丢增量 | SQL 原子 increment |
| Admin 改配置 | 其他实例不热更新 | runtime_config version + Redis pub/sub |
| Admin 改凭据 | 其他实例不 reconcile | credentials version + Redis pub/sub |
| Redis release 失败 | 其他实例看到 lease 仍占用直到 TTL | release 重试/短 TTL/后台清理 |
| 一实例释放并发槽 | 其他实例等待请求不立即醒 | Redis pub/sub 唤醒 |
| prompt cache 模拟 | 同会话打到不同实例可能 usage 不连续 | 可选 Redis 化 tracker |

## 建议的新状态模型

### PgSQL

PgSQL 应该成为这些数据的唯一权威：

- 运行配置
- 凭据基础信息
- 凭据敏感字段
- 凭据手动禁用/永久禁用状态
- 凭据失败/刷新失败持久状态
- 凭据预热剩余次数
- 凭据长期统计
- usage 记录
- 模型价格
- admin 操作审计

### Redis

Redis 应该只存短期、可 TTL 恢复的分布式协调状态：

- 当前并发 lease
- 临时冷却
- 本地 RPM rate limit
- 会话绑定
- 会话软失败计数
- Token refresh lock
- 余额缓存
- 配置/凭据变更通知
- 可选：短期 recent score
- 可选：prompt cache 模拟 fingerprint

### 进程内存

进程内存只保存：

- 当前进程的凭据快照
- 当前进程的配置快照
- Redis 调度状态的短期镜像
- Prompt cache 模拟状态
- 最近 usage ring buffer

内存不能再承担写入权威。

## 推荐改造优先级

### P0：必须优先修

1. 凭据新增 ID 改为 PgSQL sequence/identity。
2. 凭据重复检测改为 DB hash 唯一索引。
3. Admin 凭据操作改 DB-first row-level transaction。
4. `persist_credentials()` 不再作为日常保存入口，只保留 bootstrap 或 migration 用途，后续删除。
5. success_count、last_used_at 改成 PgSQL 原子增量。
6. warmup_remaining 改成单行原子更新。
7. `reload_credentials_from_postgres()` 改为完整 reconcile。

### P1：多实例能力

1. `runtime_config` 增加 version，配置更新后 Redis pub/sub 通知所有实例 reload。
2. 凭据表增加 version/updated_at watermark，凭据变化后 pub/sub 通知所有实例 reconcile。
3. Redis session binding 和 soft failure 改 Lua 原子脚本。
4. Redis 连接去掉全局 mutex 串行瓶颈。
5. 释放并发 lease 后发布 Redis wakeup，其他实例等待请求可以立即醒。
6. usage writer 改 bounded channel + retry + shutdown flush。

### P2：生产治理

1. 正式迁移系统：`sqlx::migrate!` 或独立 migration 工具。
2. 凭据敏感字段加密。
3. admin audit log。
4. usage retention/export/soft delete 策略。
5. 模型价格拆分价格表和同步状态表。
6. 可选 Redis 化 prompt cache tracker。

## 建议的表结构演进方向

### credentials

建议从 JSONB 主表演进为结构化主表：

```sql
CREATE TABLE credentials (
  id BIGSERIAL PRIMARY KEY,
  auth_method TEXT NOT NULL,
  email TEXT,
  priority INTEGER NOT NULL DEFAULT 0,
  disabled BOOLEAN NOT NULL DEFAULT false,
  disabled_reason TEXT,
  endpoint TEXT,
  subscription_title TEXT,
  region TEXT,
  auth_region TEXT,
  api_region TEXT,
  machine_id TEXT,
  refresh_token_hash TEXT,
  api_key_hash TEXT,
  data JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  deleted_at TIMESTAMPTZ
);
```

敏感字段可以拆到 `credential_secrets`：

```sql
CREATE TABLE credential_secrets (
  credential_id BIGINT PRIMARY KEY REFERENCES credentials(id) ON DELETE CASCADE,
  access_token_enc BYTEA,
  refresh_token_enc BYTEA,
  kiro_api_key_enc BYTEA,
  client_secret_enc BYTEA,
  proxy_password_enc BYTEA,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

### credential_stats

建议用于长期统计：

```sql
CREATE TABLE credential_stats (
  credential_id BIGINT PRIMARY KEY REFERENCES credentials(id) ON DELETE CASCADE,
  success_count BIGINT NOT NULL DEFAULT 0,
  failure_count BIGINT NOT NULL DEFAULT 0,
  refresh_failure_count BIGINT NOT NULL DEFAULT 0,
  last_used_at TIMESTAMPTZ,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

如果 failure_count 是调度短期状态，也可以拆到 Redis；如果后台要重启后保留，则 PgSQL 保留。

### runtime_config

```sql
CREATE TABLE runtime_config (
  id TEXT PRIMARY KEY,
  config JSONB NOT NULL,
  version BIGINT NOT NULL DEFAULT 1,
  updated_by TEXT,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

### admin_audit_logs

```sql
CREATE TABLE admin_audit_logs (
  id BIGSERIAL PRIMARY KEY,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  actor TEXT,
  action TEXT NOT NULL,
  target_type TEXT,
  target_id TEXT,
  detail JSONB NOT NULL DEFAULT '{}'::jsonb
);
```

## 代码层改造建议

### 新增 repository 层

当前 `PostgresStore` 同时承担 schema、bootstrap、credentials、stats、usage、pricing。建议拆成逻辑上的 repository，即使物理文件暂时不拆，也应该先拆方法职责：

- `ConfigRepository`
- `CredentialRepository`
- `CredentialStatsRepository`
- `UsageRepository`
- `PricingRepository`
- `AuditRepository`

### Admin 写操作推荐模式

旧模式：

```text
改内存 -> persist 全量 -> 尝试清 Redis -> 返回
```

新模式：

```text
validate -> PgSQL transaction -> commit -> update local snapshot -> clear Redis state -> publish change -> return
```

如果 DB 失败，内存不能变。

### 调度主路径推荐模式

请求调度不应该依赖 PgSQL 热路径：

- 凭据基础 snapshot 来自内存。
- 临时调度状态来自 Redis。
- 请求成功/失败的长期统计异步或批量写 PgSQL，但关键禁用状态必须可靠写入。
- 并发 lease 和 cooldown 必须 Redis 成功，否则应 fail closed，避免绕过全局限制。

### 变更通知

建议 Redis channels：

```text
kiro_rs:config_changed
kiro_rs:credentials_changed
kiro_rs:dispatch_wakeup
```

payload：

```json
{"version":123,"changedAt":"2026-05-24T00:00:00Z"}
```

每个实例：

- 订阅 channel。
- 收到 config 变更 reload。
- 收到 credentials 变更 reconcile。
- 收到 dispatch wakeup 后 notify 本地等待队列。
- 定时检查 version 作为兜底。

## 测试缺口

当前已有不少测试：

- PgSQL 集成测试：runtime_config、credentials、stats、usage、pricing。
- Redis 集成测试：JSON、session binding、cooldown、rate limit、in-flight、refresh lock。
- TokenManager 测试：并发排队、禁用、冷却、rate limit、预热、Redis 多 manager 共享。
- usage 和 cache 测试：分页、summary、cache 上报策略。

还缺的测试：

1. 多实例同时新增凭据，验证 DB sequence 和唯一索引。
2. 多实例同时更新同一凭据，验证不会快照覆盖。
3. Admin 修改配置后，另一个 manager 热 reload。
4. Admin 删除凭据后，另一个 manager reconcile 并清 Redis 状态。
5. Redis session binding 脚本在中途失败时不产生反向索引残留。
6. Redis release 失败后，lease TTL/cleanup 能恢复调度。
7. usage writer 在 PgSQL 短暂失败后重试。
8. migration 从当前 JSONB schema 到列化 schema 的数据迁移。
9. 敏感字段加密后，导出/API 展示不泄露密文或明文。

## 迁移实施建议

建议不要一次性把所有逻辑重写，按下面顺序做：

1. 增加正式 migration 框架，先承接当前 schema。
2. 给 `credentials` 增加 sequence 和 hash 列，写入时填充。
3. 改 `add_credential()` 为 DB insert 返回 ID。
4. 改重复检测为 DB unique constraint。
5. 改 `set_disabled`、`set_priority`、`delete_credential`、`set_warmup` 为 DB-first。
6. 改 `report_success` 为 stats 原子增量。
7. 改 token refresh 为单行 token update。
8. 改 `reload_credentials_from_postgres()` 为完整 reconcile。
9. 增加 config/credentials version + Redis pub/sub。
10. 优化 Redis 原子脚本和连接并发。
11. 最后再做敏感字段加密和 audit log。

## 最终建议

当前 Redis + PgSQL 迁移已经解决了“文件不适合长期运行态”的大方向问题，但还没有完全解决“谁是状态权威”的问题。

最关键的改造不是继续增加表，而是改变写入策略：

- 凭据不能再全量快照覆盖。
- 统计不能再内存累加后整表保存。
- 新增/删除/禁用必须是 DB transaction。
- 多实例一致性必须靠 version + Redis pub/sub。
- Redis 中的调度状态需要继续保持 TTL、原子性和失败闭环。

如果只跑单实例，当前版本可以继续使用；如果要把这个系统作为长期后台服务或多实例部署，P0 项应该尽快完成，否则后续遇到的很多“账号状态不一致、凭据莫名消失、统计跳变、某实例配置没生效”的问题，本质都会来自当前的文件时代状态模型残留。
