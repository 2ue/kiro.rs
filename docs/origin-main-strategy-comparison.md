# 当前工作区与远端 main 策略对比说明

本文对比当前工作区与远端 `origin/main` 的逻辑策略差异。对比基线是：

- 当前分支：`main`
- 当前 HEAD：`dcf1b1f9ce3248c3251173da44e6185899217682`
- 远端 main：`origin/main`，同为 `dcf1b1f9ce3248c3251173da44e6185899217682`
- 差异来源：当前工作区未提交改动和新增文件

结论先行：

1. 账号调度的核心策略和远端 `main` 保持一致：优先级模式、均衡模式、会话粘性、软失败 fallback、402 禁用、401/403 累计失败、429/408/5xx 临时冷却、单凭据并发排队、预热权重这些规则没有换成另一套业务策略。
2. 缓存策略和远端 `main` 保持一致：`/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` 的路径级 high-cache 计算和下游 usage 上报语义没有改变。当前工作区对 `src/anthropic/handlers.rs` 和 `src/anthropic/router.rs` 的实际差异只是测试里 `UsageRecorder::new(...)` 构造参数变化。
3. 当前工作区的主要变化是存储和分布式一致性优化：把本地文件/单进程内存状态迁移到 PgSQL + Redis，并把部分旧的“整份文件覆盖”实现改成行级写、Redis lease、Redis 会话绑定、Redis 冷却和跨实例刷新锁。
4. 有少量可感知行为差异，不是调度策略改变，而是新架构的故障边界变化：PgSQL/Redis 现在是运行硬依赖；Redis 调度状态写入失败时，当前实现会返回明确错误，而不是继续只靠本进程内存运行。

## 对比范围

本次覆盖了当前工作区相对 `origin/main` 的所有业务相关差异：

- 启动与配置：`src/main.rs`、`src/model/config.rs`、`config.example.json`
- PgSQL/Redis 存储：`src/storage/postgres.rs`、`src/storage/redis_cache.rs`
- 账号调度：`src/kiro/token_manager.rs`、`src/kiro/provider.rs`
- 缓存和路径 usage：`src/anthropic/router.rs`、`src/anthropic/handlers.rs`、`src/anthropic/cache.rs`、`src/anthropic/prompt_cache.rs`
- usage、价格和统计：`src/anthropic/usage.rs`、`src/anthropic/pricing.rs`
- Admin API 和 UI：`src/admin/*`、`admin-ui/src/*`
- 部署与文档：`docker-compose.local-infra.yml`、`docker-compose.database.yml`、`README.md`、`docs/*`

## 总体差异

| 模块 | 远端 main | 当前工作区 | 策略是否一致 | 说明 |
| --- | --- | --- | --- | --- |
| 凭据事实源 | `credentials.json` + 进程内 `entries` | PgSQL `credentials` + 进程内快照 | 基本一致 | 凭据字段和启停策略一致，事实源从文件迁到数据库 |
| 运行配置 | `config.json`，Admin 修改后写回文件 | PgSQL `runtime_config`，首次从文件 bootstrap | 基本一致 | 可热加载范围一致，写入介质变化 |
| 调度短期状态 | 进程内 memory | Redis + 进程内镜像 | 策略一致，能力增强 | 多实例可共享冷却、限流、并发、会话粘性 |
| 使用记录 | 内存 ring + JSONL 文件 | 内存 ring + PgSQL | 策略一致，能力增强 | 查询、分页、统计改为 SQL，下游语义不变 |
| 余额缓存 | 进程内 + 本地 JSON 文件 | Redis TTL 缓存 | 策略一致，能力增强 | 仍是 5 分钟缓存，跨实例可共享 |
| 模型价格 | 内存目录，启动同步 | PgSQL 持久目录 + 启动同步 | 策略一致，能力增强 | 仅统计使用，失败不影响调度 |
| 高缓存路径 | `/v1`、`/cc`、`/ha`、`/na` 路径级策略 | 同远端 main | 一致 | 当前工作区没有改缓存策略代码 |
| Admin 审计 | 无独立审计表 | PgSQL `admin_audit_logs` | 新增能力 | 记录管理操作，不影响调度 |

## 账号调度策略

### 仍然一致的策略

当前工作区保留了远端 `main` 的核心调度规则。

优先级模式保持一致：

- 默认 `loadBalancingMode=priority`。
- 优先使用 `current_id` 指向的凭据。
- 当前凭据不可调度时，再选择优先级数字最小的可用凭据。
- 手动调整优先级后，会重新选择优先级最高的可用凭据。

均衡模式保持一致：

- `loadBalancingMode=balanced` 时，新会话不固定使用 `current_id`，而是按 `success_count` 少者优先，再按优先级兜底。
- 预热凭据仍然不会伪造成功次数。
- 预热只是在真实业务请求中按 `credentialWarmupSelectionPercent` 低概率参与调度。
- 成功后才扣减 `warmup_remaining`。

模型能力过滤保持一致：

- Opus 模型仍会检查凭据是否支持 Opus。
- 不支持目标模型的凭据不会参与本次调度。

会话粘性保持一致：

- 同一 `conversationId` / session 优先绑定同一个凭据。
- 绑定 TTL 仍是 6 小时。
- 软失败计数达到 `MAX_SESSION_SOFT_FAILURES = 2` 后，本次请求才临时 fallback。
- `excluded_ids` 仍只影响本次请求，不代表凭据被禁用。
- 只有存在其他可调度凭据时，才会把当前凭据加入本次排除集合，避免唯一可用凭据被临时排除后误报“全部禁用”。

错误分类保持一致：

- `402 Payment Required` 且识别为额度用尽：禁用该凭据并尝试下一个。
- `401/403`：按凭据问题累计失败，达到阈值后禁用。
- `408/429/5xx`：视为上游瞬态错误，进入临时冷却，不直接禁用。
- 网络错误和 retryable AWS exception 仍走软失败/重试路径。
- 非流式成功、流式 EOF 成功、客户端断开、流错误的 success/failure 上报语义不变。

并发限制保持一致：

- `credentialMaxConcurrentRequests=0` 表示不限制。
- 大于 0 时，同一个凭据并发占满后，请求先尝试其他可用凭据。
- 如果所有可用凭据都占满、冷却或本地限流，则进入排队等待，而不是立即报错。
- `credentialDispatchMaxWaitSecs` 控制最长等待时间，超时后才返回明确的调度等待超时错误。
- `credentialInFlightLeaseMaxSecs` 控制异常未释放 lease 的自动回收。

### 当前工作区的优化

调度短期态从单进程内存迁到 Redis：

- 会话绑定：远端 `main` 存在 `MultiTokenManager.session_bindings`；当前工作区优先写 Redis `scheduler:session:*`，并保留本进程镜像作为无 Redis 的测试兜底。
- 软失败计数：远端 `main` 只在本进程计数；当前工作区用 Redis Lua 脚本原子更新，跨实例共享。
- 瞬态冷却：远端 `main` 只在本进程 `cooldown_until` 标记；当前工作区写 Redis `scheduler:cooldown:{id}`，其他实例也会避开这个凭据。
- 单凭据 RPM：远端 `main` 只在本进程计算 `rate_limit_available_at`；当前工作区写 Redis `scheduler:rate_limit:{id}`，跨实例累积节流窗口。
- 并发 lease：远端 `main` 只记录本进程 in-flight；当前工作区用 Redis sorted set/hash 记录 lease，多个实例共享同一个凭据的并发上限。
- Token 刷新锁：远端 `main` 只有本进程 `refresh_lock`；当前工作区增加 Redis `scheduler:refresh_lock:{id}`，避免多实例同时刷新同一凭据。
- 调度唤醒：当前工作区释放 Redis in-flight lease 后会发布 Redis wakeup 事件，其他实例可以更快结束等待。

持久态从本地文件迁到 PgSQL：

- 凭据、token 刷新结果、禁用状态、优先级、订阅等级等写入 PgSQL。
- `success_count` 和 `last_used_at` 用 PgSQL 原子增量记录。
- `failure_count`、`refresh_failure_count`、`disabled_reason`、`warmup_remaining` 拆到 PgSQL runtime state。
- 新增凭据 ID 由 PgSQL sequence 分配，避免多实例 `max(id)+1` 冲突。
- API Key / refreshToken 的 active hash 增加唯一约束，内存重复检测仍用于快速报错，数据库约束负责跨实例兜底。
- 删除凭据改为 PgSQL soft delete，并清理统计/运行态。

配置热加载能力增强：

- 远端 `main` 的 Admin runtime config 更新会写回 `config.json`。
- 当前工作区写入 PgSQL `runtime_config`，发布 Redis `runtime_config_changed` 事件。
- 主进程监听 Redis pub/sub，并每 60 秒做一次兜底 reload。
- 这让多实例下配置修改能传播，不需要每个实例重启。

### 可感知行为差异

PgSQL/Redis 变成运行硬依赖：

- 远端 `main` 可以只靠 `config.json` 和 `credentials.json` 启动。
- 当前工作区启动时必须成功连接 PgSQL 和 Redis。
- `config.json` / `credentials.json` 只用于首次 bootstrap 和 CLI 诊断，不再是运行时写入目标。

Redis 写入失败的错误边界更严格：

- 远端 `main` 的 429/408/5xx 临时冷却只写本进程内存，通常不会因为调度状态写入失败而中断。
- 当前工作区 `report_transient_failure()` 返回 `Result`；Provider 如果写 Redis 调度状态失败，会返回“调度状态写入失败”。
- 这是为了避免多实例下某个实例继续使用已应冷却的凭据。它是分布式一致性的优化，但 Redis 故障时会比远端 `main` 更早暴露基础设施错误。

Token 刷新跨实例等待：

- 当前工作区如果发现其他实例正在刷新同一凭据，会等待 PgSQL 凭据同步，最多约 15 秒。
- 如果超时，会返回“等待其他实例刷新 Token 超时”。
- 远端 `main` 没有这个跨实例等待，因为没有跨实例刷新锁。

Admin 写操作的失败语义更严格：

- 远端 `main` 很多操作是先改内存，再写文件；文件写失败可能出现“本进程已改、文件未持久化”的情况。
- 当前工作区对禁用、优先级、预热等管理操作更偏向先写 PgSQL，再更新内存。
- 因此 PgSQL 写失败时，操作会更明确地失败，不会假装已经持久化。

环境变量 `KIRO_API_KEY` 的行为需要注意：

- 当前启动仍支持 `KIRO_API_KEY`。
- 该入口已经明确为“一次性导入/复用”：启动时先按 active `api_key_hash` 查询 PgSQL，存在则复用，不存在才插入。
- 它不是临时覆盖；导入后 PgSQL 仍是凭据事实源。

## 缓存策略

### 路径策略与远端 main 一致

当前工作区没有改变远端 `main` 的缓存策略实现。

路径级行为仍是：

| 路径 | 底层 prompt-cache 计算 | 下游 usage 上报策略 |
| --- | --- | --- |
| `/v1/messages` | high-cache | 默认原样上报本地 high-cache usage |
| `/cc/v1/messages` | high-cache | 改写 `input_tokens` 和 `cache_creation_input_tokens`，其余按策略保留 |
| `/ha/v1/messages` | high-cache | 只改写 `input_tokens`，writer/read/output 其余保留 |
| `/na/v1/messages` | high-cache 仍开启 | 对本地模拟 usage 关闭 cache 上报；真实上游 metadata cache 原样保留 |

`/v1/models`、`/cc/v1/models`、`/ha/v1/models`、`/na/v1/models` 都仍能返回 models；`count_tokens` 仍是读取/计算接口，不受 usage writer 上报策略影响。

### `/cc` writer 和 input 策略一致

`/cc/v1/messages` 的下游上报仍按路径覆盖：

- `input_tokens`：采样到 `1..=96` 左右，并把被压低的 input 差值转入 `cache_read_input_tokens`。
- `cache_creation_input_tokens`：围绕 `targetTokens=3000` 做自然采样，默认常规范围约 `0..3600`，不是固定 3000，也不是递增。
- `output_tokens`：不被 `/cc` 特殊策略改写。
- 上述改写只作用于 `UsageSource::LocalPromptCache` 的下游上报和后台 usage record，不影响上游请求、reader 计算、prompt-cache tracker 更新。
- 如果 usage 来源是真实 upstream metadata，`reported_usage_for_downstream()` 不应用本地上报改写，真实 cache 字段保持权威。

### `/ha` 与 `/cc` 仍是独立配置

当前 `reportedUsage.pathOverrides` 是按路径前缀独立覆盖：

- `/cc` 独立配置 input 和 writer。
- `/ha` 独立配置 input，当前不改 writer。
- `/na` 独立配置关闭本地模拟上报。
- 默认策略适用于 `/v1`。

这符合之前的要求：后续可以单独改变 `/ha` writer 或其他字段，不会因为 `/cc` 改动影响 `/ha`。

### 实现层面差异

当前相对远端 `main`，缓存相关文件的业务逻辑几乎没有变化：

- `src/anthropic/cache.rs` 没有修改。
- `src/anthropic/prompt_cache.rs` 没有修改。
- `src/anthropic/router.rs` 只有测试构造 `UsageRecorder::new(10, None)` 改为 `UsageRecorder::new(10)`。
- `src/anthropic/handlers.rs` 也主要是测试构造参数变化。

所以缓存策略可以判定为与远端 `main` 一致。

## Usage 记录、模型价格和统计

### Usage 记录

远端 `main`：

- 内存保存最近 `usageRecordLimit` 条。
- 可选追加写 `kiro_usage_records.jsonl`。
- 查询和统计主要基于内存/文件读取。

当前工作区：

- 仍保留内存 ring，作为热点和 PgSQL 查询失败时的 fallback。
- 使用 PgSQL `usage_records` 作为长期记录。
- 写入走有界异步队列，最多重试 3 次。
- 分页查询按 `limit+1` 判断 `hasNext`，不再依赖总数。
- `clear` 改为 soft delete，不再截断文件。
- summary、credential cost summary、top credentials、top conversations 下推到 SQL 聚合。

策略一致点：

- 成功、错误、stream error、timeout、client dropped 的 usage record 语义不变。
- `usageSource`、`simulated`、`stickyBound`、`fallbackFromSticky` 等字段仍按原先请求链路记录。
- 价格估算仍是统计用途，不参与调度决策。

优化点：

- 使用记录跨重启保留。
- 查询不需要读整个 JSONL。
- 后台记录可以展示更多历史。
- 成本汇总可以按凭据聚合到凭据卡片。

### 模型价格

远端 `main`：

- `PricingCatalog` 启动后异步同步公开价格源。
- 同步失败只记录状态，不影响请求调度。
- 价格主要保存在内存。

当前工作区：

- 启动时先从 PgSQL 加载已持久化价格。
- 启动后仍异步同步公开价格源。
- 手动同步后保存 PgSQL。
- `model_pricing` 存当前模型价格，`model_pricing_sync_status` 存同步状态。
- 估算失败或价格缺失仍不影响调度，只体现在统计状态。

策略结论：一致，当前只是让价格目录可恢复、可页面查看。

## Admin API 和 UI

当前工作区新增或优化了这些后台能力：

- 余额缓存从本地文件迁到 Redis，TTL 仍是 5 分钟。
- 管理操作写入 PgSQL 审计日志：添加/删除凭据、禁用/启用、优先级、预热、清理并发、负载模式、强刷 token、清空 usage、导出凭据、同步价格。
- 新增 `/api/admin/audit-logs`。
- UI 新增审计页签。
- 运行时配置页说明从“写回配置文件”改为“写入 PgSQL 并热加载”。
- Usage 记录分页默认 20 条，用 `hasNext` 控制下一页，不查询总页数。

这些属于可观测性和管理能力增强，不改变账号调度或缓存计算策略。

## 配置项变化

当前工作区删除了这些文件时代开关：

- `credentialsPersist`
- `credentialStatsPersist`
- `usageRecordPersist`

原因：

- 当前方向是不再兼容运行时写本地文件。
- 凭据、统计、运行态、usage 都进入 PgSQL。
- 这些开关保留会造成“明明数据库是事实源，却还能关闭持久化”的歧义。

新增启动配置：

- `postgres.url`
- `postgres.maxConnections`
- `postgres.migrateOnStart`
- `redis.url`
- `redis.keyPrefix`
- 环境变量覆盖：`KIRO_RS_POSTGRES_URL`、`KIRO_RS_REDIS_URL`

需要重启的仍是启动期配置：

- PgSQL/Redis 连接信息
- 监听 host/port
- Admin API key
- 代理客户端底层配置

可热加载的仍是运行期配置：

- 凭据 RPM
- 单凭据并发
- 临时冷却/最大冷却
- 调度等待超时
- 并发 lease 自动回收
- 预热参数
- 压缩开关
- high-cache 模拟参数
- 路径级 `reportedUsage`
- high-cache 统计阈值
- compat profile
- thinking 提取
- proxy warning header 开关

## 部署差异

新增：

- `docker-compose.local-infra.yml`：仅启动本地 PgSQL 和 Redis，默认端口 `25432`、`26379`，避免和常见 `5432`、`6379`、`15432`、`16379` 冲突。
- `docker-compose.database.yml`：部署当前服务 + PgSQL + Redis。

保留：

- 原有 `docker-compose.yml` / `docker-compose.deploy.yml` 没有被覆盖。

策略影响：

- 部署形态变化是运行依赖变化，不影响调度和缓存业务策略。
- 当前服务如果按 PgSQL + Redis 模式运行，必须确保数据库和 Redis 健康，否则会启动失败或调度状态写入失败。

## 可能的风险点

1. Redis 故障会更直接影响调度。
   当前设计把会话绑定、冷却、限流、in-flight lease 都放到 Redis。好处是多实例一致；代价是 Redis 不可用时不能像远端 `main` 那样完全靠单进程内存继续。

2. `KIRO_API_KEY` 仍是导入入口，不是临时覆盖。
   当前已避免重复导入和重启唯一约束冲突；但如果产品语义希望环境变量完全不落库，后续应移除该入口。

3. usage writer 队列满时会丢弃持久化记录。
   请求响应不会因此失败，符合“不影响调度”的原则，但极端高并发或 PgSQL 故障时统计会有缺口。

4. `query()` 仍会查询 total。
   分页接口已经使用 `hasNext`，但兼容旧接口 `get_usage_records` 仍返回 `total`。如果旧接口在大库上高频使用，后续可以降级或只供小范围查询使用。

5. PromptCacheTracker 仍是进程内。
   这点和远端 `main` 一致。多实例下，高缓存 reader 仍依赖同一会话落到同一实例的内存 tracker，或者同一实例已有本地缓存状态。Redis 化 prompt-cache tracker 属于后续 P2，不在当前迁移里强做。

## 行为边界优化建议

这些优化点不代表当前策略错误，而是 PgSQL + Redis 成为运行核心后，可以把故障边界和可观测性做得更稳。

### 已落地优化

当前工作区已经落地以下优化：

- `KIRO_API_KEY` 改为查重后一次性导入：启动时先按 active `api_key_hash` 查询 PgSQL；已存在则复用，不再插入无 ID 凭据；不存在才插入。这样避免同一个环境变量在重启时重复导入，也避免触发唯一约束后导致 Token 管理器创建失败。
- PgSQL/Redis 启动连接增加有限重试：启动时最多等待约 60 秒，减少 Compose 或容器重启时数据库刚启动但应用先启动造成的失败。
- 新增 `/healthz` 和 `/readyz`：`/healthz` 表示进程存活；`/readyz` 会检查 PgSQL ping、Redis ping、Redis 运行时事件订阅状态。
- Admin API 新增 `/api/admin/usage-writer-stats`：暴露 usage writer 是否启用、队列容量、当前可用容量、内存记录数和已丢弃的 PgSQL 持久化记录数。该接口只用于观测，不参与调度。

### P0：明确 `KIRO_API_KEY` 的导入语义

原先启动逻辑会把 `KIRO_API_KEY` 环境变量插入凭据列表；因为它没有 ID，`MultiTokenManager::new_with_stores()` 会通过 PgSQL 分配 ID 并写入数据库。

这个原行为有两个问题：

- 它实际是“自动导入并持久化”，不是临时覆盖。
- 如果同一个 API Key 已经存在于 PgSQL，再次启动时可能因为 active `api_key_hash` 唯一约束触发“凭据已存在”并导致 Token 管理器创建失败。

长期产品语义仍然可以二选一：

- 如果保留 `KIRO_API_KEY`，就把它定义成“一次性 bootstrap/import”。启动时先按 hash 查询是否已存在，存在则复用或提升优先级，不再插入新凭据；不存在时再插入，并写清楚会持久化。
- 如果不希望环境变量影响数据库事实源，就移除自动导入，只允许通过 Admin/API/首次凭据文件导入。这样语义最干净。

优先级：P0。这个点可能直接影响重启稳定性。

当前状态：已按“一次性 bootstrap/import”落地。后续如果不希望环境变量影响数据库事实源，可以再进一步移除该入口。

### P0：启动连接增加有限重试和 readiness

当前 PgSQL/Redis 是硬依赖，这是符合当前架构方向的；但启动时 `PostgresStore::connect()` 和 `RedisStore::connect()` 只要瞬时失败就退出。

建议：

- 对 PgSQL/Redis 启动连接增加有限 retry/backoff，例如总等待 30 到 60 秒。
- 增加 `/healthz` 和 `/readyz`：`healthz` 只表示进程活着；`readyz` 检查 PgSQL、Redis、Redis pub/sub listener、运行配置版本是否正常。
- Docker Compose 仍保留 DB/Redis healthcheck，但应用自身 readiness 应该能反映真实依赖状态。

优先级：P0。它不改变调度策略，只减少部署和重启时的偶发失败。

当前状态：已落地启动重试、`/healthz` 和 `/readyz`。后续可以继续把 readiness 状态接入 Dockerfile/Compose 的应用 healthcheck。

### P0：Redis 失败按关键程度分级

当前多数调度关键写入是 fail-closed，这是合理的：

- 占用 in-flight lease 失败，不能继续请求，否则并发上限会失效。
- 写 rate limit 失败，不能继续请求，否则本地 RPM 会失效。
- 写 cooldown 失败，不能假装已经冷却，否则多实例可能继续打同一凭据。

但仍建议优化失败边界：

- 对关键 Redis 写入加短重试和 jitter，避免单次网络抖动直接变成下游错误。
- 将“release 成功但 publish wakeup 失败”的日志拆开；释放失败和唤醒失败不是同一级别。
- Token refresh lock 当前 Redis 获取失败时会 fallback 到本进程锁。多实例下这可能导致多个实例同时刷新同一凭据。建议用 PgSQL advisory lock 作为 Redis refresh lock 的兜底，或者把 Redis refresh lock 获取失败也改成 fail-closed。

优先级：P0/P1。并发 lease、rate limit、cooldown 保持 fail-closed；refresh lock fallback 建议优先收紧。

### P1：usage writer 不影响调度，但不能静默丢统计

当前 `UsageRecorder` 使用 4096 容量的有界 mpsc 队列，队列满时会丢弃本条 PgSQL 持久化记录，请求本身不失败。这符合“统计不能影响调度”的原则，但统计侧需要更可观测。

建议：

- 把 `dropped_persist_records`、writer retry 失败次数、队列容量、最近写入错误暴露到 Admin API/UI。
- writer 改成批量 insert，降低 PgSQL 抖动时的队列积压。
- 服务优雅退出时 drain 队列，尽量减少停机丢记录。
- 如果后续要求 usage 几乎不丢，可以引入 Redis Stream / PgSQL outbox 做缓冲，但仍要保持请求链路非阻塞。

优先级：P1。它影响统计完整性，不影响调度正确性。

当前状态：已暴露 writer 队列和丢弃计数；批量 insert、优雅退出 drain、Redis Stream/PgSQL outbox 仍是后续优化项。

### P1：PromptCacheTracker 是否 Redis 化需要单独决策

当前 `PromptCacheTracker` 是进程内 HashMap，这和远端 `main` 一致，也避免了本次迁移改变 reader 计算逻辑。

如果未来要支持多实例下更像官方缓存的效果，可以做 Redis L2：

- key 使用 scope + fingerprint。
- TTL 按 cache breakpoint TTL 设置。
- 本地内存作为 L1，Redis 作为跨实例共享的 L2。
- lookup + write 最好用 Lua 保证一次请求内的读写判断一致。

但这会影响本地 high-cache reader 的计算结果，不只是 writer 上报变化。因此它不应该混在“存储迁移”里悄悄做，必须单独评估和测试 `/v1`、`/cc`、`/ha`、`/na` 的 usage 语义。

优先级：P1/P2。单实例可以暂缓；多实例部署且要求缓存读稳定时再做。

### P1：调度可观测性继续增强

当前后台已经能看到 in-flight 数、最老占用时间、最近活跃时间、lease 最大存活时间，并支持手动清理。

建议继续补充：

- in-flight lease 增加 `request_id`、`instance_id`、`path`、`model`、`conversation_id_hash`。
- Admin 展示 Redis pub/sub 监听状态、最后一次收到事件时间、最后一次 PgSQL reload 成功时间。
- 统计 Redis 操作失败次数，并按 `cooldown_write`、`rate_limit_write`、`lease_acquire`、`lease_release`、`publish_wakeup` 分类。

优先级：P1。主要用于排查“为什么账号不可调度”和“是不是 Redis/DB 抖动”。

### P2：正式迁移系统

当前 schema 仍在代码里集中执行。后续如果要长期运行，建议引入正式 migration 版本管理：

- 每个 schema 变更一个版本文件。
- `migrate_on_start` 只执行未执行版本。
- migration 表记录版本、执行时间、checksum。
- 生产环境可以关闭自动迁移，改为部署前显式执行。

优先级：P2。当前开发和小规模部署可用，但长期维护需要正规化。

## 最终结论

从策略层看，当前工作区与远端 `main` 的账号调度和缓存策略保持一致：

- 没有改变优先级/均衡选择规则。
- 没有改变会话粘性和软失败阈值。
- 没有改变 402、401/403、429/408/5xx 的错误分类。
- 没有改变 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` 的缓存路径语义。
- 没有让 usage writer 影响 reader 计算或上游请求。

当前工作区的主要价值是把远端 `main` 的单机文件式实现升级为 PgSQL + Redis 的可持久、可共享、可观测实现：

- PgSQL 负责长期事实源和统计。
- Redis 负责短期调度态和跨实例通知。
- 进程内存保留为快照和 hot path 缓存。
- 本地文件只负责首次 bootstrap 和 CLI 诊断。

需要重点关注的不是“策略是否变了”，而是新架构的依赖边界：PgSQL/Redis 现在是服务运行的一部分，基础设施故障会比远端 `main` 更显性地暴露出来。这是可接受的架构取舍，但部署和告警必须跟上。
