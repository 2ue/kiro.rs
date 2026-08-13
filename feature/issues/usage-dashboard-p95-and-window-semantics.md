# Usage Dashboard P95 And Window Semantics

Role: Usage Dashboard duration percentile and window-population correctness authority

Status: `implementation-complete / current-source-recheck-pass / runtime-reverification-pending`

Severity: P1

Last updated: 2026-07-21

Evidence: [Usage Dashboard P95 and window semantics evidence](../evidence/usage-dashboard-p95-window-semantics-20260716.md)

## 当前源码复核

当前源码已把用户可见的 `/api/admin/usage-dashboard` 与 `/api/admin/usage-dashboard/windows`
收敛到 PgSQL-only 权威：

- `UsageRecorder::dashboard()` / `dashboard_windows()` 在没有 `postgres_store` 时直接报错，
  不再回退 Redis max 口径。
- `PostgresUsageStore::dashboard_windows()` 使用 weighted nearest-rank P95，按累计权重选择
  95 分位，而不是 `MAX(duration_ms_max)`。
- `redis_cache.rs` 中 `duration_ms_max -> p95_duration_ms` 的转换仍保留在 Redis helper /
  series / test 路径，但不再是用户可见 dashboard 的权威。

下面保留 2026-07-16 旧构建的历史红证据，用于说明修复前的问题形态和验收目标。

## 问题与影响

以下内容保留为 2026-07-16 旧构建的历史红证据。两套 Admin UI 显示的 `P95 耗时`
曾经是所选窗口中的最大耗时，不是第 95 百分位。
PostgreSQL 对小时 rollup 取 `MAX(duration_ms_max)`，Redis 也只保存并合并
`duration_ms_max`，随后后端把它序列化为 `p95DurationMs`。一个极慢请求因此可以把整窗
“P95”抬到最大值，误导容量、上游质量和告警判断。

滚动窗口还有来源漂移：PostgreSQL 排除下边界所在的整个部分小时，Redis 却包含该整小时。
Redis 失效、慢、断开或 cleanup 后回退 PostgreSQL 时，同一个 API 的统计人口会改变。

影响面包括：`/api/admin/usage-dashboard`、`/api/admin/usage-dashboard/windows`、两套 UI
卡片与 60 秒 warning tone、Redis-first/PgSQL fallback、soft cleanup 后 Dashboard，以及所有
把 `p95DurationMs` 当成真实 percentile 的外部 Admin API 消费者。旧 `/usage-summary` 不含
该字段，不在直接影响面内。

## 用户可见现象与指纹

可见指纹：

- UI 标题 `P95 耗时` 或描述 `P95 <value>ms`；
- API 字段 `p95DurationMs`；
- 源码 `MAX(b.duration_ms_max) AS p95_duration_ms`；
- Redis `duration_ms_max -> p95_duration_ms`；
- `p95DurationMs >= 60000` 触发 warning。

无指纹变体更常见：HTTP 200、数值格式正常、没有 error ID/日志/异常 payload，只是统计含义
错误。小样本或尾部都等于最大值时，错误结果会碰巧等于 P95；当前三记录 Redis 测试因此未
暴露问题。Redis 与 PostgreSQL 样本边界不同也不会报错，只会在 fallback 前后静默跳数。

## 源码链与根因

```text
UsageRecord.duration_ms
  -> PgSQL hourly duration histogram + rollup duration_ms_max
  -> dashboard_windows MAX(duration_ms_max)
  -> UsageDashboardSummary.p95_duration_ms
  -> p95DurationMs
  -> 两套 UI 显示 P95 / 60s warning
```

```text
UsageRecord.duration_ms
  -> Redis hourly hash __USAGE_DURATION_MAX__
  -> multi-hour max merge
  -> duration_ms_max 映射到 p95_duration_ms
```

根因不是 percentile 公式写错，而是系统从未保存 Redis percentile 所需的分布，却把已有最大值
字段改名为 P95。PostgreSQL 已有可加减的精确 duration histogram，但 Dashboard 查询没有使用
权重累计。

窗口根因是两个存储使用不同边界规则：PgSQL 用 `bucket_start >= from`，Redis 用
`floor_to_hour(from)` 枚举。小时 rollup 本身无法判断部分小时中哪些记录在精确 `[from,to)` 内。

## 最小复现

在隔离存储中写入同一小时的 100 条 usage，duration 为 1..100 ms，然后读取 Dashboard：

```text
expected discrete weighted P95 = 95 ms
current PostgreSQL result       = 100 ms
current Redis result            = 100 ms
```

再写入 95 条 10 ms 和 5 条 1000 ms：

```text
expected P95 = 10 ms
current result = 1000 ms
```

窗口漂移复现：固定 `now` 在 `12:30`，`last24h.from` 为前一天 `12:30`。在前一天
`12:45` 写一条记录。PgSQL 因 bucket start 为 `12:00 < 12:30` 而排除；Redis 从 `12:00`
开始枚举而包含。再在 `12:15` 写一条窗外记录，Redis 仍包含它。

## 多轮、长窗口、异常与并发复现

- 1..100、95/5 重权、94/6 临界权重各 5 轮。
- 两个小时中放置相反权重，证明按请求权重而不是按小时 max/桶数计算。
- UTC、Asia/Shanghai、`UTC+05:30`、`UTC-03:30` 的六种窗口各 5 轮固定时钟测试。
- Redis hit/empty/timeout/disconnect、cleanup invalidation、PgSQL-only 各 3 轮，值和人口必须一致。
- 同 ID 从 1000 ms 更新为 10 ms，旧 duration 贡献必须扣除。
- soft cleanup 删除高尾后 P95 必须下降；hard cleanup 不得再次下降。
- 30 天 x 每小时 100/1000 个 distinct duration，100 次查询/轮，记录 query plan、p50/p95/p99、
  buffers、temp spill、CPU、连接、RSS 和并发 writer 吞吐。

## 候选方案与权衡

方案 A：把 API/UI 改名为最大耗时。改动最小、结果诚实，但不提供真实 P95。它是安全回滚和
无法按期完成 weighted P95 时的最低发布修复。

方案 B：在 Redis 每小时保存 exact duration histogram。可在 Redis-first 路径直接算，但会新增
高基数字段和每请求写命令，重新把 usage 统计压力带回 scheduler 共用 Redis；same-ID/cleanup
负增量也更复杂。本轮拒绝。

方案 C：用 PostgreSQL 正权 duration histogram 做 weighted nearest-rank P95，并使 Dashboard
窗口采用单一 PgSQL 权威。PgSQL 是服务启动必需依赖，已有可精确负增量的 histogram，适合
same-ID 和 cleanup。本轮选定。

方案 D：t-digest/HDR 等近似 sketch。固定空间，但精确删除和历史迁移复杂，且会改变 API
精度合同。本轮不引入。

## 选定修复方案

1. 定义离散 nearest-rank：`rank=ceil(0.95*N)`，返回累计正权首次达到 rank 的 duration。
2. 一条 PostgreSQL 查询同时计算六个 Dashboard window，禁止逐窗口全扫或按 requests 展开行。
3. 完整小时使用 `usage_duration_rollup_time_buckets`；精确半开区间的首尾部分小时只扫描
   `usage_records` 对应边界范围。
4. PgSQL 作为完整 window summary/P95 的同代权威，避免把更新更快的 Redis totals 与滞后的
   PgSQL P95 拼成混代响应。
5. 不新增 Redis duration histogram。Redis 中现有 max 可保留为内部 maximum，但不得再输出为
   P95。
6. full 与 split windows route 共用同一有界查询/缓存路径；失败时返回 unavailable/明确错误，
   不静默回退 Max。
7. 若精确边界的所有 Dashboard 指标不能在本次安全统一，发布前至少端到端改名 Maximum；不得
   保留 P95 名称和 Max 内容。

## Weighted SQL 与时间窗合同

先按 `(window_key,duration_ms)` 聚合正 `requests`，再按 duration 升序累计权重；选择
`cumulative*100 >= total*95` 的最小 duration。乘法前转 `numeric`，避免长生命周期 BIGINT
理论溢出。

精确 `[from,to)`：完整小时范围为 `[ceil_hour(from), floor_hour(to))`；非对齐的首尾最多两个
片段由 active detail 的 `created_at` partial index 读取。from/to 在同一小时则只读一次 detail，
不能重复首尾片段。boundary detail duration 必须和 histogram 一样 clamp 到 `i32::MAX`。

## 兼容性与性能风险

- 保留 `p95DurationMs` wire 名并改为真 P95 是语义纠错，但依赖“最大值”的外部消费者会看到
  数值下降；发布说明必须写明。
- 改名 Maximum 会涉及 Rust DTO、两个 TypeScript contract 和两套 UI，但语义最安全。
- duration 表 PK `(bucket_start,duration_ms)` 可先承担范围扫描；现有同键普通索引重复，不能在
  没有 EXPLAIN 证据时再叠加 covering index。
- `INCLUDE(requests)` 可能减少 heap fetch，却会让每次 requests 更新维护索引并抑制 HOT；必须
  用 writer 吞吐和 buffer 证据决策。
- split windows 当前没有 full route 同等的显式 timeout/cache；加 weighted query 前必须统一。
- 禁止 `generate_series(requests)`、六窗口串行查询、30 天 detail 全扫和 Redis 高基数 histogram。
- PostgreSQL 权威会增加 Dashboard 读负载，但 Admin 默认低频刷新；冻结候选仍必须达到查询
  p95 <=50 ms、p99 <=150 ms，且相对基线退化不超过 10%/15%。

## 验收矩阵

| Case | 期望 |
| --- | --- |
| 1..100 单权 | P95=95，不是100 |
| 10ms x95 + 1000ms x5 | P95=10 |
| 10ms x94 + 1000ms x6 | P95=1000 |
| 跨 hour 重权 | 按总请求权重，不按每小时 max |
| empty | 0 或明确 unavailable，不得负数/溢出 |
| same-ID 1000->10 | 旧 histogram 扣除，P95=10 |
| soft cleanup 删除 96..100 | 剩余1..95，P95=91 |
| 后续 hard cleanup | 仍为91，不双扣 |
| lower partial hour | 只含 `[from,to)` 内记录 |
| `UTC+05:30` calendar window | local midnight 边界精确 |
| Redis hit/fault/invalidation | 同一已提交数据值不漂移 |
| 30d high cardinality | 无展开、无全 detail 扫描、满足 p95/p99 budget |
| 两套 UI/API | 文案、字段、warning 均与真实 metric 一致 |

每类至少 3 个外轮；percentile/边界核心 fixture 5 轮。保存 `EXPLAIN (ANALYZE,BUFFERS,FORMAT
JSON)`、源码/二进制 SHA、数据规模、状态分布和清理结果。

## 修复后验证结果

当前只有静态红证据和 execute-ready 测试设计，尚未修改 Rust，也没有共享受控 build 的修复后
运行结果。不得把现有 Redis 三样本测试或 `duration_ms_max` cleanup 测试当作 P95 通过证据。

## 残余风险与回滚

- nearest-rank 与插值 percentile 不同；本项目选定离散 nearest-rank，不能在 UI/后端混用。
- 历史 histogram 只精确到持久化的毫秒/clamp 口径；不虚构未存储分布。
- Dashboard exact rolling window 若暂缓，必须显式改成 hourly-bucket 合同和文案，不能继续返回
  看似精确的 from/to。
- PgSQL 查询超预算时优先回滚到“Maximum 诚实命名”，不得回滚到“Max 冒充 P95”。
- 不允许以增加 shared Redis 高基数写入作为性能回滚。
