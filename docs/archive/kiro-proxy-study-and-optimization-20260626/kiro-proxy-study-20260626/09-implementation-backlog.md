# 当前项目后续学习落地清单

本篇把各项目的优点转成当前 `kiro.rs` 可执行的后续改造方向。当前任务不实施，这里只给学习与实现优先级。

## P0：应该优先做

### 1. 拆分 `src/kiro/token_manager.rs`

来源：`kirocc-prox`、`ndycode/kiro-rs`

建议拆分：

- `selection.rs`：候选过滤、策略、score breakdown。
- `capacity.rs`：RPM、并发、global capacity、queue。
- `lease.rs`：local/Redis in-flight lease、lease guard、过期清理。
- `cooldown.rs`：TransientFailureKind、cooldown/backoff、model cooldown。
- `session.rs`：sticky binding、soft failure、Redis session binding。
- `runtime_store.rs`：Redis runtime state。
- `admin.rs`：管理端 snapshot/update。
- `report.rs`：success/failure/latency report。

要求：

- 第一阶段只做无行为变化拆分。
- 所有现有测试必须通过。
- 拆分后再谈策略优化。

### 2. 结构化调度失败原因

来源：`dntproxy`

内部新增原因模型：

- `NoCredentials`
- `AllDisabled`
- `ModelUnsupported`
- `ProxyUnavailable`
- `RpmLimited`
- `Cooldown`
- `ModelCooldown`
- `ConcurrencyFull`
- `GlobalConcurrencyFull`
- `QueueFull`
- `StickyUnavailable`
- `RedisDegraded`
- `ExternalCapacityFull`
- `ExternalCooldown`

用途：

- usage diagnostics。
- admin UI 展示。
- call trace。
- release/test 断言。

对外：

- 仍使用统一英文错误。
- 不暴露 credential/external pool/backup pool 等内部代码概念；面向下游统一使用 account/request/model 口径。
- 下游只看到 account/model/request 级别概念和 request id。

### 3. 真实压测工具

来源：`kiroxy/scripts/loadtest`、`cp-coder9` 测试矩阵

建议新增 `tools/loadtest`：

- 支持 stream/non-stream。
- 支持 Anthropic Messages 请求。
- 支持 long session。
- 支持 thinking。
- 支持 tool_use/tool_result。
- 支持 cache_control/high-cache route。
- 支持并发、总请求数、ramp-up。
- 统计 TTFB、first thinking、first text、total latency、status、error type。
- 支持抓取 RSS 内存。
- 输出 p50/p95/p99。

需要覆盖：

- 正常请求。
- 账号 429。
- 账号 400 malformed。
- 上游 200 JSON exception。
- 上游 eventstream stall。
- 客户端断开。
- 突发大并发。
- 失败后恢复。
- RPM 和 max concurrent 生效。

### 4. Kiro malformed/tool-use 回归矩阵

来源：`9router`、本地 `kiro2api`、`Kiro-Go`

必须覆盖：

- 无 tools 的 follow-up，历史有 tool_use/tool_result：结构化 tool 必须 flatten。
- 有 tools 但 tool_result orphan：fold 回 user text。
- 重复 tool_use_id：rename 或去重，最终 Kiro body 合法。
- 空 tool_result：不能发非法空结构。
- error tool_result：按 Kiro 兼容格式输出。
- tool input JSON 不能被 max_tokens 截断。
- 400 malformed 不切账号、不外部池 fallback。

### 5. Stream 防卡死测试

来源：`kirocc-prox`、`kiroxy`、`9router`

必须覆盖：

- 200 + eventstream header + body idle。
- 200 + JSON exception。
- stream 中途断开。
- error event 后必须终止。
- client dropped 后释放 lease。
- thinking-only / empty visible end_turn 行为符合预期。

## P1：P0 稳定后做

### 1. `weighted_least_inflight` 策略

来源：`kirocc-prox`

用途：

- 给用户一个比 `health_balanced` 更容易理解的策略。
- 适合容量不同但健康差异不大的账号池。

注意：

- 当前项目优先级是数值越小越高。
- 文档和 UI 必须说清楚。
- 可以先作为 `balanced` 的替代/增强，不要影响默认策略。

### 2. Health score breakdown

来源：`kiroxy`

管理端展示：

- priority contribution。
- load ratio。
- recent error rate。
- latency EWMA。
- probation。
- selection pressure。
- recent request count。
- RPM wait。
- concurrency usage。

目标：让用户知道“为什么这个账号被选中/没被选中”。

### 3. profileArn region 自愈

来源：`Kiro-Go`

实现方向：

- 从真实 profileArn 解析 region。
- streaming/models/usage/subscription 都优先使用 profileArn region。
- profileArn lookup 不支持时 suppress。
- 自愈成功后持久化。
- call trace 记录 region source。

### 4. 真实 Kiro cachePoint feature flag

来源：本地 `kiro2api`、`kiroxy`

初期策略：

- 默认关闭。
- 只对 tool-level `cache_control`。
- 插入 `cachePoint` 后真实测试 Kiro 接受。
- 失败自动降级。
- usage 记录是否 applied。

不要：

- 不要一次性处理所有 system/message cache_control。
- 不要替代现有 reported usage。

### 5. Endpoint failover 管理开关

来源：`kiroxy`、`9router`

策略：

- 默认关闭。
- 账号级或全局开关。
- endpoint order 根据 auth method 决定。
- 只对 429/network/5xx/200 JSON throttle fallback。
- 400 malformed 不 fallback。
- call trace 完整记录 endpoint attempts。

### 6. OTel / trace exporter

来源：`kirocc-prox`、本地 `kiro2api`

要求：

- 默认关闭。
- 不默认记录 body。
- body capture 必须长度上限和脱敏。
- trace id 与 request id 关联。
- usage/call trace 仍保留，不依赖 OTel。

## P2：谨慎评估

### 1. Full response true cache

来源：`pluto2sun/kiro2api`

只建议学习 normalization，不建议默认实现 response cache。

如果未来实现：

- 只允许无 tools/images/web/search 的纯文本。
- TTL 很短。
- admin 显式开启。
- usage 明确标记 cache hit。
- 禁止用于 Claude Code 写文件/读文件/工具链场景。

### 2. OpenAI Responses API

来源：`Kiro-Go`

可作为扩展兼容能力，但不是当前系统主线。

### 3. 桌面账号管理器能力

来源：`Kiro-account-manager`

可以学习 batch check、账号详情、profileArn 自愈，不建议把桌面登录/MITM/证书逻辑内置到现网服务。

## 不建议学习的内容

- 不要把 PgSQL/Redis 退化到内存或 JSON 文件。
- 不要默认暴露账号/外部池/备用池等内部概念给下游。
- 不要默认开启 endpoint failover。
- 不要默认 full response cache。
- 不要用 prompt injection 替代真实 thinking 模型。
- 不要在请求热路径加入概率 retry。
- 不要把多 provider registry 的复杂度搬进当前 Kiro 专项项目。

## 推荐后续实施顺序

1. 先补测试和压测工具。
2. 再无行为变化拆分 `token_manager.rs`。
3. 加结构化 failure reason。
4. 加 score breakdown 和管理端解释。
5. 小步引入 `weighted_least_inflight`。
6. 做 profileArn region 自愈。
7. feature flag 实验真实 cachePoint。
8. feature flag 实验 endpoint failover。

这样能先降低维护和验证风险，再吸收外部项目的能力，不会因为“重构”影响现网调度和下游接口。
