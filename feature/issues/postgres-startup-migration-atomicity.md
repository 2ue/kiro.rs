# PostgreSQL 启动迁移全链原子性与确定性失败放大

Status: `fixed / focused-and-upgrade-matrix-pass / final-release-binary-rebind-pending`

Severity: P0 release gate

Date: 2026-07-16

## 范围与结论

本专题包含两个互相放大的缺陷：

1. 默认 startup migration 只有单个 versioned migration 各自的事务，没有覆盖 `SCHEMA_SQL`、hash repair、五个 versioned migration 和 `inline-schema` marker 的全链事务。链尾 checksum mismatch 时，链头 DDL 已提交，数据库会停在半迁移状态。
2. `main` 把 `PostgresStore::connect` 整体放进统一 60 秒依赖重试，因而也重试 checksum、认证、配置和 SQL invariant 等确定性错误。真实服务 fixture 中，同一个 checksum mismatch 被执行 17 次，约 64 秒后才被泛化成“PgSQL 在 60 秒内未就绪”。

当前工作树已修复这两条链。默认启动迁移全链处于同一个 PostgreSQL transaction；确定性 PostgreSQL 错误 fail-fast 并保留完整 error chain，只有明确瞬态 SQLx/SQLSTATE 错误继续等待和重试。

## 用户可见现象与影响

- 旧版本升级失败后，表、列和索引已经增加，但 migration marker 或后续数据修复没有完成；下一次启动面对的不是原始旧 schema，而是不可审计的半迁移 schema。
- 日志持续打印大量 `relation/column already exists, skipping`，但最终只显示“PgSQL 在 60 秒内未就绪”，管理员看不到 checksum 根因。
- 同一确定性错误在每个实例启动时反复执行完整 DDL 检查，放大 PostgreSQL CPU、catalog/lock 压力和日志写入；多实例同时重启时影响更大。
- 错误恢复时间至少被人为增加约 60 秒，编排系统可能继续重启进程并形成周期性负载。
- 失败发生在服务监听前，`/readyz` 不会成功，不能通过请求级 error ID 定位。

## 修复前根因链

```text
main::retry_startup_dependency(60s)
  -> PostgresStore::connect
     -> migrate_with_options
        -> session advisory lock
        -> SCHEMA_SQL 逐条直接在 connection 上提交
        -> startup-safe index / credential hash repair 各自提交
        -> versioned migration 各开一个 transaction
        -> 链中 checksum mismatch
           -> 只回滚当前 versioned transaction
           -> 之前 schema/data 已永久提交
  -> anyhow error 被统一当作“依赖暂不可用”
  -> 500ms/1s/2s/4s... 重试整条迁移
  -> 60s 后只打印外层错误
```

代码层面的关键错误不是 PostgreSQL DDL 不支持事务，而是事务边界放错了：`run_versioned_migration` 本身有 transaction，但调用它之前的 `SCHEMA_SQL` 已经提交；多个小事务无法提供整条 startup chain 的 all-or-nothing 合同。

## 修复前最小复现

新增集成测试 `postgres_startup_migration_checksum_failure_rolls_back_entire_default_chain` 使用真实 PostgreSQL：

1. 创建完整当前 schema，写入 runtime config 和 credential sentinel。
2. 删除 `credential_stats_delta_batches`、`credential_runtime_mutations`。
3. 删除 `credentials.revision`、`credential_runtime_state.revision` 和 `credential_runtime_state.generation`。
4. 删除四个后续 migration marker。
5. 将 `credential-storage-revision-v1` checksum 改成 `fixture-corrupt-checksum`。
6. 连续执行五次默认 migration，每次比较 tables、columns、indexes、markers、runtime config 和 credentials 的完整快照。

修复前红测：

```text
before: 21 tables / 267 columns / 46 indexes
after failed attempt 1: 24 tables / 307 columns / 55 indexes
FAILED: failed startup migration attempt 1 left a partial schema, marker, or business-data mutation
```

真实 `v0.0.101 -> current` 服务链另行证明 retry 放大。旧 binary 建 schema并写 fixture，随后插入错误的 `credential-storage-revision-v1` marker。修复前连续三次进程启动分别为：

| 启动 | 退出耗时 | PostgreSQL notice | checksum mismatch 次数 | 最终错误 |
| --- | ---: | ---: | ---: | --- |
| 1 | 63.901s | 1980 | 17 | `PgSQL 在 60 秒内未就绪` |
| 2 | 64.112s | 1980 | 17 | `PgSQL 在 60 秒内未就绪` |
| 3 | 63.505s | 1980 | 17 | `PgSQL 在 60 秒内未就绪` |

每次失败后 schema/data fingerprint 虽因本轮原子性修复保持不变，但进程级重试仍制造了明显放大并隐藏根因，因此不能只修 transaction。

## 方案比较

| 方案 | 原子性 | 性能/锁 | 兼容性 | 结论 |
| --- | --- | --- | --- | --- |
| 保留每个 versioned migration 的小事务 | 仅局部 | 短事务 | 不改变现状 | 拒绝，不能回滚前置 `SCHEMA_SQL` |
| 对失败后已执行 DDL 做补偿 SQL | 依赖补偿完整性 | 复杂且可能再次失败 | 每次新增 DDL 都要维护逆操作 | 拒绝，无法可靠恢复数据 backfill/hash repair |
| 默认 startup chain 使用一个外层 transaction | 全链 all-or-nothing | transaction/DDL lock 持续到链尾 | PostgreSQL DDL 支持事务；改动集中 | 采用 |
| shadow schema/双写后切换 | 可做到更强隔离 | 实现和磁盘成本高 | 现有表/FK/运行时写入迁移复杂 | 当前规模不采用 |
| 所有 PostgreSQL 错误仍重试，但缩短为数秒 | 无分类 | 仍重复确定性错误 | 认证/配置错误仍被掩盖 | 拒绝 |
| 按错误文本识别 checksum | 只覆盖一个指纹 | 低成本但脆弱 | 新 invariant/本地化文本会漏判 | 拒绝 |
| 按 SQLx variant 与 SQLSTATE 分类 | 明确区分瞬态/确定性 | 确定性错误只执行一次 | 不依赖错误文案 | 采用 |

## 选定修复

`src/storage/postgres.rs` 的默认 migration transaction 现在包含：

1. `schema_migrations` 表创建。
2. `SCHEMA_SQL`。
3. startup-safe usage index 检查与小表索引创建。
4. active credential hash repair。
5. `credential-storage-revision-v1`。
6. `credential-runtime-revision-v1`。
7. `credential-runtime-generation-v1`。
8. `credential-runtime-mutation-cleanup-v1`。
9. `credential-stats-delta-batches-v1`。
10. `inline-schema` marker 写入。

任一步失败都会显式 rollback；若 rollback 本身失败，返回值同时保留原错误和 rollback 错误。session advisory lock `4950531234001` 仍覆盖整个流程并在 transaction 外持有，因此多实例串行语义不变。

显式 `compress_usage_rollups=true` 是例外：它可能扫描和重写大 rollup 表，不属于有界默认启动路径。默认 transaction 成功提交后，它在同一 session lock 下使用自己的 transaction。这样既不把大 maintenance 操作塞入启动长事务，也不改变显式 maintenance 的原子性。

`src/main.rs` 的 PostgreSQL startup classifier 只重试：

- SQLx `Io`、`Protocol`、`PoolTimedOut`、`PoolClosed`、`WorkerCrashed`；
- SQLSTATE connection class `08`、resource class `53`；
- `40001`、`40P01`、`55P03`、`57014`、`57P01`、`57P02`、`57P03`。

checksum mismatch、认证失败、配置错误、SQL 语法/约束等错误立即退出。最终日志用 `{:#}` 展开 anyhow chain。Redis 仍保留原来的统一 60 秒重试合同，本次没有借机改变 Redis 行为。

## 修复后验证

### 原子性与回归

- 原子性精确测试：初次绿测 1/1；最终又独立执行 3 轮，每轮内部连续五次 checksum mismatch，全部状态完全不变。
- `cargo +1.92.0 test migration -- --nocapture`：最终 14/14；新增 classifier 和 inline marker 幂等测试名都包含 `migration`，因此比修复早期的 12 项多 2 项。
- 修复早期完整 migration 聚焦回归另有 3 轮，每轮 12/12。
- `cargo +1.92.0 check --tests`：通过，只有现有 dead-code warnings。
- `lifecycle_tests`：4/4，包括 non-retryable 单次调用和 transient 第三次恢复。

### 真实服务失败路径

三版本正式矩阵共 27 次 checksum failure：

| 指标 | 修复前 v101 样本 | 修复后正式矩阵 |
| --- | ---: | ---: |
| 单次失败耗时 | 63.505-64.112s | 212.659-293.120ms |
| 每进程 mismatch | 17 | 1 |
| 每进程 DDL notice | 1980 | 110-118 |
| 根因是否出现在最终日志 | 否 | 是 |
| 失败后 schema/marker/business fingerprint | 修复前会半迁移 | 27/27 完全不变 |

错误 marker 删除后，27 个 fixture 都能正常 recovery，随后第二次启动的语义 schema 和业务 fingerprint 不变。

### 瞬态与确定性 classifier

- 固定宿主端口 PostgreSQL 延迟 1 秒启动：v4 服务在 2032.676ms 后 ready。SQLx 首次 connect 自身跨过短暂不可用窗口，因此该样本没有触发外层 warning；它证明新 classifier 没有破坏恢复。
- 错误 PostgreSQL 密码：v4 在 34.030ms 内 `rc=1`，日志同时包含“初始化失败且不可重试”和 `password authentication failed`。
- 单元测试另行证明外层 retryable operation 在第三次成功，non-retryable operation 严格只调用一次。

完整三版本结果见 [升级 smoke 专题](upgrade-v101-v102-v103-smoke.md) 和 [2026-07-16 证据](../evidence/upgrade-v101-v102-v103-20260716.md)。

## 性能、锁与兼容风险

- 外层 transaction 会让本次启动 DDL lock 持续到整条小型 migration 结束。大 usage 历史不在默认扫描路径；50,000 usage + 5,000 totals + 1,000 buckets 的真实 fixture 中，升级 readiness 为 381.156-617.596ms，没有随历史行数出现数量级增长。
- 三版各持有 advisory lock 约 750ms；进程均在 1105.828-1129.159ms ready，证明等待后成功而非误判失败。
- v101 与 v102/v103 的四个物理 column ordinal 不同，但忽略 `ordinal_position` 后列集合差异为 0，index 和 marker 差异也为 0。物理列顺序不作为 API/查询合同。
- `inline-schema` conflict update 现在带 `checksum IS DISTINCT FROM EXCLUDED.checksum` 条件。相同 checksum 的重复启动不写 marker；首次插入或真实 checksum 变化仍更新 checksum 和 `applied_at`。精确测试连续 3 轮覆盖“不变、stale 修复、修复后再不变”，v4 矩阵 81 行的 `metadata_churn` 全为 0。
- SQLSTATE allowlist 可能需要随新瞬态数据库故障补充。发布后应按错误 code 而非错误文本统计 fail-fast 与 retry 分布。
- 一个使用 Docker 随机宿主端口的 late-PG 初始测试在 stop/start 后端口发生变化，服务按旧端口等待 60 秒。该轮是 invalid fixture，已从通过证据排除并在 raw classifier 目录显式标记。

## 验收与发布计划

- [x] 建立能证明修复前半迁移的真实 PostgreSQL 红测。
- [x] 默认 startup migration 全链事务化，保留 session advisory lock。
- [x] 显式大 rollup compression 保持独立 transaction。
- [x] 确定性 PostgreSQL 初始化错误 fail-fast，完整错误链可见。
- [x] transient/non-transient classifier 单元与真实服务验证。
- [x] v101/v102/v103 普通、大数据、重复启动、锁等待、失败、恢复各三轮。
- [x] schema、marker、runtime config、credential/runtime revision、usage/rollup/pool、Redis keys 做机器断言。
- [x] `inline-schema.applied_at` 仅在 checksum 变化时更新；重复启动 timestamp 不变。
- [ ] 最终发布二进制若 SHA-256 不等于证据候选 `992214b5...4929f`，必须至少重跑本专题精确测试和三版本升级脚本；不得把 dirty-tree binary 的结果无条件外推到新 tag。
- [ ] 发版后执行一次只读 startup/migration 日志审计，确认没有 checksum retry 风暴或长事务告警。

## 回滚与残余风险

若外层 transaction 在特定生产 schema 上产生不可接受的 lock 时间，应先停止自动滚动升级并使用 maintenance 窗口重放同一 migration；不要回退为非原子小事务。代码回滚会重新引入半迁移风险，只能在同时提供等价原子 migration runner 时进行。

当前残余项：最终 tag 二进制绑定、显式大 compression 的独立事务合同、未知 SQLSTATE 分类补充，以及生产规模 catalog/lock 观测。`inline-schema.applied_at` churn 已修复，不再列为接受差异。上述残余不否定当前 P0 correctness 修复，但最终 binary 绑定必须在发布说明中明确。

## 2026-07-26 增量：114+ dashboard/usage 旧表缺列修复

现网 113 升 114 后出现过：

```text
Dashboard 总览加载失败
error returned from database
```

本轮复核确认一个独立迁移缺口：`CREATE TABLE IF NOT EXISTS` 中已经包含 114+ 新增的 usage/dashboard/外部池计费/耗时字段，但既有旧表不会走 create path；对应 `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` 不完整，导致旧实例升级后 writer 或 dashboard 查询在运行期碰到缺列。

当前工作树补齐：

- `usage_records`：补 endpoint/stream/model/status/token/cost/pricing/duration/data 等旧表缺列；
- `usage_rollup_totals` 与 `usage_rollup_time_buckets`：补 upstream/sticky/fallback、本地 prompt cache、external billing、duration aggregate、updated_at 等列；
- `usage_credential_cost_summary`：补 requests、estimated/priced/unpriced、updated_at 等列；
- `REQUIRED_POSTGRES_SCHEMA_COLUMNS` 增加 dashboard/usage 必需列，`migrate_on_start=false` 且 schema 不兼容时启动阶段明确失败，不让 dashboard 请求阶段才失败。

验证：

```text
postgres_startup_migration_repairs_usage_dashboard_upgrade_columns ... ok
postgres_persists_runtime_config_credentials_stats_usage_and_pricing ... ok
postgres_dashboard_read_transaction_is_bounded_and_read_only ... ok
```

生产发布后验证：

- 查看启动日志中是否有 schema compatibility fail-fast；
- 对 113/114 历史升级机器，访问 dashboard 不应再出现 `error returned from database`；
- usage writer 不应因 `duration_ms_sum`、`external_pool_reported_cost_usd`、`pricing_available` 等缺列失败。
