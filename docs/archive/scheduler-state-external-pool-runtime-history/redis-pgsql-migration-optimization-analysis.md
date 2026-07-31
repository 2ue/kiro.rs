# PgSQL + Redis 迁移后的旧实现优化空间分析

本文补充 [`redis-pgsql-state-model-full-analysis.md`](redis-pgsql-state-model-full-analysis.md)。

前一份文档重点回答：当前哪些状态在进程内存、PgSQL、Redis、本地文件中分别归属，哪里存在一致性风险。

本文重点回答：以前很多实现是按“本地文件 + 单进程内存”方式设计的，迁移到 PgSQL + Redis 后，哪些实现方式可以变得更合理、更可靠、更容易多实例部署。

## 总结

当前迁移已经把主要持久化能力搬到了 PgSQL，把调度临时态搬到了 Redis，方向是对的。但不少代码仍然沿用本地文件时代的实现习惯：

- PgSQL 被当成“整份配置文件”的替代品，而不是业务状态数据库。
- `MultiTokenManager.entries` 仍然是凭据事实源，PgSQL 更像同步副本。
- 凭据保存、统计保存、运行态保存仍有整份快照覆盖语义。
- 多实例下，任意实例持有的旧内存快照都可能覆盖或软删除其他实例刚写入的数据。
- Redis 已承担并发、冷却、会话绑定，但部分操作还不是原子脚本，调度前同步 Redis 的成本也偏高。
- usage、pricing、runtime config 已入库，但还可以利用数据库索引、版本、审计、批量写、聚合表进一步优化。

所以答案是：有明显优化空间。迁移不能只做到“文件换成数据库”，应该进一步调整为：

- PgSQL：持久事实源、审计、查询、统计、价格、配置、凭据生命周期。
- Redis：短期调度态、锁、lease、冷却、速率限制、会话粘性、跨实例通知。
- 进程内存：只做热点快照和本实例执行态，不再作为最终事实源。
- 本地文件：只做首次 bootstrap 和部署启动参数，不参与运行时写入。

## 2026-05-24 落地状态

本轮已经把本文中需要优先修改的 PgSQL + Redis 状态模型问题落到代码中：

- 凭据日常写操作改为行级写入：新增、禁用、优先级、删除、Token 刷新、订阅等级更新都不再通过“旧内存快照覆盖整份凭据列表”来表达删除。
- 新增凭据 ID 由 PgSQL sequence 分配，且 sequence 只在首次 bootstrap 显式导入旧 ID 后同步，避免并发新增时被 `setval(max(id)+1)` 倒回。
- 凭据重复检测由 PgSQL 未软删除 hash 唯一索引兜底，API Key 和 refreshToken 都有未软删除唯一约束。这里不按 `disabled` 过滤，禁用凭据也应该阻止重复导入。
- `success_count`、`last_used_at`、API 失败计数、刷新失败计数、额度禁用、refreshToken 失效禁用都走 PgSQL 单行事务或原子增量。
- `reload_credentials_from_postgres()` 已改成完整 reconcile，能处理其他实例新增、删除、更新凭据。
- runtime config 有 version，后台更新后发布 Redis 事件；主进程监听 pub/sub，并有 60 秒周期兜底 reload。
- Redis session binding、soft failure 改为 Lua 原子脚本；调度态读取改成 pipeline；释放并发 lease 后发布 Redis wakeup。
- usage 写入改成有界异步队列和重试；清空 Usage 改成 PgSQL soft delete，不再 TRUNCATE 生产记录。
- 模型价格同步状态拆到 `model_pricing_sync_status`；价格同步失败只影响统计状态，不影响调度。
- 后台关键写操作写入 `admin_audit_logs`，管理页增加“审计”页签分页查看完整 detail。

本文后面保留的是分析过程和长期演进建议。其中 P2 项仍不建议在当前轮次强行做：凭据敏感字段加密、pricing 历史快照、usage 物化聚合、PromptCacheTracker Redis 化、Redis 原子“选择凭据 + acquire lease”、完整调度诊断页。这些需要额外产品/部署约束，不应为了“数据库化”破坏当前特殊业务逻辑。

## 分析范围

本次分析覆盖以下链路：

- 启动和 bootstrap：`src/main.rs`、`src/model/config.rs`
- PgSQL 存储层：`src/storage/postgres.rs`
- Redis 存储层：`src/storage/redis_cache.rs`
- 凭据调度与持久化：`src/kiro/token_manager.rs`
- 上游调用与失败兜底：`src/kiro/provider.rs`
- usage 记录和统计：`src/anthropic/usage.rs`
- 模型价格：`src/anthropic/pricing.rs`
- cache usage 和路径上报策略：`src/anthropic/cache.rs`、`src/anthropic/handlers.rs`、`src/model/config.rs`
- Admin API：`src/admin/service.rs`、`src/admin/types.rs`
- Admin UI：`admin-ui/src/*`
- 配置、部署、文档：`config.example.json`、`README.md`、`docs/*`、`docker-compose*.yml`

本文不建议破坏现有特殊业务逻辑，例如：

- `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` 的路径级缓存上报语义。
- reader 计算和 writer 下游上报解耦。
- 上游真实 metadata 优先，本地 prompt cache 只在需要模拟时补充。
- 调度失败兜底、402/429/401/403 分类和会话粘性失败处理。
- 凭据预热不是凭空增加成功次数，而是降低调度选择概率。

## 迁移后的核心原则

### 1. 不要把 PgSQL 当成 JSON 文件

本地文件时代最常见的模式是：

1. 读取完整 JSON。
2. 修改内存对象。
3. 写回完整 JSON。

现在 PgSQL 已经存在，如果继续把完整对象写到单行 JSONB 或全量 credential snapshot，就会保留旧问题：

- 并发实例互相覆盖。
- 没有局部更新语义。
- 很难做审计。
- 很难做唯一约束。
- 很难做条件更新和乐观锁。
- 很难判断某个字段是谁、什么时候、为什么改的。

合理方向是：把关键业务字段拆成可查询列，用事务表达业务动作。

### 2. Redis 只保存短期调度态

Redis 适合保存：

- 并发 lease。
- 速率限制窗口。
- 临时冷却。
- 会话绑定。
- 软失败计数。
- 跨实例锁。
- 配置变更通知。
- 调度唤醒信号。

Redis 不适合保存：

- refresh token、api key、access token 等长期凭据事实源。
- usage 长期记录。
- 模型价格长期记录。
- 管理员审计记录。

Redis 可以开 AOF，但业务仍不应该依赖 Redis 做持久事实源。

### 3. 进程内存只做快照和执行态

当前 `MultiTokenManager.entries` 仍然是很多操作的事实源。迁移后更合理的模型是：

- 启动时从 PgSQL 加载一份凭据快照。
- 调度 hot path 可以继续用内存快照，避免每次请求查 PgSQL。
- Admin 写操作必须先落 PgSQL，再更新本进程快照，并通过 Redis pub/sub 通知其他实例 reload。
- 如果本进程快照落后，版本号可以检测并增量刷新。

这样既保留性能，也避免本地内存覆盖数据库事实。

## 启动与 bootstrap 优化

### 当前状态

当前启动流程已经改为 PgSQL/Redis 必需，并支持首次从 `config.json`、`credentials.json` 导入：

- `Config::from_file()` 读取本地配置。
- 使用文件配置中的 PgSQL/Redis 地址连接数据库。
- 如果 PgSQL 没有运行配置，从文件配置导入。
- 如果 PgSQL 没有凭据，从文件凭据导入。
- 之后运行时配置主要来自 PgSQL。

这个方向合理。

### 仍有旧实现痕迹

本地文件现在仍承担两个角色：

- 启动前连接信息：PgSQL URL、Redis URL、监听端口等。
- 首次导入数据：运行配置、凭据。

这两个角色应该明确分离，否则后续维护会混乱：

- 启动前配置不能热加载，因为服务没它连不上数据库。
- 运行时配置应该从 PgSQL 加载，可以由后台热修改。
- 首次导入完成后，本地文件不应该再被认为是事实源。

### 优化建议

建议把配置分成两类：

| 类型 | 示例 | 来源 | 是否热加载 | 是否入库 |
|---|---|---|---|---|
| 启动配置 | PgSQL URL、Redis URL、bind host、port、日志级别 | env 或启动文件 | 否 | 否 |
| 运行配置 | 调度、缓存上报、压缩、兼容 profile、冷却、并发 | PgSQL | 是 | 是 |
| 首次导入数据 | 初始凭据、初始 runtime config | 文件 | 只导入一次 | 导入后以 PgSQL 为准 |

建议增加 bootstrap 记录：

- `bootstrap_imports` 表记录是否从文件导入过运行配置和凭据。
- 记录导入时间、来源文件 hash、导入凭据数量。
- 避免误删库后重复导入旧凭据时没有任何迹象。

## 凭据管理优化

### 当前状态

当前 `PostgresStore::save_credentials()` 已经改成非破坏性保存：

- 遍历传入凭据 upsert。
- 不查询数据库中其他未软删除凭据并推导删除。
- 明确删除只能走 delete action 的软删除路径。

`MultiTokenManager::persist_credentials()` 会从当前 `entries` 生成完整凭据列表，再调用 `save_credentials()`。

这保留了文件覆盖语义。

### 主要问题

#### 旧快照软删除新数据

多实例场景下：

1. 实例 A 启动，内存里有凭据 1、2。
2. 实例 B 添加凭据 3，并写入 PgSQL。
3. 实例 A 后续刷新 token 或修改任意凭据，调用 `persist_credentials()`。
4. A 传入的完整列表没有凭据 3。
5. `save_credentials()` 认为凭据 3 不在 incoming list，于是软删除。

这就是本地文件覆盖模型迁移到数据库后的典型风险。

#### ID 分配仍依赖内存 max + 1

`add_credential()` 里新 ID 是当前内存最大 ID + 1。

多实例同时新增时可能发生：

- 两个实例看到相同最大 ID。
- 两个实例分配相同新 ID。
- 后提交的覆盖先提交的，或触发冲突。

数据库应该负责分配 ID。

#### 重复检测仍靠内存扫描

当前重复检测是对内存凭据做 token hash 比较。

问题：

- 多实例时检测不到其他实例刚加的凭据。
- 软删除凭据是否允许重新导入缺少明确约束。
- 无法在数据库层防止并发重复导入。

### 优化建议

#### P0：凭据写操作改为行级事务

把以下操作改成专用 repository 方法：

- `insert_credential()`
- `update_credential_disabled()`
- `update_credential_priority()`
- `update_credential_secret_after_refresh()`
- `soft_delete_credential()`
- `update_warmup_remaining()`
- `mark_quota_exhausted()`
- `mark_invalid_refresh_token()`
- `reset_failure_state()`

不要日常调用 `save_credentials(full_snapshot)`。

`save_credentials()` 只保留为 bootstrap/import/migration 工具方法，并且名称改成类似：

- `bootstrap_replace_credentials_if_empty()`
- `import_credentials_snapshot()`

避免业务代码误用。

#### P0：ID 由 PgSQL 分配

建议 `credentials.id` 改为：

```sql
id BIGSERIAL PRIMARY KEY
```

新增凭据时：

```sql
INSERT INTO credentials (...) VALUES (...) RETURNING id
```

如果必须兼容老 ID，也可以保留导入时显式 ID，但日常新增必须由数据库生成。

#### P0：增加 token hash 唯一约束

建议在 `credentials` 表中增加：

- `auth_kind`
- `api_key_hash`
- `refresh_token_hash`
- `credential_fingerprint`
- `deleted_at`

并加部分唯一索引；索引名里的 `active` 仅表示 `deleted_at IS NULL`，不表示 `disabled = false`：

```sql
CREATE UNIQUE INDEX uniq_active_api_key_hash
ON credentials(api_key_hash)
WHERE deleted_at IS NULL AND api_key_hash IS NOT NULL;

CREATE UNIQUE INDEX uniq_active_refresh_token_hash
ON credentials(refresh_token_hash)
WHERE deleted_at IS NULL AND refresh_token_hash IS NOT NULL;
```

这样并发导入也能由数据库兜住。

#### P1：凭据列表查询分页直接走 PgSQL

当前后台凭据状态主要来自 `entries`。如果凭据数量增长，进程内快照和真实库状态会有差异。

建议：

- 调度仍用内存快照。
- 后台列表可以从 PgSQL 查询基础信息，再合并 Redis 调度态。
- 支持分页、搜索、按状态过滤。

这样后台看到的是数据库事实，而不是某个实例的内存视角。

#### P1：凭据敏感字段结构化加密

现在 `credentials.data JSONB` 里保存完整凭据对象。

建议：

- 可查询字段拆列。
- 敏感字段单独加密列保存。
- API 返回和导出时按权限决定是否解密。
- 导出操作写审计。

这不影响调度策略，但会显著改善生产安全。

## Token 刷新优化

### 当前状态

Token 刷新已有两层锁：

- 本进程 `refresh_lock: TokioMutex<()>`
- Redis `scheduler:refresh_lock:{credential_id}`

刷新后仍通过 `persist_credentials()` 全量保存。

### 问题

#### 本进程刷新锁粒度太大

当前本进程 `refresh_lock` 是全局锁。一个凭据刷新时，同进程其他凭据刷新也被阻塞。

迁移后 Redis 已有按凭据 refresh lock，本进程锁也应该按凭据拆分。

#### Redis 锁失败时 fail-open 风险

部分逻辑中 Redis 刷新锁获取失败后，会使用本进程锁继续刷新。

单实例时可以接受，多实例时如果 Redis 短暂不可用，多个实例可能同时刷新同一凭据。

需要明确策略：

- 如果 Redis 是必需依赖，刷新锁失败应更偏 fail-closed。
- 如果为了可用性允许 fail-open，需要记录明显 warning 和指标。

#### 刷新后的写入仍是全量凭据快照

刷新只改变某个凭据的 access token、expires_at、profile_arn 等字段，不应该触发全量凭据保存。

### 优化建议

#### P0：刷新结果行级更新

新增方法：

```rust
update_credential_token_fields(id, refreshed_credentials)
```

SQL 层只更新当前凭据：

- access token
- expires_at
- profile arn
- refresh 相关 metadata
- updated_at

不要软删除其他凭据。

#### P1：按凭据本地锁

本进程内使用：

```rust
DashMap<u64, Arc<TokioMutex<()>>>
```

或封装成 `RefreshLockRegistry`。

这样凭据 #1 刷新不会阻塞凭据 #2。

#### P1：刷新审计表

可以增加：

- `credential_refresh_events`
- credential_id
- started_at
- finished_at
- result
- error_type
- error_message
- token_expires_at_before
- token_expires_at_after
- instance_id

后台排查“为什么凭据被禁用”“为什么频繁刷新”会更直接。

## 调度与并发优化

### 当前状态

当前调度已经使用 Redis 保存：

- 临时冷却。
- rate limit。
- in-flight lease。
- session binding。
- session soft failure。
- refresh lock。

并发限制和等待排队也已经加入。

### 优化空间

#### Redis 调度态读取可以批量化

`scheduler_state_for_credentials()` 当前对每个凭据逐个读：

- cooldown
- rate limit
- in-flight leases

凭据数量少时没问题，凭据多或请求量大时，调度前同步 Redis 会变成明显成本。

建议：

- 用 pipeline 批量读取多个凭据状态。
- 或用 Lua 一次返回多个凭据的简化状态。
- 调度 hot path 只拿必要字段，完整 lease 列表用于后台详情。

#### session binding 多步写入应原子化

当前 `set_session_binding()` 流程大致是：

1. 读旧 binding。
2. 如果旧 credential 不同，移除旧反向索引。
3. 写新 session key。
4. 写 sessions_by_credential set。
5. 设置 set TTL。

这在 Redis 中是多步操作，中间失败可能导致：

- session key 指向新凭据。
- 旧凭据反向索引没清。
- 新凭据反向索引没加。
- TTL 不一致。

建议改为 Lua 脚本：

- 输入 session hash、新 binding、旧 binding、TTL。
- 在一个脚本里完成删除旧反向索引、写新 key、写新反向索引、设置 TTL。

`record_session_soft_failure()` 也应该是 Lua：

- 校验当前绑定 credential_id。
- 原子递增 soft_failure_count。
- 更新 last_used_at。
- 刷新 TTL。
- 返回是否达到阈值。

#### 并发 lease 可增加 owner 信息

当前 lease 主要有：

- id
- acquired_at
- last_seen_at
- kind

建议增加：

- request_id
- instance_id
- path
- model
- conversation_id hash
- stream/api/mcp kind

这样后台“清理占用”时能知道是谁占着，而不是只能看到数量。

#### 等待唤醒可以从本进程 Notify 扩展到 Redis pub/sub

当前本进程内释放 lease 会 `notify_waiters()`，只能唤醒同一进程等待者。

多实例时：

- 实例 A 释放凭据 #1。
- 实例 B 正在等凭据 #1。
- B 不会被 A 的本地 Notify 唤醒，只能等超时或轮询。

建议：

- Redis 发布 `scheduler:capacity_changed`。
- 各实例订阅后唤醒本地等待者。
- 释放 lease、清理 cooldown、禁用/启用凭据、配置变更都可以发通知。

#### 调度状态可以从“同步镜像”升级为“Redis 判定”

当前模式更像：

1. 从 Redis 同步状态到 entries。
2. 在内存里判断哪个凭据可调度。
3. 再到 Redis acquire lease。

这会有窗口期：同步后、acquire 前状态可能变了。

更强的方式是：

- 内存筛出候选凭据。
- Redis Lua 根据候选列表、并发上限、冷却、rate limit 原子选择并 acquire lease。
- 返回选中的 credential_id 和 lease_id。

这样可以把“选择 + 占位”变成一个原子动作。

这属于 P1/P2，不必第一阶段做，但它是 PgSQL+Redis 后真正适合多实例的方向。

## 失败兜底和错误分类优化

### 当前状态

当前 provider 对多种错误已经有分类：

- 402 monthly/overage limit：禁用凭据并换号。
- 429：临时冷却或会话 soft failure。
- 401/403：刷新或禁用。
- 上游网络错误：重试。
- 会话软失败达到阈值但没有其他可用凭据时保留当前凭据。

这是合理的业务逻辑，不建议破坏。

### 优化空间

#### 错误事件应结构化入库

现在 usage record 会记录 error type/message/detail，但调度层自己的决策历史还不完整。

建议增加 `credential_events` 表：

- credential_id
- event_type
- reason
- upstream_status
- retry_after_secs
- request_id
- conversation_id hash
- endpoint
- model
- path
- instance_id
- created_at

事件类型包括：

- transient_cooldown_set
- quota_exhausted_disabled
- refresh_token_invalid_disabled
- refresh_failed
- fallback_to_next_credential
- session_soft_failure
- dispatch_wait_timeout
- concurrency_lease_cleared

这样后台可以解释“为什么可用 2/6，但临时可调度 0”。

#### 下游错误脱敏和后台完整错误分离

用户之前要求“不要把上游 402/429 直接暴露给下游，后台仍能看到完整错误”。

迁移到 PgSQL 后更适合做：

- 下游 response 返回归一化错误。
- usage/error record 保存完整 upstream body。
- 后台详情弹窗展示完整 error_detail。
- 凭据卡片展示最近错误摘要。

这样不会影响调度，也便于排查。

## 统计与运行态优化

### 当前状态

统计和运行态当前有两张表：

- `credential_stats`
- `credential_runtime_state`

但保存方式仍然偏全量：

- `save_credential_stats()` 传入全 map 后 upsert，再删除不在 incoming IDs 里的行。
- `save_credential_runtime_state()` 同样是全 map 保存。

### 问题

多实例下，某个实例内存没有其他实例新凭据，会删除对应 stats/runtime_state。

此外，success count 由内存累加后保存，存在覆盖风险：

- 实例 A 和 B 同时各成功一次。
- A 内存 success_count = 10 写入。
- B 内存 success_count = 10 写入。
- 最终数据库还是 10，而不是 11 或 12。

### 优化建议

#### P0：统计改为原子增量

成功时：

```sql
INSERT INTO credential_stats (credential_id, success_count, last_used_at)
VALUES ($1, 1, now())
ON CONFLICT (credential_id) DO UPDATE
SET success_count = credential_stats.success_count + 1,
    last_used_at = EXCLUDED.last_used_at,
    updated_at = now();
```

不要把内存里的 success_count 覆盖到数据库。

#### P0：运行态按字段更新

失败计数、刷新失败计数、disabled_reason、warmup_remaining 都应该按事件更新：

- 成功：failure_count = 0，refresh_failure_count = 0，warmup_remaining = greatest(warmup_remaining - 1, 0)
- 失败：failure_count = failure_count + 1
- 刷新失败：refresh_failure_count = refresh_failure_count + 1
- reset：相关字段归零
- set warmup：只更新 warmup_remaining

不要全量 map 保存。

#### P1：把“运行态”和“持久配置态”边界拆清楚

建议：

- `credentials.disabled`：是否启用，这是持久配置态。
- `credential_runtime_state.disabled_reason`：为什么不可用，这是运行态/事件态。
- `scheduler cooldown/rate limit/in-flight`：短期态，放 Redis。
- `failure_count/refresh_failure_count/warmup_remaining`：可以入 PgSQL，因为需要重启后保留。

## Usage 记录优化

### 当前状态

usage 已入 PgSQL，并有结构化列：

- endpoint
- stream
- model
- conversation_id
- credential_id
- status
- usage_source
- token 字段
- pricing 字段
- error 字段
- data JSONB

分页接口已有 `has_next` 模式，这是正确方向。

### 优化空间

#### 写入不应只 fire-and-forget

当前 `UsageRecorder.record()` 会更新内存 ring buffer，然后 `tokio::spawn` 异步写 PgSQL。

如果 PgSQL 短暂失败：

- 只打 warning。
- 没有重试。
- 进程退出时可能丢最后一批 usage。

建议：

- 使用 bounded async queue。
- 后台 worker 批量写 PgSQL。
- 写失败按指数退避重试。
- 队列满时明确记录 dropped usage 数。
- shutdown 时 flush。

这不会影响调度，也符合“计费统计失败不能影响请求”的要求。

#### clear 不应生产环境 TRUNCATE

当前 `clear()` 是 `TRUNCATE TABLE usage_records`。

建议改为：

- 后台开发按钮仍可“清空展示”，但生产更建议 soft delete 或按时间删除。
- 支持 retention：保留最近 N 天。
- 支持导出后删除。
- 清空动作写 audit log。

#### 查询可以进一步索引优化

当前已有基础索引：

- created_at
- credential_id + created_at
- model + created_at
- status + created_at
- conversation_id

如果 usage 量增长，建议：

- `endpoint, created_at`
- `usage_source, created_at`
- `pricing_available, created_at`
- `estimated_cost_usd` 聚合可以用日聚合表。

#### 聚合可以物化

后台 dashboard 不一定每次扫全表聚合。

建议根据量级选择：

- 小量：实时 SQL 聚合即可。
- 中量：按天聚合 `usage_daily_summary`。
- 大量：按 credential/model/day 维护 rollup。

## 模型价格优化

### 当前状态

价格目录有：

- 内置 fallback。
- 启动时远程同步。
- PgSQL 持久化。
- 后台手动同步。
- usage record 记录 estimated_cost 和 pricing_available。

这符合“计价不影响调度”的要求。

### 优化空间

#### usage 应记录价格快照

当前 usage record 记录：

- estimated_cost_usd
- pricing_available
- pricing_model

但如果后续价格表变了，历史费用是按当时价格算的还是新价格算的，需要明确。

建议：

- `model_pricing_snapshots`
- `pricing_snapshot_id`
- usage record 记录使用的 snapshot id 或同步时间。

这样历史成本可复算、可解释。

#### 同步状态和价格行拆分

当前 `model_pricing` 每行带 source、source_url、last_synced_at、last_error。

可以继续用，但更规范是：

- `model_pricing`：模型价格行。
- `pricing_sync_status`：最近同步状态、错误、来源、同步耗时。

避免每个模型重复保存同一份同步错误。

#### 价格源失败不应污染当前可用价格

当前失败时会保留内存当前价格并记录 last_error，这是合理的。

进一步建议：

- 远程同步成功后事务替换。
- 失败只更新 sync status，不删除旧价格。
- 后台明确显示“当前正在使用的价格版本”和“最近同步失败原因”。

## Runtime Config 热加载优化

### 当前状态

运行配置已存 PgSQL：

- Admin 修改后写 PgSQL。
- 同时更新当前进程内存。

### 多实例问题

如果有多个服务实例：

- 实例 A 后台修改配置。
- A 内存立即生效。
- B/C 不会自动热加载，除非重启或有手动 reload。

### 优化建议

#### P1：runtime_config 增加 version

建议字段：

- version bigint
- updated_by
- updated_at
- update_reason

更新时：

```sql
UPDATE runtime_config
SET config = $1,
    version = version + 1,
    updated_at = now()
WHERE id = 'default'
RETURNING version;
```

#### P1：Redis pub/sub 通知

配置更新成功后：

- 发布 `runtime_config_updated:{version}`。
- 其他实例收到后从 PgSQL reload。
- 如果 pub/sub 消息丢失，也可定期轻量检查 version。

#### P1：区分启动配置和运行配置

建议明确哪些字段不可热修改：

- PgSQL URL
- Redis URL
- host/port
- 是否 migrate_on_start
- 日志级别

后台不要展示这些字段为可热修改项，避免误解。

## Prompt Cache 和路径上报优化

### 当前状态

路径级上报策略已经比较新：

- 默认策略。
- path prefix override。
- `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` 可以独立配置。
- reader 计算和下游 writer/input 上报解耦。

这是合理的，不建议回退。

### 迁移后可优化点

#### 本地 PromptCacheTracker 可选择 Redis 化

当前 prompt cache tracker 是进程内存。

如果多实例部署，同一会话打到不同实例，会出现：

- 实例 A 认为已有缓存。
- 实例 B 不知道这个 profile，可能第一次还是 creation。

这对“模拟上报一致性”有影响，但不影响真实上游调用。

可选优化：

- Redis 保存 prompt cache fingerprint。
- key 包含 credential_id、model、conversation_id、profile hash。
- TTL 模拟 5m/1h。
- 只保存用于模拟的 profile 摘要，不保存完整 prompt 内容。

注意：这个优化必须保持 reader 计算语义，不应凭空制造不符合条件的缓存。

#### 路径策略可以从 JSON 配置演进为表结构

当前 `reported_usage` 在 runtime config JSONB 中。

短期可继续保留。后续如果后台需要更复杂的路径策略编辑、审计、回滚，可以独立成表：

- `usage_report_policies`
- path_prefix
- enabled
- input policy
- output policy
- cache_read policy
- cache_write policy
- version
- updated_at

但这不是 P0。当前 JSONB 方式足够灵活，优先级低于凭据写入和调度一致性。

## Admin API 和 UI 优化

### 当前状态

后台已经支持：

- 凭据列表。
- 凭据测试。
- 余额查询。
- 启用/禁用。
- 优先级。
- 预热。
- 清理并发占用。
- usage 分页。
- 价格同步。
- 凭据导出。
- runtime config 热修改当前实例。

### 优化空间

#### Admin 操作应写 audit log

迁移到 PgSQL 后，后台操作可以完整审计：

- 谁操作。
- 操作类型。
- 操作对象。
- old value。
- new value。
- 操作结果。
- IP/User-Agent。
- request_id。

高价值操作包括：

- 导出凭据。
- 删除凭据。
- 禁用/启用凭据。
- 修改优先级。
- 修改运行配置。
- 清空 usage。
- 清理 in-flight lease。
- 强制刷新 token。

#### 凭据导出应提供安全等级

当前导出完整凭据包含敏感字段，这是用户明确需要的能力，但生产上建议分等级：

- full：完整敏感字段，仅高级权限。
- backup：完整字段 + 元数据。
- redacted：脱敏字段，用于排查。
- public-summary：只导出 ID、email、状态、统计。

#### 错误详情弹窗应从数据库读完整 detail

页面表格可以截断错误摘要，但点击详情时应显示：

- error_message。
- error_detail。
- credential_id/label。
- upstream status。
- request_id。
- model/path/conversation。
- fallback 过程。

PgSQL 已经能支撑这个能力。

#### 后台调度诊断页

Redis/PgSQL 迁移后，可以增加一个调度诊断面板：

- 每个凭据当前 in-flight。
- 每个 lease 的 request_id、kind、age、idle。
- cooldown 剩余时间和原因。
- rate limit 剩余等待。
- session binding 数量。
- 最近 fallback 事件。
- 最近禁用事件。

这比单纯“可用/不可用”更适合排查线上问题。

## Redis 客户端实现优化

### 当前状态

`RedisStore` 使用一个 `ConnectionManager` 包在 `Arc<Mutex<...>>` 中，所有 Redis 操作串行化。

### 问题

Redis 本身可以高并发处理，但当前客户端侧 mutex 会让所有操作排队：

- 每个请求调度前读 Redis。
- lease acquire/release。
- session binding。
- cooldown/rate limit。
- refresh lock。

在并发上来后，单 mutex 可能成为瓶颈。

### 优化建议

#### P1：减少全局 Redis mutex 粒度

可以选择：

- 每次 clone `ConnectionManager`，让 manager 自身处理连接复用。
- 对不同功能使用独立 manager。
- 高频路径用 pipeline，减少加锁次数。

具体取决于当前 `redis` crate 的 `ConnectionManager` 是否已实现 clone 轻量共享。目标是不要让一个 `tokio::sync::Mutex` 串行化所有 Redis I/O。

#### P1：Lua 脚本封装核心原子动作

优先脚本：

- acquire in-flight lease。
- release in-flight lease。
- touch lease。
- cleanup expired lease。
- set session binding。
- record session soft failure。
- scheduler select and acquire。

这样既提升一致性，也减少 round trip。

## PgSQL schema 和迁移优化

### 当前状态

schema 通过 `SCHEMA_SQL.split(";")` 执行 `CREATE TABLE IF NOT EXISTS` 和 `CREATE INDEX IF NOT EXISTS`。

这是早期快速迁移方式，可用但不足以长期维护。

### 问题

- 无 schema version。
- 无升级记录。
- 修改字段类型、加非空约束、数据回填会比较困难。
- 多实例启动同时 migrate 缺少明确锁。
- 失败后不容易知道迁移到哪一步。

### 优化建议

#### P1：引入 migrations 表

建议增加：

```sql
CREATE TABLE schema_migrations (
    version TEXT PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    checksum TEXT NOT NULL
);
```

迁移执行前用 PgSQL advisory lock：

```sql
SELECT pg_advisory_lock(...);
```

执行后写入 version/checksum。

#### P1：迁移文件化

建议目录：

```text
migrations/
  0001_initial.sql
  0002_credentials_row_level_fields.sql
  0003_runtime_config_version.sql
  0004_audit_log.sql
```

这样比把全部 schema 放在 Rust 字符串里更容易审阅和回滚。

## 多实例一致性优化

### 当前风险

当前代码已经比纯本地文件更适合多实例，但仍不是完全多实例安全。

主要风险：

- 凭据整份保存导致旧实例覆盖新实例。
- runtime config 修改只热更新当前实例。
- stats/runtime_state 仍有全量覆盖。
- session binding 多步 Redis 操作可能不一致。
- 本地 prompt cache 多实例不共享。
- 本进程 Notify 不能唤醒其他实例等待者。

### 推荐多实例模型

| 状态 | 事实源 | 本地内存 | Redis |
|---|---|---|---|
| 凭据基础信息 | PgSQL | 快照 | 无 |
| access token/refresh token | PgSQL 加密列 | 快照 | refresh lock |
| 启用/禁用/优先级 | PgSQL | 快照 | 发布变更 |
| failure_count/warmup | PgSQL | 快照 | 可选镜像 |
| success_count | PgSQL 原子增量 | 可缓存 | 无 |
| cooldown/rate limit | 无长期事实源 | 镜像 | 事实源 |
| in-flight | 无长期事实源 | 镜像 | 事实源 |
| session binding | 无长期事实源 | 可选镜像 | 事实源 |
| runtime config | PgSQL versioned | 快照 | pub/sub |
| usage | PgSQL | 短期 ring buffer 可选 | 无 |
| pricing | PgSQL + 内存快照 | 快照 | 可选通知 |

## 测试优化

### 当前测试基础

当前已有：

- model/config 测试。
- reported usage path override 测试。
- prompt cache 测试。
- Redis scheduler 测试。
- PgSQL store 测试，依赖 `KIRO_RS_TEST_POSTGRES_URL`。

### 需要补的测试

#### 凭据多实例测试

1. 实例 A 和 B 同时从同一 PgSQL 加载凭据。
2. B 新增凭据。
3. A 刷新 token 或修改其他凭据。
4. 验证 B 新增凭据不会被 A 删除。

这是验证“去掉全量快照保存”的关键测试。

#### DB ID 并发测试

1. 并发新增 N 个凭据。
2. 验证 ID 唯一递增。
3. 验证 token hash 唯一约束生效。

#### Stats 原子增量测试

1. 两个 manager 并发 report_success。
2. 验证 PgSQL success_count 是累计值。

#### Runtime config 跨实例热加载测试

1. A 更新配置。
2. B 收到 Redis pub/sub。
3. B runtime_config 变更。
4. 调度参数无需重启生效。

#### Redis 原子 session 测试

1. 并发 set_session_binding 同一 session 到不同 credential。
2. 验证最终 session key 和反向索引一致。

#### Usage 写入队列测试

1. PgSQL 正常，usage 写入成功。
2. PgSQL 短暂失败，队列重试。
3. 队列满时 dropped counter 正确记录。
4. shutdown flush。

## 优先级建议

### P0：必须优先处理

这些和数据一致性、多实例安全直接相关：

1. 凭据日常写操作从全量 `persist_credentials()` 改成行级 PgSQL 事务。
2. 新增凭据 ID 由 PgSQL 生成。
3. token hash 唯一约束由 PgSQL 保障。
4. token refresh 后只更新当前凭据行。
5. stats/runtime_state 改成原子增量和按字段更新。
6. `reload_credentials_from_postgres()` 改为完整 reconcile，支持新增、删除、更新。
7. Admin 修改凭据后发布变更事件，其他实例 reload。

### P1：建议随后处理

这些提升稳定性、性能和可运维性：

1. runtime_config 增加 version，Redis pub/sub 热加载。
2. Redis session binding 和 soft failure 改 Lua 原子脚本。
3. Redis 调度态读取 pipeline 化。
4. 跨实例 dispatch capacity pub/sub 唤醒。
5. Usage 写入改队列 + 重试 + shutdown flush。
6. schema migration 文件化和版本化。
7. Admin audit log。
8. 错误事件结构化入库。

### P2：可选增强

这些是更长期的生产化能力：

1. 凭据敏感字段加密。
2. pricing snapshot 历史版本。
3. usage 日聚合/materialized view。
4. prompt cache tracker Redis 化。
5. Redis Lua 原子“选择凭据 + acquire lease”。
6. 后台调度诊断页。

## 不建议改的点

以下内容当前不建议为了“数据库化”而强行改：

- 每次请求实时从 PgSQL 选凭据：会增加请求延迟，调度 hot path 应保留内存快照 + Redis 调度态。
- 把所有 Redis 临时态写回 PgSQL：cooldown、rate limit、in-flight 是短期态，写 PgSQL 会放大 I/O。
- 把完整 prompt 内容放 Redis：会增加敏感数据暴露面，只保存 fingerprint/摘要即可。
- 价格同步失败影响调度：计费统计必须保持非硬性失败条件。
- 为了多实例一致性破坏 `/cc`、`/ha`、`/na` 的路径上报语义：路径策略应独立演进。

## 推荐落地顺序

### 第一步：去掉凭据全量覆盖写

目标：

- `persist_credentials()` 不再作为日常路径。
- 添加、删除、禁用、优先级、刷新 token 都是单行事务。
- 数据库约束防并发重复。

这是最关键的一步，因为它直接消除“旧实例覆盖新实例”的风险。

### 第二步：运行态和统计原子化

目标：

- success_count 原子增量。
- failure_count/refresh_failure_count 按事件更新。
- warmup_remaining 原子扣减或设置。
- 禁用原因由明确事件写入。

这样多实例下统计不会丢。

### 第三步：配置和凭据变更跨实例通知

目标：

- runtime config version。
- Redis pub/sub。
- manager reload。
- 后台显示当前 version。

这样后台修改不需要重启。

### 第四步：Redis 原子脚本和性能优化

目标：

- session binding 一致。
- soft failure 一致。
- pipeline 批量读调度态。
- 跨实例释放 lease 可唤醒等待者。

### 第五步：审计、诊断、usage 写入队列

目标：

- 生产可排查。
- usage 更可靠。
- 敏感操作有记录。
- 后台能解释调度状态。

## 结论

迁移到 PgSQL + Redis 后，确实存在大量优化空间。最重要的不是增加更多功能，而是把旧的“本地文件整份读写”心智模型彻底切换成“数据库事实源 + Redis 临时调度态 + 内存快照”的模型。

当前代码已经完成了迁移的第一层：数据落到了 PgSQL/Redis。

下一层应该做的是：

- 凭据行级写。
- 统计原子写。
- 配置版本化热加载。
- Redis 原子脚本。
- usage 可靠异步写。
- 管理后台审计和调度诊断。

这些改完后，系统才算真正从单机文件型服务演进为适合多实例、可运维、可审计的 PgSQL + Redis 服务。
