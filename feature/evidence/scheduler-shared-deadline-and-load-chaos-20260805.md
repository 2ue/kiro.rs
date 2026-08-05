# Scheduler Shared Deadline And Redis Chaos Validation 2026-08-05

## 结论

本轮最新失败不是可稳定复现的生产调度代码缺陷。失败发生在
`redis_usage_writer_and_scheduler_joint_fault_matrix_recovers_without_spin_or_false_disable`
的 `latency-75-round-1` 场景：Redis 下游响应注入 75ms 延迟，同时运行 4 个 usage writer，
调度热路径偶发超过 250ms 共享期限并返回“Redis 调度协调状态不可用”。

同一候选在随后完成的 3 轮完整 Redis chaos 矩阵中均通过，且 75ms 场景的调度耗时为
77–101ms，未打开调度 breaker，健康恢复和 usage writer 均正常。因此当前证据更符合本机
Toxiproxy/Redis/测试编译与进程调度的瞬时边界抖动，而不是调度路径固定进行了多次 Redis
往返或存在必现的 75ms 放大缺陷。没有为了“变绿”放宽 250ms 生产期限或改变 usage 与
调度隔离语义。

测试诊断已增强：若该边界再次失败，断言会同时打印实际耗时、Redis breaker 统计、usage
写入往返数和本地路由状态，避免只看到一个泛化容量错误。

## 现场与候选

- 日期：2026-08-05（Asia/Shanghai）
- 源码基线：`82a1c9922b8f7e79237436c526ff1dfe16684878`
- 外部池动态验证候选（生产代码包含半开放恢复修复）：
  `c957027d7bf85da9631111c8dd7e47d9daa6be2083ce42b7089900d92aaaa8fa`
- Redis：本地 Docker `kiro-rs-redis-local`，通过 loopback `127.0.0.1:26379`
- 使用数据库：Redis DB5、DB7、DB9，均在测试前为空；每轮结束为 0 keys
- Toxiproxy：测试 runner 自有 loopback proxy；没有访问或停止现有 `9022` 服务（PID
  `13048`）

## 复现与验证命令

单轮诊断矩阵：

```bash
KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL='redis://127.0.0.1:26379/9' \
KIRO_RS_TEST_REDIS_ISOLATED=1 \
KIRO_SCHEDULER_CHAOS_OUTER_ROUNDS=1 \
KIRO_SCHEDULER_CHAOS_SCOPE=scheduler-redis-diagnostics-20260805 \
node feature/tests/run-scheduler-redis-chaos-validation.mjs
```

完整 3 轮矩阵：

```bash
KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL='redis://127.0.0.1:26379/7' \
KIRO_RS_TEST_REDIS_ISOLATED=1 \
KIRO_SCHEDULER_CHAOS_OUTER_ROUNDS=3 \
KIRO_SCHEDULER_CHAOS_SCOPE=scheduler-redis-chaos-3round-20260805 \
node feature/tests/run-scheduler-redis-chaos-validation.mjs
```

## 结果

### 完整矩阵

- 3 outer rounds × 8 exact tests = `24/24` 通过。
- `redis_affinity_latency_does_not_degrade_capacity_coordination`：通过。
- `redis_capacity_latency_boundary_and_recovery_matrix`：50ms 请求均成功；500ms 请求均在
  约 251–272ms fail-closed；移除延迟后恢复。
- `redis_capacity_consecutive_timeouts_open_breaker_without_all_disabled`：通过，超时不会
  伪装成“所有账号均已禁用”。
- `redis_lease_release_is_non_blocking_under_latency_and_burst`：300 leases 释放不阻塞，
  无隐藏重试放大。
- `redis_capacity_disconnect_reconnect_recovers_same_manager`：通过。
- 联合 usage/scheduler 故障矩阵：低延迟 25/50/74/75/90/150ms 均通过；500ms、
  WRONGTYPE、断连场景按预期 fail-closed，恢复后 5 次探测全部成功。
- usage writer 每条记录保持单 Redis 往返；低延迟矩阵每轮 `16 attempted / 16 succeeded /
  16 round-trips`。
- 资源观测：联合测试 RSS 结束值相对开始值约增加 13MiB，FD 增加 4，且 runner 正常清理；
 该短测试结果没有持续增长趋势。
- 每轮 cleanup：自有 Redis DB 清空、子进程组停止、代理端口释放、临时目录删除。

### 外部池优先级故障波

同一候选的真实 loopback HTTP 外部池验证报告：

```json
{
  "result": "pass",
  "yuenan": { "hits": 28 },
  "kkkkyue": { "hits": 26 },
  "jinnyapi": { "hits": 2 },
  "localHits": 1
}
```

`localHits=1` 是服务启动阶段的本地模型发现辅助请求，不是外部直连失败后的本地
rescue；外部直连全部失败阶段没有新增本地请求。高优先级池失败冷却后能够切换健康池，
恢复后能够获得半开放探测流量，三池请求均携带 `anthropic-version`。

## 代码变更边界

- 生产调度代码的关键修复是外部池冷却结束后的半开放恢复候选：历史瞬态失败罚分不再
  永久阻止恢复池获得探测请求；探测成功清理失败计数，探测失败重新冷却。
- 本轮针对 Redis 75ms 边界只增强测试失败诊断，不改变生产期限、breaker 阈值、usage
  writer 往返数或调度/usage 故障域边界。
- 测试夹具此前显式使用无限制凭据容量，已修复为只在该 fixture 中设置
  `credential_max_concurrent_requests=0`，生产默认并发上限保持 30。

## 后续门禁

本证据关闭本地 Redis 单实例 chaos 边界和外部池优先级恢复 focused gate；最终发布仍需
重新构建冻结候选并完成全量 Rust/UI/Node 静态门禁、L1–L5 真实隔离验证、产物清理和发布
流程。不得把本次 3 轮本地 Redis 结果直接等同于生产多实例或真实上游验证。

## 最终候选 L3/L4/L5 动态验证补充

最终候选二进制：

- `kiro-rs` SHA-256：
  `881bca30a2dbef5f38c0b6e3ce8386a0cb7d0fecb93e9505d6256b2c805556f4`
- `kiro_loadtest` SHA-256：
  `be3a43e51b6e946c13e6d6698b42eeb7c538b44ef223ba32ca66c917f0f3cf48`

为排除旧 PostgreSQL 凭据/运行态污染，L3、L4、L5 均使用本轮新建的 caller-owned
数据库和独立 Redis DB/prefix。结果：

| 门禁 | 场景 | 结果 |
| --- | --- | --- |
| L3 | 正常 1/5/10/40 并发、错误波恢复、非法工具恢复 | `9/9` 通过；正常/恢复请求全部 200；错误波按预期返回错误并随后恢复 |
| L4 | 代理重启、429、500、协议错误、客户端断开、mixed-chaos | `12/12` 通过；重启中传输错误符合预期，重启/故障解除后恢复请求 `12/12` |
| 外部池优先级 | yuenan(1)/kkkkyue(10)/jinnyapi(20) | 通过；故障高优先级池不垄断，低优先级接管，恢复后半开放探测；直连失败无本地 rescue；三池均收到 `anthropic-version` |
| L5 | 180 秒长流、20 并发、60 秒空闲恢复 | `3/3` 通过；`1380/1380` 长流成功，恢复 `12/12`，FD 回到基线 `+5` 内，RSS 空闲采样稳定 |

L5 runner 增加了 12 请求的成功 warm-up 后再采集资源基线。此前直接在进程启动后采样，
会把首次连接池/序列化缓冲区分配误判成泄漏；生产代码未放宽资源阈值。warm-up 后最终
候选的 `rssReturnedWithin32MiB=true`、`idleRssSettled=true`、`fdReturnedWithin5=true`。

本轮原始报告保留在仓库外临时目录，摘要和 SHA 已记录；测试结束后应删除原始目录和
caller-owned 测试数据库，仅保留本证据中的脱敏结论。
