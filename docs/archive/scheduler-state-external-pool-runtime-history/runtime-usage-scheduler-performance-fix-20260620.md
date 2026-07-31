# Runtime Usage, Scheduler, Redis/PgSQL Performance Fix - 2026-06-20

## 背景

本次处理覆盖三类已经在现网和本地代码中确认的问题：

1. `kiro.rs` usage 页面展示的模拟 cache 数字，与下游 `sub2api` 看到的 Claude 标准 `usage` 字段不一致。
2. 运行一段时间后出现内存/Redis 压力，服务变慢甚至卡死。
3. 调度与 usage 写入路径存在同步等待 Redis/PgSQL 的热路径，突发并发、凭据异常、队列堆积时会放大阻塞。
4. 0.0.55 现网回滚监控后确认 Redis 不是主故障点，日志压力集中在 `payload_guard` 和 `Invalid tool use format`，说明大历史 payload 修复链路仍有 CPU 放大。

本次只改 `kiro.rs`，不改 `sub2api`。

## usage 下游口径

下游 `sub2api` 按 Claude 标准字段计费：

- `input_tokens`
- `output_tokens`
- `cache_creation_input_tokens`
- `cache_read_input_tokens`

因此 `kiro.rs` 对下游返回的 SSE/HTTP usage 必须与本系统最终记录/页面展示的模拟 cache 口径一致。不能让页面记录一套数字、返回给下游另一套数字。

改造后：

- 本地凭据成功请求的 top-level usage 记录使用最终 canonical reported usage。
- 非流式响应体 usage 使用同一套 canonical reported usage。
- 流式最终 `message_delta.usage` 使用同一套 canonical reported usage。
- 原始上游/估算 usage 通过 `rawUsage` 保留在 `UsageRecord` 中，便于后续排查。

验证点：

- `cargo test reported_usage`
- `cargo test test_stream_usage_is_sub2api_compatible`

## Redis 内存压力

现网观察到 Redis key 数量异常放大，主要来自：

- `usage:records:item:*`
- `usage:summary:seen:*`

根因：

- usage record index trim 只裁剪 zset，没有同步删除对应 item JSON key。
- seen key TTL 过长，长期保留大量去重 key。

改造后：

- usage record snapshot 写入改为 Lua 原子流程：
  - `SETEX usage:records:item:<member>`
  - `ZADD usage:records:index`
  - 清理过期 index
  - 清理超量 index
  - 同步删除被裁剪 member 对应的 item key
- `USAGE_SUMMARY_SEEN_TTL_SECS` 从 35 天降到 1 小时。

验证点：

- `KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:26379 cargo test storage::redis_cache::tests -- --nocapture`
- 新增/覆盖测试：`redis_usage_record_snapshot_trims_orphan_items_with_index`

## 调度热路径阻塞

原问题不是简单的“200 个凭据很多”，而是请求热路径存在多个同步存储操作。典型阻塞点：

- Redis 并发 lease 获取/释放/touch/kind 更新。
- Redis 调度队列占位/释放。
- Redis 会话绑定读写。
- Redis 调度状态同步。
- 凭据失败、禁用、刷新失败时同步写 PgSQL。
- Token 刷新成功后同步回写凭据。

这些操作在正常情况下不明显；一旦 Redis/PgSQL 抖动、突发高并发、凭据集中失败、请求排队，就会把一次请求放大成多次同步等待。

改造后：

- `block_on_storage` 增加 100ms 慢日志，能看到具体同步操作和耗时。
- Redis 调度热路径加 75ms 短超时和 2s 退避。
- Redis 健康时仍优先使用 Redis 跨实例调度能力。
- Redis 慢/失败时，本进程降级为本地内存调度，不让请求被 Redis 拖死。
- Redis 调度全量同步节流从 250ms 放宽到 1s。
- Redis 并发 lease 获取改为单条 Lua：生成 lease id + 校验容量 + 占位，减少一次 Redis 往返。
- `InFlightLeaseGuard::drop` 只同步释放本地计数并唤醒等待者；Redis release 后台执行。
- lease touch/kind 更新后台写 Redis。
- 队列占位先维护本地队列计数；Redis 队列只做快速跨实例校验，释放后台执行。
- Admin summary/global capacity 改用本地容量计数，避免高频轮询卡 Redis。
- 会话粘性绑定使用本机内存作为当前进程调度依据；Redis 绑定写入/删除/软失败计数后台同步。
- RPM rate limit 本地立即递增；Redis rate limit 后台同步。
- 自动失败/禁用/刷新失败路径只更新本地状态和 pending runtime snapshot，PgSQL 由后台 flush。
- Token 刷新成功后新 token 立即进内存，PgSQL 凭据 upsert 后台执行。

关键语义：

- 当前进程调度正确性以本地内存态为准。
- Redis 正常时仍参与跨实例并发/队列控制。
- Redis 变慢时不会持续阻塞请求，而是短暂本地降级。
- Redis 降级期间跨实例全局并发可能短时间只由各实例本地约束，但单实例不会被拖死。

验证点：

- `cargo test token_manager`
- 覆盖项包括：
  - 500 日抛 / 1000 rpm 模拟。
  - 多调度策略。
  - 备用/本地容量 fail-fast。
  - 全局容量限制。
  - 并发排队超时。
  - 所有凭据自动失败/禁用后的行为。
  - Redis 共享并发 lease。
  - Redis 会话绑定和冷却。

## PgSQL usage 写放大

原 `UsageRecorder` 已有异步 writer，但 writer 内部仍是一条 usage 一个事务：

- upsert 一条明细。
- SELECT 旧记录。
- 逐维度更新 rollup total。
- 逐维度更新 hour bucket。
- 更新 cache/duration bucket。
- 更新 credential cost summary。
- commit。

在 500-1000 rpm 或更高突发下，这会产生 PgSQL 热点行竞争和 commit 放大。

改造后：

- writer 从队列中按最多 64 条 drain 成小批量。
- `PostgresUsageStore::record_batch` 在一个事务内处理整批。
- 同一批中相同 usage id 只保留最后一条，保持幂等。
- 批量读取数据库旧记录，旧值做负向 rollup，新值做正向 rollup。
- rollup delta 先在内存按以下维度合并后再 upsert：
  - total dimension
  - hour bucket dimension
  - cache read total
  - cache read hour bucket
  - duration hour bucket
  - credential cost summary
- `record_batch` 增加 100ms 慢日志，包含输入数量、去重后数量、耗时。
- `block_on_usage_store` 增加 100ms 慢日志。

验证点：

- `cargo test recorder_`
- `cargo test`
- PgSQL rollup 相关测试继续通过：
  - `postgres_usage_rollup_writes_hour_buckets`
  - `postgres_rolls_up_external_pool_billing_for_large_samples_and_keeps_after_cleanup`
  - `postgres_persists_runtime_config_credentials_stats_usage_and_pricing`

## payload guard / tool-use 热路径

现网 0.0.55 回滚后持续监控显示：

- 进程没有明显崩溃/OOM，Redis 也没有 blocked clients / rejected connections / slowlog 异常。
- CPU 负载会波动拉升。
- 日志大头集中在 `payload_guard` 和 `Invalid tool use format`。
- 这说明剩余压力主要来自请求进入上游前的大历史修复、payload shaping、tool_use/tool_result 配对校验，而不是 Redis 单点异常。

已修复点：

- 本地 `/v1/messages` 和 `/cc/v1/messages` 不再对每个请求无条件执行 `breakdown_kiro_request`。
- 小且未被 payload guard 修改的成功请求只保留基础 guard report，不再额外遍历并 JSON 序列化大历史来生成 payload breakdown。
- 大包、被修改、仍超限、接近阈值的请求仍会生成 payload breakdown，用于日志和 usage 诊断。
- 外部池 guarded route 同样改成按需生成 `breakdown_anthropic_messages_request`。
- converter 的 `validate_tool_pairing` 合并历史扫描：一次顺序扫描同时收集历史 tool_use、历史 tool_result、最后 assistant 的当前可配对 tool_use、未配对历史 tool_use。
- 删除 converter 里额外的 `last_assistant_tool_use_ids_for_converter` 和 `unpaired_historical_tool_use_ids` 二次扫描。
- payload shaping 后不再重复执行完整 repair；保留初始 repair 和 trim 后 repair。原因是当前 shaping 只截断/压缩内容，不改变 tool_use/tool_result 拓扑。
- `align_history_to_user` 从多次 `remove(0)` 改为一次 `drain(0..n)`，避免长历史开头连续 assistant 时出现 Vec 搬移放大。

关键语义：

- tool_use/tool_result 配对规则不变。
- trim history 后仍会 repair，因为删除历史条目可能制造 orphan。
- payload shaping 后不会 repair，因为它只改内容大小和文本，不删除配对节点。
- 小请求不再为诊断字段付出大历史 breakdown 成本。

验证点：

- `cargo test payload_guard`
- `cargo test validate_tool_pairing`
- `cargo test`

## 本次验证结果

已执行并通过：

```bash
cargo fmt --check
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo check
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test reported_usage
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test test_stream_usage_is_sub2api_compatible
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test recorder_
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test token_manager
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test payload_guard
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test validate_tool_pairing
KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:26379 CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test storage::redis_cache::tests -- --nocapture
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test
```

结果：

- `reported_usage`: 23 passed
- `test_stream_usage_is_sub2api_compatible`: 1 passed
- `recorder_`: 5 passed
- `token_manager`: 107 passed
- `payload_guard`: 30 passed
- `validate_tool_pairing`: 9 passed
- Redis storage tests: 11 passed
- full test suite: 625 passed

## 上线后观察项

重点观察日志：

- `同步存储操作耗时较长`
- `同步 usage 存储操作耗时较长`
- `PgSQL usage 批量写入耗时较长`
- `Redis 调度热路径不可用，本进程暂时降级为本地调度`
- `Kiro payload guard timing`
- `Kiro payload byte breakdown skipped for small unmodified request`

重点观察指标：

- Redis key 数：`usage:records:item:*` 是否继续无界增长。
- Redis memory：是否随 usage 记录持续线性膨胀。
- PgSQL 慢 SQL：`usage_rollup_*`、`usage_records`、`COMMIT` 是否下降。
- payload guard timing：大历史请求的 `repair_elapsed_ms`、`serialize_elapsed_ms` 是否下降。
- `Invalid tool use format`：频率是否下降；如果仍高，说明进入系统的历史仍在持续产生畸形 tool 链，需要继续前移归一化。
- 请求首字时间：突发并发下是否还出现 10-20s admin/调度卡顿。
- 调度日志：并发上限应看 effective credential concurrency，不再只看全局默认值。

## 测试建议

本地压测建议覆盖：

1. 500 个日抛凭据，1000 rpm 模拟请求，确认调度分布和错误处理。
2. 单凭据并发 override 大于全局默认，确认 effective concurrency 生效。
3. 全局容量限制开启/关闭。
4. 本地备用池开启/关闭。
5. 多备用池可用、部分不可用、全部不可用。
6. 突发高并发下 Redis 暂停或变慢，确认请求不会长时间卡在调度。
7. 突发凭据不可用，确认本地状态立即禁用/冷却，PgSQL 后台落库。
8. usage 高速写入，确认 PgSQL rollup 和 usage dashboard 数字一致。
9. 下游 `sub2api` 读取 SSE/HTTP usage，确认四个 Claude 标准字段等于 `kiro.rs` usage 页面最终口径。
