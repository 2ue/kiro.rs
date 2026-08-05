# 外部池高可用调度修复与真实 HTTP 验证（2026-08-05）

Status: `verified-local / release-candidate-passed / production-rollout-pending`

Scope: 修复外部池管理变更后的本进程权威池快照被自身 Redis 事件清空，导致
高优先级池和备用池从候选集合消失、最后创建的池独占流量的问题；同时复核
普通错误的软失败、优先级接管、外部直连边界、恢复和资源稳定性。

## 根因

Admin 创建/更新外部池时，服务先把变更合并到本地权威池快照，再异步发布
Redis 跨实例失效事件。当前进程也订阅同一频道，但事件没有发布者标识，因此
本进程把自己发出的事件当作远端变更，调用快照失效逻辑并清空刚合并的快照。
连续创建三个池时，快照演变为：

```text
创建 yuenan  -> [yuenan]
自身事件失效 -> []
创建 kkkkyue -> [kkkkyue]
自身事件失效 -> []
创建 jinnyapi -> [jinnyapi]
```

随后排序算法收到的候选确实只有 `jinnyapi`；页面状态从 PostgreSQL 查询仍显示
三个池可用，所以表面上像“优先级选择错误”，实际是快照代际/自发事件竞态。

## 修复

- `ExternalPoolManager` 为每个进程生成不含敏感信息的实例标识。
- 外部池变更 Redis 事件携带发布者标识；兼容没有标识的旧事件。
- 当前进程观察到自己的事件时只更新已观察的 Redis 代际，不清空本地刚合并的
  权威/静态池快照。
- 其他进程的事件仍然正常使本地快照失效并从 PostgreSQL 刷新。
- 调度、重试、冷却和 usage 计算链路没有耦合改动；外部直连仍禁止本地救援。

## 真实 HTTP 复现与修复后结果

所有运行均使用：

- 冻结二进制：`/tmp/kiro-release-candidate.7kctZt/kiro-rs`
- SHA-256：`9356d0d2f6d683f83626cf09e3d0f7daee7a07cfd4376afc50a71d061d400f66`
- 隔离 PostgreSQL：`kiro-rs-postgres-local:25432`，每轮独立数据库
- 隔离 Redis：`kiro-rs-redis-local:26379`，每轮独立 DB/Key 前缀
- 三个 loopback fake Anthropic 上游：`yuenan` 优先级 1、`kkkkyue` 优先级 10、
  `jinnyapi` 优先级 20
- 本地 Kiro fake 上游仅用于验证外部直连不得回本地

### 修复前证据

失败运行的外部池命中分布：

```text
yuenan=0, kkkkyue=0, jinnyapi=24
```

同一时刻 Admin 状态仍报告三个池 `dispatchable=true`、冷却剩余 0、瞬态失败
streak 0。调试日志显示每次 Admin 变更后的本地快照计数都是 1，确认快照被
本进程自身事件反复清空。

### 修复后普通基线

3 轮最终冻结二进制真实 HTTP 产品运行全部通过：

```text
每轮 48 个故障/恢复请求全部 200
故障阶段：yuenan=48, kkkkyue=48, jinnyapi=0
外部直连本地推理命中：0
主池恢复后自动重新承接流量：通过
503/429 组合下优先级 20 池接管：通过
全部外部池失败时不回本地：通过
```

### 修复后高并发与持续到达率

最终冻结二进制执行：

```text
突发并发：256
固定到达率：1800 RPM，持续 60 秒
请求总数：1800
完成数：1800
HTTP 200：1800
失败：0
外部直连命中本地：0
```

资源采样（服务停止前最后一轮）：

```text
RSS：约 115856 KiB -> 121520 KiB，未持续单调增长
FD：73 -> 80 -> 72
ESTABLISHED TCP：45 -> 52 -> 44
```

停止流量后 FD/TCP 回落，临时服务、端口、数据库和 Redis 前缀均清理。

## 代码/回归门禁

- 新增真实 PostgreSQL/Redis 回归：
  `external_pool_local_mutations_keep_all_pools_when_own_event_is_observed`：`1/1`
- 远端事件仍能失效本地快照：
  `external_pool_data_generation_invalidates_peer_without_clearing_on_policy_only_change`：`1/1`
- 全量 Rust：
  `1896 passed / 0 failed / 6 ignored`
- `kiro_loadtest`：
  `31 passed / 0 failed`
- `cargo fmt --all -- --check`：通过
- `git diff --check`：通过
- `node feature/tests/inventory-build-artifacts.mjs --gate`：
  `targets=0 reservations=0 target_processes=0 blockers=0`

## 仍需单独观察

- 以上是本地隔离 fake 上游和冻结候选验证，不等同于三台现网机器已经升级。
- 生产发布后仍需观察候选池分布、外部池错误率、冷却/软失败状态和 Dashboard
  usage；usage 本轮未被调度代码修改。
