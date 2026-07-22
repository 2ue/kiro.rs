# v0.0.101、v0.0.102、v0.0.103 升级 Smoke

Status: `focused-pass / non-rolling-transition-gate-added / final-release-binary-rebind-pending`

Severity: P0 release gate

Date: 2026-07-16

## 结论

真实 tag binaries 创建的三套旧版本 PostgreSQL schema 已分别通过当时的当前候选升级。每个旧版本执行 3 个外层轮次，每轮包含普通升级、大数据升级、成功后二次启动、3 次 checksum failure、修复 marker 后 recovery、recovery 二次启动；每版另有一次 750ms advisory lock 等待。

正式结果为 81/81 phase 通过。所有 fixture 的 runtime config、credential/runtime state、usage、rollup、external pool 和 Redis 前缀数据均保留；失败路径没有留下 schema、marker 或业务数据变更；三条升级路径收敛到同一语义 schema、index 集和 marker 集。

2026-07-17 新增升级语义：缺少整个 `requestAdmission` 字段的旧 runtime config 不再解析为 unlimited，而采用 `300 RPM / 32 concurrent / 64 queued / 1000ms`；RPM 与并发显式为 0 时保持 disabled，queue 字段随无效组合规范化为 `0 / 0ms`，部分对象的未写字段取保守默认。此前 81/81 没有断言这一新合同，不能作为它的通过证据。最终旧版本 smoke 必须补 missing/explicit-zero/partial 三类数据、启动后持久化和 Admin save-refresh，并记录运维回滚为显式关闭 RPM 与并发。

本结论绑定到 dirty-tree release binary SHA-256 `992214b562c468a6761a6e97e1c141359010cf891b13a84af11e89f35054929f`。最终 tag binary 若不同，必须重跑发布绑定 smoke，不能把本状态直接改成 `released`。

后续 same-ID/maintenance 审计增加了一个此前 81/81 未覆盖的升级合同：旧 binary 不获取新的 per-request-ID advisory lock 或 service/maintenance lifecycle fence，因此旧/新 writer 不能滚动混跑。最终发布 smoke 必须增加“停止 admission、排空 usage writer、停止全部旧实例、再启动全部新实例”的编排，并在切换前后验证 accepted=finished、dropped=0。81/81 证明旧数据可升级，不证明 mixed-version online writer 安全。

## 为什么需要真实旧版本

仅对当前 schema 做 `CREATE TABLE IF NOT EXISTS` 单测不能证明：

- 旧版本真实 column ordinal、default、index 和 marker 组合能否升级；
- 旧 binary 写出的 credential/runtime revision 能否被当前代码读取；
- 大 usage/rollup/Redis 历史是否被 startup 扫描；
- migration 中途失败是否会留下半迁移；
- 同一数据库第二次启动是否重复修改业务数据；
- 多实例 advisory lock 是否等待后继续，而不是超时或并发迁移。

此前 `migration-service-101-verify.log` 为空，无法作为证据。本轮没有复用该空文件或从一次当前版本启动外推旧版本兼容性。

## 根因与本专题边界

升级 smoke 缺证据的根因是旧流程没有用真实旧 tag binary 生成 schema，也没有把 migration failure 前后状态、二次启动和锁等待做机器 fingerprint；空日志或当前 schema 的 `IF NOT EXISTS` 单测无法证明升级。执行矩阵时另发现产品根因：默认 startup migration 事务边界只覆盖单个 versioned migration，链尾 checksum mismatch 会留下前置 DDL；通用 dependency retry 又把确定性错误重放约 17 次。两项修复由独立的 [PostgreSQL 启动迁移全链原子性](postgres-startup-migration-atomicity.md) 专题负责。

## 构建身份

| 候选 | source commit | binary SHA-256 |
| --- | --- | --- |
| v0.0.101 | `737f9f14cb831b8c4978536a850a63c8b1103195` | `7d35fcb8b56e8beffbce6cb3e0b0c6302cf092fdabd92881dbd89ef62ecd429e` |
| v0.0.102 | `e9479df71ee0044cfa0da8acbf69d98c2259a66f` | `3b380e7da5688aff80a5e13e48c3a37b8ba9185218fcd19683ce23dd762ca7f6` |
| v0.0.103 | `ec44f5f80fc76d49d773c0e6c82ef8d14abff28d` | `63be31b0d47303c25b9345ffd97b68a1552f7af0c715e4596b3c6280b122ee9b` |
| current candidate | base `401473ca1649997bdeccf4468e3add1bdb187248` + dirty snapshot | `992214b562c468a6761a6e97e1c141359010cf891b13a84af11e89f35054929f` |

当前 snapshot 的 tracked diff SHA-256 为 `289355042235405e9c07e3dab73d7e6438aead5ef79f807f4d65a2af515d71d3`；5 个 untracked 编译源文件 manifest SHA-256 为 `4c7c387493390fc68f2cdd4feb2bc1bd4f9fd0115629ae175e8ad2116f77a55a`。两套 UI 从 snapshot 源码构建后共 54 个 asset，manifest SHA-256 为 `8662b7b820d3b6e2300c90063f668598fb1982a88ac6069d7574e54ba8dd9c60`。

旧 tag binaries 的 Rust 后端来自对应 tag，但 build 时嵌入的是测试时可用的 UI 资产；因此本专题只证明后端数据库/Redis 升级，不证明旧 tag 的历史 UI 发布制品。

## 隔离与安全合同

- PostgreSQL 使用临时 `postgres:18-alpine` 容器，Redis 使用临时 `redis:7-alpine` 容器。
- 每个版本使用唯一容器名、随机数据库/Redis 宿主端口和独立 Redis key prefix。
- 服务只监听 `127.0.0.1:19131/19132/19133`；没有访问 `9022`、`19422` 或生产服务。
- 凭据、external pool URL/API key 都是不可用 fixture 值，不会发起真实模型请求。
- 每个版本结束即删除自己的 PostgreSQL/Redis 容器；最终三个服务端口监听数为 0。
- 没有读取、复制或写入 `kiro_idc_users*.txt`。

## 可执行复现

权威脚本：`feature/tests/postgres-upgrade-v101-v103.sh`。

```bash
SMOKE_ROOT=/tmp/kiro-upgrade-smoke-20260716-a \
CURRENT_BINARY=/tmp/kiro-upgrade-smoke-20260716-a/bin/kiro-rs-current-fixed \
ROUNDS=3 \
VERSIONS='v101 v102 v103' \
RESULT_ROOT="$PWD/target/validation/f04-upgrade-20260716" \
feature/tests/postgres-upgrade-v101-v103.sh
```

运行前脚本验证所需 binaries、测试端口和容器 readiness；运行中 fail-fast；退出 trap 只删除本轮唯一命名的容器和进程。raw artifact 包含日志、schema/business snapshot、columns/indexes/markers manifest 和结果 TSV。

## Fixture 数据集

| 数据 | normal/failure | large |
| --- | ---: | ---: |
| `usage_records` | 25 | 50,000 |
| `usage_rollup_totals` | 2 | 5,000 |
| `usage_rollup_time_buckets` | 1 | 1,000 |
| external pool | 1 | 1 |
| runtime config sentinel/version | 1 / 17 | 1 / 17 |
| credential + runtime state | 1 + 1 | 1 + 1 |
| Redis fixture keys | 6 | 40,000 |

大 Redis 数据分布在 `usage:summary:cache_read`、`usage:records:item`、`sticky` 和 `credential:inflight` 四个 family，各 10,000 key。服务配置关闭 startup rollup compression，验证默认 startup 不会扫描或压缩历史数据。

## 每版本每轮步骤

### Normal

1. 清空独立 storage。
2. 用真实旧 tag binary 启动并等待 `/readyz`，从而创建该版本真实 schema/marker。
3. 停止旧进程，写入 normal fixture 和 runtime/config sentinel。
4. 当前 release 启动、readiness、停止并断言所有数据。
5. 当前 release 第二次启动，比较语义 schema、marker、runtime config、credential、runtime state、usage、rollup 和 pool fingerprint。
6. 每版第 1 轮在当前 release 启动前由独立 session 持有 migration advisory lock 750ms，确认锁已经 held 后再启动。

### Large

步骤与 normal 相同，但使用 50,000 usage、6,000 rollup/bucket rows 和 40,000 Redis keys。它验证 startup latency 不随历史规模出现数量级增长，不等价于生产千万行 benchmark。

### Failure And Recovery

1. 由旧 tag binary 建 schema 并写 normal fixture。
2. 插入或覆盖 `credential-storage-revision-v1` 为 `fixture-corrupt-checksum`。
3. 保存包括 schema、marker applied time 和业务数据在内的完整 snapshot。
4. 连续启动当前 release 三次；每次必须非零退出、最终日志含 checksum 根因，且失败前后 snapshot byte-identical。
5. 删除错误 marker，启动当前 release recovery。
6. 再次启动，验证 recovery 后幂等。

## 机器断言

- 最终 schema 为 24 tables、307 columns、55 indexes、6 markers。
- 三条升级路径的语义 schema hash 都是 `376ca14a5d995957552574ac30fa59b9`。
- 三条升级路径的 marker semantic hash 都是 `7f86721d704fb1e1ee2f6d7f84b29163`。
- normal/large 的 usage、rollup、bucket、pool 和 Redis key 数量不得减少。
- runtime config version 必须仍为 17。
- credential/runtime revision/generation 列必须存在、值非负，并在第二次启动保持不变。
- 每次 failure 必须只出现一次 checksum mismatch；失败 snapshot 不得变化。
- recovery 后 marker 恢复为 6，第二次启动不得改变语义 fingerprint。
- 750ms advisory lock 已被确认 held；启动必须等待至少 700ms 后成功。

## 正式结果

每个版本每个外层轮次有 9 个 phase：normal upgrade/repeat 2 个、large upgrade/repeat 2 个、failure 3 个、recovery/recovery-repeat 2 个。3 版本 x 3 轮 x 9 phase = 81。

| 版本 | phase | pass | ordinary normal upgrade，round 2/3 | large upgrade，3 rounds | failure，9 attempts | lock-held startup |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| v101 | 27 | 27 | 389.741-511.243ms | 382.987-617.596ms | 213.365-293.120ms | 1111.173ms |
| v102 | 27 | 27 | 495.722-508.419ms | 381.311-509.922ms | 212.659-291.661ms | 1129.159ms |
| v103 | 27 | 27 | 375.241-501.520ms | 381.156-506.461ms | 224.788-285.567ms | 1105.828ms |

聚合结果：

- ordinary normal upgrade 6 次：平均 463.648ms，范围 375.241-511.243ms。
- large upgrade 9 次：平均 474.504ms，范围 381.156-617.596ms。
- checksum failure 27 次：范围 212.659-293.120ms；27/27 只有 1 次 mismatch，完整错误链可见。
- 27 个成功 fixture 都完成第二次启动；语义 schema 和业务 fingerprint 全部不变。
- 27 个成功 fixture 的 `inline-schema.applied_at` 在 checksum 相同时全部保持不变；81 行 `metadata_churn` 全为 0。
- v4 正式运行总墙钟时间 118.5s；结果文件 81 行且 `non_pass=0`。

large 与 ordinary normal 的范围重叠，说明本 fixture 下 startup 没有扫描 50,000 usage/大 Redis keyspace。这个结论不能外推为任意生产数据量零成本；生产仍需监控 catalog lock 和 startup duration。

## Revision 与 schema 差异解释

最终值：

| 升级来源 | credential revision | runtime revision | runtime generation |
| --- | ---: | ---: | ---: |
| v101 | 1 | 1 | 0 |
| v102 | 2 | 0 | 0 |
| v103 | 2 | 0 | 0 |

v101 缺少部分 revision/generation schema，当前 migration 负责增加并回填；v102/v103 已有 migration marker，fixture runtime row 是 marker 应用后新建的合法初始 row，因此 revision 可以为 0。不能把所有旧版本硬编码成相同数值。专门的缺列集成测试仍断言 legacy runtime revision 回填为 1、generation 初始为 0。

最初按 `ordinal_position` 排序的 schema fingerprint 发现 v101 与 v102/v103 不同。导出 manifest 后确认：

- 忽略 ordinal 的 column 集合差异：0。
- index 差异：0。
- marker 差异：0。
- 只有四个物理顺序差异：`credentials.revision/deleted_at`、`credential_runtime_state.generation/updated_at`。

SQL 查询和 API 不应依赖 `SELECT *` 的物理顺序；脚本主 hash 已改为按 table/column name 排序，物理顺序保留在单独 manifest 中。

## 测试中发现并修复的问题

1. 修复前默认 migration 非全链原子，checksum failure 会把 21/267/46 推进到 24/307/55 后才失败。已改为单一外层 transaction。
2. 确定性 checksum error 被通用 dependency retry 重放约 17 次/64 秒，且根因被外层日志隐藏。已改为 SQLx/SQLSTATE classifier 和完整 error chain。
3. 官方 PostgreSQL image 的临时 bootstrap server 会短暂 `pg_isready`。脚本改为等待 `PostgreSQL init process complete` 后再执行 `SELECT 1`。
4. 初始脚本错误要求所有 runtime generation/revision 都为 1。对照 tag 源码后改为版本语义正确的非负/幂等断言，并单独记录实际值。
5. 初始 schema hash 把物理 ordinal 当成语义。已补 columns/indexes/markers manifest 并改为集合 hash。
6. 成功启动会无条件刷新 `inline-schema.applied_at`。已改为 checksum 相同不写、真实变化才更新，并将脚本升级为 churn 非零立即失败。

前两项是产品缺陷；后三项是测试 fixture/证据模型缺陷。失败的 fixture 轮不计入 81/81。

## 发布计划与验收

- [x] 获取并校验三个旧 tag 的 source commit 和 release binary hash。
- [x] 使用真实旧 binary 创建旧 schema，而不是手写假 schema。
- [x] normal/large/failure 每版三轮。
- [x] 二次启动、失败三连、恢复、advisory lock 等待。
- [x] schema/data/Redis 机器 fingerprint 与 manifest。
- [x] 修复测试暴露的 migration atomicity 和 retry amplification。
- [x] 最终 `cargo check --tests`、migration 14/14、atomicity 3 轮、inline marker 3 轮、lifecycle 4/4。
- [ ] 最终 release/tag binary 重建后记录 SHA-256；若与当前候选不同，重跑本脚本或至少做脚本定义的三版本发布绑定 smoke。
- [ ] 增加 non-rolling 多实例切换 gate：旧实例 drain/stop 全完成后才允许新实例写；禁止旧/新 usage writer 重叠。
- [ ] 验证 `compressUsageRollupsOnStart=true` fail-fast，离线 maintenance lifecycle fence 的 online 拒绝、离线成功和释放后恢复。
- [ ] 发布后只读检查 migration duration、checksum mismatch、startup retry 分类和 PostgreSQL lock。

## 残余风险

- 旧 binary 的 UI 资产不是历史 release artifact，本专题不提供 UI 兼容结论。
- 50,000 usage 和 40,000 Redis keys 是有界回归，不是最大生产容量证明。
- `inline-schema` 相同 checksum 的重复启动已零写；真实 checksum 变化仍会更新 marker，这是预期 migration 观测语义。
- 显式 rollup compression 不在默认 transaction 内；它有自己的 transaction 和 maintenance 合同。
- 旧 tag binary 不认识当前 per-ID/lifecycle advisory locks；本次升级必须全停全起，不能从单实例 schema smoke 推导为可滚动升级。
- 最终 release binary 尚未与本证据 hash 绑定，因此状态不是 `released`。

原子性根因和设计见 [PostgreSQL 启动迁移全链原子性](postgres-startup-migration-atomicity.md)，逐命令和 raw artifact hash 见 [升级证据](../evidence/upgrade-v101-v102-v103-20260716.md)。
