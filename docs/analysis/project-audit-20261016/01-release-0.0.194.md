# 01 v0.0.194 发版记录与线上验证

## 1. 提交清单（相对 v0.0.193）

| 提交 | 内容 |
|---|---|
| `f691213` | 热路径：prompt cache 过期清理节流与 O(1) 计数；PDF 去全局锁、panic hook 只装一次；≥256 KiB 请求体 `block_in_place`；自愈写 PG 移出 `selection_reservation_gate`；Redis 探测移到 `entries` 锁之前 |
| `d6f6904` | Redis：会话反向索引写入时惰性清理死成员；本地 in-flight 墓碑改为每 lease 一个带 TTL 的 key；5 个本地租约脚本改用 Redis TIME |
| `b25953e` | prompt cache 淘汰改为借用 + `select_nth_unstable_by`，不再全量克隆排序 |
| `a211c2d` | 自愈 single-flight（异步 + 同步共用一个门）；PDF 抽取并发按 CPU 核数封顶；带 document 块的请求一律 `block_in_place`；时钟回拨时节流清理视为到期 |
| `8a2b310` | 自愈进行中时，路由预检/入口快速失败等待其完成后按恢复状态路由，不再直接 503 |
| `317a8b1` | `chore(release): 0.0.194` |

## 2. 各项问题的根因与修法

### 2.1 同步存储桥阻塞调度（原分析中的 P1-1）

- 原分析称 manager.rs 有 17 处 `block_on_storage`，并认为都在请求路径上。调用图可达性检查后修正：请求路径大部分已走 `*_deferred` 变体，真正阻塞的只有两处：
  1. 账号选择时持 `selection_reservation_gate`（parking_lot 互斥锁）调用 `auto_heal_too_many_failures_if_applicable`，对每个候选同步写 PG（单次最长 5s）。其他线程拿这把锁会阻塞 worker 线程本身。
  2. `compute_local_pool_route_state` 持 `entries` + `proxy_resources` 锁做同步 Redis ping（最长 250ms）。
- 修法：自愈拆成"候选收集 / PG 写入 / 本地回写 / 审计通知"，acquire 路径决策为 `AcquireDecision::AutoHeal` 后先释放门锁再 `await`；探测挪到拿锁之前。

### 2.2 自愈惊群

- 线上 0.0.193 行为：账号池全部因连续失败被禁用时，每个并发请求都在入口（`local_pool_route_state_cached`）完整执行一遍自愈。
- 第一版只给 acquire 加了 single-flight，实测发现入口同步路径才是首个触发点，补上同步 `try_lock` 共用同一把门。
- 第二版实测又发现：持锁请求在自愈，其余请求 `try_lock` 失败后按"全部禁用"被快速失败（40 并发 39 个 503）。最终修法：同步调用方遇到"自愈进行中"时等待其完成（多线程运行时 `block_in_place`，上限 30s），再按恢复后状态重算。

### 2.3 Redis 集合无界增长

- `scheduler:sessions_by_credential:{id}`：每次写入 `SADD + EXPIRE` 刷新整体 TTL，session key 自然过期时成员不被移除。修法：写入后 `SRANDMEMBER 4` 抽样，`EXISTS` 不存在则 `SREM`；稳态集合大小约为存活数 × 4/3。SET 类型不变，兼容旧实例。
- `scheduler:inflight:{id}:released`：墓碑 SET 每次释放刷新 TTL，持续流量下永不过期。修法：每 lease 一个 `SET ... EX 120` 的 key；acquire 同时检查新 key 与旧 SET（旧 SET 升级后 120s 自然过期）。
- 租约时钟：5 个本地租约脚本改用 Redis TIME；读取状态时把分数换算到本地时钟，下游新鲜度判断不受漂移影响。

### 2.4 prompt cache 锁热点

- 第一版只做了过期清理节流和计数，release 基准几乎没变化（111.7s → 111.5s）。
- 真正热点：缓存写满 2 万条后，每次写入都超过全局上限，淘汰时克隆两个 String 并全量排序 2 万条。改为借用候选 + 部分选择后 111.5s → 3.8s，淘汰集合与全量排序完全一致（有等价性测试）。

## 3. 验证证据

### 3.1 测试

| 项 | 结果 |
|---|---|
| 全量测试（空闲机器串行，各约 1615s） | 0.0.194：2411 通过 / 9 失败；0.0.193：2390 通过 / 10 失败 |
| 新增失败 | 无。0.0.194 的 9 个失败全部是 0.0.193 的已知失败 |
| 新增测试 | 21 个，关键测试都做过"去掉修复后必须失败"的反向验证 |
| clippy（rustc 1.92，CI 同版本） | 750 个 warning，基线允许 849 |
| CI | `Publish Docker Images #252` 全绿（质量门禁、amd64、arm64、manifest） |
| 镜像 | `ghcr.io/2ue/kiro-rs:0.0.194` 与 `latest` 同 digest `sha256:ffa4a3ab1ed4…`，含 amd64/arm64 |

反向验证样例：

| 测试 | 去掉修复后 |
|---|---|
| `redis_session_index_prunes_expired_session_members_on_write` | 集合涨到 550，失败 |
| `postgres_concurrent_acquires_auto_heal_with_a_single_pgsql_pass` | PG 自愈 72 次（应为 3），失败 |
| `postgres_concurrent_route_state_checks_auto_heal_with_a_single_pgsql_pass` | 无门 48 次；不等待时 15/16 调用方看到 AllDisabled，失败 |

### 3.2 真实账号实测（220 个 KIRO FREE，隔离环境）

| 场景 | 结果 |
|---|---|
| 额度刷新 | 218 可用，2 个 refresh token 失效 |
| haiku 4.5 / sonnet 4.5 流式与非流式 | 全部 200 |
| 三轮工具调用 + 缓存 | turn1 `tool_use`，turn2/3 缓存读命中 |
| 15 / 30 并发 | 全部 200；结束后 Redis 租约、全局计数、墓碑归零 |
| 租约计时 | 长流式进行中，租约分数与 Redis TIME 差值等于请求已运行时长 |
| PDF / 4 个 290 KiB 大请求 + 8 个小请求 | 全部 200；小请求 1.0–4.1s，未被拖慢 |

自愈 A/B（220 个账号全部 TooManyFailures，40 并发）：

| 指标 | 0.0.193 | 0.0.194 |
|---|---|---|
| 自愈执行次数 | 40 | 1 |
| 成功请求 | 2 | 28 |
| 典型延迟 | 20.8s | 15.2s |

0.0.194 剩余失败与代码无关：导入的 token 全部过期，被本地刷新令牌桶（60 RPM / burst 8）排队；3 个账号返回 403 被暂停，触发 30 秒风控熔断。被暂停的 6 个账号用 0.0.193 单独测试同样 403。

## 4. 线上 59137 巡检（部署后约 2 小时，只读）

### 4.1 运行状态

- 容器 `kiro-rs-2ue-59137-app` 运行 0.0.194（label revision `317a8b1`），15:20:07 UTC 启动，重启次数 0，无 panic。
- 部署时 redis、postgres 容器一起重启（15:20:02）。

### 4.2 前后对比（`usage_records`，部署时刻前后各 2 小时）

| 指标 | 0.0.193 | 0.0.194 |
|---|---|---|
| 请求数 | 5143 | 4055 |
| 成功率 | 97.41% | 98.55% |
| p50 | 2.75s | 2.61s |
| p95 | 85.3s | 51.8s |
| `api_error` | 101 | 20 |

两个时段流量构成不同，只能说明方向，不是严格 A/B。

### 4.3 部署后新出现的错误

- 5 次 `external_pool_coordinator_unavailable`，全部在 15:20:40–45，此后为 0。原因是 Redis 随应用一起重启，协调器 epoch 尚未就绪。见 R10。

### 4.4 Redis 清理生效情况

- 会话索引 39 个，最大基数 582；抽样 60 个成员只有 1–8 个存活，积压明显。
- 间隔 4 分钟两次采样：25 个有写入的索引中 18 个下降，成员总数 6657 → 6548。
- 无写入的索引会在 6 小时 TTL 到期后整体删除。
- 墓碑：旧 SET 0 个，新 key 0 个（采样时无未知结果的 acquire）。

## 5. 部署注意

- 新旧版本可滚动混跑：所有 Redis key 类型不变，旧实例写的墓碑 SET 新版本仍会识别。
- 只重启应用容器，不要连带 PG/Redis：`docker compose up -d --no-deps app`（或对应服务名）。
- 59139 实例仍为 0.0.189，0.0.194 修复的问题它都存在。
