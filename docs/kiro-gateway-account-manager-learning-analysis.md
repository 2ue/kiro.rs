# kiro-gateway 与 kiro-account-manager 可学习点分析

日期：2026-05-29

当前项目：`/Users/yuanfeijie/Desktop/procode/kiro.rs`

对照项目：

- `../kiro-gateway`
- `../kiro-account-manager`

本文目的：独立分析这两个相邻项目中是否存在值得 `kiro.rs` 学习、吸收或避免的设计。本文不依赖任何聊天上下文，读者只看本文即可理解分析背景、结论、原因和后续建议。

## 1. 总结结论

结论：两个项目都有值得学习的地方，但都不适合整体照搬。

`kiro.rs` 当前已经明显更偏生产级服务：

- PostgreSQL/Redis 是核心运行依赖，能支持多实例共享调度状态。
- 已有凭据调度、冷却、并发 lease、全局队列、会话绑定、调用链路记录、RPM/TPM 统计、代理资源、模型目录初始化和同步等能力。
- 最近已经吸收过一部分 Kiro-Go / 同类项目中的协议兼容点，例如 tool schema 递归清洗、tool name 映射、模型映射和日志记录。

但从 `../kiro-gateway` 和 `../kiro-account-manager` 还能继续学习几个高价值方向：

1. 请求打到 Kiro 上游前的 payload 大小防御。
2. 400 `Improperly formed request` 的更细粒度定位、日志和错误提示。
3. 历史消息裁剪时对 tool_use/tool_result 配对的保护。
4. 无 tools 定义但历史中存在 tool 内容时，将 tool 内容文本化保留，而不是直接丢弃。
5. Responses API 有状态会话恢复时，继承上一轮 tools/tool_choice。
6. 调度健康分可以纳入响应时间、近期成功率等信号，但不能替代当前 Redis lease 和冷却机制。
7. UI/运维体验上可以学习更清晰的请求日志、模型映射日志和可诊断信息。

不建议照搬的部分：

1. `kiro-gateway` 的单进程 state.json 状态模型不适合当前项目。
2. `kiro-gateway` 的“gateway, not gatekeeper”模型透传理念不适合当前项目的产品目标，因为当前项目已经要求减少下游 400，并且维护本地模型目录参与映射。
3. `kiro-account-manager` 的负载均衡只是本地桌面应用级别，不适合直接替换当前服务端调度。
4. `kiro-account-manager` 的响应缓存、prompt 过滤、模型自动降级策略需要谨慎，直接用于代理服务可能改变下游语义。

## 2. 分析范围

本次重点查看了以下方向：

- Kiro 上游请求格式转换。
- Anthropic/OpenAI/Responses API 的兼容转换。
- 模型名称解析与映射。
- tool schema、tool name、tool_result/tool_use 的兼容处理。
- Kiro 上游 400/402/403/429/5xx 的错误分类。
- payload 体积控制和历史裁剪。
- 账号调度、健康状态、冷却与失败处理。
- 代理、profileArn、上游请求日志、会话恢复。
- 当前 `kiro.rs` 已有对应能力，避免重复建议。

本文不会评价这两个项目整体代码质量，只判断它们对于当前 `kiro.rs` 是否有可吸收价值。

## 3. 当前 kiro.rs 的已有能力基线

在分析外部项目前，需要先明确当前项目已经具备哪些能力，否则容易把已经实现的能力误判为待吸收。

### 3.1 调度与状态

当前项目在 `src/kiro/token_manager.rs` 已经实现较完整的服务端调度能力：

- Redis 调度状态同步。
- 凭据级并发 lease。
- 全局并发 lease。
- 调度排队上限。
- 会话绑定和 sticky fallback。
- 冷却、rate limit、近期调度次数、总调度次数。
- 支持 Redis 下的多实例共享状态。
- 新系统冷启动/均衡模式下已经针对“所有账号预热次数相同导致长期打第一个账号”的问题做过优化。

典型位置：

- `src/kiro/token_manager.rs`：`acquire_context_for_session`、`select_next_credential_excluding`、`mark_rate_limited_at`、`acquire_in_flight_slot`。
- `src/storage/redis_cache.rs`：Redis 状态持久化与分布式 lease。

因此，不应再从外部项目照搬单机调度器、state.json 状态文件或本地内存负载均衡器。

### 3.2 模型目录与模型映射

当前项目已经有本地 seed 文件：

- `data/kiro-upstream-models.seed.json`

并且在 `src/anthropic/model_capabilities.rs` 中实现：

- seed 加载。
- 上游模型同步。
- 请求模型解析。
- alias/family-normalized 映射。
- 不支持模型时前置返回 400，而不是盲目打到 Kiro 上游。

典型位置：

- `src/anthropic/model_capabilities.rs`：`resolve_model_with_catalog`。
- `src/anthropic/handlers.rs`：`resolve_request_model`。
- `src/anthropic/converter.rs`：`map_model`。

这比简单的静态模型映射更适合当前服务，因为当前目标是“减少下游因为模型不匹配产生 400”。

### 3.3 tool schema 与 tool name

当前项目已经在 `src/anthropic/converter.rs` 中处理了 Kiro 对工具定义的敏感点：

- 递归移除 `additionalProperties`。
- 移除非法或空 `required`。
- 修复根 schema 缺失 `type/properties`。
- 将 `_`、`-`、非 ASCII 字母数字分隔符转换为 Kiro-safe 的 camelCase 风格名称。
- 超长 tool name 使用确定性 hash 后缀缩短。
- 保留 short -> original 的映射，用于响应中还原工具名称。

这部分已经吸收了 `Kiro-Go`/同类实现的核心思想，且当前实现比简单截断更安全，因为能避免 `foo-bar` 与 `foo_bar` 这类映射碰撞。

### 3.4 错误处理

当前项目在 `src/kiro/provider.rs` 中已经区分：

- 402 月度额度/支付类问题。
- 401/403 凭据/权限问题。
- 429 限流问题。
- 400 Bad Request 不做账号轮换。
- 上下文超长类错误映射为下游 400。

这一点方向正确：400 通常是请求结构问题，盲目切账号会浪费请求、污染调度健康状态，也可能让日志更难定位。

## 4. ../kiro-gateway 的可学习点

`../kiro-gateway` 是 Python/FastAPI 风格的网关项目。它的强项不在生产级调度，而在 Kiro 协议兼容、防止上游 400、请求转换前防御。

### 4.1 值得学习：payload size guard

`../kiro-gateway/kiro/payload_guards.py` 有单独的 payload 体积防护模块。

核心思想：

- Kiro 上游在 payload 超大时可能返回误导性的 `400 Improperly formed request`。
- 所以在发送前先计算 JSON 序列化后的字节数。
- 超限时裁剪最旧 history。
- 裁剪后确保 history 起点是 userInputMessage。
- 裁剪后修复孤立 toolResults。
- 移除空的 `toolUses: []`，因为这也是 Kiro 敏感点。

参考实现：

- `kiro/payload_guards.py:20-28`：说明 Kiro 超大 payload 会返回 misleading 400。
- `kiro/payload_guards.py:46-48`：计算 UTF-8 JSON 字节数。
- `kiro/payload_guards.py:51-56`：去掉空 `toolUses`。
- `kiro/payload_guards.py:59-63`：裁剪后对齐到 user 消息。
- `kiro/payload_guards.py:66-118`：修复孤立 toolResults，并把文本保留到用户内容中。
- `kiro/payload_guards.py:121-164`：裁剪主流程。

当前 `kiro.rs` 的状态：

- 当前项目已处理上下文超长错误返回。
- 当前项目已有 token 估算、模型上下文窗口判断。
- 但没有看到一个统一的“最终 Kiro payload 序列化字节数 preflight + 自动裁剪”模块。

建议：

优先吸收这个思想，在 `kiro.rs` 中新增最终 payload guard，位置建议在转换完成之后、调用 `KiroProvider` 之前。

设计要求：

- 对最终发给 Kiro 的 JSON payload 做字节级测量，不只看 token。
- 限制值建议可配置，默认保守，例如 450KB 或 512KB，不直接顶到疑似 615KB。
- 如果超限，裁剪最旧 history。
- 裁剪必须保持 user/assistant 结构合法。
- 裁剪必须保护 tool_use/tool_result 配对。
- 裁剪后记录日志和调用链路字段：
  - original_payload_bytes
  - final_payload_bytes
  - trimmed_history_entries
  - repaired_orphan_tool_results
  - removed_empty_tool_uses

收益：

- 减少线上 `400 Improperly formed request`。
- 当仍然 400 时能排除“payload 太大”这个原因。
- 对 Claude Code、MCP、多轮长会话尤其有价值。

风险：

- 自动裁剪会改变模型上下文。
- 必须向调用日志暴露裁剪事实，否则下游用户不知道为什么模型“忘记”了早期上下文。

推荐等级：P0。

### 4.1.1 当前项目已实施的吸收结果

本次已按当前 `kiro.rs` 的真实请求链路实施，不是旁路实验代码。

新增模块：

- `src/anthropic/payload_guard.rs`

接入位置：

- `/v1/messages`、`/na/v1/messages`、`/ha/v1/messages` 共用的 `post_messages_inner`。
- `/cc/v1/messages` 专用的 `post_messages_cc`。
- 都是在 Anthropic 请求转换成 `KiroRequest` 后、调用 `KiroProvider` 前执行。

已实现能力：

- 按最终 Kiro JSON request body 的真实字节数计算 payload 大小。
- 默认最大值为 `450 * 1024` bytes。
- 超限时裁剪最旧 history，而不是继续打上游碰运气。
- 裁剪后对齐 history 起点，避免以 assistant 消息开头。
- 移除空 `toolUses: []`。
- 修复 history 中孤立 tool_result，把可读内容文本化追加到 user content。
- 修复 current message 中孤立 tool_result。
- 移除没有下一个 tool_result 配对的 tool_use。
- 特别保护“历史最后 assistant tool_use + 当前 user tool_result”这一 Claude Code/MCP 常见合法形态，避免误删。
- 仍超限时前置返回下游 `400 invalid_request_error`，不再浪费账号打 Kiro 上游。
- 当启用 debug/告警头时，把裁剪和修复动作合并进 `x-kiro-rs-warnings`。
- 日志记录 original/final bytes、裁剪条数、修复计数、endpoint、模型映射和 conversation id。

运行时配置：

- `payloadGuardEnabled`
- `payloadGuardMaxBytes`
- `payloadGuardTrimHistory`

配置已写入：

- `src/model/config.rs`
- `src/admin/types.rs`
- `src/admin/service.rs`
- 新 UI：`ui/src/features/runtime/runtime-sections.tsx`
- 旧 UI：`admin-ui/src/components/runtime-config-panel.tsx`

这些配置写入 PgSQL runtime_config，热加载后对新请求生效。

### 4.2 值得学习：将 orphan tool_result 文本化保留

`kiro-gateway` 对孤立 tool_result 有一个更温和的做法：

- 如果 tool_result 找不到前面的 tool_use，不直接让请求进入 Kiro。
- 也不是简单丢弃。
- 而是把 tool_result 文本转换成普通 user content，保留上下文信息。

参考实现：

- `kiro/converters_core.py:995-1068`：`ensure_assistant_before_tool_results`。
- `kiro/converters_core.py:863-904`：`tool_results_to_text`。
- `kiro/payload_guards.py:66-118`：裁剪后 orphan toolResults 修复。

当前 `kiro.rs` 的状态：

- 当前项目已经有 tool_use/tool_result 配对校验。
- 孤立 tool_result 会被跳过。
- 孤立 tool_use 会从 history 中移除。
- 这能减少 400，但可能损失一部分对话信息。

参考位置：

- `src/anthropic/converter.rs`：`validate_tool_pairing`。
- `src/anthropic/converter.rs`：`remove_orphaned_tool_uses`。

建议：

可以引入一个非 strict 模式下的增强策略：

- 孤立 tool_result 不作为 Kiro toolResult 发送。
- 但将其内容追加为普通文本，例如：

```text
[Tool Result: <tool_use_id>]
<content>
```

注意：

- 只在兼容模式启用。
- strict profile 仍应返回错误或保持严格行为。
- 需要记录 warnings，避免静默改变协议语义。

收益：

- 对截断历史、Responses 会话恢复、客户端少传上一轮 assistant tool_use 的场景更稳。
- 减少因 tool 配对问题导致的 400，同时减少信息损失。

风险：

- 模型可能把工具结果当作普通用户文本，不再拥有严格 tool_result 语义。
- 对某些 agent 流程可能改变行为。

推荐等级：P1。

### 4.3 值得学习：无 tools 定义时将历史 tool 内容文本化

`kiro-gateway` 明确处理一种常见客户端行为：

- 请求历史里带了 tool_calls/tool_results。
- 但当前请求没有 tools 定义。
- Kiro 上游可能因为历史引用了未知工具而 400。

它的处理方式是把 tool_calls/tool_results 转成普通文本，保留上下文，不作为工具协议字段发送。

参考实现：

- `kiro/converters_core.py:911-992`：`strip_all_tool_content`。
- `kiro/converters_core.py:826-860`：`tool_calls_to_text`。
- `kiro/converters_core.py:863-904`：`tool_results_to_text`。

当前 `kiro.rs` 的状态：

- 当前项目会收集 history 中出现的工具名，并为缺失工具生成 placeholder tool。
- 这可以解决一部分 Kiro 要求“历史工具必须有定义”的问题。

参考位置：

- `src/anthropic/converter.rs`：收集历史工具名、生成 placeholder tool。

对比判断：

- `kiro.rs` 当前 placeholder tool 策略更接近保留工具语义。
- `kiro-gateway` 的文本化策略更保守，适合不确定工具定义是否可靠时兜底。

建议：

不要替换当前 placeholder tool 策略。可以作为第二层 fallback：

1. 如果 history 工具名能安全映射且能生成 placeholder tool，继续当前策略。
2. 如果工具名/schema 无法安全生成，或 payload guard 裁剪后工具链断裂，则把相关工具内容文本化。

推荐等级：P2。

### 4.4 值得学习：模型名规范化测试用例

`kiro-gateway` 的模型解析有大量面向客户端格式的测试，例如：

- `claude-haiku-4-5-20251001` -> `claude-haiku-4.5`
- `claude-sonnet-4-20250514` -> `claude-sonnet-4`
- `claude-3-7-sonnet-20250219` -> `claude-3.7-sonnet`
- `claude-4.5-opus-high` -> `claude-opus-4.5`
- 带 `[1m]` 或 `[200k]` 的客户端窗口后缀。

参考实现：

- `kiro/model_resolver.py:87-189`：模型名称规范化。
- `tests/unit/test_model_resolver.py`：覆盖大量模型名变体。

当前 `kiro.rs` 的状态：

- 当前项目已有更适合本项目目标的模型目录、seed、同步和解析。
- 当前项目已经将“不支持模型”前置返回，而不是直接透传给上游。

建议：

不是照搬 `kiro-gateway` 的 pass-through 策略，而是吸收测试样本。

应补充测试覆盖：

- date suffix。
- dash-to-dot。
- thinking suffix。
- `[1m]` suffix。
- legacy Claude 3.x。
- Cursor/其他客户端 inverted format，如 `claude-4.5-opus-high`。
- 下游传未知模型时，按当前项目规则返回可解释错误或映射到配置项，而不是无日志打到上游。

推荐等级：P1。

### 4.5 可以参考但不建议照搬：错误分类

`kiro-gateway` 对错误分为：

- FATAL：请求问题，不继续换账号。
- RECOVERABLE：账号问题，可以换账号。

参考实现：

- `kiro/account_errors.py`。

其中：

- 402、403、429 归为 RECOVERABLE。
- 400 + `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 归为 FATAL。
- 400 + null reason 归为 FATAL。
- 422 归为 FATAL。
- 5xx 归为 FATAL。

当前 `kiro.rs` 的状态：

- 当前项目已经有类似思想，并且更贴合生产调度。
- 但是 5xx 是否一定 FATAL 要更谨慎。

建议：

不要完全照搬。当前项目应该按 Kiro 上游实际语义拆分：

- 400 malformed/request schema：fatal，不换账号。
- 400 INVALID_MODEL_ID：如果模型已通过本地 catalog 映射仍出现，记录模型映射缺陷；不要无脑轮换。
- 402 quota/monthly：账号级冷却或禁用，必要时切换。
- 403 auth：凭据级失败，刷新 token 或切换。
- 429 rate limit：凭据级冷却，尊重 retry-after。
- 502/503/504：
  - 如果是 Kiro 全局服务异常，短暂全局 backoff。
  - 如果仅单账号/单代理持续失败，账号/代理维度冷却。
  - 不应简单全部 fatal，也不应无限切账号。

推荐等级：P1。

### 4.6 不建议照搬：state.json 单机状态

`kiro-gateway` 使用文件状态：

- `state.json`
- account failures
- model_to_accounts
- current_account_index

这对单机 Python 网关简单有效，但不适合当前项目。

原因：

- 当前项目是服务端系统，Redis/PostgreSQL 是设计前提。
- 多实例部署需要共享调度状态。
- 文件状态无法可靠支持并发实例、容器滚动更新、跨进程 in-flight、全局队列。

结论：只学习“记录必要运行状态”的思想，不使用 state.json 模型。

推荐等级：不采纳。

## 5. ../kiro-account-manager 的可学习点

`../kiro-account-manager` 是 Tauri 桌面应用，带一个本地 gateway。它的强项在本地 UI、会话恢复、请求日志、负载健康指标、prompt 过滤等桌面使用体验；弱项是生产级多实例服务端调度。

### 5.1 值得学习：Responses API 会话恢复继承 tools/tool_choice

`kiro-account-manager` 在 Responses API 场景中维护本地 response session：

- 保存 `response_id`。
- 保存上一轮 request messages。
- 保存上一轮 tools。
- 保存上一轮 tool_choice。
- 当下一次请求携带 `previous_response_id` 但没有重传 tools/tool_choice 时，从历史 session 继承。
- 对最后一轮 tool_calls 还会根据当前 tool_result_id 做过滤，避免恢复过多无关 tool_call。

参考实现：

- `src-tauri/src/gateway/proxy.rs:80-160`：恢复 previous_response_id 链路下的 messages。
- `src-tauri/src/gateway/proxy.rs:163-196`：继承 tools/tool_choice。
- `src-tauri/src/gateway/proxy.rs:198-222`：持久化 response session entry。
- `src-tauri/src/gateway/proxy.rs:1229-1246`：实际请求中应用恢复。

当前 `kiro.rs` 的状态：

- 当前主入口是 Anthropic `/v1/messages`。
- 如果项目后续要完整支持 OpenAI Responses API 或更强的 Claude Code 会话兼容，这个能力有价值。

建议：

如果当前项目已有 Responses 兼容入口，应补齐：

- previous_response_id -> session chain。
- tools/tool_choice 继承。
- tool_call 与 tool_result 按 id 过滤。
- session TTL。
- session 存储位置应使用 Redis，而不是本地内存。

如果当前项目暂时不做 Responses API，则无需立即实施。

推荐等级：P2，取决于是否要支持 OpenAI Responses API。

### 5.2 值得学习：payload 大小裁剪的第二个实现样本

`kiro-account-manager` 也实现了 payload 字节限制裁剪：

- 常量 `MAX_KIRO_PAYLOAD_SIZE: 450 * 1024`。
- 构建 Kiro payload 后再做 JSON value 级别裁剪。
- 超限后循环裁剪历史。
- 优先保留最近消息。
- 尝试保护 tool call/result 对。

参考实现：

- `src-tauri/src/gateway/proxy.rs:40`：最大 payload 字节常量。
- `src-tauri/src/gateway/proxy.rs:668-770`：`trim_kiro_payload_history`。
- `src-tauri/src/gateway/proxy.rs:1413-1435`：发送前裁剪。

需要注意：

这个实现中的 JSON 字段名检查疑似使用了 snake_case，例如 `assistant_response_message`、`tool_uses`，而实际 Kiro payload 序列化常见是 camelCase，例如 `assistantResponseMessage`、`toolUses`。如果直接照搬，可能导致 tool 配对保护失效。

建议：

可学习“450KB 保守阈值”和“发送前二次检查”的思路，但不要直接复制实现。当前项目应基于自己的 Rust 类型和实际序列化字段写测试。

推荐等级：P0 思想吸收，代码不照搬。

### 5.3 可参考：负载健康分

`kiro-account-manager` 的 `load_balancer.rs` 有一个简单健康模型：

- active_connections
- recent_failures
- recent_successes
- avg_response_time_ms
- is_healthy
- health_score

参考实现：

- `src-tauri/src/gateway/load_balancer.rs:44-62`：健康状态字段。
- `src-tauri/src/gateway/load_balancer.rs:77-98`：健康分计算。
- `src-tauri/src/gateway/load_balancer.rs:100-145`：成功/失败/连接数更新。
- `src-tauri/src/gateway/load_balancer.rs:174-203`：按策略选择账号。

当前 `kiro.rs` 的状态：

- 当前项目已有比它更强的并发 lease、冷却、近期/总调度次数、会话绑定、代理维度检查。
- 但当前项目可以继续增强调度评分信号：
  - 最近成功率。
  - 最近失败率。
  - 平均响应时间或 P95 响应时间。
  - 首 token 延迟。
  - 代理资源失败率。

建议：

不要引入它的 LoadBalancer 类，不要替换当前调度。可以把健康分作为 current scheduler 的一个额外 scoring factor。

推荐设计：

```text
final_score =
  base_strategy_score
  + low_in_flight_bonus
  + low_recent_selection_bonus
  + warmup_bonus
  - cooldown_penalty
  - rate_limit_penalty
  - recent_error_penalty
  - high_latency_penalty
```

调度硬过滤仍应优先：

1. disabled 不参与调度。
2. proxy disabled 不参与调度。
3. cooldown/rate-limit 未到期不参与调度。
4. 并发无空位不参与调度。
5. 模型不可用不参与调度。

健康分只能在“可调度候选集合”内排序，不能让冷却中的账号提前被选中。

推荐等级：P2。

### 5.4 可参考但需要谨慎：prompt_filter

`kiro-account-manager` 有系统提示过滤器：

- 检测 Claude Code 系统提示并替换成简化版。
- 去除边界标记。
- 去除环境噪音行。
- 支持自定义规则。

参考实现：

- `src-tauri/src/gateway/prompt_filter.rs`。
- `src-tauri/src/gateway/proxy.rs:1248-1263`：请求中应用 prompt filter。

当前 `kiro.rs` 的状态：

- 当前项目更像通用代理服务。
- 直接过滤/替换用户系统提示可能改变语义，风险高。

建议：

不要默认启用。最多作为可选兼容 profile：

- 默认关闭。
- 只对明确选择 Claude Code compat profile 的 key/请求启用。
- 日志记录是否应用过滤，但不记录完整敏感 system prompt。

推荐等级：P3。

### 5.5 不建议照搬：MostQuota 基于 remaining/total

`kiro-account-manager` 有 `MostQuota` 策略，使用 `remaining/total` 计算剩余额度百分比。

问题：

- 用户之前已经明确指出：Kiro 官方接口查出来的是 credits，不是美元。
- 官方 credits 设置为 2000 或其他值时，实际可能超出这个积分继续可用。
- 当前系统记录的是本地估算美元成本，不应与 credits 混为一个硬调度阈值。

因此：

- 不应把 credits remaining 作为硬性不可调度条件。
- 不应简单做 `remaining <= 0` 禁用。
- credits 可作为展示信息和软参考。
- 调度更可靠的硬信号应该来自上游错误：
  - 402
  - 429
  - auth failure
  - repeated 5xx/network/proxy failure
  - 明确 subscription/model unavailable

推荐等级：不采纳。

### 5.6 不建议照搬：本地内存响应缓存

`kiro-account-manager` 对非流式请求有响应缓存：

- 按 session、messages hash、message_count、chars 匹配。
- 命中后直接返回缓存响应。

这对桌面本地 app 可能有体验价值，但对当前代理服务风险较高：

- 代理服务通常应保持请求语义透明。
- 工具调用、状态查询、实时上下文不能被缓存。
- 下游可能依赖每次请求真实执行。
- 多账号调度下，缓存可能掩盖账号真实状态。

建议：

当前项目不引入响应缓存。可以仅保留 prompt cache usage 模拟，但不要返回旧响应。

推荐等级：不采纳。

## 6. 当前项目相对两个项目更强的地方

### 6.1 生产级调度

`kiro.rs` 当前的调度目标是服务端、多实例、高并发。

相比之下：

- `kiro-gateway` 偏单机 Python 网关。
- `kiro-account-manager` 偏桌面本地网关。

当前项目更强的点：

- Redis 分布式 lease。
- PostgreSQL 持久化。
- 凭据和代理资源统一调度。
- 冷却期内保证其他请求不会继续打到出错账号。
- 大并发下可控排队。
- 调用链路记录。
- 管理 UI 状态可见。

因此调度主体不应被两个项目替换。

### 6.2 模型目录持久化

当前项目已经维护本地 seed 文件、数据库初始化和页面同步上游模型能力。

这比 `kiro-gateway` 的 pass-through 更符合当前目标：

- 下游模型可能不固定。
- Kiro 上游模型可能不固定。
- 需要尽可能把下游模型映射到 Kiro 支持模型，减少 400。
- 日志里记录模型转换链路。

### 6.3 代理资源模型

当前项目已经做了代理资源/凭据绑定：

- 可以在代理 tab 统一维护代理。
- 凭据可以绑定代理。
- 禁用代理和可用代理需要区分样式。
- 调度时需要考虑凭据绑定代理是否可调度。

`kiro-account-manager` 的本地 gateway 里虽然也涉及 proxy 逻辑，但不是当前系统这种“资源池 + 凭据绑定 + UI 管理 + 调度约束”的完整模型。

## 7. 当前项目仍建议补齐的能力

### 7.1 P0：最终 Kiro payload 字节级 guard

这是本次最重要的建议。

原因：

- Kiro 上游的 `Improperly formed request` 不一定是 schema 错，也可能是 payload 超大。
- 单靠 token 估算不能覆盖 JSON 序列化体积、base64 image/document、tool schema 巨大、历史 tool result 巨大等场景。

建议实现：

1. 在转换完成后，对最终 Kiro 请求体序列化。
2. 计算字节数。
3. 如果超过配置阈值，裁剪 history。
4. 裁剪后再次验证：
   - history 第一条应是 user。
   - 不存在空 `toolUses: []`。
   - 不存在 orphan toolResults。
   - 不存在 orphan toolUses。
5. 如果仍超限，返回 400，并明确说明 payload too large，而不是继续发给上游。

建议配置：

```toml
kiro_payload_max_bytes = 524288
kiro_payload_guard_enabled = true
kiro_payload_trim_history = true
```

日志建议：

```json
{
  "payload_guard": {
    "enabled": true,
    "original_bytes": 711203,
    "final_bytes": 498112,
    "trimmed_history_entries": 8,
    "removed_empty_tool_uses": 2,
    "repaired_orphan_tool_results": 1
  }
}
```

### 7.2 P0：增强 Improperly formed request 诊断

当前已经有文档 `docs/kiro-400-improperly-formed-request-analysis.md` 分析 400。

下一步建议在运行日志/调用链路里补齐：

- upstream_status
- upstream_reason
- upstream_message
- request_model
- upstream_model
- model_resolution_source
- final_payload_bytes
- message_count
- history_count
- tools_count
- tool_name_mappings_count
- schema_sanitized_count
- payload_trimmed
- has_images
- has_documents
- has_tool_results
- has_orphan_tool_warning

这样线上看到：

```text
400 Bad Request {"message":"Improperly formed request.","reason":null}
```

时，可以快速判断是：

- 模型映射问题。
- tool schema 问题。
- tool pair 问题。
- payload 过大。
- 多模态 source 问题。
- 当前服务转换问题。
- 下游请求本身不合法。

### 7.3 P1：orphan tool_result 文本化保留

当前跳过孤立 tool_result 能减少 400，但会丢信息。

建议在兼容模式下：

- 孤立 tool_result 不进入 Kiro toolResults。
- 将其文本追加到 user content。
- 增加 warning。

### 7.4 P1：模型规范化测试矩阵

当前模型映射方向正确，但建议吸收 `kiro-gateway` 的测试样本。

建议加入测试用例覆盖：

```text
claude-3-5-haiku-20241022
claude-3-5-sonnet-20240620
claude-3-5-sonnet-20241022
claude-3-7-sonnet-20250219
claude-haiku-4-5-20251001
claude-opus-4-1-20250805
claude-opus-4-20250514
claude-opus-4-5-20251101
claude-opus-4-6
claude-opus-4-7
claude-sonnet-4-20250514
claude-sonnet-4-5-20250929
claude-sonnet-4-6
claude-4.5-opus-high
claude-4.5-sonnet-low
claude-opus-4.7[1m]
```

### 7.5 P2：调度评分增加延迟/成功率维度

当前调度已经比两个项目强，但可以进一步加入：

- 最近成功率。
- 最近失败率。
- 平均响应时间。
- P95 延迟。
- 首 token 延迟。
- 代理维度失败率。

注意：

- 这些只作为排序分，不作为绕过 cooldown/rate-limit/lease 的理由。
- 不能影响硬过滤。

### 7.6 P2：Responses API 会话恢复

如果后续要增强 OpenAI Responses API 兼容，建议学习 `kiro-account-manager`：

- previous_response_id session chain。
- tools/tool_choice 继承。
- tool_call id 过滤。
- Redis TTL 存储。

如果当前只保证 Anthropic `/v1/messages`，此项可以后置。

## 8. 不建议实施的能力

### 8.1 不引入 state.json

当前系统 PostgreSQL/Redis 是核心设计，不要引入文件状态作为调度来源。

### 8.2 不默认过滤系统提示

prompt_filter 只能作为可选 compat profile，不能默认开启。

### 8.3 不把 credits 当硬调度阈值

credits 用于展示和统计，不应作为账号不可调度的硬判断。

账号是否暂时不可调度应由上游真实错误和冷却状态决定。

### 8.4 不做透明响应缓存

代理服务默认不应缓存模型响应。

### 8.5 不整体替换当前调度器

两个项目的调度都不如当前项目适合多实例服务端。

## 9. 建议落地顺序

建议按以下顺序实施：

### 第一阶段：减少 400

1. 实现最终 Kiro payload byte guard。
2. 裁剪 history 并保护 tool pair。
3. 增强 400 日志字段。
4. 为 `Improperly formed request` 返回更明确下游错误。

### 第二阶段：增强兼容

1. orphan tool_result 文本化。
2. 无 tools 定义但历史有工具内容时 fallback 文本化。
3. 模型规范化测试矩阵。

### 第三阶段：增强调度评分

1. 加入近期成功率/失败率。
2. 加入响应时间/首 token 延迟。
3. 加入代理资源维度健康。
4. 在 UI 展示健康信号。

### 第四阶段：Responses API 兼容

1. previous_response_id 会话链。
2. tools/tool_choice 继承。
3. Redis TTL 存储。

## 10. 最终判断

`../kiro-gateway` 值得主要学习“协议转换前防御”和“测试样本”：

- payload size guard。
- tool schema 清洗。
- orphan tool_result 文本化。
- 模型名规范化测试。
- 错误分类思想。

`../kiro-account-manager` 值得主要学习“本地网关体验”和“Responses 会话兼容”：

- previous_response_id 会话恢复。
- tools/tool_choice 继承。
- payload byte guard 的另一个样本。
- 请求详情日志。
- 健康分的信号设计。

但当前 `kiro.rs` 不应把它们当成整体替代方案。当前项目的服务端调度、PG/Redis 状态、模型目录持久化、代理资源管理和调用链路记录更适合现有目标。最值得立即吸收的是最终 payload 字节级防御和 400 诊断增强，这两个点最能直接减少线上 `Improperly formed request` 和长会话/MCP 场景的问题。
