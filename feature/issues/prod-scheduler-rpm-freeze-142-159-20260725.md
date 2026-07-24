# 生产低并发高 RPM / 前端慢 / 调度卡死排查：152.53.194.142 与 152.53.243.159

Status: `patched-in-working-tree / production-evidence-collected / needs-release-rollout-and-monitoring`

Severity: P0

## 状态

open / needs release rollout and post-release monitoring

## 范围

- 生产机器：
  - `152.53.194.142`
  - `152.53.243.159`
- 本地证据目录：
  - `tmp/prod-evidence/20260724-163730-142`
  - `tmp/prod-evidence/20260724-162943-159`
  - `tmp/prod-evidence/20260725-023209-current-142-159`
- 相关代码基线：
  - `v0.0.113`：`36b65ce509809120ba53bb46c6b536e3658a6129`
  - `v0.0.114`：`18b286efa47759b95b581f76a465a2bd9cb02983`
  - `v0.0.115 / HEAD`：`7982ab8f8116ad1130f54b735c940d1ca92140ad`

## 关键结论

这次不是单纯“全局并发没限制住”。生产现象由三类问题叠加：

1. **旧版本调度设计缺陷**：`v0.0.113` 把主业务调度 Redis 与 usage/dashboard 统计 Redis 混用；统计高基数操作会进入 Redis slowlog，影响本地账号调度热路径。
2. **失败路径 RPM 放大**：本地调度失败、全账号临时冷却或全账号禁用时，失败请求几毫秒返回；客户端/调用方重试会把每分钟请求数打到 1000+，但系统并发仍很低。
3. **风险/协议错误会打穿账号池**：旧配置没有本地账号池 risk circuit；多个账号连续 403 temporary suspended 或 200 JSON protocol error 时，一个请求可能继续尝试后续账号，扩大上游尝试次数并把账号池拖入冷却/禁用状态。

当前 `v0.0.115 / HEAD` 已包含针对 1、2、3 的代码层修复，但两台机器当前运行状态不同：

- `.159` 仍是 `0.0.113`，仍处于危险配置：`requestAdmission=null`、`fallbackOnSchedulerRedisDegraded=false`、`localPoolCircuitEnabled=false`、`observabilityRedis.urlSet=false` 且业务 Redis 中存在大量 usage slowlog。
- `.142` 当前是 `0.0.114`，已经有 `requestAdmission={rpm:300,maxConcurrentRequests:32,...}` 和 `fallbackOnSchedulerRedisDegraded=true`，但 `localPoolCircuitEnabled=false`，且外部池全局关闭。它仍无法阻断 risk/protocol 账号组被连续打穿的问题。

## 生产现象证据

### 152.53.243.159（v0.0.113）

采集时间：`2026-07-24 16:32~16:35 +08` 与 `2026-07-25 02:32 +08`。

运行版本：

```text
version=0.0.113
revision=36b65ce509809120ba53bb46c6b536e3658a6129
```

关键配置：

```json
{
  "credentialRpm": 60,
  "credentialMaxConcurrentRequests": 15,
  "dispatchGlobalMaxConcurrentRequests": 300,
  "requestAdmission": null,
  "externalPoolsEnabled": true,
  "fallbackOnSchedulerRedisDegraded": false,
  "localPoolCircuitEnabled": false,
  "observabilityRedisUrlSet": false
}
```

近 2 小时异常峰值：

- `2026-07-24 15:57~16:25 +08` 多个分钟出现 `Redis 调度协调状态不可用`。
- 单分钟请求峰值：
  - `16:10 +08`：1473 requests，1447 errors。
  - `16:22 +08`：1243 requests，1185 errors。
  - `16:23 +08`：1006 requests，942 errors。
- 主要错误：

```text
本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=1/2/3/.../30）
routeKind=local_credential
routeSubtype=local_error_no_fallback
```

当前 `2026-07-25 02:32 +08` 状态：

- 最近 10 分钟没有 Redis degraded，业务在跑：
  - 每分钟约 45~94 requests。
  - 最近 10 分钟几乎全成功。
- 但配置风险仍在：
  - `requestAdmission=null`
  - `fallbackOnSchedulerRedisDegraded=false`
  - `localPoolCircuitEnabled=false`
- Redis slowlog 仍显示 usage 统计高基数操作：
  - `DEL kiro_rs:59137:usage:records:index`
  - 大批量 `DEL kiro_rs:59137:usage:summary:top:conversation:*`
  - `HMGET kiro_rs:59137:usage:summary:cache_read` 带数万参数

这说明 `.159` 当前没卡死只是暂时没踩中热点；代码/配置仍可复发。

### 152.53.194.142（v0.0.113 → v0.0.114）

`2026-07-24 16:37 +08` 首轮采集时运行 `0.0.113`：

```text
version=0.0.113
revision=36b65ce509809120ba53bb46c6b536e3658a6129
```

当时关键现象：

- `36/36` runtime credentials 均为 `TemporarilySuspended`。
- 每分钟 1000+ 请求但几乎都是本地快速错误：
  - `16:21 +08`：1242 requests，1217 errors。
  - `16:33 +08`：1148 requests，1146 errors。
  - `16:34 +08`：1011 requests，1008 errors。
- 主要错误：

```text
本地账号调度容量暂不可用（Redis 调度协调状态不可用）
所有账号均已禁用（0/36）
流式 API 请求失败：403 Forbidden ... Your User ID is temporarily suspended
```

`2026-07-25 02:32 +08` 当前轻量复核时已运行 `0.0.114`：

```text
version=0.0.114
revision=18b286efa47759b95b581f76a465a2bd9cb02983
```

当前关键配置：

```json
{
  "credentialRpm": 70,
  "credentialMaxConcurrentRequests": 10,
  "dispatchGlobalMaxConcurrentRequests": 300,
  "dispatchMaxQueuedRequests": 100,
  "requestAdmission": {
    "rpm": 300,
    "maxConcurrentRequests": 32,
    "maxQueuedRequests": 64,
    "queueTimeoutMs": 1000
  },
  "externalPoolsEnabled": false,
  "fallbackOnSchedulerRedisDegraded": true,
  "localPoolCircuitEnabled": false,
  "observabilityRedisUrlSet": false
}
```

当前近 10 分钟没有流量，但近 2 小时仍有错误：

- `所有账号均已禁用（0/18）`：3110 条，集中在 `2026-07-25 01:07~01:31 +08`。
- `所有可用账号均处于上游临时冷却（可用: 2/2, 临时可调度: 0, ... retry_after_secs=N）`：大量出现。
- `upstream_failure class=protocol_error upstream_status=200 ... content_type=json reason=api_protocol_error`：持续出现。
- `upstream_failure class=risk_control upstream_status=403 ... reason=temporarily_suspended`：有样本。
- `request_rejection sampled request rejection`：少量，说明 requestAdmission 已开始生效，但不是主错误来源。

`.142` 当前主要矛盾已从 Redis degraded 转向：

- 账号组被禁用/临时冷却后，调用方仍继续高频请求；
- social/personal 账号出现 `200 JSON api_protocol_error`；
- 外部池全局关闭，所以不会接管；
- `localPoolCircuitEnabled=false`，所以 risk/protocol 账号组仍可能被连续探测。

相关 social/gmail 详细文档见：

```text
feature/issues/social-gmail-api-protocol-error.md
```

## 为什么“并发低但 RPM 高”成立

系统并发统计的是正在处理/等待的请求数量；RPM 是单位时间进入系统的请求数。

当本地调度或账号状态已经快速失败时，请求可能在 0~200ms 内返回：

```text
所有账号均已禁用
Redis 调度协调状态不可用
所有可用账号均处于上游临时冷却
```

如果调用方或客户端收到 429/5xx/临时错误后立即重试，系统会出现：

- 并发低：因为每个请求很快失败释放。
- RPM 高：因为失败路径每秒能处理很多次快速失败。
- 上游 RPM 低：因为大多数请求在本地失败，没真正发到 Kiro。
- 前端慢：同一时间 usage/dashboard 统计、错误写入、Redis/PG 统计聚合被大量错误记录放大。

因此这不是全局并发限制失效，而是缺少“失败后按下游 API key / 本地账号池状态做 admission backoff”的设计缺口。

## 根因

根因是旧版本在异常路径上把三个故障域耦合到一起：

1. 业务调度 Redis 与 usage/dashboard 统计共享 Redis 单线程，统计高基数操作能干扰 scheduler 热路径。
2. 本地调度失败或账号池不可用时，请求快速失败但缺少下游 request API key 维度的短 backoff，调用方重试会把入口 RPM 放大。
3. 风控/协议类上游错误缺少本地账号池级别 risk circuit，一个请求或一组突发请求可以连续探测多个账号，最终把可用账号池拖入临时暂停/禁用。

## 是否属于设计缺陷

是，至少包括三处设计缺陷或不完整设计：

1. **业务调度 Redis 与统计 Redis 不应共用故障域。**
   - `.159` 的 Redis slowlog 明确显示 usage summary/top/cache_read 高基数操作。
   - 调度热路径一旦和这些操作共享 Redis 单线程，就会出现热路径超时、scheduler degraded、账号池不可调度。

2. **本地调度临时失败不应允许同一个下游 key 立即高速重试。**
   - 旧版本没有 per-key local temporary backoff。
   - 快速失败路径会把调用方重试转换为系统内部高 RPM。

3. **连续 risk/protocol 错误不应打穿整个本地账号池。**
   - `.142` 出现全账号 `TemporarilySuspended`/`所有账号均已禁用`。
   - 旧配置缺少 local pool risk circuit，不能在同类风险错误出现后短时停止继续探测剩余账号。

## HEAD / v0.0.115 已覆盖的修复

当前 HEAD：`7982ab8 fix: harden scheduler risk backoff and usage telemetry`。

### 1. requestAdmission null 兼容

生产 `.159` 的 runtime_config 是：

```json
"requestAdmission": null
```

HEAD 已兼容显式 `null`，升级后会归一为默认：

```json
{
  "rpm": 300,
  "maxConcurrentRequests": 32,
  "maxQueuedRequests": 64,
  "queueTimeoutMs": 1000
}
```

验证：

```text
cargo test request_admission_has_conservative_defaults_and_explicit_zero_disables
结果：passed
```

### 2. per-key local temporary backoff

HEAD 在 `src/anthropic/request_admission.rs` 增加：

- `AdmissionLocalTemporaryBackoff`
- 每个 request API key 的 `local_temporary_backoff_until_ms`
- backoff 范围：1~8 秒
- 在 RPM 扣减之前拒绝，避免本地失败继续消耗 RPM token。

`src/anthropic/handlers.rs` 会把这些错误反馈到 admission：

- `Redis 调度协调状态不可用`
- `本地账号调度容量暂不可用`
- `local_scheduler_redis_degraded`
- `local_pool_risk_circuit_open`
- 队列满/排队超时/并发槽位满等本地临时错误

验证：

```text
cargo test local_temporary_backoff
结果：4 passed
```

### 3. local pool risk circuit

HEAD 在 `src/kiro/token_manager/manager.rs` 增加本地账号池风险 circuit：

- `localPoolCircuitEnabled`
- 窗口默认 60 秒
- 触发阈值默认 3 次失败
- 需要至少 2 个不同 credential
- 打开后默认暂停 30 秒

runtime migration v8 会把旧 persisted config 的：

```json
"localPoolCircuitEnabled": false
```

迁移为：

```json
"localPoolCircuitEnabled": true
```

验证：

```text
cargo test local_pool_risk_circuit
结果：1 passed
```

### 4. usage/statistics 与业务 Redis 故障域隔离

HEAD 的启动逻辑使用独立 `observabilityRedis`。如果没有配置独立 Redis，不会退回使用业务 Redis 做 usage/dashboard cache。

这能避免 `.159` 中看到的：

```text
usage:summary:*
usage:dashboard:*
usage:records:*
```

继续和 scheduler/local-pool/external-pool 调度 key 竞争同一个 Redis。

注意：如果要启用观测 Redis，应使用独立 Redis 实例/地址。当前代码会校验 observability Redis 与 business Redis 不是同一个 authority。

## 版本拓扑风险

本地 tag 拓扑不是线性：

```text
v0.0.109
├─ v0.0.110 → v0.0.111 → v0.0.112 → v0.0.113
└─ v0.0.114 → v0.0.115 / HEAD
```

merge-base：

- `v0.0.113` 与 `HEAD` 的 merge-base 是 `v0.0.109`。
- `v0.0.114` 与 `HEAD` 是线性关系。

这意味着从 `0.0.113` 升级到 `0.0.115` 不是普通小版本前进，而是从 113 分支切到主线。必须重点复核：

- endpoint 选择；
- CLI/IDE payload/header；
- eventstream/JSON 解析；
- thinking 透传；
- usage status/error_type；
- cooldown/risk circuit 语义。

## Kiro social / personal / Gmail 链路差异

### endpoint 选择

- `v0.0.100`：没有 `KIRO_API_KEY_DEFAULT_ENDPOINT=cli`。
- `v0.0.110` / `v0.0.113`：API key/headless 凭据默认 endpoint 为 `cli`。
- social/OAuth/Gmail 个人账号如果 credential 没有显式 `endpoint`，仍使用 `Config.defaultEndpoint`，默认 `ide`。
- 当前 `KiroProvider::endpoint_for` 逻辑仍是：

```text
credentials.endpoint.unwrap_or(config.defaultEndpoint)
```

风险：如果导入/迁移把 social 账号错误写成 `cli` endpoint，会改变协议 host/header/body。

### CLI GenerateAssistantResponse payload/header

`v0.0.110` / `v0.0.113`：

- CLI endpoint：
  - URL：`https://runtime.{region}.kiro.dev/`
  - `content-type: application/x-amz-json-1.0`
  - `x-amz-target: AmazonCodeWhispererStreamingService.GenerateAssistantResponse`
  - `host: runtime.{region}.kiro.dev`
  - body 重写 `origin=KIRO_CLI`
  - 会删除 `additionalModelRequestFields.thinking`

`v0.0.114` / HEAD：

- CLI endpoint 仍使用 runtime GenerateAssistantResponse。
- body transform 改为只处理 `origin/profileArn`，不再删除 schema-owned fields。
- `additionalModelRequestFields.thinking` 会被保留。

风险：如果上游 CLI runtime 不接受某些 thinking schema，v114/HEAD 相比 113 可能改变成功率；如果上游已经支持 thinking，则 v113 反而会丢能力。

### IDE/social 请求

IDE endpoint 当前：

- URL：`https://q.{region}.amazonaws.com/generateAssistantResponse`
- `content-type: application/json`
- `x-amzn-codewhisperer-optout: true`
- `x-amzn-kiro-agent-mode` 由 `resolve_agent_mode` 决定
- body 只注入 `profileArn`，不主动注入 `thinking`，不主动裁剪 `output_config.effort`

HEAD 中已有测试覆盖：

- 不凭空创建 `thinking`。
- 保留已有 schema-owned `thinking`。
- 不改变无 profile 的 body。

### 200 JSON / eventstream 处理

`v0.0.110` / `v0.0.113`：

- 流式请求如果 2xx 但不是 eventstream：
  - 读取 body 原文；
  - attempt error_type 为 `non_eventstream`；
  - 可能把 body 内容进入错误消息/日志；
  - 主要按 AWS exception 是否 retryable 判断。

`v0.0.114` / HEAD：

- 2xx 非 eventstream 或 body 读取失败改为严格分类：
  - `content_type=json/eventstream/other/missing`
  - `ApiUpstreamFailureKind::{Protocol,RiskControl,RateLimit,...}`
  - 错误消息不再带原始 body，只带 body_bytes、content_type、reason。
- 生产 `.142` 当前的 `api_protocol_error` 就是这个新分类：

```text
upstream_failure class=protocol_error upstream_status=200 public_status=200 body_bytes=...
content_type=json reason=api_protocol_error
```

这更安全，但也意味着旧版“看 body 文本判断”的线索消失；后续需要脱敏 body fingerprint/top-level keys 诊断，而不是存原文。

## 复现方案

### 本地/测试环境复现调度放大

1. 启动一套隔离 Postgres/Redis 和 kiro.rs。
2. 配置：
   - credential pool 多账号；
   - `requestAdmission` 关闭或设置很高；
   - `localPoolCircuitEnabled=false`；
   - `fallbackOnSchedulerRedisDegraded=false`；
   - 让 usage/dashboard 使用同一个业务 Redis（旧版行为）。
3. 注入 Redis scheduler 热路径超时或制造 usage high-cardinality slowlog。
4. 并发不高但客户端快速重试。
5. 观察：
   - `Redis 调度协调状态不可用`
   - `local_error_no_fallback`
   - 每分钟 1000+ errors
   - 上游真实发送量不高
   - global in-flight 不高

### 本地/测试环境复现 risk circuit

1. 多个本地凭据模拟返回 403 temporary suspended。
2. 旧配置：`localPoolCircuitEnabled=false`，观察单请求打多个账号。
3. 新配置：`localPoolCircuitEnabled=true`，观察达到阈值后：
   - 返回 `local_pool_risk_circuit_open`
   - 带 retry-after
   - 不继续探测剩余账号
   - admission 对该 request API key 施加短时 backoff

### social/gmail protocol error 复现

见 `feature/issues/social-gmail-api-protocol-error.md`。必须覆盖：

- `/v1` non-stream / stream
- `/ha` non-stream / stream
- `/cc` stream
- thinking on/off
- 小 prompt / 长上下文 / tool / 图片
- social endpoint=ide 与误配 endpoint=cli 的差异

## 已完成验证

本地验证命令均使用 scoped target，结束后自动删除构建产物：

```text
RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh request-admission-null -- cargo test --locked --all-targets request_admission_has_conservative_defaults_and_explicit_zero_disables
结果：1 passed

RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh fmt-check -- cargo fmt --all -- --check
结果：passed

RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh scheduler-risk -- bash -lc 'cargo test --locked --all-targets local_temporary_backoff && cargo test --locked --all-targets local_pool_risk_circuit'
结果：5 passed
```

## 2026-07-25 02:39~02:41 +08 追加只读复核

证据目录：

```text
tmp/prod-evidence/20260725-023209-current-142-159/raw/current/
```

关键文件：

- `159-monitor-023924.txt`
- `159-db-redis-024021.txt`
- `142-monitor-db-redis-024058.txt`

### 152.53.243.159 当前快照

- 容器仍运行 `0.0.113`，app 已运行约 41 小时，`readyz=200`，本轮采样 app CPU 约 `4.76% -> 9.37% -> 4.88%`。
- 最近 30 分钟没有再次出现 `Redis 调度协调状态不可用` 或 `所有账号均已禁用` 的本地错误风暴。
- 但本地启用凭据状态仍然不可用：
  - `credentials=15`
  - `disabled=15`
  - `enabled_not_deleted=0`
  - runtime `TemporarilySuspended=15`
  - 事件窗口：`2026-07-24 17:07:23~17:07:25 UTC`
- 最近 30 分钟请求基本成功，但都由外部池承接：
  - 每分钟约 `29~94` 请求；
  - 主要成功路由是 `/ha`、`/cc`、`/v1` 的 external pool；
  - 平均耗时较高，多个模型分钟级长流请求 max 可达 `129s~543s`。
- 结论：`.159` 当前“不卡”不是因为旧缺陷不存在，而是因为本地账号池已经不可用、流量主要由外部池接住；其旧版本配置仍有复发条件：`requestAdmission=null`、`fallbackOnSchedulerRedisDegraded=false`、`localPoolCircuitEnabled=false`、业务 Redis 仍承载 usage 高基数 key。

### 152.53.194.142 当前快照

- 容器当前运行 `0.0.114`，app/PG/Redis 已运行约 9 小时，`readyz=200`，本轮采样 app CPU 约 `0.02% -> 0.02% -> 0.36%`。
- 最近 30 分钟没有 usage 流量，说明当前“前端慢/系统卡住”不是本轮采样时的持续 CPU/Redis/PG 饱和。
- 当前启用凭据为：
  - `credentials=2`
  - `disabled=0`
  - `enabled_not_deleted=2`
  - runtime disabled reason 全空。
- 过去 2 小时仍有风险控制事件：
  - `credential_risk_controlled / TemporarilySuspended`: `18`
  - `credential_deleted`: `16`
  - 说明此前确实发生过账号组被上游风险控制/后续删除的窗口。
- 当前配置仍显示：
  - `requestAdmission={rpm:300,maxConcurrentRequests:32,maxQueuedRequests:64,queueTimeoutMs:1000}`
  - `externalPoolsEnabled=false`
  - `fallbackOnSchedulerRedisDegraded=true`
  - runtimeConfigMigrationVersion=`7`
  - `localPoolCircuitEnabled` 在当前查询路径下为 `null`，与 0.0.114 仍未启用本地池风险熔断的判断一致。

### 这轮复核对根因判断的影响

这轮快照强化了前面的结论：

1. “并发低但 RPM 高”不是并发限制失效，而是失败路径太快返回，调用方重试导致入口 RPM 放大。
2. `.159` 的主要历史缺陷仍是 0.0.113 的组合：无 request admission、scheduler degraded 不 fallback、业务 Redis 与 usage Redis 共故障域、本地池 risk circuit 缺失。
3. `.142` 从 0.0.113 到 0.0.114 后 requestAdmission 和 scheduler degraded fallback 已改善，但 risk/protocol 账号组打穿仍缺 0.0.115+ 的 local pool circuit。
4. 生产“前端慢”在错误风暴窗口更可能来自大量失败 usage 写入、dashboard/usage 聚合、日志写入和大 body 在最终失败前仍被 parse/guard，而不是当前采样时 Redis/PG 原始资源耗尽。

## 2026-07-25 入口 fast-fail 追加修复

本轮新增代码硬化，目标是解决“所有账号不可用/Redis degraded/risk circuit 已打开，但请求仍先反序列化、图片/tool 处理、payload guard、日志记录后才失败”的异常路径放大。

修改范围：

- `src/kiro/token_manager/manager.rs`
  - 新增 `local_pool_route_state_cached(model)`。
  - 该方法只使用本地内存快照，不做 scheduler Redis read/probe。
  - 保留 TooManyFailures 自动自愈，避免 fast-fail 让可自愈账号池永久停住。
- `src/kiro/provider.rs`
  - 暴露 `KiroProvider::local_pool_route_state_cached`。
- `src/anthropic/handlers/request_entry.rs`
  - 在 raw external direct/preflight 尝试之后、完整 typed parse 之前执行本地池 fast-fail。
  - 仅当外部池全局未启用或 manager 不存在时生效；如果存在外部池接管可能，仍让 normalized external fallback 正常工作。
  - 只提前拒绝这些明确状态：
    - `NoCredentials`
    - `AllDisabled`
    - `ProxyBlocked`
    - `SchedulerRedisDegraded`
    - `RiskCircuitOpen`
  - 不提前拒绝这些可能需要正常解析/排队/模型判定的状态：
    - `Ready`
    - `NoModelCompatible`
    - `AllCoolingDown`
    - `CapacityFull`
  - 对 `SchedulerRedisDegraded` 返回 `429 rate_limit_error` 并带 `retry-after`。
  - 对 `RiskCircuitOpen` 返回 `503 api_error` 并带 `retry-after`。
  - 对 `NoCredentials/AllDisabled/ProxyBlocked` 返回 `503 api_error`。
  - 记录 usage preflight rejection：
    - `local_pool_unavailable`
    - `local_pool_temporary_unavailable`
  - 对临时类状态调用 request admission 的 per-key local backoff，避免同一 request API key 继续高频进入 provider/Redis scheduler。
- `src/anthropic/request_admission.rs`
  - 新增上述两个 rejection reason。

性能边界：

- 正常可调度请求只多一次本地内存状态计算，不访问 scheduler Redis。
- 外部池启用且 manager 存在时不触发 fast-fail，避免阻断 external raw/normalized fallback。
- model 缺失或空字符串时不触发 fast-fail，继续走原有 invalid request 语义，避免把客户端错误误报成本地账号不可用。
- AllCoolingDown/CapacityFull 不抢跑，保留现有等待/队列语义。

追加验证：

```text
RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh fastfail-contract -- \
  bash -lc 'cargo fmt --all -- --check && cargo test --locked --all-targets local_pool_fast_fail'

结果：
- local_pool_fast_fail_maps_only_terminal_or_temporary_pool_states_for_five_rounds: passed
- local_pool_fast_fail_does_not_preempt_waitable_or_model_states_for_five_rounds: passed
- scoped target cleaned: size_kib=1729392 removed=true reservation_released=true
```

回归验证：

```text
RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh fastfail-focused -- \
  bash -lc 'cargo test --locked --all-targets local_temporary_backoff && cargo test --locked --all-targets local_pool_risk_circuit && cargo test --locked --all-targets request_admission_has_conservative_defaults_and_explicit_zero_disables'

结果：
- local_temporary_backoff: 4 passed
- local_pool_risk_circuit: 1 passed
- requestAdmission null/default/zero: 1 passed
- scoped target cleaned: size_kib=1724008 removed=true reservation_released=true
```

## 待发布/上线后的验证

## 2026-07-25 04:54 +08 当前态复核

本轮按 `kiro-prod-evidence-audit` 只读流程重新连接两台机器，证据目录：

```text
tmp/prod-evidence/20260725-044228-rpm-slow-142-159
```

采集范围包括主机资源、Docker 健康、PostgreSQL usage/活动、Redis stats/slowlog、runtime 关键配置和有限日志尾部。未修改生产配置、未重启服务、未写 Redis/PG。

### 152.53.243.159

当前运行：

```text
version=0.0.113
revision=36b65ce509809120ba53bb46c6b536e3658a6129
compose=/root/docker-compose/kiro-rs-2ue-59137
port=59137
```

当前主机和容器并未打满：

```text
load average: 0.17, 0.14, 0.10
app CPU: 3.86%
app RSS: 252 MiB
postgres RSS: 1.13 GiB
redis RSS: 412 MiB
healthz: ok
readyz: postgres=true redis=true redisRuntimeEvents=true
```

最近 30 分钟入口 RPM 并不高，约 `19~83 rpm`，但平均和最大耗时很高：

```text
20:56 UTC 24 req/min, 24 success, avg/max not in first run
20:55 UTC 76 req/min, 74 success, 2 error
20:54 UTC 66 req/min, 66 success
20:53 UTC 70 req/min, 67 success, 3 error
20:49 UTC 83 req/min, 83 success
```

按路由看，近 30 分钟几乎全部是外部池直连：

```text
routeKind=external_pool
routeSubtype=external_direct_policy
success=1256
error=14
avg duration ≈ 24.9s
max duration ≈ 503s
```

近 2 小时最慢请求集中在外部池，多个成功请求耗时 `250~720s`，且 first-token latency 有的达到几十秒到 128 秒。这说明 `.159` 当前“慢”的主要来源已经不是本地 scheduler 当场卡死，而是本地账号已不可调度后，流量全部转外部池，外部池/上游本身排队或超时很慢。

当前本地凭据运行态：

```text
credential_runtime_state total=310
disabled_reason empty=295
disabled_reason TemporarilySuspended=15
```

也就是说 `.159` 当前 15 个本地活跃/近期可用账号仍被标记为 `TemporarilySuspended`，不会承接本地流量；这不是本轮新代码能自动恢复的状态，升级只能降低后续放大和打穿风险。

`.159` 仍有两个旧版本风险：

- `version=0.0.113`，没有当前分支的 local-pool fast-fail / risk circuit / JSON content-type eventstream sniffing / dotted pricing fix。
- `runtime_config` 关键配置只读查询只返回 `observabilityRedisUrlSet=false`；没有看到 `requestAdmission`、`fallbackOnSchedulerRedisDegraded`、`localPoolCircuitEnabled` 等新字段，符合旧版本配置形态。

### 152.53.194.142

当前运行：

```text
version=0.0.114
revision=18b286efa47759b95b581f76a465a2bd9cb02983
compose=/root/docker-compose/kiro-rs
port=40182
```

当前主机和容器也没有打满：

```text
load average: 1.10, 0.36, 0.24
app CPU: 0.09%
app RSS: 142 MiB
postgres RSS: 162 MiB
redis RSS: 257 MiB
healthz: ok
readyz: postgres=true redis=true redisRuntimeEvents=true
redis instantaneous_ops_per_sec=0
redis slowlog_len=0
```

当前 `2026-07-25 04:54 +08` 近 2 小时没有普通 usage traffic；因此它不是“此刻仍被请求打卡死”。近 8 小时只看到 admission 采样诊断记录：

```text
error_type=request_rejection
error_message=sampled request rejection
model=unknown
usage=0
```

源码确认这是 request API key admission 层的采样诊断 usage，`error_metadata` 记录 `sampled/observedCount/stage/reason`，不是上游模型成功/失败 usage，不应参与正常模型计费判断。

`.142` 当前 runtime_config 包含：

```json
{
  "requestAdmission": {
    "rpm": 300,
    "maxConcurrentRequests": 32,
    "maxQueuedRequests": 64,
    "queueTimeoutMs": 1000
  },
  "observabilityRedisUrlSet": false
}
```

本地凭据运行态：

```text
credential_runtime_state total=2
disabled_reason empty=2
warmup_remaining=2
```

这说明 `.142` 当前并非账号持久禁用，而是可用账号很少且处于 warmup；如果入口突然并发很高，仍会更多依赖 admission/排队/外部池策略。

### 对“并发低但 RPM 高”的最新判断

两台机器当前态进一步拆开了两个不同现象：

1. `.159` 当前入口 RPM 不高，但请求耗时长，因为本地账号处于 `TemporarilySuspended` 后几乎全走外部池；慢点在外部池/上游响应，不是 CPU/PG/Redis 当前打满。
2. `.142` 当前无近 2 小时业务流量，不能用当前态证明“正在卡死”；历史的 `sampled request rejection` 是 admission 采样，不是外部池 0 计费，也不是本地账号禁用。

因此，“并发低但 RPM 高”仍是旧窗口里的真实设计问题；当前窗口的 `.159` 更多是“本地池不可调度 + 外部池慢 + 旧版本无当前修复”，`.142` 当前则是空闲/低流量状态。

### 新增上线后监控点

发布后除了原监控项，还必须区分三类计数，避免把不同问题混在一起：

- `request_rejection sampled request rejection`：admission 采样诊断，模型固定 `unknown`、usage 为 0；看 `error_metadata.reason/stage`。
- `local_pool_unavailable / local_pool_temporary_unavailable / admission_local_temporary_backoff`：当前分支新增的本地池快速拒绝/短 backoff，应控制入口放大。
- `external_pool external_direct_policy success` 且 `duration_ms` 很高：外部池/上游慢，不是本地 scheduler Redis degraded。

发布 `0.0.115` 或更高版本后，每台生产机必须验证：

1. 版本：

```text
/api/admin/system/version 或 docker inspect labels
version >= 0.0.115
revision = 目标 commit
```

2. runtime_config：

```json
{
  "requestAdmission": {
    "rpm": 300,
    "maxConcurrentRequests": 32,
    "maxQueuedRequests": 64,
    "queueTimeoutMs": 1000
  },
  "fallbackOnSchedulerRedisDegraded": true,
  "localPoolCircuitEnabled": true
}
```

3. Redis：

- business Redis slowlog 不应再出现大量 `usage:summary:*` / `usage:dashboard:*` / `usage:records:*`。
- 如需观测 Redis，应配置独立 Redis authority。

4. usage：

- Redis degraded 发生时，应该看到 request admission 的本地短 backoff，不应每分钟继续 1000+ 本地错误。
- risk/protocol burst 发生时，应该看到 `local_pool_risk_circuit_open`，不应一轮打穿整个账号池。
- `.142` social/gmail 的 `api_protocol_error` 应单独隔离，不应误判为 Redis 调度问题。

## 当前建议动作

1. 优先把 `.159` 从 `0.0.113` 升级到 `0.0.115+`。
   - 这是仍存在 Redis/statistics 共享、无 requestAdmission、无 scheduler degraded fallback、无 risk circuit 的机器。
2. `.142` 虽已到 `0.0.114`，仍建议升级到 `0.0.115+`。
   - `requestAdmission` 已有，但 `localPoolCircuitEnabled=false`，仍会出现账号组被打穿。
3. `.142` 的 social/gmail `api_protocol_error` 不要和 Redis degraded 混为一谈。
   - 这需要按 social/personal 协议单独修复/隔离。
4. 发布后至少监控 30~60 分钟：
   - usage 每分钟 errors；
   - Redis slowlog；
   - `local_pool_risk_circuit_open`；
   - `admission_local_temporary_backoff`；
   - `Redis 调度协调状态不可用` 是否下降；
   - dashboard/前端接口响应时间。
