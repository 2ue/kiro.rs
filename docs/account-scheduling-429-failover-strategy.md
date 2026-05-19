# 账号调度、429 冷却与故障转移最终方案

本文档记录当前项目在账号调度、sticky 会话、429 / 5xx / 认证错误处理上的现状、问题、设计目标、错误分类、指数退避、fallback 策略、永久不可调度策略、前后端可观测性和测试清单。

目标是让后续实现者脱离当前对话也能完整理解方案，并按本文稳定实施，不依赖口头上下文。

## 状态

- 文档状态：方案设计完成，尚未实施业务代码。
- 适用范围：`kiro.rs` 当前多凭据账号池、Anthropic-compatible API、流式/非流式请求、MCP/工具调用、Admin UI 与 Console UI。
- 参考项目：`~/Desktop/project/sub2api`，但只学习其“可调度性前置、sticky 健康检查、状态影响调度”的架构思想，不照搬其 reset-time 驱动的 429 策略。
- 核心结论：不能只靠 `priority` / `balanced` / round-robin 解决 429；必须把账号健康状态变成调度前置条件。

## 背景

当前系统支持多个 Kiro 凭据，通过 `MultiTokenManager` 选择账号并由 `KiroProvider` 调用上游。

用户观察到的问题是：

1. 某个账号持续返回 429 时，后台日志会长期反复调用这个账号。
2. 流式请求会在重试中等待很久，最终仍然失败。
3. 同步请求更容易直接返回失败。
4. 前端账号列表看不出该账号正在 429 或已经不可用。
5. usage 记录里的失败请求经常没有 `credentialId`。
6. sticky 会话可能让一个长会话持续粘在坏账号上。

这些现象成立，但原因不只是“没有轮询”。真实原因是：当前 429 没有进入账号级调度状态，只是一次请求内部的瞬态错误。

## 对用户假设的辨别

用户提出：“账号调用 429 或失败可能是临时的，也可能是账号确实不可用了；应该区分哪些原地重试，哪些直接 fallback 到其他账号；需要指数级退避，直到彻底禁用，或者以某种形式让账号不能被调度，除非手动恢复。”

这个方向是正确的，但需要细化：

1. 429 不能一律永久禁用。
2. 429 也不能一律原地重试。
3. 本系统里的 429 通常没有明确恢复时间，因此不能把 sub2api 的 `resetAt` 策略原样套过来。
4. Kiro 429 更应视为“未知恢复时间的临时限流”：当前请求立即 fallback，后续短冷却，到期后探测恢复。
5. 只有长期统计上高度确认账号不可用，才进入“人工恢复要求”；默认不因为 429 直接 `disabled=true`。
6. 认证永久失效、refresh token revoked、配置错误等应进入人工恢复状态。
7. 5xx / 408 / 网络错误通常不应直接禁用账号，但可以短暂惩罚或影响调度评分。
8. sticky 是偏好，不是强制；sticky 账号不健康时必须被跳过或解绑。
9. “原地重试”只适合少数可能瞬间恢复且换号没有收益的错误；大部分账号级 429 应该立即 fallback 到其他健康账号。
10. 用户随口举的“最近 100 次”可以变成一个可配置统计窗口，但不能简单理解为“100 次都是 429 就禁用”；必须排除同一请求 retry 放大的样本，并判断是否存在全局上游波动。

## 当前代码链路

### API 主调用

文件：`src/kiro/provider.rs`

函数：`call_api_with_retry(...)`

当前行为：

1. 最大重试次数为 `min(total_credentials * MAX_RETRIES_PER_CREDENTIAL, MAX_TOTAL_RETRIES)`。
2. 当前常量：
   - `MAX_RETRIES_PER_CREDENTIAL = 3`
   - `MAX_TOTAL_RETRIES = 9`
3. 每次 attempt 都通过 `token_manager.acquire_context_for_session(model, conversation_id, excluded_ids)` 获取账号。
4. 402 且被识别为 monthly request limit 时，调用 `report_quota_exhausted(ctx.id)`。
5. 401 / 403 调用 `report_failure(ctx.id)`。
6. 429 / 408 / 5xx 只按“上游瞬态错误”处理：
   - 写日志；
   - 保存 `last_error`；
   - 若存在 `conversation_id`，调用 `record_session_soft_failure(session_id, ctx.id)`；
   - 如果 sticky 软失败达到阈值，把账号加入本次请求内的 `excluded_ids`；
   - sleep 后继续；
   - 不调用账号级 cooldown；
   - 不解绑 sticky；
   - 不记录账号级 429 状态；
   - 不持久化事件。

这意味着 429 不会影响后续请求的账号调度。

### MCP / 工具调用

文件：`src/kiro/provider.rs`

函数：`call_mcp_with_retry(...)`

当前行为：

1. 使用 `token_manager.acquire_context(None)`。
2. 不带 session。
3. 没有 request-local `excluded_ids`。
4. 429 / 408 / 5xx 只 sleep 后继续。
5. 不记录 sticky soft failure。
6. 不记录账号 cooldown。

因此工具调用场景比普通对话更容易反复打到同一个 429 账号。

### Sticky 会话

文件：`src/kiro/token_manager.rs`

关键逻辑：

1. sticky TTL：`SESSION_BINDING_TTL_SECS = 6 * 60 * 60`。
2. 同一 session 绑定账号连续软失败达到 `MAX_SESSION_SOFT_FAILURES = 2` 后，本次请求允许临时 fallback。
3. `get_bound_credential(...)` 只检查：
   - 是否在本次请求 `excluded_ids` 中；
   - 是否 disabled；
   - 是否支持模型。
4. 不检查 429 冷却，因为当前没有 429 冷却状态。
5. fallback 到其他账号成功后，不一定改绑 sticky。

当前绑定逻辑的含义是：

```text
如果 session 已经绑定 A，本次临时 fallback 到 B 并成功，
只要 A 的绑定还存在，就不会把 session 改绑到 B。
下一次请求 excluded_ids 清空后，又可能继续命中 A。
```

这会造成“当前请求临时绕过，下一请求继续粘坏号”。

### 错误 usage 记录

文件：`src/anthropic/handlers.rs`

当前 provider 返回失败时，handler 使用：

```text
attach_credential(None, None, false, false)
```

所以错误记录无法显示最后尝试的账号，更无法显示完整 attempt 链路。这不是前端展示问题，而是错误对象没有携带账号上下文。

## sub2api 可借鉴点

参考项目：`~/Desktop/project/sub2api`

### 可调度性是一等概念

文件：`backend/internal/service/account.go`

sub2api 使用 `IsSchedulable()` 作为调度前置条件。账号如果处于以下状态，不会被调度：

1. inactive。
2. schedulable=false。
3. expired。
4. overloaded。
5. rate limited。
6. temp unschedulable。
7. quota exceeded。

这点是当前 `kiro.rs` 缺失的核心。

### 429 不等于永久禁用

文件：`backend/internal/service/ratelimit_service.go`

sub2api 的 `HandleUpstreamError(...)` 对 429 的处理是：

1. 调用 `handle429(...)`。
2. 尝试解析 reset headers / body。
3. 能解析 reset 就 `SetRateLimited(account.ID, resetAt)`。
4. 解析不到时，使用可配置 fallback cooldown。
5. `shouldDisable=false`。

这说明 429 是“临时不可调度”，不是“永久坏号”。

需要注意：sub2api 的很多平台能从 header/body 中解析出相对可信的 reset 时间；`kiro.rs` 当前遇到的 Kiro 429 往往没有明确恢复时间。因此本文后续策略以“无 reset 时间的 429”为默认主路径，reset 时间只作为兼容分支。

### Sticky 命中也必须重新检查健康状态

文件：`backend/internal/service/openai_account_scheduler.go`

sub2api 在 sticky 命中后会判断：

```text
如果账号不可调度，则删除 sticky session 并返回 nil。
```

这点应该直接吸收进 `kiro.rs`：sticky 只能命中健康账号。

### 状态变更要立即影响调度

文件：`backend/internal/repository/account_repo.go`

sub2api 的 `SetRateLimited(...)` 会：

1. 写入 rate limit 时间。
2. 发 scheduler outbox。
3. 同步 scheduler cache。

`kiro.rs` 应优先使用 Redis 让状态变更立即影响调度。PG 不适合承载每次 attempt / outcome 的高频写入，PG 应只保留账号配置、人工恢复标记、hard disabled 状态和必要审计事件。

## 本系统与 sub2api 的关键差异

不能把 sub2api 的 429 方案机械迁移到当前系统，原因如下：

1. sub2api 常见上游会返回 `Retry-After`、`x-ratelimit-reset`、`anthropic-ratelimit-*-reset` 或 body 中的 reset timestamp；当前 Kiro 429 通常没有明确恢复时间。
2. sub2api 的账号模型包含平台、分组、并发槽位、scheduler cache 等；当前系统的账号池更轻，但已有 Redis 句柄，调度动态状态应尽量放 Redis，而不是依赖 PG 高频写入。
3. sub2api 的 `SetRateLimited(resetAt)` 适合“知道何时恢复”的场景；当前系统更适合“短冷却 + half-open 探测 + 统计升级”。
4. 当前系统的 sticky session 和 prompt-cache / 长会话稳定性相关，不能粗暴取消 sticky；应该让 sticky 服从账号健康状态。
5. 当前系统 UI 里 `disabled=true` 已经带有“手动停用/账号不可用/可删除”的强语义，429 不应该轻易写成 disabled，否则会误导用户删除仍可能恢复的账号。
6. Kiro 429 可能是账号窗口、上游临时拥塞、风控、请求频率或全局波动造成的混合信号，单次或少数几次 429 不能证明账号彻底不可用。

因此，本项目的 429 策略必须满足：

```text
当前请求：快速 fallback，保护用户体验
后续调度：短冷却，避免持续撞击
恢复判断：冷却到期后探测，而不是固定等到某个 resetAt
长期处置：统计高置信度后进入人工恢复要求，而不是默认永久禁用
全局保护：如果多个账号同时 429，优先判定为上游/全池波动，避免误杀账号
```

## 数字设计原则

用户提到的“最近 100 次”“超过多少次禁用”等数字应视为问题方向，不应直接落为固定策略。合理数字需要遵守以下原则：

1. **按 original request 计数，不按 raw attempt 计数**  
   同一个客户端请求内部如果对账号 A 重试 3 次都 429，只能算账号 A 的 1 次 429 outcome。否则 retry 机制会把失败样本人为放大。

2. **先进入人工恢复要求，不直接 disabled**  
   429 只证明“当前不可用或暂时限流”，不证明凭据永久失效。默认最终状态应是 `manual_recovery_required` / `rate_limit_suspended_manual`，而不是 `disabled=true`。

3. **必须有最小样本数**  
   例如 5 次里 5 次都是 429，比例是 100%，但样本太少，只能冷却升级，不能人工恢复。

4. **必须观察到足够长的无成功时间**  
   如果账号中间成功过，说明它仍可恢复，应衰减 429 计数。

5. **必须排除全局上游波动**  
   如果多数账号同时 429，不能逐个禁用账号，应进入全池短退避。

6. **必须区分真实业务请求与 probe**  
   probe 失败可以加重冷却，但 probe 样本不能和大量真实业务样本完全等价，否则冷却后的探测会过快把账号推到人工恢复。

7. **所有时间都需要 jitter**  
   冷却到期不能让所有账号同一秒恢复，否则会形成同步重试，再次打出成片 429。

## 设计目标

最终方案必须满足以下目标：

1. 账号出现明确限流后，不继续被大量调度。
2. 临时错误可以自动恢复，不需要用户频繁手动干预。
3. 永久错误必须退出调度，避免浪费请求和拖慢用户。
4. sticky 不得压过账号健康状态。
5. priority / balanced 只在健康账号集合里生效。
6. API 请求和 MCP / 工具调用使用同一套账号健康判断。
7. 流式请求在还没向客户端写出时可以 fallback；已经写出后不做无感换号，只影响后续调度。
8. 所有调度动作可解释：为什么选择账号，为什么跳过账号，为什么进入冷却，何时恢复。
9. 账号状态需要在两个 UI 页面可见：
   - 旧 Admin UI：`admin-ui`
   - 新 Console UI：`frontend`
10. 不把所有 429 一刀切成 30 分钟，也不把所有 5xx 一刀切成禁用。
11. 对 Kiro 无 reset 429 使用统计判断，避免用户随口给出的阈值变成武断禁用规则。
12. 当多个账号同时 429 时，优先保护账号池，不能把全局上游波动误判为每个账号都坏了。

## 核心概念

### Original Request

客户端发来的一次请求。一次 original request 内可能包含多个 upstream attempt。

### Attempt

一次对某个上游账号的实际调用。

### 原地重试

同一个 original request 内，继续使用同一个账号再试一次。

适用场景必须很窄，因为如果错误是账号级限流，原地重试只会拖慢用户并继续打爆上游。

### Fallback

同一个 original request 内，把当前账号加入本次请求的 `excluded_ids`，然后选择另一个健康账号重试。

### Cooldown

账号临时不可调度，到期后自动恢复。

在 Kiro 429 场景下，Cooldown 不代表“等到明确 reset 时间”，而是“短时间避开该账号，之后通过 half-open probe 判断是否恢复”。

### Hard Disable

账号永久退出调度，除非用户手动恢复或重新配置。

注意：429 默认不进入 Hard Disable。429 的长期最终状态应先进入 `Manual Recovery Required`，它表示“调度层暂停自动使用，需要用户手动确认恢复”，而不是证明凭据永久失效。

### Half-open Probe

冷却结束后账号不应立即承接大量并发。可以先允许少量探测请求，成功后恢复健康，失败则重新冷却。

第一版可以不实现完整 half-open 并发闸门，但设计上应预留。

Kiro 429 场景建议第一版至少实现轻量规则：

```text
同一账号同一时间最多 1 个 probe
probe 成功 1 次即可恢复普通调度
probe 429 则升级冷却档位
probe 5xx / timeout 按 transient failure 处理，不等同 429
```

### Schedulable

账号可被调度的统一判断。任何调度入口都必须先检查它。

### Outcome

账号维度的一次归因结果。它不是 raw attempt。

计数规则：

```text
同一个 original request 对同一个账号最多产生 1 个 outcome
429 outcome 表示该账号在该请求中最终表现为 429
success outcome 表示该账号在该请求中成功完成
400/input too long/客户端取消不计入账号健康 outcome
```

引入 outcome 是为了避免同一个请求内部 9 次 retry 把“最近 100 次”的统计瞬间填满。

## 账号状态模型

建议把账号状态分为以下几类。

### Healthy

账号正常，可调度。

条件：

1. `disabled=false`。
2. 没有未到期的 `rate_limited_until`。
3. 没有未到期的 `temp_unschedulable_until`。
4. 没有 hard quota / auth / config 错误。
5. 支持请求模型。

### Soft Degraded

账号最近出现过网络失败、短流中断、个别 5xx，但还没有达到临时不可调度阈值。

行为：

1. 仍可调度。
2. 在 balanced scoring 中降低权重。
3. priority 模式下不应完全忽略该状态；如果有同优先级健康账号，应优先健康账号。

### Rate Limited

账号返回 429 或明确限流错误，进入冷却。

字段：

```text
rate_limited_until
rate_limited_reason
rate_limited_status
rate_limited_count
last_rate_limited_at
```

行为：

1. 冷却期内不可调度。
2. sticky 命中该账号时必须解绑或跳过。
3. 冷却到期后不应直接恢复满量调度，应先进入 half-open probe。
4. 如果 probe 成功，恢复 Healthy。
5. 如果 probe 仍 429，升级冷却档位。

### Half Open

账号冷却结束后的探测状态。

字段：

```text
half_open_started_at
probe_in_flight
probe_failure_count
last_probe_at
```

行为：

1. 不参与普通并发调度。
2. 只允许少量真实请求或专门测试请求作为 probe。
3. probe 成功后清理 `rate_limited_until`，账号恢复 Healthy。
4. probe 429 后重新进入 Rate Limited，冷却档位加一。

### Manual Recovery Required

账号不是永久认证失败，但统计上长期 429，系统不再自动调度它，等待用户手动恢复或重新测试。

它和 `disabled=true` 的区别：

```text
Manual Recovery Required:
  说明账号可能仍可恢复，但系统不再自动尝试
  默认不应被“删除已禁用账号”功能选中
  用户可以手动 reset 429 状态后重新探测

Hard Disabled:
  说明凭据/配置/额度有明确永久问题
  只有手动启用、重新导入或修复配置后才能使用
```

429 的长期处置应优先进入 Manual Recovery Required，而不是 Hard Disabled。

### Temp Unschedulable

账号临时不可调度，但不一定是 429。例如：

1. OAuth 401 等待 token refresh。
2. 连续 5xx。
3. 上游 overload。
4. suspicious activity。
5. 短期风控。

字段：

```text
temp_unschedulable_until
temp_unschedulable_reason
```

行为：

1. 到期自动恢复。
2. 连续触发可升级到更长冷却或 hard disable。

### Hard Disabled

账号不再自动恢复。

原因：

1. 手动禁用。
2. refresh token 永久失效。
3. API key 无效。
4. 上游明确 account disabled。
5. 配置错误。
6. quota hard exhausted。
7. 明确账号永久不可用。

行为：

1. 不参与调度。
2. 不自动恢复。
3. 前端必须显示原因。
4. 只能手动 reset / enable / 重新导入凭据。

不要因为普通 Kiro 429 自动进入 Hard Disabled，除非后续实现明确增加了用户可配置的危险开关，并且默认关闭。

## Redis 优先状态设计

本方案要求尽可能使用 Redis 处理调度相关状态。原因：

1. 429、cooldown、probe、outcome 窗口都属于高频、短生命周期、强实时调度状态。
2. PG 高频写入会带来不必要的 IO 压力，也会让调度路径耦合数据库延迟。
3. Redis 的 TTL、SET NX、LIST/LTRIM、ZSET、Lua 原子脚本非常适合实现 cooldown、窗口统计、probe 锁和 sticky。
4. 当前系统已经有 Redis 句柄，`AdminService` 也在用 Redis 做 balance cache，应复用这条基础设施。

设计边界：

```text
Redis:
  调度实时状态
  429 cooldown
  half-open probe 锁
  最近 outcome ring buffer
  global 429 backoff
  sticky session binding
  短期事件流

PG:
  账号配置
  手动禁用/启用
  hard disabled 原因
  manual recovery required 低频持久标记
  quota hard/soft 状态
  usage_records
  长期审计事件，可由 Redis stream 异步落盘

进程内内存:
  单次请求 excluded_ids
  单次 request attempt 上下文
  Redis 不可用时的临时降级缓存
```

### Redis Key 设计

以下 key 使用统一前缀，建议：

```text
kiro:sched:v1:...
```

账号调度状态：

```text
Hash key:
  kiro:sched:v1:cred:{credential_id}:state

Fields:
  rate_limited_until_ms
  rate_limited_reason
  rate_limited_status
  rate_limit_level
  last_rate_limited_at_ms
  probe_failure_count
  last_probe_at_ms
  temp_unschedulable_until_ms
  temp_unschedulable_reason
  transient_failure_count
  last_upstream_status
  last_upstream_error_at_ms
  last_upstream_error
  manual_recovery_required
  manual_recovery_reason
```

说明：

1. 该 hash 是调度读取的主要状态。
2. 时间使用 Unix milliseconds，避免时区和字符串解析问题。
3. `manual_recovery_required` 应同时低频写 PG，Redis 用于快速调度读取。

账号 cooldown 索引：

```text
ZSET:
  kiro:sched:v1:cooldowns:rate_limit
member:
  credential_id
score:
  rate_limited_until_ms

ZSET:
  kiro:sched:v1:cooldowns:temp_unschedulable
member:
  credential_id
score:
  temp_unschedulable_until_ms
```

用途：

1. 快速查看哪些账号正在冷却。
2. Admin UI 可以展示冷却列表。
3. 后台可清理已过期状态。

半开探测锁：

```text
String key:
  kiro:sched:v1:cred:{credential_id}:probe_lock

Value:
  request_id

TTL:
  60s 或 request_max_retry_elapsed_seconds * 3
```

获取方式：

```text
SET key request_id NX PX 60000
```

作用：

1. 同一个账号同一时间最多一个 probe。
2. 避免冷却结束后多个并发请求同时把账号打爆。

Outcome 去重：

```text
String key:
  kiro:sched:v1:outcome_dedupe:{request_id}:{credential_id}

Value:
  outcome

TTL:
  24h
```

写入方式：

```text
SET key outcome NX EX 86400
```

只有 SET 成功时才把 outcome 写入账号窗口，确保同一 original request 对同一账号最多计一次。

账号 outcome 窗口：

```text
List key:
  kiro:sched:v1:cred:{credential_id}:outcomes

Value JSON:
  {
    "ts": 1770000000000,
    "requestId": "...",
    "conversationId": "...",
    "model": "...",
    "outcome": "429",
    "status": 429,
    "source": "request"
  }

Write:
  RPUSH key value
  LTRIM key -100 -1

TTL:
  7d
```

说明：

1. 默认窗口长度 100，但由 `rate_limit_429_window_size` 配置控制。
2. 读取最近 100 条 JSON 做比例计算即可，O(100) 成本可接受。
3. 不建议每条 outcome 直接写 PG。

全局 429 波动统计：

```text
ZSET:
  kiro:sched:v1:pool:429_accounts
member:
  credential_id
score:
  last_429_at_ms

ZSET:
  kiro:sched:v1:pool:success_accounts
member:
  credential_id
score:
  last_success_at_ms

String key:
  kiro:sched:v1:pool:global_backoff_until_ms
TTL:
  global backoff remaining seconds
```

判断全局 429：

```text
now_ms = current time
window_start = now_ms - global_429_window_seconds * 1000
recent_429_accounts = ZCOUNT pool:429_accounts window_start +inf
recent_success_accounts = ZCOUNT pool:success_accounts window_start +inf
```

如果近期 429 账号占比过高且成功账号明显减少，则设置 `global_backoff_until_ms`。

Sticky session：

```text
String key:
  kiro:sched:v1:sticky:{session_hash}

Value:
  credential_id

TTL:
  SESSION_BINDING_TTL_SECS
```

可选附加 hash：

```text
Hash key:
  kiro:sched:v1:sticky_meta:{session_hash}

Fields:
  credential_id
  last_used_at_ms
  soft_failure_count
```

第一版可以只用 string 存绑定账号，soft failure 仍放进现有内存；更完整方案应将 soft failure 也迁到 Redis，避免多进程时 sticky 失败状态不一致。

事件流：

```text
Stream key:
  kiro:sched:v1:events

XADD fields:
  kind
  credential_id
  request_id
  conversation_id
  model
  status
  reason
  cooldown_until_ms
  note

Retention:
  XTRIM MAXLEN ~ 10000
```

用途：

1. Admin UI 查询近期事件。
2. 后台异步消费者可批量落 PG。
3. 服务重启后仍能看到近期状态变化。

### Redis 原子操作要求

以下操作应使用 Lua 脚本或 Redis transaction 保证原子性：

1. 写 outcome 去重 + push ring buffer + 更新 pool zset。
2. 设置 429 cooldown + 更新账号 state hash + 写 cooldown zset + 写事件 stream。
3. half-open probe 获取锁 + 标记 probe started。
4. probe 成功恢复 + 清理 cooldown zset + 清理 probe lock + 写 success outcome。
5. sticky compare-and-delete：只有当前 sticky value 等于坏账号 id 时才删除。
6. manual recovery reset：清理 Redis state、outcomes、cooldowns、probe lock，并同步 PG 低频字段。

### Redis 与 PG 的持久化边界

当前 `credentials` 表已有：

```text
disabled
disabled_reason
failure_count
refresh_failure_count
success_count
last_used_at
quota_strike_count
cooldown_until
```

不建议把所有 429 动态字段都加到 PG。PG 只建议新增低频、需要重启后保留、且需要用户可见的人工恢复字段：

```sql
ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS rate_limit_manual_recovery_required BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS rate_limit_manual_recovery_reason TEXT,
    ADD COLUMN IF NOT EXISTS rate_limit_manual_recovery_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS last_upstream_status SMALLINT,
    ADD COLUMN IF NOT EXISTS last_upstream_error_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS last_upstream_error TEXT;
```

说明：

1. `rate_limited_until`、`rate_limit_level`、`probe_failure_count`、outcome 窗口优先 Redis，不进 PG。
2. `manual_recovery_required` 是低频状态，必须落 PG，否则 Redis 重启后会重新调度长期 429 账号。
3. `last_upstream_*` 可选落 PG，用于服务重启后 UI 仍能展示最近错误摘要；高频更新应做防抖或只在状态转换时写。
4. 事件表可以暂缓；第一版可用 Redis Stream 展示近期事件，后续再由后台消费者批量落 PG。

如果需要长期审计，再新增 PG 事件表，但只能异步批量写：

```sql
CREATE TABLE IF NOT EXISTS credential_events (
    id BIGSERIAL PRIMARY KEY,
    credential_id BIGINT NOT NULL REFERENCES credentials(id) ON DELETE CASCADE,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    kind TEXT NOT NULL,
    upstream_status SMALLINT,
    reason TEXT,
    cooldown_until TIMESTAMPTZ,
    request_id TEXT,
    conversation_id TEXT,
    model TEXT,
    note TEXT
);
```

### Redis 不可用时的降级策略

Redis 是调度状态的首选，但服务不能因为 Redis 短暂不可用直接崩溃。

建议：

```text
Redis 可用:
  使用 Redis 作为动态调度状态源

Redis 不可用，单进程模式:
  降级到进程内 HashMap
  打 warn 日志
  Admin UI 显示“调度状态未持久化”

Redis 不可用，多进程模式:
  风险较高，因为各进程无法共享 cooldown
  应返回健康检查降级状态
  可选择 fail-closed：对高风险 429 probe 减少自动重试
```

本项目当前本地/单服务部署可以先实现进程内降级；如果后续多实例部署，Redis 应成为强依赖。

## 统一可调度判断

新增逻辑概念：

```text
credential_is_schedulable(entry, redis_state, model, now):
  if entry.disabled:
    return false
  if model is opus and !entry.supports_opus:
    return false
  if redis_state.manual_recovery_required:
    return false
  if redis_state.rate_limited_until_ms exists and now < rate_limited_until_ms:
    return false
  if redis_state.temp_unschedulable_until_ms exists and now < temp_unschedulable_until_ms:
    return false
  if entry.cooldown_until exists and now < cooldown_until:
    return false
  return true
```

注意：

1. `cooldown_until` 当前用于 quota soft cooldown，仍从 PG/内存 entry 判断。
2. rate limit、temp unschedulable、manual recovery 优先从 Redis state hash 判断。
3. Redis 不可用时可使用进程内降级状态，但必须打 warn 并在 UI/health 中体现。
4. 后续应把 quota cooldown、rate limit cooldown、temp unschedulable 分开显示。
5. 所有调度入口必须使用这个函数。

需要覆盖的入口：

1. `get_bound_credential(...)`
2. `select_next_credential_excluding(...)`
3. `current_id` 命中逻辑
4. `acquire_context(...)`
5. `acquire_context_for_session(...)`
6. `call_mcp_with_retry(...)`
7. 任何未来新增的测试调用或账号验活调用

### Redis 读取性能要求

调度时不能对每个账号串行请求 Redis。应使用批量读取：

```text
候选账号 ids = 从内存/PG credentials 得到
Redis HMGET / pipeline 批量读取 kiro:sched:v1:cred:{id}:state
在内存中组合 entry + redis_state 做 schedulable 过滤
```

如果账号数量较少，第一版可 pipeline 多个 `HGETALL`；如果账号数量变大，应改为 Lua 或 hash 聚合结构减少 round trip。

## 错误分类矩阵

### A 类：请求本身错误，不重试，不 fallback

例子：

1. 400 invalid request。
2. input too long。
3. JSON/schema/参数错误。
4. 模型不存在且与账号无关。
5. 客户端传入不支持的媒体格式。

动作：

```text
same-account retry: no
fallback: no
account cooldown: no
failure_count: no
response: 4xx to client
```

原因：

换账号无法修复请求本身错误，重试只会增加延迟。

### B 类：明确认证永久错误，hard disable

例子：

1. refresh token revoked。
2. refresh token invalid。
3. API key invalid。
4. account disabled。
5. organization disabled。
6. profile/account 配置明确错误。

动作：

```text
same-account retry: no
fallback: yes, immediately
account cooldown: no
hard disable: yes
manual recovery required: yes
unbind sticky: yes
```

原因：

这类错误不会因为等待几秒恢复。继续调度会长期伤害所有用户。

### C 类：认证可能临时失败，短期 temp unschedulable

例子：

1. OAuth access token expired，但 refresh 可能成功。
2. 401 且语义更像 token 过期，而不是 revoked。
3. token refresh 服务短暂失败。

动作：

```text
same-account retry: only after forced refresh succeeds
fallback: yes if refresh fails
temp unschedulable: 5-10 minutes
hard disable: after repeated refresh invalid / explicit revoked
unbind sticky: yes when entering temp unschedulable
```

原因：

这类错误可能恢复，但不能让用户在当前请求里等待长时间刷新和反复失败。

### D 类：明确 quota / monthly limit

例子：

1. 402 monthly request limit。
2. 上游 body 明确 `MONTHLY_REQUEST_COUNT`。
3. 明确余额不足、额度耗尽。

动作：

```text
same-account retry: no
fallback: yes, immediately
quota cooldown: yes for soft quota
hard disable: after quota strike limit
manual recovery: for hard quota exhausted
unbind sticky: yes
```

当前系统已有 402 三次冷却策略，建议保留，但应在 UI 中更明确区分“quota cooldown”和“rate limit cooldown”。

### E 类：有 reset 时间的 429，立即 fallback，冷却到 reset（兼容分支）

例子：

1. `Retry-After` header。
2. `x-ratelimit-reset` / `anthropic-ratelimit-*-reset` 类 header。
3. body 中包含明确 reset timestamp。
4. 上游明确窗口耗尽。

动作：

```text
same-account retry: no
fallback: yes, immediately
rate_limited_until: parsed reset time
cooldown jitter: usually no for authoritative reset, but can add small positive jitter
hard disable: no
unbind sticky: yes
```

原因：

reset 时间已经说明该账号在窗口内不可用。原地重试只会浪费时间。

注意：这类策略是为了兼容未来或某些 endpoint 返回 reset 信息的情况，不是当前 Kiro 429 的默认路径。当前系统观察到的 Kiro 429 应主要落入 F 类。

### F 类：Kiro 默认无 reset 429，立即 fallback，短冷却后 probe

例子：

1. `429 Too Many Requests`。
2. body 只有 “rate limit” / “too many requests”。
3. 没有可解析 reset。
4. 当前 Kiro 上游返回 429，但没有提供可信的恢复时间。

动作：

```text
same-account retry: no by default
fallback: yes, immediately
rate_limited_until: fallback cooldown
half-open probe: yes after cooldown expires
manual recovery required: only after high-confidence long-term 429
hard disable: no by default
unbind sticky: yes
```

推荐冷却策略：

```text
连续 429 档位按账号维护，成功后衰减。

level 1:
  cooldown = 45s ± 20%

level 2:
  cooldown = 2min ± 20%

level 3:
  cooldown = 5min ± 20%

level 4:
  cooldown = 15min ± 20%

level 5:
  cooldown = 30min ± 20%

level 6+:
  cooldown = min(2h, previous * 2) ± 20%
```

说明：

1. 第一轮使用 45 秒，是为了让临时 Kiro 429 有较快恢复机会，同时避免当前请求继续撞同一账号。
2. 第二、三轮仍是分钟级，适合“过一会儿可能好了”的账号。
3. 第四轮开始进入明显隔离，避免坏账号持续影响用户。
4. 第六轮以上最多 2 小时，不建议无限增长，否则账号可能长期沉没且无人注意。
5. 每次 cooldown 到期后先进入 half-open probe，不直接恢复满量调度。
6. hard disable 默认关闭。429 的长期最终状态优先是 `manual_recovery_required`。

### F1 类：无 reset 429 的人工恢复判定

只有统计上高度确认账号长期不可用，才进入人工恢复要求。

不要使用单一条件：

```text
最近 100 次都是 429 -> 禁用
```

必须使用组合条件：

```text
rateLimit429WindowSize = 100
rateLimit429MinSamples = 50
rateLimit429ManualRecoveryRatio = 0.98
rateLimit429ManualRecoveryMinProbeFailures = 8
rateLimit429ManualRecoveryNoSuccessSeconds = 6h
rateLimit429ManualRecoveryRequireOtherAccountsHealthy = true
```

进入 `manual_recovery_required` 需要同时满足：

1. 最近 `rateLimit429WindowSize` 个账号 outcome 中，样本数至少 `rateLimit429MinSamples`。
2. 429 占比 `>= rateLimit429ManualRecoveryRatio`。
3. 最近 `rateLimit429ManualRecoveryNoSuccessSeconds` 内没有成功 outcome。
4. 冷却到期后的 probe 失败次数至少 `rateLimit429ManualRecoveryMinProbeFailures`。
5. 同一时间池内其他账号存在成功请求，证明不是全局 Kiro 上游波动。
6. 这些 outcome 按 `request_id + credential_id` 去重，不能由同一个 original request 的多次 retry 堆出来。

进入该状态后的动作：

```text
rate_limit_manual_recovery_required = true
rate_limit_manual_recovery_reason = repeated_429_high_ratio
disabled = false by default
普通调度不再选择该账号
UI 显示“429 高比例，需手动恢复/测试”
用户手动 reset 后清理 429 统计并允许 probe
```

如果用户明确开启危险配置，才允许把该状态升级为 `disabled=true`：

```text
rate_limit_429_auto_disable_enabled = false by default
```

默认不应开启自动禁用，因为 Kiro 429 无 reset，且可能恢复。

### F2 类：全局 429 波动保护

如果多个账号同时 429，不能把每个账号都按单账号坏号处理。

建议维护 pool-level 统计：

```text
global429WindowSeconds = 300
global429MinAccounts = 3
global429AccountRatio = 0.6
global429SuccessDropRatio = 0.5
globalBackoffSeconds = 30-90s ± 20%
```

触发条件：

1. 最近 5 分钟内至少 3 个账号有请求样本。
2. 其中超过 60% 的账号出现 429。
3. 池整体成功率相比正常水平明显下降，或者最近成功率低于 50%。

触发后的动作：

```text
暂停把单账号 429 升级到 manual_recovery_required
对全池设置短 global backoff
保留少量 probe
客户端请求在无健康账号时快速返回 503 + retry-after
记录 pool_rate_limited / upstream_global_429 事件
```

原因：

这能避免 Kiro 上游全局波动时，系统把所有账号逐个隔离或误标记为人工恢复。

### G 类：疑似风控 / suspicious / abuse 的 429 或 403，长冷却或人工恢复

例子：

1. body 包含 suspicious activity。
2. body 包含 abuse。
3. body 包含 unusual activity。
4. body 包含 risk / policy / blocked。
5. 403 但语义不是参数错误，而是账号/风控问题。

动作：

```text
same-account retry: no
fallback: yes, immediately
temp unschedulable: 30min ± 20%
repeated: 2h -> 12h -> manual recovery
hard disable: if explicit account blocked / repeated suspicious
unbind sticky: yes
```

推荐策略：

```text
第 1 次 suspicious:
  temp_unschedulable = 30min ± 20%

24 小时内第 2 次:
  temp_unschedulable = 2h ± 20%

24 小时内第 3 次:
  temp_unschedulable = 12h ± 20%

24 小时内第 4 次:
  hard disable, manual recovery required
```

原因：

风控类错误继续打会加重风险，不适合短间隔 probe。

### H 类：408 / request timeout，短原地重试一次，然后 fallback

例子：

1. upstream 408。
2. 请求发送成功但上游超时。
3. 没有明确账号限流。

动作：

```text
same-account retry: yes, max 1
fallback: yes after one retry fails
cooldown: no on first occurrence
transient_failure_count: yes
temp unschedulable: after repeated timeouts
```

推荐策略：

```text
第 1 次 408:
  same-account retry after 300-800ms

同一 original request 内第 2 次仍 408:
  exclude current account for this request
  fallback to next healthy account

同账号 10 分钟内累计 3 次 408:
  temp_unschedulable = 2min ± 20%

10 分钟内累计 6 次:
  temp_unschedulable = 10min ± 20%
```

原因：

408 可能是瞬时网络或上游排队，原地重试一次有意义，但不应拖住用户多轮等待。

### I 类：普通 5xx，上游服务错误，短原地重试一次，然后 fallback

例子：

1. 500。
2. 502。
3. 503。
4. 504。

动作：

```text
same-account retry: yes, max 1 for generic 5xx
fallback: yes after one same-account retry fails
cooldown: short only after repeated 5xx
hard disable: no unless repeated over long window and no successes
```

推荐策略：

```text
第 1 次 generic 5xx:
  same-account retry after 300-1000ms

同一 original request 内第二次仍 5xx:
  fallback

同账号 10 分钟内累计 3 次 5xx:
  temp_unschedulable = 1min ± 20%

10 分钟内累计 6 次:
  temp_unschedulable = 5min ± 20%

10 分钟内累计 10 次:
  temp_unschedulable = 30min ± 20%
```

原因：

5xx 往往是上游瞬态服务错误，不应永久禁用账号。但如果同一个账号持续 5xx，继续调度会伤害用户，应短冷却。

### J 类：明确 overload / capacity / 529，立即 fallback，短冷却

例子：

1. 529。
2. body 包含 overloaded。
3. body 包含 capacity。
4. body 包含 high load。

动作：

```text
same-account retry: no by default
fallback: yes
temp unschedulable: 30s-2min
hard disable: no
```

推荐策略：

```text
第 1 次 overload:
  cooldown = 60s ± 20%

10 分钟内第 2 次:
  cooldown = 3min ± 20%

10 分钟内第 3 次:
  cooldown = 10min ± 20%
```

原因：

overload 一般不是账号永久坏，而是短期容量问题。换健康账号更快。

### K 类：网络发送失败，本次退避，谨慎 fallback

例子：

1. TCP connect fail。
2. TLS handshake fail。
3. proxy connection fail。
4. DNS fail。
5. request send error。

动作取决于错误范围：

```text
如果是全局网络/代理问题:
  same-account retry: yes, short
  fallback: maybe useless
  account cooldown: no

如果是凭据级 proxy 问题:
  fallback: yes
  temp unschedulable: 5min
```

推荐策略：

```text
无凭据级 proxy:
  不立刻给账号 cooldown
  original request 内最多 retry 1-2 次
  如果全部账号都是同类网络错误，返回 503

凭据级 proxy:
  当前账号 temp_unschedulable = 5min ± 20%
  fallback

同账号 30 分钟内 proxy/network 失败 5 次:
  temp_unschedulable = 30min

连续多轮无成功:
  标记 degraded，需要 UI 提示检查代理
```

原因：

网络失败可能不是账号问题。如果把全局网络抖动计入账号禁用，会误伤整个池。

### L 类：流式 body 中断 / idle timeout

例子：

1. 已经返回 200 eventstream。
2. body stream read error。
3. idle timeout。
4. 上游流中途错误事件。

动作：

```text
same-request fallback: no, because client stream may already started
account cooldown: depends on error type
session soft failure: yes
future scheduling penalty: yes
```

推荐策略：

```text
单次读断:
  transient_failure_count += 1
  不立刻 cooldown

同账号 10 分钟内 3 次流中断:
  temp_unschedulable = 2min ± 20%

同账号 10 分钟内 6 次:
  temp_unschedulable = 10min ± 20%

如果流内错误明确 rate_limit:
  按 429 规则处理
```

原因：

流已经开始后不能无损换账号，只能影响后续请求。

## 原地重试与 fallback 的总规则

默认规则：

```text
账号级明确错误:
  不原地重试，立即 fallback

请求级错误:
  不原地重试，不 fallback

瞬态上游错误:
  原地重试最多 1 次，然后 fallback

网络错误:
  判断是否凭据级代理；凭据级代理可 fallback，全局网络谨慎 fallback

流式已写出:
  不在当前请求 fallback，只记录账号状态影响后续请求
```

明确账号级错误包括：

```text
429 with reset
429 no reset
quota exceeded
auth invalid
account disabled
suspicious / abuse
credential-level proxy failure
```

可以原地重试一次的错误包括：

```text
408
generic 500
generic 502
generic 503 without rate-limit / overload semantics
generic 504
temporary send error when not credential-specific
```

不应 fallback 的错误包括：

```text
400 invalid request
input too long
unsupported media
bad model payload
client-side schema problem
```

## 指数退避设计

需要区分两条退避线：

1. original request 内的 attempt 退避。
2. 账号级 cooldown 退避。

### Request 内 attempt 退避

目的：避免同一个请求内瞬时错误放大。

推荐：

```text
base = 250ms
factor = 2
max = 2s
jitter = 0%-25%
```

示例：

```text
attempt 1 retry delay: 250-312ms
attempt 2 retry delay: 500-625ms
attempt 3 retry delay: 1000-1250ms
attempt 4+: 2000-2500ms
```

限制：

1. 对 429 / auth / quota 不走多次 request 内 sleep，直接 fallback。
2. 对 408 / generic 5xx 最多原地 retry 1 次。
3. original request 总 attempt 数仍应有硬上限。
4. 不应该因为账号数量多，就让单个用户等待过长。

建议 request retry budget：

```text
max_total_attempts = min(total_schedulable_credentials + 2, 6)
max_same_credential_attempts = 2
max_fallback_credentials = min(total_schedulable_credentials, 5)
max_total_retry_elapsed = 15s for non-stream before response
max_total_retry_elapsed = 20s for stream before response
```

说明：

当前 `MAX_TOTAL_RETRIES = 9` 在 429 每次耗时较长时会让用户等待过久。新策略应该更重视“快速换健康账号”和“全池不可用时快速失败”。

### 账号级 cooldown 退避

目的：避免坏账号被所有用户持续撞击。

需要 per credential 保存连续状态：

```text
recent_429_count
rate_limit_level
rate_limit_probe_failure_count
rate_limit_manual_recovery_required
recent_5xx_count
recent_timeout_count
recent_suspicious_count
window_started_at
last_success_at
last_failure_at
```

Kiro 无 reset 429 的退避不应只按“次数”简单累计，而应按“档位 + probe 结果 + outcome 比例”组合判断：

```text
429 detected:
  current request: exclude credential and fallback
  account state: Cooling(level = max(1, current_level))

cooldown expired:
  account state: HalfOpen
  allow at most 1 probe

probe success:
  state = Healthy
  rate_limit_level = max(0, rate_limit_level - 2)
  rate_limit_probe_failure_count = 0
  keep outcome history for ratio calculation

probe 429:
  rate_limit_level += 1
  rate_limit_probe_failure_count += 1
  state = Cooling(next_level)

manual recovery required:
  only if high-ratio window + no success + enough probe failures + other accounts healthy
```

成功时衰减：

```text
成功一次:
  transient_failure_count = 0
  recent_5xx_count = max(0, recent_5xx_count - 2)
  recent_timeout_count = max(0, recent_timeout_count - 2)
  rate_limit_level = max(0, rate_limit_level - 2)
  recent_429_count = max(0, recent_429_count - 1)
```

不要在一次成功后清掉所有长期风险，特别是 suspicious 类错误。

### 429 统计窗口合理性

默认推荐使用最近 100 个 outcome，不是因为 100 是绝对正确，而是它在“足够稳定”和“不至于恢复太慢”之间比较平衡：

1. 小于 20 的窗口太敏感，几个请求就可能把账号打入人工恢复。
2. 50 左右可以作为最小可判定样本，但仍应只用于长冷却，不建议直接人工恢复。
3. 100 能较好过滤短期抖动，适合作为人工恢复的默认窗口。
4. 大于 200 会让确实坏掉的账号太久无法被隔离，继续影响调度。

推荐默认：

```text
rateLimit429WindowSize = 100
rateLimit429MinSamples = 50
rateLimit429ManualRecoveryRatio = 0.98
```

含义：

```text
最近 100 个账号 outcome 中，至少要有 50 个样本；
其中 429 比例达到 98%；
还必须满足无成功时间、probe 失败次数、其他账号健康等条件；
才进入人工恢复要求。
```

这比“最近 100 次都是 429 就禁用”更安全，因为它：

1. 不会被少量样本误导。
2. 不会被同一请求多次 retry 放大。
3. 不会在全局上游波动时误伤账号。
4. 不会直接把可能恢复的账号变成 disabled。

## Sticky 策略

sticky 应该保留，但必须降级为“健康时优先”。

### Sticky 命中规则

```text
如果 session 绑定账号 A:
  如果 A schedulable:
    使用 A
  否则:
    删除该 session 的 sticky binding
    记录 sticky_unbound 事件
    进入普通调度
```

### Sticky 失败规则

如果 sticky 账号返回：

```text
429 / quota / auth / suspicious:
  立即解绑 sticky
  当前请求 fallback

408 / generic 5xx:
  原地 retry 一次
  若仍失败，当前请求排除该账号
  是否解绑取决于是否进入 temp_unschedulable

stream read error:
  不在当前请求 fallback
  增加 session soft failure
  达到阈值后后续请求可跳过
```

### Fallback 成功后的改绑

当前系统 fallback 成功后不一定改绑 sticky，这是问题。

新规则：

```text
如果 session 原绑定 A，本次 fallback 到 B 成功:
  如果 A 当前不可调度:
    绑定改为 B
  如果 A 只是本次 request-local excluded:
    可选择不改绑
  如果连续 2 次 fallback 到 B 成功:
    绑定改为 B
```

推荐第一版实现：

```text
只要原绑定账号进入 rate_limited / temp_unschedulable / disabled，fallback 成功后立即改绑到成功账号。
```

这样能避免长会话持续回到坏账号。

## Priority 与 Balanced 策略

### Priority 模式

priority 模式不应理解为“永远打优先级最高账号”。它应该是：

```text
在所有 schedulable 账号中选择 priority 最小的账号。
```

如果最高优先级账号处于 cooldown，必须跳过。

同优先级账号建议按：

```text
1. 非 degraded 优先
2. last_used_at 更早优先
3. success_count / load 更低优先
4. id 稳定排序兜底
```

### Balanced 模式

balanced 模式不应只看 `success_count`。

第一版可以使用：

```text
score = success_count_weight + priority_weight + degraded_penalty + last_used_penalty
```

更完整可以参考 sub2api：

```text
priority
load
queue
error rate
TTFT
last used
```

但第一阶段最重要的是：balanced 也必须先过滤不可调度账号。

## MCP / 工具调用策略

MCP / 工具调用必须和普通 API 使用同一套账号健康状态。

当前 `call_mcp_with_retry(...)` 没有 session，也没有 request-local excluded。建议改为：

```text
let excluded_ids = HashSet::new()
for attempt in retry_budget:
  ctx = acquire_context_for_session(None, None, &excluded_ids)
  call upstream
  classify error
  apply account state
  if should_fallback:
    excluded_ids.insert(ctx.id)
    continue
```

工具调用没有 conversation sticky，但仍然需要：

1. 429 cooldown。
2. transient failure count。
3. fallback。
4. attempt-level usage / event。

否则工具调用会持续撞坏账号。

## 流式请求策略

### 响应头返回前

还没有向客户端写出 SSE 时，可以安全 fallback。

适用：

```text
upstream status = 429 / 5xx / 408 / 401 / 403 / 402
```

动作按错误分类矩阵执行。

### 响应头返回后

一旦已经开始向客户端写 SSE：

1. 不做当前请求内 fallback。
2. 不重新生成一条新的完整 response。
3. 只能发送错误事件或结束流。
4. 记录该账号 stream failure。
5. 如果错误可分类为 rate limit，则设置账号 cooldown，影响后续请求。

### Idle timeout

当前有 upstream idle timeout。建议：

```text
首次 idle timeout:
  record transient failure
  不直接 cooldown

短窗口多次 idle timeout:
  temp_unschedulable
```

## 全池不可用策略

当所有账号都不可调度时，不应该继续执行 9 次无意义 retry。

应该计算：

```text
earliest_recover_at = min(
  rate_limited_until,
  temp_unschedulable_until,
  quota cooldown_until
)
```

响应：

```text
HTTP 503
error type: overloaded_error
message: All upstream credentials are temporarily unavailable.
retry-after: seconds until earliest_recover_at, clamped to 1..300
```

如果所有账号都是 hard disabled：

```text
HTTP 503
retry-after: 300
message: All upstream credentials are disabled or require manual recovery.
```

不要对客户端暴露敏感账号信息，但日志和 admin event 应记录详细原因。

## 人工恢复策略

进入 hard disabled 的账号不得自动恢复。

触发 hard disabled 的情况：

```text
manual disabled
invalid config
refresh token permanently invalid
api key invalid
account disabled by upstream
quota hard exhausted
repeated suspicious events beyond threshold
repeated rate limit cycles beyond threshold with no success
```

恢复方式：

1. 前端手动启用。
2. 前端点击 reset failure。
3. 重新导入 / 更新凭据。
4. 强制刷新 token 成功后恢复。

手动恢复时应清理：

```text
failure_count
refresh_failure_count
transient_failure_count
rate_limited_until
rate_limited_reason
temp_unschedulable_until
temp_unschedulable_reason
last_upstream_error
```

但不应清理历史事件表。

## 前端展示方案

项目有两个 UI，需要同时覆盖：

1. `admin-ui`
2. `frontend`

账号卡片应新增状态展示：

```text
状态:
  正常
  已禁用
  限流冷却中
  临时不可调度
  配额冷却中
  需要手动恢复

字段:
  cooldownUntil
  cooldownRemaining
  cooldownReason
  lastUpstreamStatus
  lastUpstreamErrorAt
  transientFailureCount
  rateLimitedCount
```

建议后端 API 在 `CredentialStatusItem` 增加：

```text
schedulingStatus: "healthy" | "disabled" | "rate_limited" | "temp_unschedulable" | "quota_cooldown" | "manual_recovery_required"
schedulingReason?: string
schedulingUntil?: string
lastUpstreamStatus?: number
lastUpstreamErrorAt?: string
rateLimitedCount?: number
transientFailureCount?: number
```

UI 行为：

1. rate limited：显示倒计时。
2. temp unschedulable：显示原因和倒计时。
3. manual recovery：显示醒目的“需手动恢复”。
4. reset 按钮可以清理冷却状态。
5. disable/enable 仍然保留手动控制语义。

## 可观测性方案

### 日志

每次 upstream attempt 应记录：

```text
request_id
conversation_id
credential_id
attempt_index
status
classification
action
cooldown_until
sticky_bound
fallback_from_sticky
excluded_count
```

示例：

```text
upstream_attempt_failed request_id=... credential_id=2 status=429 classification=rate_limited_no_reset action=fallback cooldown_until=...
```

### Usage

当前失败 usage 丢失 credential id。建议 provider 错误携带：

```text
last_credential_id
attempted_credential_ids
attempts: [
  { credential_id, status, classification, action, duration_ms }
]
```

handler 失败时至少记录：

```text
credentialId = last_credential_id
attemptedCredentialIds
fallbackCount
finalUpstreamStatus
```

### Events

事件表用于排查账号状态变化：

```text
rate_limited
temp_unschedulable
hard_disabled
recovered
manual_reset
fallback_success
sticky_unbound
```

## 配置建议

建议新增 app_config 项：

```json
{
  "rate_limit_429_initial_cooldown_seconds": 45,
  "rate_limit_429_level2_cooldown_seconds": 120,
  "rate_limit_429_level3_cooldown_seconds": 300,
  "rate_limit_429_level4_cooldown_seconds": 900,
  "rate_limit_429_level5_cooldown_seconds": 1800,
  "rate_limit_429_max_cooldown_seconds": 7200,
  "rate_limit_429_jitter_ratio": 0.2,
  "rate_limit_429_window_size": 100,
  "rate_limit_429_min_samples": 50,
  "rate_limit_429_manual_recovery_ratio": 0.98,
  "rate_limit_429_manual_recovery_no_success_seconds": 21600,
  "rate_limit_429_manual_recovery_min_probe_failures": 8,
  "rate_limit_429_require_other_accounts_healthy": true,
  "rate_limit_429_auto_disable_enabled": false,
  "rate_limit_429_state_backend": "redis",
  "rate_limit_429_redis_key_prefix": "kiro:sched:v1",
  "rate_limit_429_outcome_ttl_seconds": 604800,
  "rate_limit_429_outcome_dedupe_ttl_seconds": 86400,
  "rate_limit_429_event_stream_max_len": 10000,
  "rate_limit_429_pg_event_flush_enabled": false,
  "rate_limit_429_pg_event_flush_interval_seconds": 30,

  "global_429_window_seconds": 300,
  "global_429_min_accounts": 3,
  "global_429_account_ratio": 0.6,
  "global_429_success_drop_ratio": 0.5,
  "global_429_backoff_min_seconds": 30,
  "global_429_backoff_max_seconds": 90,

  "half_open_probe_enabled": true,
  "half_open_max_concurrent_probes_per_credential": 1,
  "half_open_successes_to_recover": 1,

  "suspicious_cooldown_first_seconds": 1800,
  "suspicious_cooldown_second_seconds": 7200,
  "suspicious_cooldown_third_seconds": 43200,
  "suspicious_hard_disable_daily_count": 4,

  "transient_5xx_window_seconds": 600,
  "transient_5xx_soft_threshold": 3,
  "transient_5xx_medium_threshold": 6,
  "transient_5xx_hard_threshold": 10,

  "timeout_same_account_retry_max": 1,
  "generic_5xx_same_account_retry_max": 1,
  "request_max_total_attempts": 6,
  "request_max_retry_elapsed_seconds": 20
}
```

默认值必须保守：

1. Kiro 无 reset 429 第一轮冷却不要太长，默认 45 秒。
2. suspicious 第一轮可以较长。
3. 429 默认不自动 hard disable，只进入人工恢复要求。
4. 所有 cooldown 加 jitter。
5. 人工恢复要求必须同时满足样本数、比例、无成功时间、probe 失败次数和其他账号健康条件。
6. 全局 429 波动时暂停单账号人工恢复升级。
7. 调度动态状态默认使用 Redis。
8. PG 事件异步落盘默认关闭，除非需要长期审计。

## 实施步骤

### 第一阶段：状态与调度

1. 新增 Redis 调度状态访问层，封装 state hash、cooldown zset、outcome list、dedupe key、probe lock、global backoff、event stream。
2. 新增进程内降级状态，仅在 Redis 不可用时使用。
3. 只给 PG 增加低频人工恢复字段，不把 outcome/cooldown 高频字段写 PG。
4. 新增 `credential_is_schedulable(entry, redis_state, model, now)`。
5. 替换所有调度入口的可用性判断。
6. sticky 命中前检查 schedulable。
7. sticky 账号不可调度时用 Redis compare-and-delete 解绑。
8. current_id 命中前检查 schedulable。
9. balanced/priority 只在 schedulable 集合内选择。
10. 新增 429 outcome 统计，确保按 `request_id + credential_id` 通过 Redis `SET NX` 去重。
11. 新增 half-open probe lock，第一版只实现“每账号同时一个 probe”。

### 第二阶段：错误分类与 cooldown

1. 新增 upstream error classifier。
2. 在 API 主调用中按分类决定：
   - no retry；
   - same-account retry；
   - fallback；
   - cooldown；
   - hard disable。
3. 429 有 reset 时冷却到 reset；这是兼容分支，不是 Kiro 默认路径。
4. Kiro 无 reset 429 按 `45s -> 2min -> 5min -> 15min -> 30min -> max 2h` 分级 cooldown。
5. suspicious 长冷却。
6. 5xx / 408 短原地 retry 后 fallback。
7. 429 cooldown 到期后进入 half-open probe，probe 成功恢复，probe 429 升级档位。
8. 实现全局 429 波动保护，避免大量账号同时被误判为坏号。
9. 实现 manual recovery required，但默认不把 429 升级为 `disabled=true`。
10. manual recovery required 需要写 Redis state，并低频同步到 PG。

### 第三阶段：MCP / 工具调用

1. 给 MCP 调用增加 request-local `excluded_ids`。
2. 复用同一套错误分类。
3. 复用同一套账号 cooldown。
4. 记录 MCP attempt 事件。

### 第四阶段：usage 与事件

1. provider 错误携带 attempt 上下文。
2. usage failure attach last credential。
3. 新增 Redis Stream credential event 写入。
4. 可选实现 Redis Stream 到 PG 的异步批量落盘。
5. admin API 返回调度状态，优先读 Redis，PG 只补充人工恢复和最近错误摘要。

### 第五阶段：双 UI

1. 更新 `admin-ui` 类型和账号卡片。
2. 更新 `frontend` 类型和账号卡片。
3. 增加冷却倒计时。
4. 增加手动 reset/恢复入口。

### 第六阶段：测试

1. 单元测试错误分类。
2. 单元测试 cooldown 计算和 jitter 范围。
3. 单元测试 sticky 不健康解绑。
4. 单元测试 fallback 成功改绑。
5. 单元测试 priority 跳过 rate-limited 账号。
6. 单元测试 balanced 跳过 temp-unschedulable 账号。
7. 集成测试 429 有 reset 兼容分支。
8. 集成测试 Kiro 无 reset 429 连续升级。
9. 集成测试 MCP 429 fallback。
10. 集成测试流式 header 前 429 fallback。
11. 集成测试流式 body 中断只影响后续调度。
12. UI 测试两个页面都显示状态。
13. 单元测试同一 `request_id + credential_id` 多次 429 只记一个 outcome。
14. 单元测试最近 100 outcome 的人工恢复条件。
15. 单元测试全局 429 波动时不升级单账号人工恢复。

## 关键测试场景

### 场景 1：sticky 账号 429，无 reset

输入：

```text
session S 绑定账号 A
A 返回 429 no reset
B 健康
```

预期：

```text
A rate_limited_until = now + 45s ± 20%
S 解绑 A
当前请求 fallback 到 B
B 成功后 S 绑定 B
usage 记录 attempted=[A,B], final=B
账号列表显示 A 限流冷却中
```

### 场景 2：priority 最高账号 429

输入：

```text
priority A=0
priority B=1
A rate_limited_until 未过期
```

预期：

```text
调度 B
不会因为 A priority 更高而继续打 A
```

### 场景 3：balanced 模式下坏账号 success_count 最低

输入：

```text
A success_count=0 但 rate_limited
B success_count=100 且 healthy
```

预期：

```text
选择 B
不能因为 A success_count 低而选择 A
```

### 场景 4：MCP 工具调用 429

输入：

```text
MCP 第一次选择 A
A 返回 429
B 健康
```

预期：

```text
A 进入 cooldown
本次 MCP fallback 到 B
不会连续 9 次都打 A
```

### 场景 5：generic 500

输入：

```text
A 返回 500
```

预期：

```text
同账号原地 retry 一次
第二次仍失败则 fallback
A transient_failure_count += 1 或 2
不立即 hard disable
```

### 场景 6：suspicious 429

输入：

```text
A 返回 429，body 包含 suspicious activity
```

预期：

```text
A temp_unschedulable = 30min ± 20%
当前请求 fallback
sticky 解绑
UI 显示风控冷却
```

### 场景 7：所有账号都 cooldown

输入：

```text
A rate_limited_until = now + 40s
B temp_unschedulable_until = now + 120s
```

预期：

```text
不执行多轮无意义 retry
返回 503
retry-after = 40
事件记录 all_credentials_unavailable
```

### 场景 8：账号多次 no-reset 429 后恢复成功

输入：

```text
A 第一次 429 -> 45s cooldown
冷却后 probe 成功
```

预期：

```text
A 恢复 healthy
recent_429_count 衰减
不 hard disable
```

### 场景 9：账号 24 小时内大量 no-reset 429 且无成功

输入：

```text
A 最近 100 个有效 outcome 中 98 个是 429
A 至少 6 小时没有成功
A 至少 8 次 half-open probe 失败
池内其他账号同期有成功请求
```

预期：

```text
A 进入 manual_recovery_required
A disabled=false
普通调度不再选择 A
UI 显示“429 高比例，需手动恢复/测试”
用户手动 reset 后清理 429 状态并允许 probe
```

### 场景 10：同一请求内部多次 retry 不放大 429 统计

输入：

```text
request R1 对账号 A 发生 3 次 upstream attempt
3 次 attempt 都返回 429
```

预期：

```text
credential_outcomes 只记录 1 条 A/R1 的 429 outcome
rate_limit_level 只按一次 original request 升级
不会因为内部 retry 直接填满统计窗口
```

### 场景 11：全局 Kiro 429 波动

输入：

```text
5 分钟内 A/B/C/D/E 中 4 个账号都出现 429
池整体成功率低于 50%
```

预期：

```text
触发 global_429_backoff
暂停单账号 manual_recovery_required 升级
无健康账号时快速返回 503 + retry-after
保留少量 probe
```

## 风险与约束

### 风险 1：cooldown 太激进导致账号池被打空

缓解：

1. no-reset 429 第一轮只短冷却。
2. hard disable 阈值要高。
3. 有成功就衰减计数。
4. 全池不可用时快速失败并给 retry-after。

### 风险 2：cooldown 太保守导致继续撞坏号

缓解：

1. 明确 429 立即 fallback。
2. sticky 不健康必须解绑。
3. MCP 也必须使用 cooldown。
4. 连续 429 快速升级。

### 风险 3：把请求错误误判为账号错误

缓解：

1. 400 / input too long 不影响账号。
2. 明确区分 request-level 和 account-level。
3. classifier 需要单元测试覆盖典型 body。

### 风险 4：流式请求中途无法 fallback

缓解：

1. header 前 fallback。
2. body 中断只影响后续调度。
3. usage 记录 stream error 和账号。

### 风险 5：多进程部署状态不同步

缓解：

1. 调度动态状态优先 Redis。
2. sticky、cooldown、probe lock、outcome 窗口都使用 Redis key，天然跨进程共享。
3. PG 只持久化人工恢复等低频状态。
4. Redis 不可用时单进程可降级内存，多进程应在 health 中标记 degraded。

### 风险 5.1：Redis 状态丢失导致冷却丢失

缓解：

1. Redis key 设置合理 TTL，短期状态允许丢失后重新学习。
2. `manual_recovery_required` 必须同步 PG，避免长期坏账号因 Redis 重启重新进入调度。
3. Redis Stream 事件只作为近期排查，长期审计需要异步落 PG。
4. 服务启动时从 PG 加载 manual recovery 状态，并写回 Redis state hash。

### 风险 6：两个 UI 状态不一致

缓解：

1. 后端 API 增加统一字段。
2. 两个 UI 使用相同 API 类型。
3. 测试同时覆盖 `admin-ui` 和 `frontend`。

## 不做的事

第一版不建议做：

1. 完整机器学习式调度评分。
2. 跨进程强一致调度锁。
3. 模型级 rate limit 全量实现。
4. 复杂 half-open 并发闸门。
5. 把所有历史错误迁移为新事件。

可以先做账号级状态机，把最严重的“坏账号持续被调度”解决。

## 最终推荐方案摘要

最终方案不是“改成 balanced”，也不是“所有 429 冷却 30 分钟”，而是：

1. 建立账号可调度状态机。
2. 调度动态状态尽可能使用 Redis，不把 outcome/cooldown 高频数据写 PG。
3. PG 只保留账号配置、hard disabled、manual recovery required 和必要审计。
4. 所有调度入口先读取 Redis state 并过滤不可调度账号。
5. 429 进入 Redis rate limit cooldown，不永久禁用。
6. 429 立即 fallback，不默认原地重试。
7. Kiro 无 reset 429 使用短冷却 + half-open probe + outcome 比例判断。
8. 最近 100 次这类阈值只是默认窗口，人工恢复必须同时满足样本数、比例、无成功时间、probe 失败次数和其他账号健康。
9. 全局 429 波动时暂停单账号人工恢复升级。
10. 408 / generic 5xx 原地 retry 最多一次，然后 fallback。
11. suspicious / abuse 长冷却，重复后手动恢复。
12. auth permanent / invalid config 直接 hard disable。
13. sticky 只在账号健康时命中，不健康就通过 Redis compare-and-delete 解绑。
14. fallback 成功后，在原 sticky 账号不可调度时改绑成功账号。
15. MCP / 工具调用复用同一套策略。
16. provider error 携带 attempt 信息，usage 和 UI 能解释发生了什么。
17. 全池不可用时快速 503，不让用户长时间等待无意义重试。
