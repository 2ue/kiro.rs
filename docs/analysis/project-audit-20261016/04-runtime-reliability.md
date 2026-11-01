# 04 运行时可靠性、调度与可观测性遗留问题

0.0.194 已修复 R1–R4（见 01）。本文记录剩余问题。

## R6（P2）请求准入限流只在进程内生效

### 现状

`src/anthropic/request_admission.rs:3` 明确写了热路径只在进程内生效、不依赖 Redis。按 key 的 RPM 令牌桶和并发闸门在每个实例上独立计数，多实例部署时实际上限等于配置值乘以实例数。

### 方案

1. 文档先写清楚这个语义：配置值是"每实例"的上限。
2. 可选：在 Redis 上实现全局 RPM（滑动窗口 ZSET 或 GCRA 单 key Lua），本地令牌桶保留作为 Redis 不可用时的兜底。注意不能把 Redis 往返加到每个请求的热路径上：先过本地令牌桶，只对放行的请求做 Redis 校验。
3. 只有实际跑多实例、并且对外按 key 计费或限额时才需要做第 2 步。现在 59137 和 59139 是两个独立部署，不共享 key，这个问题不紧迫。

## R7（P2）两套刷新锁协议并存

### 现状

- 普通刷新走 `TOKEN_REFRESH_BEGIN_SCRIPT`：leader / wait / replay / succeeded，带 outcome hash 和 generation fence。
- `force_refresh_token_for_with_budgets`（`manager.rs:13824`）走 `acquire_refresh_lock` 的简单锁，持有本地 `gate` 期间以 500ms 间隔轮询 Redis，最长 15 秒，而且不写 outcome。
- 两者共用同一个 lock key，所以互斥成立。但 force 刷新期间，其他实例的 BEGIN 只会看到 wait，看不到结果。force 期间本地同一凭据的请求也都排在 `gate` 上。

### 方案

force 刷新改为复用 BEGIN/COMPLETE 协议，只是带一个"强制"标记：跳过"已刷新成功"的短路判断，其余逻辑相同。这样只剩一套协议，outcome 也能被其他实例看到。

## R8（P2）回落决策依赖错误文案的字符串匹配

### 现状

`is_local_temporary_scheduler_error`（`handlers.rs:5600`）、`is_upstream_payload_too_long_error`（`:5930`）、`is_upstream_context_window_full_error`（`:5946`）、`classify_local_error_for_external_fallback`（`:2632`）等函数，通过匹配错误消息里的中英文片段（如 `所有账号均已禁用`）决定回落、重试和对外状态码。上游或本地任何一处改了文案，路由行为都可能悄悄改变。

### 方案

1. 本地错误改为带类型：调度器的 bail 换成枚举错误（`SchedulerError::AllDisabled { available, total }` 等），handlers 用 `downcast_ref` 判断。
2. 上游错误在 provider 层解析成 `UpstreamFailureClass`（项目已有 `class=` 字段），handlers 只看分类，不看文案。
3. 过渡期保留字符串匹配作为兜底，同时打 DEBUG 日志统计命中次数，确认为 0 之后再删掉。
4. 每个枚举分支都加测试，固定错误到状态码、到是否回落的映射。

## R10（P2）部署方式导致短暂协调不可用

### 现状

0.0.194 部署时，redis 与 postgres 容器在 15:20:02 和 app 一起重启。Redis 数据会从持久化文件恢复，但外部池协调器的 epoch 需要重新初始化，15:20:40–45 出现了 5 次 `external_pool_coordinator_unavailable`。

### 方案

1. 部署脚本或文档改为只更新应用：`docker compose pull app && docker compose up -d --no-deps app`。
2. 检查 compose 里 app 的 `depends_on` 是否带 `condition: service_healthy`，避免依赖服务被一起重建。
3. 可选：协调器在 Redis 刚恢复时，把 epoch 未就绪当作"短暂等待"而不是立即报不可用，例如最多等待 2 秒。

## R11（P3）payload_guard 对大请求反复整体序列化

### 现状

`guard_kiro_request`（`payload_guard.rs:445`）在历史裁剪循环里每轮都完整序列化一次（`serialize_request`）。最大 50 MiB 的请求可能序列化很多次。0.0.194 已经把它挪出 tokio worker（`block_in_place`），但 CPU 开销本身没变。

### 方案

1. 增量估算：每个 history 条目预先算好序列化后的字节数和权重，裁剪时直接减去，只在最终输出前序列化一次。
2. 需要保证估算与实际序列化结果一致。加属性测试：随机请求下，估算权重和真实权重之差为 0。
3. 收益取决于线上大请求的比例。建议先在 `PayloadGuardReport.guard_serializations` 上做统计，确认确实有多次序列化的大请求后再做。

## R12（P3）选择窗口计数仍用实例本地时钟

`record_scheduler_selection` 用本地时钟写 ZSET 分数，`SCHEDULER_STATE_BATCH_SCRIPT` 用调用方的本地时间做 `ZCOUNT`（代码里已注释为已知残留，`redis_cache.rs:630`）。时钟漂移只会影响 10s/60s/5m 选择计数的精度，影响均衡打分，不影响正确性。改法：写入和计数都用 Redis TIME。

## R13（P3）批量刷新额度在默认参数下大面积被本地限流

实测：每批 50 个账号刷新时，173/220 失败，原因是 `stage=coordination kind=rate_limited`，也就是本地刷新令牌桶（默认 60 RPM / burst 8，`DEFAULT_TOKEN_REFRESH_MAX_RPM`、`DEFAULT_TOKEN_REFRESH_BURST`）限流了。改成每批 10 个、批间隔 1.5 秒后全部成功。

方案：批量刷新接口按令牌桶的剩余配额自动节流，或者后台改为异步任务加进度条（前端已有导入进度的先例），不要让一次性大批量请求在本地被拒。

## R14（P3）上游指纹硬编码（需用户决策）

- UA 写死：`aws-sdk-rust/1.3.15 ... lang/rust/1.92.0 app/AmazonQ-For-CLI`（`src/kiro/endpoint/cli.rs:58-72`），以及 IDE 端点的 `aws-sdk-js/1.0.34 ... KiroIDE-...`。
- `amz-sdk-request: attempt=1; max=3` 在所有请求上都是固定值，重试时也不变（`cli.rs:154,172,200`、`ide.rs:114,132,155`）。
- 这是项目最大的外部风险：上游一旦调整校验或识别出这些特征，影响的是全部账号。代码层面只能缓解：
  1. `attempt=N` 随实际重试次数递增，与真实 SDK 行为一致。
  2. 版本号集中到一个配置结构里，支持热更新，跟进官方客户端的版本节奏。

## R15（P3）59139 实例仍是 0.0.189

0.0.194 修复的热路径阻塞、自愈惊群、Redis 集合增长等问题，59139 都存在。是否升级由用户决定。升级时同样只重启 app 容器（R10）。
