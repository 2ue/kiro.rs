# 项目分析：`kirocc-prox`

路径：`/Users/yuanfeijie/Desktop/procode/kirocc-prox`  
最新本地提交：`6acfa81`，2026-06-07  
相关度：很高

`kirocc-prox` 和当前项目方向接近，但它的核心价值不是功能更多，而是调度模块边界更清楚。当前项目后续做架构重构时，最应该参考它的调度拆分方式。

## 关键文件

| 文件 | 作用 |
| --- | --- |
| `internal/pool/selector.go` | 定义 `Selector` 接口和策略枚举 |
| `internal/pool/selector_strategies.go` | `round-robin`、`fill-first`、`least-used`、`least-inflight`、`weighted-least-inflight` |
| `internal/pool/scheduler.go`、`scheduler_default.go` | 管理账号快照、ready 列表、success/rate limit/auth error 状态 |
| `internal/pool/conductor.go`、`conductor_default.go` | 把 sticky、selector、reservation、refresh 编排起来 |
| `internal/pool/runtime_state.go` | Redis in-flight/cooldown/affinity lease |
| `internal/pool/cooldown.go` | backoff/cooldown 计算 |
| `internal/kiroclient/client.go` | Kiro upstream HTTP client、retry、200 JSON exception 识别、OTel |
| `internal/kiroclient/idle_reader.go` | stream body idle timeout |
| `internal/kiroproto/eventstream.go` | AWS EventStream 解析 |
| `internal/promptcache/reporter.go`、`tracker.go` | prompt cache 统计 |

## 架构拆分

### Selector

`internal/pool/selector.go` 定义：

```go
type Selector interface {
    Pick(ready []*Credential, model string) (*Credential, error)
}
```

选择器只做一件事：从已经过滤好的 ready 集合里挑一个账号。它不负责 Redis、refresh、cooldown 持久化，也不负责 session sticky。

这比当前项目 `src/kiro/token_manager.rs` 把策略、容量、Redis、sticky、refresh 写在一起更清楚。

可学习点：

- 当前项目可以抽出 `SelectionStrategy` trait。
- `priority`、`balanced`、`health_balanced` 只做排序/打分。
- Redis lease、RPM、并发、cooldown 过滤放在策略外层。
- 每个策略可以独立单测，不需要构造完整 `MultiTokenManager`。

### Scheduler

`DefaultScheduler` 负责：

- 注册账号。
- 保留 reload 前的 runtime state。
- 给 `Ready()` 返回 priority 排序后的账号。
- `MarkSuccess` 清除 cooldown。
- `MarkRateLimit` 写 account-level 和 model-level cooldown。
- `ReleaseReservation` 通过 `runtimeID` 防止 reload 后旧请求错误释放新账号。

这里一个特别值得学习的点是 `runtimeID`：账号 ID 被删除又重建时，旧请求释放并发槽不能误伤新对象。当前项目虽然有 lease id，但如果后续拆模块，可以把“账号运行时身份”概念明确化。

### Conductor

`DefaultConductor.Acquire()` 逻辑很清晰：

1. Redis affinity 命中时尝试 reserve。
2. 本地 affinity 命中时尝试 reserve。
3. sticky 不可用时 fall through 到 selector，但不覆盖原 binding，保留 sticky-on-recovery 语义。
4. selector 选中账号。
5. Redis `TryReserve` 或本地 `Reserve`。
6. 成功后 maybe refresh。

当前项目也有 sticky/fallback，但代码位置散在 manager 内。建议吸收 `Conductor` 这个边界：它是调度编排层，不应该关心 request 转换和 Kiro upstream。

## 调度策略

`selector_strategies.go` 有五种策略：

- `round-robin`
- `fill-first`
- `least-used`
- `least-inflight`
- `weighted-least-inflight`

最值得学习的是 `weighted-least-inflight`：

```go
left := s.inFlight * other.capacity
right := other.inFlight * s.capacity
```

它不用浮点，比较的是 `in_flight / max_in_flight`。对于容量不同的账号，这比只比较 `in_flight` 更合理。

当前项目已有 `health_balanced`，但它是综合 score。建议：

- 增加一个更直观的 `weighted_least_inflight` 策略，作为低风险策略。
- 或者把它作为 `health_balanced` 的 load 子项实现，管理端展示 load ratio。

需要注意的差异：

- `kirocc-prox` 的 `Ready()` 注释写的是 Priority descending，数字越大越优先。
- 当前项目通过 `min_by_key` 和 score，数字越小越优先。
- 后续借鉴策略时必须统一语义，不能把优先级方向弄反。

## Redis runtime lease

`internal/pool/runtime_state.go` 用 Lua 脚本原子完成：

- 检查账号 cooldown。
- 检查 model cooldown。
- 检查 account in-flight。
- `INCR` account/model in-flight。
- 设置 reservation key。

并提供：

- `TryReserve`
- `Extend`
- `Release`
- `SyncInFlight`
- `SetCooldown`
- `ClearCooldown`

当前项目已经有 Redis lease，但 `kirocc-prox` 的好处是把它作为 `RuntimeStateStore` 接口隔离。建议当前项目后续抽出：

- `SchedulerRuntimeStore`
- `InFlightReservation`
- `CapacityDecision`

这样调度逻辑可以不直接依赖 Redis 细节。

## Stream idle timeout

`internal/kiroclient/idle_reader.go` 用一个 wrapper 给 `Read` 增加 idle timeout。Kiro 有时会返回 200 + eventstream header，但后续没有 frame，普通 client 会一直挂住。

当前项目已经有一些 body/header timeout 和 stream idle 逻辑，但建议补一个专门测试：

- 上游返回 200。
- Content-Type 是 eventstream。
- body 一直不产生 frame。
- 当前服务必须在配置的 idle timeout 后结束请求，释放账号 lease，usage 记录为 stream/upstream timeout。

这里可以直接学习它的测试思路，不一定照搬实现。

## 200 JSON exception 识别

`internal/kiroclient/client.go` 在 HTTP 200 时检查 Content-Type，如果不是 `application/vnd.amazon.eventstream`，会读取 body 并按 AWS exception 处理。

这点非常重要：Kiro/AWS 有时会用 200 + JSON exception 表示 throttling/internal 错误。如果直接丢给 eventstream parser，会得到迷惑性的 frame parse 错误。

当前项目应确保已有同等能力，并补测试矩阵：

- 200 + `application/json` + `ThrottlingException`
- 200 + `application/json` + `InternalServerException`
- 200 + `text/html`
- 200 + 空 body

这些应该被归类成 protocol/upstream error，不应占用账号 lease，也不应向下游暴露原始 AWS 细节。

## OTel 和 tracing

`kirocc-prox` 的 `HTTPClient` 支持 `WithOTel`，请求时记录：

- `kiro.region`
- `kiro.endpoint`
- outgoing HTTP trace
- error record

当前项目已有 usage latency trace 和 call trace，但缺标准 trace exporter。建议后续增加可选 OTel：

- 默认关闭。
- 只记录结构化 metadata，不默认记录 body。
- body capture 必须有长度上限和脱敏。

## Prompt cache reporter

`kirocc-prox` 有 prompt cache reporter/tracker，但它更像 usage 侧统计，不如当前项目丰富。当前项目不需要照搬它的 cache 逻辑。

可以学习的只是：把 prompt cache 的“报告策略”和“请求路径 tracker”拆开，便于测试和管理端解释。

## 比当前项目强的地方

- 调度边界更清楚。
- 策略实现短小，易测，易解释。
- Redis runtime store 是独立接口。
- sticky-on-recovery 语义写得清楚。
- idle stream 和 200 JSON exception 有明确测试。
- OTel 接入点更自然。

## 当前项目比它强的地方

- PgSQL/Redis 状态更完整。
- usage 记录、dashboard、外部账号池、错误归一化更完整。
- account RPM、global dispatch queue、外部池 capacity 等能力更强。
- payload guard 和 tool-use 修复更深入。
- `/dfcache/*` 高缓存路由和 route policy 更复杂。

## 建议吸收方式

P0：

- 从 `token_manager.rs` 抽出 `SelectionStrategy`。
- 抽出 `RuntimeStateStore` / `InFlightReservation`。
- 抽出 `Conductor` 式的 acquire 编排层。
- 给 `health_balanced` 增加 score breakdown。

P1：

- 加 `weighted_least_inflight` 策略或 alias。
- 给 200 JSON exception 和 idle eventstream 增测试。
- 增可选 OTel trace exporter。

不建议：

- 不要把当前 PgSQL/Redis 模型退化成它的更轻量模型。
- 不要照搬它的 priority 方向。
- 不要为了接口清晰删掉当前已有的 RPM、queue、外部池、usage 能力。

