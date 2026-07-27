# 外部池调度影响本地凭据与 fallback 矩阵缺失

Status: `recorded / post-release-analysis-pending`

Severity: `P0`

Last reviewed: 2026-07-27 Asia/Shanghai

Related production machines:

- `152.53.243.159`
- `152.53.194.170`
- `152.53.194.142`
- `152.53.242.178`

## 0. 结论与影响

这是一个后续必须单独分析和修复的生产级问题。当前先登记，不在 runtime/storage 发布前展开修复，避免打断已验证候选。

用户侧现象：

- 159/170 开启外部池后，本地凭据调度明显异常，页面和业务接口可能卡住。
- 关闭外部池后，调度恢复正常。
- 142 在外部池关闭或未进入外部池路径时更稳定。
- 用户怀疑外部池 fallback、preflight、capacity、Redis/PgSQL snapshot 或回环调度影响本地凭据。

必须验证的核心问题：

> 外部池开启后，外部池资格判断、fallback、容量协调、坏记录刷新或 Redis/PgSQL 同步路径，是否把本地凭据调度拖入同一个故障域，导致本地凭据即使有容量也变成不可调度。

## 1. 用户可见问题

已登记的现象包括：

- 外部池调度路径非常影响本地凭证。
- 外部池开启后，即使本地账号并发/RPM 没打满，也可能出现调度异常、页面慢、业务慢。
- 外部池关闭后，所有调度恢复正常。
- 159 分析里出现外部池坏记录或空 `api_key` 记录反复刷新、静态资格解析失败、Redis scheduler 慢、PgSQL 凭据状态写入慢等链式现象。
- 需要明确哪些场景会 fallback 到外部池，哪些场景不能 fallback，哪些场景应该本地失败，哪些场景应该直连外部池。

## 2. 根因假设

当前只登记假设，不做最终定性。

主要假设：

1. 外部池开启后，每个请求额外进入外部池资格判断、snapshot、preflight、fallback capacity 协调路径。
2. 外部池 snapshot 可能依赖 PgSQL，scheduler/capacity 可能依赖 Redis，与本地 token manager/scheduler 共用故障域。
3. 外部池坏记录（例如空 `api_key`）可能被每轮重复解析、重复告警、重复刷新，放大 PgSQL/Redis 和日志压力。
4. fallback 决策可能存在回环或优先级混乱：本地容量/错误/Redis degraded/无本地凭据/外部池容量不足之间的状态转换不够明确。
5. 外部池不可用、容量不足或全部冷却时，可能没有快速短路，而是继续占用本地调度链路。

需要特别区分：

- “fallback 到外部池”：本地失败后走外部池。
- “直连外部池”：无本地凭据或配置指定时直接走外部池。
- “外部池 fallback 回本地”：如果存在，必须证明不会形成回环。
- “外部池 preflight 失败”：应不应影响本地凭据调度，需要按配置定义。

## 3. 复现矩阵

后续分析必须覆盖以下配置组合，不能只测单一 happy path。

### 3.1 本地凭据维度

- 有本地凭据，全部健康。
- 有本地凭据，但容量满。
- 有本地凭据，但 RPM 满。
- 有本地凭据，部分冷却。
- 有本地凭据，全部冷却。
- 有本地凭据，Redis scheduler degraded。
- 没有本地凭据。
- 本地凭据存在但全部 disabled。

### 3.2 外部池维度

- 外部池关闭。
- 外部池开启，但没有任何外部池。
- 外部池开启，有坏记录，例如空 `api_key`。
- 外部池开启，有一个可用池。
- 外部池开启，有多个可用池。
- 外部池开启，全部池容量不足。
- 外部池开启，全部池突发冷却。
- 外部池开启，部分池 429/5xx。
- 外部池开启，外部池模型不支持当前请求模型。

### 3.3 配置维度

必须逐项验证：

- `externalPoolsEnabled`
- `fallbackOnLocalCapacityExhausted`
- `fallbackOnNoAvailableCredentials`
- `fallbackOnLocalTransientExhausted`
- `fallbackOnSchedulerRedisDegraded`
- 是否存在 direct external pool 模式
- 是否存在 strict local-first 模式
- request admission 打开/关闭
- 每账号 RPM/并发限制
- 外部池 RPM/并发限制

### 3.4 异常维度

- PgSQL external pool snapshot 慢。
- Redis scheduler 慢。
- Redis external pool capacity 慢。
- 外部池 endpoint timeout。
- 外部池返回 401/403/429/500。
- 外部池非流式/流式 usage 缺失。
- 下游客户端中断。
- 长流集中完成。

## 4. 方案方向

后续设计必须满足：

1. 外部池资格 snapshot 单飞、缓存、有 TTL，有坏记录隔离；坏记录不能每个请求重复解析和告警。
2. 外部池调度 Redis/PgSQL 与本地凭据主调度故障域隔离；外部池慢不能拖住本地凭据选择。
3. fallback 决策必须是单向有限状态机，不允许外部池和本地池来回回环。
4. 每种失败类型必须有明确路由：
   - 本地容量耗尽；
   - 本地无可用凭据；
   - 本地 Redis scheduler degraded；
   - 本地 transient exhausted；
   - 外部池容量耗尽；
   - 外部池模型不支持；
   - 外部池认证失败；
   - 外部池全部冷却。
5. 外部池不可用时应快速降级或快速失败，并记录明确 `routeSubtype/selectionFailure`，不能表现成本地账号全部不可用。
6. 外部池坏配置应在管理面显示并禁用该池，不能污染请求热路径。

## 5. 验证计划

后续修复后至少需要：

- 单元测试：fallback classifier 和 route decision 状态机。
- 集成测试：本地凭据 + 外部池矩阵，覆盖上面的配置/容量/异常组合。
- chaos 测试：外部池 PgSQL snapshot 慢、Redis 慢、外部池 timeout 时，本地凭据成功请求不被拖慢。
- 真实协议测试：Claude Code CLI 多轮、tools、stream、non-stream 在外部池开启/关闭下都正常。
- 指标验证：usage 中 routeKind/routeSubtype/selectionFailure 能准确解释每次路由，不出现“本地有容量但被外部池拖死”的不可解释状态。

## 6. 残余风险与回滚

残余风险：

- 当前文档只是问题登记，未完成源码级根因闭环。
- 用户提供的 159 分析结论需要在当前仓库和现网证据上重新复核，不能直接当成最终修复依据。
- 外部池和本地凭据调度耦合可能跨越 provider、external_pool、token_manager、storage、Redis cache 多个模块，修复需要完整矩阵验证。

临时回滚/止血：

- 如果生产再次出现外部池开启后本地调度异常，优先关闭 `externalPoolsEnabled` 或禁用坏外部池记录。
- 不应通过降低本地账号 RPM/并发作为根治手段。
- 不应把外部池 fallback 默认扩大到所有本地错误，避免掩盖本地调度故障并制造回环。
