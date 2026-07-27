# 159/170/142 生产实例运行时卡死：请求完成路径与存储/调度耦合

Status: `implemented / validated / release-pending`

Severity: `P0`

Last reviewed: 2026-07-27 Asia/Shanghai

Evidence root: `tmp/prod-evidence/20260727-020431-runtime-completion-159-170-142`

Validation evidence:

- Current release-candidate evidence: [final-runtime-storage-cli-load-validation-20260727](../evidence/final-runtime-storage-cli-load-validation-20260727.md)
- Investigation/history evidence: [runtime-completion-storage-coupling-validation-20260727](../evidence/runtime-completion-storage-coupling-validation-20260727.md)

Current validation summary:

- Full Rust all-target tests passed: `1816` main tests + `31` loadtest tests, `0` failures.
- Real Claude Code CLI fake-upstream gates passed:
  - bare invoke: `20` cases;
  - long session: `5 sessions / 110 turns / 100 tool pairs / leakMatches=0`;
  - thinking/output_config wire: `60/60`, `max` preserved, adaptive thinking present.
- Load/chaos passed:
  - L3 burst/recovery: `9/9`;
  - L4 restart/failure/client-drop/mixed-chaos: `12/12`;
  - L5 long-stream soak second run: `461/461` long-stream success, recovery `12/12`, RSS/FD returned within gate after 60s idle.
- Local 9022 smoke passed for health/readiness/dashboard split endpoints. Real-account success was
  not proven on 9022 because local PgSQL has all persisted credentials disabled.

## 0. 结论摘要

这次事故不能归因成“几百 RPM 把单机打满”。当前证据支持的根因链是：

1. 159/170 在事故时出现过 HTTP 业务不可达或前端不可用，但 Docker health 一度仍显示 healthy。
2. 两台故障实例在恢复前的 `usage_records` 按 5 分钟聚合存在明显断层，说明业务请求处理或 usage 记录链路确实停顿过。
3. 事故窗口前后请求量不是异常高，159 常见约 400–800 requests/5min，170 常见约 140–300 requests/5min；但长流请求大量存在，单请求 `duration_ms` 可达 180–480s，个别 170 样本接近 982s。
4. 0.0.120 已把一部分 stream/API completion 成功路径改成 “先释放 in-flight lease，再异步持久化 PgSQL/Redis 状态”，因此 170 在 app-only 更新后能恢复，说明主事故路径被缓解。
5. 本轮候选继续补齐 0.0.120 的残余热路径：真实请求失败、quota/risk/auth 禁用、refresh-token-invalid、profileArn discovery 和 MCP/WebSearch auxiliary completion 均改为请求安全的本地优先/deferred persistence；管理面和 usage/dashboard 查询仍归类为非核心路径，完整 dashboard 产品化拆分另见 [Dashboard observability redesign](dashboard-observability-redesign.md)。
6. 本轮进一步确认 token refresh 分布式协调里存在一个真实边界错误：拿到 Redis refresh lease 后，如果在上游发送前发生 PgSQL 权威凭据 reload / coordination / persistence 失败，旧逻辑会把这个 `send_committed=false` 的本地 setup failure 写成 Redis failure outcome，导致其他 caller 复用一个并未真正命中上游的失败波次。当前修复改为“只有 shareable failure 才写 Redis outcome；pre-send setup failure 取消 lease”，避免存储抖动污染分布式刷新波次。
7. 159 app-only 更新后未恢复，full down/up 才恢复，说明该实例已经进入跨进程运行态坏状态：旧连接、长流、PgSQL backend session、Redis 连接/命令上下文、Docker 网络端点、请求重连流量共同导致新 app 进程很快重新退化。
8. 142 被用户观察为同版本正常承接更多流量，但本次只读 SSH 证据未能登录验证；它只能作为“用户侧对照现象”，不能作为已采集生产证据。

因此，工程修复目标不是单纯调低并发或继续依赖重启，而是：

- 真实请求路径不能同步等待非必要 PgSQL/Redis 写入。
- completion/failure 必须先释放本地调度容量，再异步持久化运行态。
- usage/dashboard/admin/观测统计必须与主业务调度 Redis/PgSQL 热路径隔离，并且查询慢不能阻塞主业务。
- token refresh / auxiliary 这类请求附属链路不能把未发送上游的本地存储/协调错误广播成跨实例失败；只有已经发送上游或明确可共享的失败才允许形成 Redis failure wave。
- health/readiness 必须能暴露 HTTP runtime、storage queue、scheduler degraded、PgSQL pool wait、Redis command latency，而不是只证明容器还活着。

## 1. 用户可见现象

### 1.1 159 / 170 多次服务死掉

用户观察：

- health 貌似正常；
- 前端页面和业务接口完全不通；
- 159/170 多次出现；
- 142 同时段未出现，即使 159/170 故障后承接更多流量也正常。

当前证据：

- 159 当前已恢复，app/PgSQL/Redis 均在 2026-07-26T17:06Z 左右重建/启动。
- 170 当前已恢复，app 在 2026-07-26T17:15Z 左右重建/启动；PgSQL/Redis 已运行约 37 小时，说明该次恢复未重启依赖服务。
- 159 的 5 分钟 usage 聚合在 2026-07-26T16:40Z 后出现断层，2026-07-26T16:55Z 仅 2 条，2026-07-26T17:05Z 后恢复。
- 170 的 5 分钟 usage 聚合在 2026-07-26T16:40Z 后出现断层，2026-07-26T17:15Z 后恢复。

证据文件：

- `raw/159/docker/container-state.txt`
- `raw/170/docker/container-state.txt`
- `raw/159/db/usage-5m-12h.txt`
- `raw/170/db/usage-5m-12h.txt`

### 1.2 159 app-only 更新未恢复，down/up 后恢复

用户观察：

- 159 仅重启 app 后 1 分钟以上仍未恢复，中间再次重启也没恢复。
- 159 full down/up 后恢复。

当前证据：

- 159 当前 app、PgSQL、Redis 启动时间均接近 2026-07-26T17:06Z，说明最后恢复动作确实清理了整套运行态。
- 当前恢复后 `/healthz`、`/readyz`、`/v1/models` 均毫秒级返回。

推断：

- app-only 重启只替换应用进程，保留 PgSQL backend session、Redis 服务端连接/命令上下文、持久化调度 key、Docker 网络端点和外部客户端重连压力。
- 如果旧运行态已经积累了滞留连接、lease、queue 或阻塞的存储任务，新 app 会立即接入同一依赖状态和重试流量，从而重新进入退化。
- down/up 恢复不是证明 PgSQL/Redis CPU 打满，而是一次跨进程状态清空和连接排空。

### 1.3 170 app-only 更新约 20 秒后恢复

用户观察：

- 170 未重启 PgSQL/Redis，只重启/重建 app 后恢复。

当前证据：

- 170 当前 app 启动时间约 2026-07-26T17:15Z。
- 170 PgSQL/Redis 容器未重启，仍运行约 37 小时。
- 当前 `/healthz`、`/readyz`、`/v1/models` 均毫秒级返回。

推断：

- 170 的坏状态主要在 app runtime / 请求生命周期内；替换 app 后能释放足够多的阻塞点。
- 159 的坏状态更深，涉及 app 之外的连接/依赖运行态或更强重连波次，所以 app-only 不够。

### 1.4 159 dashboard 仍加载失败

现网 0.0.120 当前证据：

- 159 `/api/admin/usage-dashboard/windows` 返回 500，耗时 5.003s：
  - `读取 PgSQL usage dashboard windows 超过 5 秒，已中止本次后台查询`
- 170 同接口 200，耗时 2.575s。
- 159 `usage_records` 表约 1909MB；170 约 497MB。

判断：

- 这是独立的 dashboard/usage 查询脆弱问题，但和主事故同属“非核心统计不能拖垮核心业务”的工程边界。
- 当前工作树已把 dashboard PgSQL 查询预算从 5s 扩展到 120s，并增加查询 gate；但仅延长超时不是完整重构。完整 dashboard 设计见 [Dashboard observability redesign](dashboard-observability-redesign.md)。

证据文件：

- `raw/159/api/admin-light.txt`
- `raw/170/api/admin-light.txt`
- `raw/159/db/pg-meta.txt`
- `raw/170/db/pg-meta.txt`

## 2. 已采集事实

### 2.1 当前版本和部署状态

159：

- app 版本：0.0.120
- app image：`ghcr.io/2ue/kiro-rs:latest`
- Redis：`redis:8-alpine`
- PgSQL：`postgres:18-alpine`
- 当前探针：
  - `/healthz` 200，约 0.0019s
  - `/readyz` 200，约 0.0018s
  - `/v1/models` 401，约 0.0017s

170：

- app 版本：0.0.120
- app image：`ghcr.io/2ue/kiro-rs:latest`
- Redis：`redis:8-alpine`
- PgSQL：`postgres:18-alpine`
- 当前探针：
  - `/healthz` 200，约 0.0036s
  - `/readyz` 200，约 0.0051s
  - `/v1/models` 401，约 0.0036s

142：

- 本轮 SSH 登录未成功，尚未采集只读证据。
- 用户描述它在同版本下未故障，并在另外两台故障时承接更多流量。
- 该信息目前作为“用户观察”，不是本轮已验证事实。

### 2.2 请求量不是线性过载

159 近 12 小时样本：

- 常见 400–800 requests/5min，约 80–160 RPM。
- 多数为 `local_credential | local_success`。
- 单请求最长常见 180–480s。

170 近 12 小时样本：

- 常见 140–300 requests/5min，约 28–60 RPM。
- 多数为 `local_credential | local_success`。
- 单请求最长常见 120–330s，最高样本约 982s。

结论：

- 这不是普通短请求压测下的 “200 RPM 必然打满”。
- 真实压力来自长流并发、集中 completion、lease 释放、失败重试、usage 记录、PgSQL/Redis 协调的组合。

### 2.3 入口级 request admission 当前关闭

159 runtime config：

- `credentialRpm=300`
- `credentialMaxConcurrentRequests=20`
- `dispatchGlobalMaxConcurrentRequests=3000`
- `dispatchMaxQueuedRequests=100`
- `credentialDispatchMaxWaitSecs=300`
- `credentialInFlightLeaseMaxSecs=300`
- `requestAdmission={"rpm":0,"queueTimeoutMs":0,"maxQueuedRequests":0,"maxConcurrentRequests":0}`

170 runtime config：

- `credentialRpm=300`
- `credentialMaxConcurrentRequests=20`
- `dispatchGlobalMaxConcurrentRequests=1000`
- `dispatchMaxQueuedRequests=500`
- `credentialDispatchMaxWaitSecs=300`
- `credentialInFlightLeaseMaxSecs=300`
- `requestAdmission={"rpm":0,"queueTimeoutMs":0,"maxQueuedRequests":0,"maxConcurrentRequests":0}`

判断：

- 每账号并发/RPM 限制存在，但入口级请求准入关闭。
- 在长流和重试场景下，入口没有先把过量请求有界排队/快速拒绝，而是让请求进入更深的调度/存储链路。
- 这会放大“并发不低、RPM 看起来不高、但内部队列/lease/完成路径阻塞”的非线性问题。

### 2.4 usage writer 当前不阻塞主请求，但内存窗口接近上限

159 `/api/admin/usage-writer-stats`：

- accepting=true
- inMemoryLimit=5000
- inMemoryRecords=4946
- postgres writer queue capacity=4096
- writerAccepted=writerFinished=4946
- droppedPersistRecords=0
- redisEnabled=false
- redisQueueEnabled=false

170：

- inMemoryRecords=2719
- droppedPersistRecords=1
- redisEnabled=false
- redisQueueEnabled=false

判断：

- 当前工作树/0.0.120 已经把 usage 持久化改成异步队列，PgSQL usage 写入不应直接阻塞主请求。
- 但 usage 查询、dashboard 聚合、rollup、admin 查询仍可能消耗 PgSQL/HTTP runtime 资源。
- 159 in-memory records 接近 5000 不是直接根因，但说明实例上 usage 数据量和展示压力更重。

## 3. 源码根因链

### 3.1 已缓解路径：成功 completion

当前源码已存在以下改动：

- `src/kiro/provider.rs`
  - `KiroApiCompletion::report_success`
  - `KiroStreamCompletion::report_success`
- `src/kiro/token_manager/manager.rs`
  - `report_success_with_latency_deferred`
  - `report_success_for_session_with_latency_deferred`
  - `record_scheduler_success_health`

成功路径现在的顺序是：

```text
stream/API EOF or body parsed OK
  -> take/drop in-flight lease
  -> update local scheduler state
  -> enqueue PgSQL runtime mutation if needed
  -> enqueue Redis scheduler health update
  -> notify dispatch
```

这解释了为什么 0.0.120 对 170 有缓解效果。

### 3.2 残余风险路径：真实请求失败和强一致禁用

0.0.120 之前仍存在这些同步等待点：

- `src/kiro/token_manager/manager.rs`
  - `block_on_storage`
  - `block_on_credential_pgsql`
  - `block_on_scheduler_redis_affinity`
  - `block_on_scheduler_redis_hot_outcome`
  - `block_on_scheduler_redis_state_sync`
- 真实请求中可能触发：
  - `report_failure`
  - `report_quota_exhausted`
  - `report_risk_controlled_outcome`
  - `report_refresh_token_invalid`
  - `unbind_sessions_for_credential`
  - `persist_disabled_state`
  - `clear_scheduler_state_for_credential`
  - `report_transient_failure_kind`

其中旧 `report_failure` 会先尝试：

```text
block_on_credential_pgsql("原子记录 PgSQL 凭据 API 失败", ...)
```

失败或超时后才回退为本地状态 + pending mutation。

问题：

- 当 PgSQL pool、事务、I/O 或连接处于慢状态时，真实请求失败路径仍会同步等待。
- 如果失败集中发生，多个 handler 会同时等待这些同步桥。
- 即使每次有 5s 超时，大量长流 completion/failure 一起结束时仍会占住请求生命周期，拖慢连接释放、lease 释放和下游响应。

本轮修复：

- `report_failure_deferred`：真实请求失败只更新本地调度状态并把 PgSQL failure mutation 放入 FIFO，不等待 PgSQL。
- `report_quota_exhausted_deferred`：额度耗尽本地立即禁用并释放调度状态，durable disable 异步重放。
- `report_risk_controlled_outcome_deferred`：风控/暂停/锁定本地立即生效，PgSQL 事件和运行态异步写入。
- `report_refresh_token_invalid_deferred`：invalid_grant 本地立即禁用，durable disable 异步。
- `update_credential_profile_arn_deferred`：profileArn discovery 本地先更新，PgSQL CAS 持久化 best-effort；持久化失败不把当前请求改成 `scheduler_state_error`。
- Provider 真实请求路径已改用 deferred 版本。

剩余边界：

- 管理面手工操作仍可使用同步/强一致路径，这是 admin 操作，不属于模型请求热路径。
- durable queue 如果长期写不进去，应通过 `runtime_persistence_degraded` 和后台重放暴露，而不是阻塞请求。

### 3.3 残余风险路径：session/Redis affinity

0.0.120 之前已有 deferred 版本：

- `unbind_session_if_bound_to_deferred`
- `record_session_soft_failure_deferred`
- `clear_session_soft_failure_deferred`

但同步版本仍存在并被部分请求路径使用：

- `unbind_session_if_bound_to`
- `unbind_sessions_for_credential`
- `record_session_soft_failure`
- `clear_session_soft_failure`

问题：

- 单个 session 绑定/解绑失败不应阻塞主业务 completion。
- 凭据禁用或批量解绑场景可以需要更强一致，但必须先释放调度容量、更新本地状态，再异步清理 Redis/PgSQL；不能让 Redis affinity 慢操作把请求 handler 卡住。

本轮修复：

- 新增 `unbind_sessions_for_credential_deferred` 和 `clear_disabled_credential_request_state`。
- 请求内强一致禁用先清理本地/进程内调度状态，再把 Redis session/soft-failure/affinity 清理作为 best-effort/deferred work。
- MCP/WebSearch auxiliary completion failure 只释放本次 lease 和记录 attribution，不再写主模型账号 cooldown，避免辅助服务故障扩散成 `local_all_disabled`。

剩余边界：

- Redis scheduler hot outcome / state sync 的调度准入路径仍是核心路径；该路径不能和 usage/dashboard 共用 Redis 竞争源。业务/观测 Redis 故障域隔离仍需要作为生产配置强约束持续验证。

### 3.4 残余风险路径：dashboard/admin 查询

当前工作树已把 dashboard 查询拆分、加 gate、延长 PgSQL statement timeout，但生产 0.0.120 仍展示出：

- 159 `/api/admin/usage-dashboard/windows` 5s 超时 500；
- 170 同接口 2.575s 返回；
- 159 usage 表 1909MB。

问题：

- dashboard 查询属于非核心路径，慢可以接受，但不能阻塞主业务。
- 如果 admin/dashboard 查询跑在同一 HTTP runtime 且使用同一个 PgSQL pool，慢查询、pool acquire、锁等待、dashboard 聚合会和主业务竞争。
- 当前工作树已有 `postgres.usageMaxConnections` 和独立 usage pool 的修改痕迹，需要继续验证所有 usage/dashboard/admin 查询是否确实走 usage pool，而不是主业务 pool。

## 4. 为什么三台表现不同

同版本、同下游流量，不等于同运行态。

当前已确认差异：

- 159/170 使用 Redis 8。
- 用户描述 142 使用 Redis 7，但本轮未 SSH 验证。
- 159 usage 表约 1909MB，170 约 497MB。
- 159 full down/up 才恢复；170 app-only 恢复。
- 三台的运行历史、旧连接、长流结束波次、sticky session、Redis key 状态、PgSQL backend session 和下游重试分布不完全一致。

判断：

- Redis 8 可能是放大因素，但不能单独判为根因；因为 170 在 Redis 8 上 app-only 更新后恢复。
- 142 未故障说明流量规模本身不是充分条件。
- 真正模式是实例级非线性退化：某个实例先积累坏状态后，completion/storage/scheduler/retry 形成反馈环。

## 5. 可复现模型

### 5.1 最小复现

目标：证明真实请求 completion/failure 不应等待 PgSQL/Redis。

步骤：

1. 构造本地 fake Kiro upstream，持续返回长流 EventStream。
2. 配置 10–20 个本地凭据，每账号并发 5–20，入口 admission 关闭或较大。
3. 发起 100–500 个长流请求，请求持续 60–300s。
4. 在流集中结束阶段注入 PgSQL 慢写或 Redis 慢命令。
5. 观察：
   - `/healthz`、`/readyz`、`/v1/models` 是否持续毫秒级响应；
   - in-flight lease 是否在 completion 立刻释放；
   - storage queue 是否有界；
   - PgSQL/Redis 慢是否只造成持久化延迟，不造成业务接口假死。

### 5.2 异常复现

必须覆盖：

- 上游 2xx headers 后 stream read error。
- 上游 2xx EventStream 没有 `completed`，最后以 `meteringEvent` EOF。
- 上游 400/401/403/429/500 JSON。
- 上游 body read timeout。
- 客户端中途断开。
- MCP/WebSearch auxiliary 失败。
- thinking signature retry 第二响应读失败。
- PgSQL pool acquire 慢。
- Redis scheduler affinity 慢。
- usage writer queue full。
- dashboard 长查询同时发生。

### 5.3 长对话/真实协议复现

必须覆盖：

- Claude Code CLI 多轮会话。
- tools/tool_result 长历史。
- MCP/web-search。
- thinking/output_config 组合。
- 流式和非流式。
- 大 body/payload guard。
- usage 返回和系统内部 usage 记录一致。

## 6. 修复方案

### 6.1 必须做

1. 请求完成路径：
   - 成功路径已基本 deferred，但要加回归测试证明“lease 释放先于 PgSQL/Redis 等待”。
   - Drop 路径必须释放 lease；client drop 不能留下长期占用。

2. 请求失败路径：
   - `report_failure` 新增 request-safe deferred 版本。
   - Provider 真实请求路径改用 deferred failure，不同步等待 PgSQL。
   - 本地 runtime state 立即更新；durable PgSQL mutation 进入 FIFO/critical queue。
   - durable queue 满时记录 degraded，不阻塞请求；不能把 PgSQL 慢转换成业务卡死。

3. 强一致禁用路径：
   - quota/risk/invalid refresh token 仍可本地立即禁用。
   - PgSQL durable disable 改为 critical async queue。
   - session unbind 和 Redis scheduler clear 先本地执行，Redis 异步 best-effort。
   - 管理面可以显示 `runtime_persistence_degraded`，但业务不能等待 PgSQL 完成。

4. Scheduler Redis：
   - 单 session affinity 操作在真实请求路径使用 await/deferred 版本。
   - 热路径 Redis 超时只影响分布式协调质量，不得让 HTTP handler 长时间同步等待。
   - Redis degraded 时可降级为本地调度缓存或受控 fallback，不能产生 “local_all_disabled” 假象。

5. Usage/dashboard/admin：
   - usage 写入继续保持队列化、可丢弃、不可阻塞主请求。
   - dashboard 查询使用独立 usage pool + 并发 gate + 长超时 + 可取消。
   - 查询慢返回 loading/partial/stale，而不是拖死主业务。
   - 新旧 UI 按 [Dashboard observability redesign](dashboard-observability-redesign.md) 的指标语义重构。

6. Health/readiness：
   - `/healthz` 保持轻量。
   - `/readyz` 增加 runtime/storage/scheduler 关键指标，不能只 ping PgSQL/Redis。
   - Docker healthcheck 不应只证明 TCP 可连；应至少覆盖 HTTP handler 能响应。

### 6.2 不应该做

- 不应通过 full down/up 作为常规恢复策略。
- 不应把所有 Redis/PgSQL 查询超时简单调大。
- 不应让 dashboard 或 usage 聚合和主调度共用 Redis 热路径。
- 不应把 MCP/WebSearch auxiliary 错误写入主模型账号 cooldown。
- 不应把入口 request admission 永久关闭并依赖深层队列承压。

## 7. 验收矩阵

| 类别 | 验收项 | 目标 |
|---|---|---|
| completion | 成功 EOF | lease 先释放，PgSQL/Redis 写入异步 |
| completion | client drop | lease 释放，无长期占用 |
| failure | 400/401/403/429/500 | 本地状态立即更新，durable 异步，handler 不等 PgSQL |
| stream | read error/idle timeout | 本次请求记录错误，不放大到全账号假禁用 |
| scheduler | Redis 慢/不可用 | 主业务继续响应，降级/限流有明确 reason |
| usage | writer queue full | 丢弃/降级 usage，不阻塞业务 |
| dashboard | 159 规模 usage 表 | 可以加载 partial/stale/长查询结果，不影响业务接口 |
| health | HTTP runtime 假死 | health/readiness 能区分 TCP healthy 和 handler 不可用 |
| load | 100–500 长流 + PgSQL/Redis 慢 | `/healthz`/`/readyz`/`/v1/models` 持续可响应 |
| release | CI + focused chaos + CLI protocol | 全部通过后发版 |

## 8. 当前状态

已完成：

- dashboard/UI 需求已重置并落文档。
- 159/170 只读证据已采集到本地 evidence root。
- 源码已确认成功 completion 使用 deferred。
- 真实请求失败/禁用/profileArn 路径已改成 request-safe deferred。
- MCP/WebSearch auxiliary completion failure 已改为不污染主模型凭据健康。
- Provider client-cache 测试已避免 macOS Keychain 反复扫描导致的发布门禁慢点。
- L3 burst/recovery `9/9` 通过。
- L4 restart/failure chaos `12/12` 通过。
- L5 60 秒长流第二轮 `461/461` + recovery `12/12` 通过；第一次 15 秒 idle 未过 RSS gate，第二次 60 秒 idle 后 RSS/FD 均回到阈值内。
- 真实 Claude Code CLI bare invoke `20/20` 通过。
- 真实 Claude Code CLI long-session `5 sessions / 110 turns / 100 tool pairs / leakMatches=0` 通过。
- thinking/output_config wire `60/60` 通过。
- full Rust all-target：`1816` main tests + `31` loadtest tests，`0` failures。
- PgSQL deferred/smoke、两套 UI build、frontend contract、fmt、clippy baseline、no-default check、最终 release binary build、artifact inventory 已通过。

当前待执行：

- 发布版本。
- 发布后继续推进 dashboard 完整产品化重构。

未完成：

- 142 只读生产证据未采集成功，需确认 SSH 登录方式。
- Redis 8 vs Redis 7 的受控对照尚未完成。
- dashboard 完整产品化重构尚未执行；本轮只完成设计文档和后端/接口局部隔离修复。
- 本地 9022 真实账号成功链路未验证；原因是本地 PgSQL 权威状态下 6 个凭据全部 disabled。未绕过持久状态强行启用账号。

## 9. 残余风险与回滚

残余风险：

- 本轮已把真实请求 completion/failure/禁用/profileArn/auxiliary 等主要热路径改为请求安全的本地优先和 deferred persistence，但不能声明所有 admin/usage/dashboard/调度边缘路径都已完成产品化隔离。
- 159/170 现网事故中的外部池触发因素需要单独复核；相关登记见 [外部池调度影响本地凭据与 fallback 矩阵缺失](external-pool-scheduler-interference-and-fallback-matrix-20260727.md)。
- Dashboard 完整重构尚未完成；当前只完成局部拆接口、超时/gate/usage pool 方向修复和产品合同文档。
- 本地 9022 真实账号成功链路未验证，因为本地 PgSQL 权威凭据全部 disabled；已完成真实 Claude Code CLI fake-upstream 协议验证和 load/chaos 验证，但不能把本地真实账号成功路径伪装成已通过。
- Redis 7/8 生产差异仍需受控对照，不能把 Redis 主版本单独定性为根因。

回滚边界：

- 发布后如出现新故障，回滚应使用二进制/tag/config 回滚到上一个线上稳定版本；不要把同步 completion/storage bridge 作为长期方案恢复。
- 如果 dashboard 查询仍影响现网，应优先关闭或降级 dashboard 重查询入口、降低 query gate 并使用 stale/partial 数据，不应回滚主业务 runtime/storage 解耦。
- 如果 deferred persistence 队列长期堆积，应通过 runtime persistence degraded 指标和后台重放处理；紧急情况下可暂时降低写入量或关闭非核心统计，但不应让模型请求重新同步等待 PgSQL/Redis。
