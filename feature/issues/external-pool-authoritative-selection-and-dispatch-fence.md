# 外部池权威选池 PostgreSQL 扇出与发送一致性

Date: 2026-07-19

Status: `reproduced / fixed-in-dirty-tree / non-storage-focused-and-isolated-storage-pass / frozen-load-pending`

Severity: P0 availability and dispatch correctness under burst; P1 internal PostgreSQL amplification

## 问题、现象与影响

external 静态 eligibility、权威 pool selection、Redis runtime snapshot、并发 lease 和最终 HTTP send 是五个不同阶段。旧实现已把静态 eligibility 改成 secret-free SWR，但每个真正进入 external 的请求仍单独执行一次完整 PostgreSQL pool list；selection breaker 只允许 32 个同时查询，c128 首波可让前 32 个访问 PostgreSQL、其余请求在实际外部池仍有容量时因 `admission_saturated` 快速失败。

请求取得 Redis lease 后虽有 revision fence，但 fence 位于 URL/header/body/model 准备之前。`forward_once` 仍会在 fence 后执行 request body 序列化、payload guard/model mapping、header 构造、request builder 和 timeout 配置，再占用 attempt budget 和 `send()`。因此源码注释所称“紧邻 HTTP send”不准确，配置更新后继续使用旧 URL/key/body policy 的竞态窗口比必要值更大。

另一个无明显 hash 指纹的同类问题是持久化坏行。`request_body_mode`、`supported_models` 和多个枚举/JSON 字段原先用宽松默认值解析；例如损坏的 `supported_models` 对象会变成空列表，而空列表语义是“支持所有模型”。这会把坏池误当成 broad eligible，或用默认 body/model/auth 策略发送。

本问题与 [本地容量预检竞态](local-capacity-preflight-race-and-external-fallback-latency.md) 及 [external Redis 协调](external-pool-redis-coordination-and-release.md) 相邻但不相同：前者决定何时从 local 转入 external，后者决定 Redis lease 是否可取得；本文决定真正 external 请求如何读取 PostgreSQL authority，以及选中对象在发送时是否仍有效。

## 源码链与根因

修复前的权威路径：

```text
forward_with_failover_result
  -> load_authoritative_pool_snapshot
       -> selection_breaker.try_begin (hard cap 32)
       -> PostgresStore::list_external_pools(false), timeout 2s
  -> select_pool_for_route_from_snapshot
  -> Redis runtime batch snapshot
  -> atomic external pool lease
  -> validate_pool_dispatch_fence, timeout 500ms
  -> forward_once
       -> URL/header/body/model preparation
       -> inference_attempt_budget.reserve
       -> request.send
```

三个根因独立存在：

1. request-scoped snapshot 只在同一请求的重选之间复用，进程内同时到达的不同请求互不共享，因此正常 c128 会产生 128 个 pool-list 意图。breaker 用 fail-fast admission 限制数据库工作量，却把内部保护转成用户可见的错误。
2. revision fence 的调用点早于同步请求准备。它仍能挡住 fence 前已提交的 update/delete/disable，但把 CPU/序列化时间纳入 fence-to-send 竞态。
3. PostgreSQL row decoder 面向 Admin/旧数据的兼容默认值被直接复用于 dispatch authority。兼容展示和发送授权使用了同一宽松 parser，损坏值被静默降级。

## 复现方法

### 权威 selection 首波

创建一个正常 pool，清空 generation cache，用 `ACCESS EXCLUSIVE` 暂时锁住 `external_upstream_pools`，同时启动 c32 和 c128 `load_authoritative_pool_snapshot`。旧实现中每个 caller 都尝试 query，最多 32 个进入 PG，超出的 caller 可被 admission 拒绝；修复后每个 cold generation 只能观察到一次实际 pool-list query，所有 waiter 共享同一结果。c32/c128 各连续 5 轮。

PG 错误变体包括：2 秒表锁 timeout、连接池耗尽、query error、leader caller cancel、generation 在 query 中途变化和恢复首波。要求 timeout/error 为 typed coordinator unavailable；失败结果短期负缓存，不能按 caller 重新扇出；解锁/恢复后新 generation 可再次查询。

### dispatch fence 首波与 TOCTOU

对同一 `(pool_id, revision)` 在表锁下同时执行 c32/c128 fence。修复后同一时刻的 callers 只共享一项正在进行的 query；锁释放后全部得到 `Current`。紧接着串行执行两次 fence，必须新增两次 PG query，证明完成结果没有 TTL 缓存。

端到端竞态使用 one-shot test gate：请求已完成 URL/header/body/model/request-builder 准备但尚未 fence 时暂停，PostgreSQL 将 pool disable 并增加 revision，再恢复请求。必须满足：fake external HTTP hit 增量 0、inference attempt consumed 0、旧 lease 完整释放、结果为规范失败或重选，不得用旧 key/URL/body 发送。内部连续 5 轮。

### 持久化坏行

保留一个仅支持健康模型的正常 pool，另一个仅支持目标模型的候选 pool。依次损坏 `base_url`、空 `api_key`、auth、concurrency、usage projection、stream mode、body mode、raw model mode、auto-disable policy、model mapping mode/rules 和 supported models；每类连续 5 轮。

每轮静态 eligibility 对目标模型必须为 false、健康模型仍为 true；权威 snapshot 必须保留健康 pool 并排除坏 pool。日志只允许记录 pool id 和字段级错误，不能记录 key、URL 原值或整行。

## 选定修复与优化

- 权威 pool list 使用 process-local、manager-owned、generation-bound 的 250ms fresh snapshot。一个 cold generation 只允许一个后台 refresh 查询 PostgreSQL；其他 caller 最多等待 2250ms，共享 success 或 typed failure。leader caller cancellation 只取消自身等待，不取消 query/cache publish，也不会让 follower 发起替代查询。
- PG 查询自身仍有 2 秒上限。失败只缓存 100ms并继续受 selection breaker backoff 约束，避免错误 burst 每请求重查；不提供 stale-success fallback。
- local Admin mutation 和 Redis cross-instance generation event 同时清空 static eligibility 与 authoritative snapshot。旧 snapshot 即使被请求持有，也不能绕过最终 revision fence。
- 同一 `(pool_id, revision)` 的并发 fence 只合并正在进行的 query。flight 在发布结果前先从 map 删除，因此查询完成后的新 caller 必须重新访问 PostgreSQL，不形成时间型发送授权 cache。
- URL/header/body/model/request builder 先准备；随后执行 revision fence；只有 `Current` 才立即占用 shared inference attempt budget 并执行 HTTP send。`Changed` 不消耗 attempt，释放 lease 后按 request snapshot 排除并重选。
- dispatch PostgreSQL decoder 使用 strict known-value parsing。静态 eligibility 只读取 secret-free 投影，但同时验证 revision、URL scheme/host、key presence、concurrency、全部相关 enum、mapping rules 与 supported models；非空数组中的空 model/source/target 也拒绝，不能被 normalize 成“空列表=全部支持”。权威 full-row decoder再次独立验证。单个坏 row 被跳过，健康 rows 继续工作；250ms full-row refresh 只输出聚合 debug，5 秒 static refresh 每批最多一个脱敏 warn，避免坏行制造 warn RPM。

未采用长期缓存完整 pool/key：更长 TTL 虽能进一步减少 PG list，但会扩大新 pool 不可见窗口并延长 secret retention。未取消最终 fence：cross-instance Redis event 是失效提示而不是线性一致 authority。未持有 PostgreSQL row/advisory lock 跨上游 HTTP：这会为每个长流占用一条 PG connection，重新制造 pool starvation。

dispatch linearization point 定义为 prepare 后的 revision query。mutation 若在该 query 确认 `Current` 之后提交，不撤销已经开始的 dispatch；要获得更强的“更新提交即撤销所有未完成 send”语义只能引入跨 HTTP 的分布式锁/租约，成本和故障面不可接受。

## 验收矩阵与当前结果

| ID | 场景 | 轮次 | 必须结果 | 当前状态 |
| --- | --- | ---: | --- | --- |
| ES01 | strict enum parser known/unknown | 内部 5 轮 | canonical/legacy known 正确；unknown 为 None | 实际 PASS |
| ES02 | raw/normalized/payload/thinking/effort 单点 | 11 exact；适用项内部 5 轮 | raw byte identical；normalized policy、thinking/effort 不变 | 实际 PASS |
| ES03 | authoritative c32/c128 cold generation | 每档 5 轮 | 每 generation 1 次 PG list；全 waiter 同结果 | isolated PG/Redis dynamic PASS |
| ES04 | PG lock timeout/error/recovery/negative cache/leader cancel | runner 3 outer | c128 timeout query=1；即时波不重查；取消不重查；c128 恢复 query=1 | isolated PG/Redis dynamic PASS |
| ES05 | fence c32/c128、timeout/breaker/recovery | 每档 5 轮；故障 3 轮 | in-flight 1 query；完成后不缓存；timeout 与恢复各 query=1 | isolated PG/Redis dynamic PASS |
| ES06 | prepare-update-fence-send | 内部 5 轮 | HTTP hit 0、attempt 0、lease 0 residue | isolated PG/Redis dynamic PASS |
| ES07 | 15 类 malformed row | 每类 5 轮 | 坏池隔离；健康池持续 eligible/dispatchable | isolated PG/Redis dynamic PASS |
| ES08 | static SWR/body-mode/cross-instance invalidation | runner 3 outer | stale/fresh 合同、raw/normalized parity、Redis RTT 0 | isolated PG/Redis dynamic PASS |
| ES09 | frozen release c32/c128、PG/Redis latency/failure | 每档 3 轮 | 0 false capacity error；PG query/attempt 有界；恢复 5/5 | pending |

Rust 1.92.0 `cargo check --all-targets` 已三次零 warning。`external-dispatch-focused-r1` 实际运行 11 个无存储测试并通过；四个早期 storage exact filters 曾因没有 `KIRO_RS_TEST_POSTGRES_URL` 提前返回，只计 compile-only。2026-07-18 当前仓库隔离 PostgreSQL/Redis 三轮执行 17 个 external storage filters，即 51/51 passed；2026-07-19 合批 storage suite 再次执行同一 17 filters × 3 outer，也为 51/51 passed。最新 scope `storage-suite-real-20260719` 清理 `size_kib=1690164 removed=true reservation_released=true`。完整命令与边界见 [专项证据](../evidence/external-pool-authoritative-dispatch-20260718.md) 和 [2026-07-19 storage evidence](../evidence/storage-integration-and-artifact-gate-20260719.md)。

## 性能、兼容性与回滚风险

健康本地路径仍只使用 5 秒 secret-free eligibility SWR，不访问 external Redis；本修复不增加 local-success I/O。external dispatch 的完整 pool-list 从“每请求一次”降为每进程每 generation/250ms 至多一次；同 revision 同时 fence 从每 caller 一次降为每 in-flight wave 一次。顺序请求仍各自 fence，保证没有 TTL 授权。

250ms snapshot 会短暂遗漏未收到 generation event 的新 pool，但不会把删除/禁用/更新后的旧 pool直接发送，因为每次 send 仍有 revision query。failure cache 100ms 与 breaker共同减少错误 RPM；它可能让恢复后的最早请求短暂 fail closed，不会 fail open。

strict decoder 可能把历史上依赖未知字符串默认值的坏行从“勉强发送”变成不可调度。这是有意的安全兼容变化；管理员需把字段保存为受支持 canonical 值。紧急回滚可关闭 external pools 或回滚 strict row isolation，但不得只删除 revision fence，也不得恢复 c128 每请求 list + 32 后 fail-fast 的组合。

## 残余风险与发布边界

- ES03-ES08 已在当前仓库专属隔离 PostgreSQL/Redis 上执行并通过；后续不能再用缺 URL skip 冒充 PASS。
- fence singleflight 以 `(id, revision)` 为 key；同时命中很多不同 pools/revisions 仍可能达到 32 项 selection breaker hard cap。真实多 pool c128 和 PG slow-query gate尚需给出 p95/p99 与 rejection=0 证据。
- 250ms cache 保存完整 pool 配置，包括进程内 key；过期结果不再授权使用，但内存清除依赖后续覆盖/失效。冻结候选的 secret dump/log 扫描与资源观察仍需执行。
- query 完成与 `request.send()` 之间仍有不可消除的本地调度间隙；本文只把 body preparation 移到 fence 前并定义可审计的 linearization point，不宣称绝对零 TOCTOU。
- 完整默认 bin 树尚未在本轮修改后重跑；all-target tests、no-default、release binary、真实 CLI、L3-L5、两实例与生产 recurrence 仍阻断发布。
- 发布保持 `NO-GO`。
