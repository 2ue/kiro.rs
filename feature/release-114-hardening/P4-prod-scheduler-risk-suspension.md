# P4 生产 Redis 调度退化与整池上游风控禁用

日期：2026-07-24

## 状态

已完成只读生产复核，待后续代码修复与验证。

生产证据来源：

- `152.53.243.159`：部署 `kiro-rs-2ue-59137`，版本 `0.0.113`，revision `36b65ce509809120ba53bb46c6b536e3658a6129`。
- `152.53.194.142`：部署 `kiro-rs`，版本 `0.0.113`，revision `36b65ce509809120ba53bb46c6b536e3658a6129`。

本地 raw 证据目录：

- `tmp/prod-evidence/20260724-162406-152-53-243-159/`
- `tmp/prod-evidence/20260724-162036-152-53-194-142/`

raw 证据可能包含生产标识，不要打包或外发。

## 现象

- 前端/admin 访问非常慢。
- 用户观测到“当前并发低，但 RPM 很高”。
- 本地账号池不可用，流量转向外部池或直接返回本地无账号/调度失败。
- 最终本地账号全部不可用。

## 结论

这次不是单一原因。

直接后果是：Kiro/AWS 上游对本地账号返回了 `403 TEMPORARILY_SUSPENDED`，系统按 `upstream_risk_controlled` 自动把凭据持久禁用。

但导致事故放大的系统侧问题包括：

1. `0.0.113` 中 Redis scheduler 热路径会被 Redis 调度协调异常打进 degraded/backoff。
2. Redis degraded 期间大量请求快速失败，形成高 completed-request RPM；这个 RPM 是结果/中间过程，不一定是最初原因。
3. 全局并发限制不能限制这种快速失败 RPM，因为这些请求没有长期占用 upstream in-flight。
4. 同批账号在短时间内持续暴露于多会话 stream/大 payload 流量，增加上游风控概率。
5. 上游开始返回 `TEMPORARILY_SUSPENDED` 后，系统逐个禁用账号，但没有整池风控熔断，导致剩余账号继续被探测，最终整池清零。
6. 老版本前端 dashboard/usage 聚合较重，事故后外部池 fallback 和 usage 记录压力叠加，会让前端表现为慢。

## `.159` 时间线

UTC 时间：

- `07:55:24-07:55:37`：新增一批凭据。
- `07:56:12`：`batch_update_credentials` 修改 15 个凭据的 RPM/并发。
- `07:57:02`：开始出现 Redis scheduler degraded：
  - `routeKind=local_credential`
  - `routeSubtype=local_error_no_fallback`
  - 错误：`本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=2）`
  - 样本没有 `credentialAttempts`，说明失败在本地调度阶段，没有打到 Kiro 上游。
- `07:57-08:23`：反复出现 Redis scheduler degraded/backoff，快速失败导致分钟 RPM 放大。
- `08:24`：估算 completed-record in-flight 达到 106，其中 85 个为超过 60 秒的长请求。
- `08:31:40-08:31:57`：仍有本地账号成功 200。
- `08:31:56-08:31:59`：30 个凭据被 `system-scheduler` 自动禁用：
  - `trigger=upstream_risk_controlled`
  - `reason=TemporarilySuspended`
  - 上游摘要为 Kiro/AWS `403 Forbidden` + `TEMPORARILY_SUSPENDED`
  - `availableCredentials` 从 29 递减到 0。

关键分钟聚合：

- `07:58`：1280 total，1275 errors。
- `08:09`：1242 total，1163 errors。
- `08:10`：1473 total，1447 errors。
- `08:22`：1243 total，1185 errors。
- `08:23`：1006 total，942 errors。

事故窗口 `07:50-08:35` 最大流量来源：

- `/cc/v1/messages` + `claude-opus-4-8` + stream：
  - 8903 total
  - 2390 success
  - 6513 errors
  - 6509 local scheduler no-fallback errors
  - 1664 distinct conversation IDs

这说明不是单一 conversation，而是广泛多会话/多客户端请求或重试。

## `.142` 时间线

Asia/Shanghai 时间：

- `15:54`：批量修改凭据 RPM/并发。
- `15:57-16:25`：Redis scheduler degraded 快速失败反复出现。
- `16:32`：61 个凭据被 `TemporarilySuspended` 风控。
- `16:32-16:35`：错误变为 `所有账号均已禁用（0/36）`。
- 复核时 `credentials` 已为 0，因为禁用账号已被删除/清空；但 `credential_events` 保留了 61 条 `credential_risk_controlled / TemporarilySuspended`。

关键分钟聚合：

- `15:58`：1476 total，1447 errors。
- `15:59`：1208 total，1176 errors。
- `16:09`：1024 total，971 errors。
- `16:10`：1328 total，1318 errors。
- `16:21`：1242 total，1217 errors。
- `16:22`：998 total，997 errors。
- `16:23`：1011 total，1011 errors。
- `16:34`：1011 total，1008 errors。

## 关于“上游封控是不是因为并发异常”

可能有关，但当前证据不能证明上游风控算法的内部条件。

能证明的是：

- 两台机器都在导入/调参后出现 Redis scheduler degraded 和快速失败风暴。
- 两台机器在风控前都有持续多会话 stream 流量和部分长请求。
- 两台机器随后都收到真实上游 `TEMPORARILY_SUSPENDED`。
- 两台机器都缺少“出现多个上游风控后立即停止继续探测本地池”的整池熔断。

因此更稳妥的判断是：

> 异常并发/调度退化/重试放大很可能提高了触发上游风控的概率；但上游是否以并发、RPM、IP、账号行为、payload 模式或综合风险分作为直接封控条件，无法从本地证据单独证明。

## 已有缓解在当前 main 中的覆盖

当前 main 已经包含部分相关缓解：

- `fallbackOnSchedulerRedisDegraded` 默认改为 true。
- runtime config migration 会把旧的 broad fallback 意图迁移到 scheduler degraded fallback。
- 外部池 dispatch wait 加了边界。
- 增加 `observabilityRedis`，并校验不能和业务 scheduler Redis 共用同一 Redis authority。
- usage/dashboard Redis 从核心调度 Redis 分离为 observability Redis。
- 旧 dashboard full endpoint 降重，admin-ui 改成拆分加载。

这些能降低 Redis scheduler degraded 对核心链路和前端的影响，但还不能完全解决“上游风控后整池被快速烧光”。

## 后续必须修复

1. 增加本地账号池风控熔断。
   - 短窗口内 N 个不同凭据出现 `TEMPORARILY_SUSPENDED`/security lock 后，立即打开 local-pool risk circuit。
   - circuit 打开后停止继续探测剩余本地凭据。
   - 有外部池时转外部池；无外部池时返回受控 retryable 错误。

2. 风控 auto-disable 保留单账号禁用，但不得继续把剩余账号全部探测到死。

3. 批量导入/批量改并发后强制缓启动。
   - 新增/刚更新凭据进入 ramp-up。
   - 初期限制高并发、高 RPM、高 token、高 cache route 选择。
   - 成功样本积累后逐步放量。

4. request API key 必须成为准入与诊断实体。
   - 按 key 维度限制 RPM、并发、排队。
   - usage 记录必须能按 key 聚合，避免 0.0.113 这种事故后无法快速定位下游来源。

5. scheduler degraded 期间要对同 key/同 route 做短 backoff，避免无限快速失败把 RPM 放大。

6. 生产部署必须保证业务 Redis 与 observability Redis 是不同 Redis 服务/容器，而不是不同 DB 或 key prefix。

## 验证计划

修复后至少执行：

1. Redis scheduler latency 注入 + usage/dashboard 写入压力，验证主链路不被 observability 拖垮。
2. scheduler degraded fallback 测试，验证不会出现 `local_error_no_fallback` 风暴。
3. mock Kiro upstream 对多个凭据返回 `TEMPORARILY_SUSPENDED`，验证 risk circuit 打开后不继续烧剩余账号。
4. 批量导入/批量调并发后 ramp-up 测试，验证不会立即满速打新账号。
5. request API key admission 测试，验证单 key 重试不会拖垮全局。
6. 前端 dashboard 在大 usage 表下的响应时间测试。

