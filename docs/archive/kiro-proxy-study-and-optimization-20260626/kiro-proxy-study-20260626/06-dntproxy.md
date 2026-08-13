# 项目分析：`dntproxy`

路径：`/Users/yuanfeijie/Desktop/procode/kiro-research/dungnt1312__dntproxy`  
最新本地提交：`14d1252`，2026-06-26  
相关度：中

`dntproxy` 是多 provider 路由系统，不是 Kiro 专项生产网关。它最值得当前项目学习的不是 Kiro upstream，而是“选择失败原因结构化”和“fallback 边界判断”。

## 关键文件

| 文件 | 作用 |
| --- | --- |
| `internal/service/account-selector.go` | 账号选择、错误原因、策略、model lock |
| `internal/service/chat-service.go` | resolve → select → execute → fallback |
| `internal/service/chat-service-error-routing.go` | 哪些错误可以 fallback，哪些不能 |
| `internal/service/combo-handler.go` | combo fallback/round-robin |
| `internal/domain/*` | provider connection、cooldown、model lock |
| `internal/http/*` | Gin/SSE handler |
| `CLAUDE.md`、`AGENTS.md` | 架构说明和行为约定 |

## 选择失败原因

`account-selector.go` 定义：

- `no_active_credentials`
- `unsupported_model`
- `no_allowed_connection`
- `rate_limited`
- `model_locked`
- `unavailable`

并使用 `AccountSelectionError` 承载：

- kind
- provider
- model

这点比当前项目更清楚。当前项目有 `LocalPoolRouteStateKind`，但 acquire/fallback 路径没有统一的 failure reason 类型。

建议当前项目学习：

- 内部定义 `SelectionFailureReason`。
- route preflight、真实 acquire、外部池 select 都返回结构化原因。
- 管理端展示结构化原因。
- usage/error diagnostics 记录结构化原因。
- 对外仍然映射为统一英文报错，不暴露内部池、账号池、外部池等内部概念。

## 选择策略

`dntproxy` 支持：

- `weighted-random`
- `priority-fallback`
- `round-robin`
- pinned connection：`model@connectionId`

其中 priority 语义是数字小优先，这和当前项目一致。

当前项目已有更强的 health scheduler，但可以学习：

- 策略名面向用户更直观。
- round-robin 按 provider/model/allowed IDs 维护 rotation key。
- pinned connection 适合 debug/admin，不适合对外普通接口。

建议当前项目：

- 增加 admin-only account pinning，用于排查单账号。
- pinning 只能管理端测试，不进入下游公共接口，避免暴露账号概念。

## Model lock

`MarkUnavailable` 遇到 fallback error 时：

- 可设置 connection `RateLimitedUntil`。
- 如果 model 非空，写 `conn.ModelLocks[model] = until`。
- 成功后 `ClearError` 清理当前 model lock，并顺手清理过期 lock。

当前项目已有 model cooldown：

- `entry.model_cooldowns`
- `entry_cooldown_remaining(entry, model, now)`

但 `dntproxy` 的 model lock 文案更容易理解。建议当前项目管理端把 model cooldown 展示为“该模型临时不可调度”，不要让用户读内部字段。

## fallback 边界

`chat-service-error-routing.go` 的 `shouldFallbackToNextAccount`：

- `domain.IsNonFallbackStatus(status)` 返回 false。
- 如果错误文本包含：
  - invalid request
  - improperly formed request
  - malformed
  - invalid json
  - missing required
  - unsupported parameter
  - tool schema
- 则不 fallback。

这很重要。当前项目之前出现的 `Invalid tool use format` 属于请求体问题，不应该通过换账号掩盖。换账号只会放大错误和污染账号状态。

建议当前项目明确：

- 400 malformed/tool schema/request body invalid 不标记账号冷却。
- 不进入外部池 fallback。
- 记录原始错误和 Kiro body 结构摘要。
- 对外返回统一 invalid request 文案和 request id。

当前项目已有部分分类逻辑，但可以用 `dntproxy` 的规则补测试。

## Chat flow

`chat-service.go` 流程：

1. resolve model。
2. policy check。
3. combo/single 统一进入 combo handler。
4. select connection。
5. executor execute。
6. 成功清错误。
7. 失败判断是否 fallback。
8. 可 fallback 则 mark unavailable + exclude 当前账号。

当前项目 `KiroProvider` 也有类似多账号尝试，但建议吸收两个点：

- 每次失败都形成 attempt 结构，不只日志字符串。
- fallback 决策函数独立出来，可单测。

## 比当前项目强的地方

- 选择失败原因结构化。
- fallback/no-fallback 规则集中。
- model lock 概念更容易向管理端展示。
- pinned connection/debug 思路实用。
- 策略名更直观。

## 当前项目比它强的地方

- 当前项目 Kiro 专项协议和上游调用更完整。
- 当前项目 Redis lease、RPM、dispatch queue 更适合高并发。
- 当前项目 usage、latency trace、外部池更完整。
- 当前项目错误对外归一化更好，`dntproxy` 仍会把很多 provider/account 文案返回出去。

## 建议吸收方式

P0：

- 定义内部 `SelectionFailureReason`。
- 抽出 `should_fallback_to_next_account` 可单测函数。
- 给 malformed/tool schema/request body invalid 加“不换账号、不外部池 fallback”测试。

P1：

- 管理端把 model cooldown 呈现为 model lock。
- admin-only pin account 测试接口。

不建议：

- 不要把 `provider/account/connection` 原始概念暴露给下游。
- 不要用 JSON 文件锁替代当前 PgSQL/Redis。
- 不要把 weighted random 作为默认生产策略替代当前 health scheduler。
