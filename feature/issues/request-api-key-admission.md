# Request API Key Admission

Status: `implemented-multi-instance-provisional-pass / non-Docker-runner-contract-pass / missing-field-upgrade-default-changed / final-release-gates-pending`

Severity: P0（下游突发与内部 attempt 放大之间的第一道边界）

Date: 2026-07-16

## 问题与影响面

修复前，已认证的请求 API Key 只承担鉴权，不是独立的 RPM、并发和排队实体。一个 key 的突发流量可直接进入 provider、external fallback 和多层 retry；即使下游 RPM 不高，单请求 attempt 放大仍可能把系统内部 RPM 放大到下游的 9x/30x。

本专题只解决“下游渠道 admission”这一层，不能替代 request-scoped shared attempt budget。provider、stream、external、OAuth 和 catalog 的 attempt/RPM 分账仍由 [重试预算专题](retry-budget-admission-and-rpm-amplification.md) 继续验收。

## 根因

修复前认证只返回“允许/拒绝”，没有稳定但不可逆的 key identity，也没有贯穿 response body 生命周期的 permit。并发/RPM若只放在 handler future 周围，会在 stream headers 返回后过早释放；fixed-window 又可能在分钟边界放大约 2 倍。多实例全局限制与生产 Redis degraded 之间存在冲突，因此当前第一层选定为进程内 O(1) hard limit，分布式 aggregate 另作有界可降级设计。

## 修复前复现

1. 配置两个合法请求 key，连续向 `/cc/v1/messages` 或 `/v1/messages` 发起并发长流。
2. 观察两个 key 没有独立并发/RPM 边界，所有请求都会进入 handler/provider。
3. 让返回 body 保持打开；handler future 已返回后，系统也没有下游 key 级 permit 可持有。
4. 使用大量合法 key 轮换请求；此前没有 admission 状态，因此也没有状态回收与内存上限设计。

无 hash、tool 或 transcript 指纹也可复现。本问题与 `bashHash...` 无关。

## 已实现方案

### 身份与隐私

- `RequestApiKeyStore::authenticate` 在鉴权成功后只生成 SHA-256 digest 身份。
- admission 分片表只以 32-byte digest 为 key；usage、查询和日志使用完整 64 位 hex
  SHA-256 `requestApiKeyId`，避免短摘要碰撞且不保存原始 key。
- limiter 状态、Debug 和拒绝响应都不保存或输出原始 API key。

### 生效路由

只作用于以下 inference 路由的精确 `/messages` MethodRouter：

- `/v1/messages`
- `/na/v1/messages`
- `/cc/v1/messages`
- `/ha/v1/messages`
- `/dfcache/{route}/v1/messages`

`/models`、`/files` 和 `/messages/count_tokens` 不持有长流并发 permit，也不消耗本 admission 的 RPM。`count_tokens` 是否需要独立的轻量 RPM 是后续 auxiliary-channel 设计，不应混入 inference permit。

### RPM

- 每 key 使用 O(1) token bucket，不使用 fixed-window 计数器。
- refill rate 为配置 RPM；短时 burst capacity 为 `min(rpm, 32)`。
- 这样消除了 fixed-window 在分钟边界瞬间放行约 2x 的问题，同时把 bucket 常驻状态固定为常数大小。
- 任意长窗口仍允许“平均速率 + 最多 32 个初始 burst token”，这是显式的短突发策略，不宣称严格 sliding-window 计数。

### 并发、排队与 permit 生命周期

- 每 key 独立计算 active/queued；默认并发 32、队列 64、等待 1000 ms。
- queue 满、queue timeout 或 RPM 不可用时在 handler 前返回，provider/external hit 为 0。
- permit 包裹 Axum `HttpBody::poll_frame`，完整转发 data、trailers、error 和 size hint。
- permit 在 response body EOF 或 body error 时释放；客户端 drop response/body 时由 Drop 释放。
- handler `next.run` 返回 response headers 不会提前释放长流 permit。
- 排队任务取消/abort 会同步撤销 queued 计数。

### 热更新语义

- Admin runtime update 成功持久化后立即更新本实例 controller。
- Redis runtime-config 通知和 60 秒 periodic reload 也更新各实例 controller；请求热路径本身不访问 Redis。
- 多实例更新是 eventual convergence，不是原子切换。Redis 通知断连期间，提交更新的实例先执行新值，
  其他实例可能继续执行旧值，直到 reconnect reload、后续事件或 60 秒 periodic reload。
- 增大并发会唤醒 waiter；把并发设为 0 会放行 waiter并禁用该限制。
- 降低 `maxQueuedRequests` 或 `queueTimeoutMs` 会唤醒并拒绝当前仍在等待的 waiter，不让它们继续沿用更宽松的旧队列配置。
- 已经持有 response body 的 active permit 不会因降低并发而被强制中断；新请求会等 active 下降到新上限以下。

### 状态有界性

- 16 个本地 shard，正常命中为期望 O(1)，锁竞争按 digest 首 byte 分散。
- 最多跟踪 4096 个 key；达到上限且没有可回收 idle state 时，新 key 规范返回 429。
- active/queued/被 future 或 body 持有的 state 不能被回收，避免回收后重建绕过并发限制。
- idle state TTL 为 10 分钟；每 64 次 admission 增量清理一个 shard，满表时才做全 shard 回收。
- 没有按请求扫描全表、没有按 RPM 分配时间戳数组、没有 Redis/数据库热路径。

## 配置与兼容性

`requestAdmission` 配置：

| 字段 | fresh install 默认 | `0` 语义 |
| --- | ---: | --- |
| `rpm` | 300 | 禁用 RPM |
| `maxConcurrentRequests` | 32 | 禁用并发限制 |
| `maxQueuedRequests` | 64 | 禁用排队，满并发立即 429 |
| `queueTimeoutMs` | 1000 | 禁用排队，满并发立即 429 |

2026-07-17 复核确认原向后兼容策略存在保护空洞：旧 PgSQL/JSON 配置缺少整个 `requestAdmission` 字段时会解析为四项全 0，因此升级部署即使遇到错误 burst 仍是 unlimited；这与 fresh 安装的安全默认和用户要求的内部 RPM 保护不一致。当前选定迁移改为：缺少整个字段时使用 `300 / 32 / 64 / 1000ms` 保守默认。部分对象只对显式字段取值，其余字段使用各自默认。RPM 与并发均为 0 时 admission disabled；queue 不是独立限制，并发为 0 时会规范化为 `0 / 0ms`。并发启用时，queue count 或 timeout 任一为 0 都规范化为二者全 0，表示满并发立即拒绝。Admin 超过硬上限的值仍拒绝，不会静默截断。

这是有意的升级行为变化，不再以“兼容”为由让旧部署无限准入。`config.example.json` 与两套 UI 已展示相同推荐值；需要临时回到旧行为的管理员应显式保存 RPM 与并发为 0，Admin 会把无效 queue 字段规范化为 0。发布前必须在 v101/v102/v103 真实旧数据集上重跑 missing-field、显式 zero、部分对象和 Admin save-refresh，证明只改变该配置语义，不丢其他 runtime 字段。

## 拒绝响应

所有 admission 拒绝均为 Anthropic 兼容 JSON：

- HTTP 429
- `error.type=rate_limit_error`
- `Retry-After`，最少 1 秒
- `request-id` 和 `anthropic-request-id`
- JSON body 的 `request_id` 与 header 一致
- 不包含 credential、scheduler、external pool、digest 或原始 key 信息

## 聚焦验证

命令：

```bash
cargo test request_admission -- --nocapture
cargo test authentication_is_outer_to_message_admission_for_five_rounds -- --nocapture
cargo test actual_anthropic_message_routes_reject_before_handler_for_five_rounds -- --nocapture
cargo check --tests
npm run build --prefix ui
npm run build --prefix admin-ui
```

当前结果：

| 场景 | 轮数 | 结果 |
| --- | ---: | --- |
| 单 key RPM 与连续拒绝 | 5 | 通过 |
| token refill / rollover | 5 | 通过 |
| 300 RPM 边界 burst cap=32 | 5 | 通过 |
| 多 key 并发隔离 | 5 | 通过 |
| queue 上限与 permit 释放唤醒 | 5 | 通过 |
| queue timeout | 5 | 通过 |
| waiter cancel/abort 清理 | 5 | 通过 |
| 热更新降低 queue/timeout | 5 | 通过 |
| response body EOF | 5 | 通过 |
| response body error | 5 | 通过 |
| 客户端 drop body | 5 | 通过 |
| 全 disabled 且不创建 key state | 5 | 通过 |
| key churn TTL/硬上限 | 5 | 通过 |
| 429 request id/Retry-After/零 handler hit | 5 | 通过 |
| models/count_tokens 不占 inference admission | 各 5 x 5 请求 | 通过 |
| 实际 Router layer：auth -> digest identity -> admission | 5 | 通过 |
| 完整 Anthropic Router 的 5 个 `/messages` 路由 | 每路径 5 | 首请求进入 handler，第二请求在 handler 前 429 |
| 双真实实例、共享隔离 PgSQL/Redis | 3 | 阶段通过；配置 4 RPM 时每实例 4、aggregate 8 |
| Redis 0/75/150ms/reset_peer admission 前拒绝 | 每格 3 x 64 | 768/768 为 429，全部 0 upstream |
| 同进程连续 rejection plateau | 3 x 5 x 64 | 960/960 为 429，FD 恒定，无线性 RSS 增长 |
| accepted + sampled rejection usage digest 归因 | 3 | 三个 key 均为完整 digest；四类拒绝原因齐全 |

聚焦结果为 14 个 admission/config 测试和 2 个实际 Router 测试通过。这里没有声称已经完成 L3/L5 或真实 Claude CLI 全门禁。

两套 UI production build 均通过；`config.example.json` 也已做 JSON parse 校验。页面 viewport/browser 截图仍属于最终 UI gate，未用 build 结果替代。

### Burst 测试稳定性复核

一次完整 `cargo test --all-targets` 曾暴露 `token_bucket_caps_boundary_bursts_for_five_rounds` 的测试时钟缺陷：该测试复用了 refill 用的 20 ms 人工“分钟”。在 300 RPM 下每约 66.7 微秒就会合法补充一个 token，因此全量并行调度期间发完初始 32 个请求后，第 33 个请求可能合法成功；这不是 production 60 秒 bucket 突破 burst cap。

修正只改变测试时钟：burst-cap 测试改用 production `RequestAdmissionController::new` 的 60 秒窗口，短 20 ms 窗口继续只用于 refill/rollover 测试。修正后 targeted 连续 2 轮通过；`cargo test --all-targets` 连续 2 轮均为 `1263 + 26` 通过、0 failed。

## 多实例与 Usage 动态证据

完整历史行为证据见 [双实例 admission evidence](../evidence/request-api-key-admission-multi-instance-20260716.md)。2026-07-21 追加 [non-Docker runner contract](../evidence/request-api-key-admission-nondocker-runner-contract-20260721.md)，把动态 runner 从 Docker PostgreSQL/Redis/Toxiproxy 改为 caller-owned PostgreSQL/Redis 和本地 `redis-chaos-proxy.mjs`。该合同只证明 runner 安全边界，不替代 frozen candidate 的动态服务运行。

当前已经确认：

- `requestApiKeyId` 已进入成功 UsageRecord，也进入有意采样的 admission rejection UsageRecord；
  PostgreSQL/Redis 查询和两套 UI 支持该字段。不能把“有采样拒绝记录”误写成“每一个 429 都有一条
  usage”：拒绝明细只记录前 8 次、2 的幂次和全局日志预算允许的样本。
- 两实例配置 `rpm=4` 时，同 key 的 aggregate accepted 是 8，不是 4；并发 1 时 aggregate active
  是 2，不是 1。当前语义严格是 `per-instance`。
- 被本地 admission 拒绝的请求在 Redis 0/75/150 ms 和 `reset_peer` 下都保持 0 upstream hit；
  注入的 Redis 延迟没有进入 rejection p95。
- Redis 断连期间 runtime config 会出现实例间短暂分歧，恢复后收敛；不能宣传为原子集群更新。
- 阶段测试发现共享 PostgreSQL usage rollup writer 的跨进程锁序死锁。虽然 writer retry 最终成功，
  但这是 release blocker；确定性排序修复已在真实 PostgreSQL 上完成 3 个外轮、每轮 64 对并发事务，
  合计 192 对/384 records 且 0 deadlock。最终五轮仍必须在统一冻结的新 service binary 上达到
  0 deadlock retry。

## 未完成、残余风险与回滚

- 配置/API/两套 UI 仍需把 scope 明确写成 `per-instance`，并提示理论 aggregate 约为实例数倍；
  当前没有 scope 字段，不能让运维人员误认为是 cluster-global hard limit。
- 不建议本版本引入同步 Redis exact-global admission。它会把每请求 Redis RTT 和 Redis
  fail-open/fail-closed 选择重新带入第一道保护。未来若需要集群配额，应单独设计带 TTL、epoch/fencing、
  crash/partition/scale 语义的异步 quota lease，或在 API gateway/LB 执行全局硬限制。
- 三轮双实例 debug 正确性和 plateau 已完成；non-Docker runner contract 5/5 与共享 runner batch 41/41 已通过；仍需在 PostgreSQL 锁序修复后的冻结 binary 上用 caller-owned PostgreSQL/Redis 完成动态五轮，并用 release binary 跑绝对延迟门槛。debug 共享机器的 178-193 ms spike 不可当成 release 性能结论。
- 仍需与 shared attempt budget 联合验证：admission 接受一个下游请求后，所有 provider/external/stream attempts 的总数必须受同一小常数预算约束。

回滚可将四个配置字段显式设为 0，恢复旧的 unlimited admission 行为；这只能用于兼容回退，不能作为错误期 RPM 放大的长期方案。任何全局配额实现若依赖 Redis，都必须证明 Redis 延迟/断连时不会产生新的同步 429 风暴或无界排队。

## 性能验收口径

正常命中：一次 SHA-256（鉴权已有）、一次 shard HashMap 查找、一次短 parking_lot 锁、token bucket 常数运算、并发 gate 短锁。长流只增加一个 `HttpBody::poll_frame` 委托，不复制 payload、不聚合 chunk、不改变 trailer/size hint。

发布前 L3/L5 若出现以下任一项则失败：RSS/FD 在 idle 后不回落；key churn 超过 4096 常驻 state；drop/error 后 active 不释放；429 请求进入 provider；p95/p99 相对无 admission 基线出现无法由排队配置解释的退化。
