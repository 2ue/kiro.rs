# 请求处理管线模块化重构分析

归档状态：2026-07-12 已归档；这是历史落地分析，不是当前实现状态或活动路线图。

原始路径：`docs/request-pipeline-modularization-analysis-20260706.md`

当前权威：[已注册的 request-body 计划](../../plantree/plans/request-body-capability-modularization/README.md)与当前源码；后续跨系统改造由[系统架构现代化计划](../../plantree/plans/system-architecture-modernization/README.md)负责。

归档索引：[Request Body Modularization Archive](README.md)

日期：2026-07-06

状态：当前工作区已在已发布的 raw/normalized 行为边界修正基础上，继续完成文件级模块化重构：入口解析、raw request facts、本地 Kiro body pipeline、外部池 body/model/retry/usage projection 已拆分为独立模块。本文仍保留后续可选的 trait/plugin 化和更深层 route planner 规划。

工作模式：execute-ready。本文记录目标、现状分析、阶段规划和本阶段落地状态。

## 执行版摘要

反思结论：完整长文有价值，但必须配一个短执行版，否则实现容易继续发散。当前已完成的不是单点补丁，而是先把最容易耦合的热路径拆到文件级模块；后续若继续做插件化，应在这些边界上扩展，而不是回到 `handlers.rs`/`external_pool.rs` 里堆分支。

当前已完成的核心内容：

1. **目标选择先于 body 处理**：raw 外部池的显式直连和本地池预检 fallback 都在 parse 前处理，不进入标准 Anthropic body parse、图片处理、Kiro 转换或 payload guard。
2. **外部池按已选 pool 配置分支**：raw pool 走 raw body，normalized pool 才走 normalized body 和外部 payload guard。
3. **payload guard 下沉到 normalized body pipeline**：外部 route 构造不再先跑 payload guard；raw route 不进入外部 guard；payload guard retry 只筛 normalized pool。
4. **token counting lazy 化到外部 usage 需要时**：外部 route 构造不再无条件 `count_all_tokens(...)`；usage projection 需要 input tokens 时再计算。
5. **model 与 body 解耦**：raw body 可以按 `rawModelMode` 选择不改、只探测、或只改顶层 model；normalized body 继续按现有模型映射写入 outbound model。
6. **usage 与 body mode 解耦**：raw body 仍可按 `usageProjectionMode=current_path_policy` 做 usage projection，路径级同步禁用仍是上层拦截。
7. **入口路径去重**：`/v1/messages`、`/na/v1/messages`、`/ha/v1/messages`、`/dfcache/.../v1/messages`、`/cc/v1/messages` 统一进入 `request_entry::handle_messages_endpoint(...)`，避免不同路径漂移。
8. **raw facts 独立**：raw 顶层 `model`/`stream` 探测和可选顶层 model rewrite 已迁移到 `src/anthropic/request_facts.rs`，不再属于外部池调度模块。
9. **本地 body pipeline 独立**：Anthropic -> Kiro 转换、Kiro request 序列化、payload guard、payload diagnostics、warnings、cache-point retry 准备已迁移到 `src/anthropic/handlers/local_body_pipeline.rs`。
10. **外部池处理拆分**：外部池 body、model、retry、usage projection 分别迁移到 `src/external_pool/body_pipeline.rs`、`model_pipeline.rs`、`retry_pipeline.rs`、`usage_projection.rs`。

本轮仍不做的部分：

- 不把当前模块强行抽成 trait/plugin 系统；当前先建立稳定文件边界和调用契约。
- 不改变本地凭证默认 Kiro 处理链的行为。
- 不改 UI 配置结构，因为本轮后端语义兼容现有配置。
- 不把容量等待、连接池 lease、外部池选择算法强拆；这块和 manager 状态强耦合，后续需要单独设计 scheduler trait。

下一步如果继续深化，应在当前文件边界上继续做 `RoutePlanner`/`ProcessingPlan` trait 化，把 direct/preflight/after-local-attempt 的目标选择显式化，并把容量等待/错误归一化拆成独立调度模块。

### 本轮落地记录

已实施：

- `ExternalRouteRequest` 保留原始 `raw_body`，外部池出站准备按已选 pool 的 `request_body_mode` 分支。
- `RawPassthrough` 分支直接走 raw body，不进入外部 payload guard。
- `Normalized` 分支才调用外部 `prepare_external_messages_payload(...)` 和 payload guard。
- payload guard retry route 明确筛选 `Normalized` pool，避免裁剪后的 normalized body 被发到 raw pool。
- 普通 fallback route 不设置 body mode filter，raw pool 不会因为显式直连关闭而被过滤。
- 新增 parse 前 raw 外部池预检 fallback：仅当 raw 外部池当前可用、本地池无模型无关的可调度能力时，在标准 body parse、图片处理、Kiro 转换、Kiro payload guard 之前转 raw 外部池。
- normalized 外部池仍走 parsed path，继续保留现有 source/image/schema 行为。
- 外部 route 构造不再无条件进行 token counting，避免 raw/外部 fallback 在未命中 usage projection 时提前扫描长上下文。
- 新增 `src/anthropic/request_facts.rs`：raw body 轻量 facts 和顶层 model rewrite，不反序列化完整 messages。
- 新增 `src/anthropic/handlers/request_entry.rs`：所有 messages 路径共享 direct/preflight raw、parse、进入 inner pipeline 的入口。
- 新增 `src/anthropic/handlers/local_body_pipeline.rs`：本地 Kiro body 准备独立于 handler 编排。
- 新增 `src/external_pool/body_pipeline.rs`：外部池 raw/normalized body 准备按已选 pool 配置分支。
- 新增 `src/external_pool/model_pipeline.rs`：外部池模型映射、raw model 探测结果处理、Claude 点号版本兼容转换。
- 新增 `src/external_pool/retry_pipeline.rs`：外部 normalized payload guard retry 条件和 retry route 构造。
- 新增 `src/external_pool/usage_projection.rs`：外部池 usage projection context 构造和 prompt-cache 成功提交。

已验证：

- `cargo fmt --check`
- `git diff --check`
- `cargo check`
- `cargo test`：主服务 896 个测试、`kiro_loadtest` 15 个测试通过。
- `cargo build --release`
- `pnpm --dir ui build`
- `cargo test raw_hints_ignore_nested_model_without_top_level_model -- --nocapture`
- `cargo test raw_top_level_model_rewrite_preserves_nested_content -- --nocapture`
- `cargo test external_pool_raw_body_mode_does_not_apply_payload_guard -- --nocapture`
- `cargo test external_pool_normalized_body_mode_applies_payload_guard -- --nocapture`
- `cargo test fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools -- --nocapture`
- `cargo test raw_passthrough_keeps_body_but_still_applies_usage_projection -- --nocapture`
- `cargo test external_payload_guard_retry_route_trims_and_disables_second_retry -- --nocapture`
- `cargo test local_pool_preflight_reason_respects_scheduler_fallback_toggles -- --nocapture`
- `cargo test external_pool_raw_passthrough -- --nocapture`
- `cargo test raw_external_route_request_is_preparse_raw_only -- --nocapture`
- `cargo test parse_messages_payload_rejects_empty_model_before_routing -- --nocapture`
- fake upstream loadtest：normal stream 10/10 200、normal non-stream 10/10 200、slow first byte 6/6 200、429 6/6 429、long stream 4/4 200。
- 临时 release 服务 19022：真实 `/v1/messages` 非流 1 次 200，真实 `/cc/v1/messages` 流式 1 次 200，`message_delta.usage` 非零。
- Claude CLI 2.1.197：隔离 HOME/CLAUDE_CONFIG_DIR 下 `claude --print --output-format=stream-json --verbose` 通过，收到 result，无 error，usage 非零。
- 临时 release 服务已停止，`19022` 端口已释放。

## 本轮目标

用户的核心目标不是“让某个外部池 raw 分支能跑”，而是把请求生命周期拆成可配置、可组合、可验证的模块。正确的高层顺序应该是：

```text
入口请求
  -> 构造不可变 RequestEnvelope 和轻量 hints
  -> 根据路由/调度配置选择目标：本地凭证或某个外部池
  -> 根据已选目标的能力和配置生成 ProcessingPlan
  -> 只执行 ProcessingPlan 中启用的处理模块
  -> 调用上游
  -> 响应、usage、错误、日志各自按配置处理
```

这意味着：

- 选择本地凭证时，默认进入本地 Kiro 兼容处理链，包括 Anthropic -> Kiro 转换、图片/schema/tool/thinking 处理、payload guard、Kiro 请求序列化、usage 记录。
- 选择外部池时，必须按这个外部池自己的配置决定 body 是 normalized 还是 raw passthrough。
- 外部池配置为 raw passthrough 时，狭义 body 处理链不能运行，包括图片物化、media type 修正、schema 修复、payload guard、正文裁剪、深度内容扫描等。
- raw passthrough 不等于禁用模型处理。模型处理应该是独立模块，可以按配置选择不写 body、只探测、或只改顶层 `model`。
- raw passthrough 不等于禁用 usage 整形。usage 是否整形由路径 usage 策略和外部池 usage 策略共同决定。
- 调度模块不能因为 body mode 的实现细节误过滤外部池。只有调用方明确声明 required capability 时，才允许按能力过滤。
- 性能目标和功能目标同等重要：长上下文、大并发、图片、tool_result、深层 JSON 都不能因为未命中的分支提前进入重处理。

## 非目标

当前规划不要求一次性重写整个网关，也不要求立刻把所有代码迁移到插件系统。

明确不做：

- 不把外部池统一改成 raw。
- 不把 raw body 透传强制绑定模型 rewrite。
- 不为了 raw 透传牺牲当前本地凭证的 Kiro 协议兼容能力。
- 不为了模块化新增与现有配置冲突的第二套配置语义。
- 不在没有测试矩阵前发布大范围重构。
- 不把 `/cc/v1/messages` 单独当成唯一标准；`/v1/messages`、`/ha/v1/messages`、`/na/v1/messages`、`/dfcache/.../v1/messages` 都必须共享同一套原则。

## 背景

当前系统已经同时支持本地凭证、外部池、显式外部直连、Raw body 透传、模型映射、usage 整形、路径缓存策略、payload guard、图片处理、错误统一包装等能力。问题是这些能力目前主要集中在 `handlers.rs` 和 `external_pool.rs` 内部，很多配置看起来是独立的，但执行路径上存在隐式耦合。

这会带来几个直接风险：

- **配置语义不聚焦**：例如 Raw body 透传本应只控制“出站 body 是否进入处理链”，但它容易和模型映射、usage 整形、显式直连、payload guard 发生耦合。
- **调度能力被处理模式误伤**：例如普通外部池 fallback 如果强制筛选 `Normalized` body mode，就会把配置为 `raw_passthrough` 的外部池过滤掉，最终出现 `external_pool_unavailable: No available external fallback pools`。
- **修复容易变成补丁堆叠**：单点修复能解决当前问题，但会继续扩大 `external_pool.rs` 的职责，后续新增能力时更容易引入新的互相影响。
- **测试边界不清楚**：如果 body 处理、模型处理、usage 整形、调度混在同一条链里，case 很难精准覆盖“某个配置只影响某个模块”。

## 当前代码现状校验

本节只描述当前代码事实，避免后续规划偏离真实实现。

### 入口和 handler 现状

- `post_messages`、`post_messages_real_cache_usage`、`post_messages_ha`、`post_messages_dfcache`、`post_claude_code_messages` 都会先调用 `maybe_raw_external_direct_response(...)`。这个入口在解析 `MessagesRequest` 前尝试显式 raw 外部直连，并设置 `body_mode_filter = Some(RawPassthrough)`。
- 这个 pre-parse raw direct 是当前少数真正能绕过 body 解析和 body 处理的路径。
- 一旦没有命中 pre-parse raw direct，handler 会解析完整 `MessagesRequest`，然后进入 `post_messages_inner` 或 `post_claude_code_messages_inner`。
- 在 normal parsed handler 内，`override_thinking_from_model_name`、`apply_thinking_trigger_mode`、`body_processing::prepare_multimodal_sources` 会先运行，随后才调用 `ExternalFallbackContext::direct_policy_response(...)`。
- 因此，显式 direct policy 如果没有走 pre-parse raw 入口，仍会先付出 thinking 和多模态处理成本；如果多模态处理提前报错，外部池 raw 目标没有机会接管。
- 本地 preflight fallback 在 `handle_stream_request` / `handle_non_stream_request` 内触发，此时 Kiro 请求已经完成转换和 `prepare_kiro_request_body`，所以“本地明显不可调度时直接外部池”当前仍会提前消耗本地 body 处理、payload guard、token 估算等成本。

### 外部池现状

- `ExternalRouteRequest` 当前同时携带 `raw_body` 和可选 `payload`。
- `maybe_raw_external_direct_response(...)` 构造的 route 没有 `payload`，只能调度 `RawPassthrough` 外部池。
- parsed fallback/direct 构造的 route 有 `payload`，当前 `body_mode_filter = None`，理论上可以选 raw 或 normalized 外部池。
- `external_pool_prepare_request(...)` 已经按已选 pool 的 `request_body_mode` 分支：
  - `RawPassthrough` 调 `external_pool_prepare_raw_request(...)`。
  - `Normalized` 调 `external_pool_prepare_normalized_payload(...)` 后再序列化。
- 当前局部改动已经把外部池 normalized 的 payload guard 从 `ExternalFallbackContext` 迁到了 `external_pool_prepare_request(...)` 的 normalized 分支，这个方向符合“选中 pool 后再按 pool 配置处理 body”。
- 当前局部改动还让 payload guard retry route 设置 `body_mode_filter = Some(Normalized)`，避免 retry 时把经过裁剪的 normalized body 发到 raw 池。这个语义合理，但仍属于局部补丁，不等于完整模块化。

### 仍存在的主要耦合

- handler 在知道最终目标之前就做了 thinking、多模态 source、模型解析、Kiro 转换、payload guard、token 估算。
- `ExternalFallbackContext::route_request(...)` 会无条件 `count_all_tokens(...)`，即使最终外部池 raw 透传也会计算输入 token。
- model、body、usage、错误分类、重试、日志诊断大多仍散落在 `handlers.rs` 和 `external_pool.rs`。
- `external_pool_prepare_request(...)` 虽然有 raw/normalized 分支，但 model rewrite、thinking 归一化、payload guard、序列化仍在同一个函数附近聚合，模块边界还不清晰。
- usage 投影和 billing 主要仍在 `external_pool.rs`，本地 usage 和外部池 usage 没有统一的 engine 边界。
- 当前代码存在重复路径：普通 Anthropic 路径和 `/cc/v1/messages` 有大量相似处理块，后续改动如果只改一条路径，很容易再次漂移。

### 当前已修方向和不足

当前工作区的局部改动方向是对的：

- raw 外部池准备请求时不进入 payload guard。
- normalized 外部池准备请求时才进入外部 payload guard。
- 普通 fallback route 不再固定 `Normalized` body mode。
- payload guard retry 明确要求 normalized pool。

但不足也很明确：

- 它只把外部池 payload guard 的位置往后挪了一段，没有建立“先选目标，再生成 ProcessingPlan”的统一入口。
- 它没有解决本地 preflight 之前已经做完整 Kiro body 处理的问题。
- 它没有把 facts/token counting 变成 lazy，也没有减少高并发长上下文下的重复 CPU 消耗。
- 它没有把 model、body、usage、error、retry、diagnostics 拆成可复用模块。
- 它没有消除 `/v1`、`/cc/v1` 等路径之间的重复逻辑。

## 核心原则

模块化重构的目标不是简单拆文件，而是拆清楚职责和数据契约。

1. **Body 处理只处理 body**
   - 决定发给上游的请求体 bytes。
   - 不决定走哪个池。
   - 不决定 usage 怎么上报。
   - 不决定模型是否命中映射，但可以接受模型模块输出的可选 patch。

2. **Usage 计算只计算 usage**
   - 输入是请求事实、上游 usage、路径策略、外部池策略、价格表。
   - 输出是下游响应 usage、系统记录 usage、成本拆分。
   - 不关心 body 是 Raw 还是 Normalized，只关心是否有足够的 `RequestFacts`。

3. **模型处理只处理模型**
   - 输入是客户端原始模型、本系统解析后的模型、外部池模型映射规则。
   - 输出是 outbound model、解析来源、可选 body patch。
   - 不强制绑定 Raw body，也不强制绑定 usage 整形。

4. **调度只做调度**
   - 决定本地凭证、外部池、哪个外部池、重试、冷却、并发、队列。
   - 不解析 body。
   - 不做 usage 改写。
   - 不因为 body mode 误过滤池，除非调用方明确要求某类能力，例如显式 Raw 直连。

5. **显式直连不是 Raw body 模式的唯一入口**
   - 显式直连是一种路由策略。
   - Raw body 透传是外部池的 body 处理策略。
   - 外部池 fallback 也应该能调度 Raw body 外部池；不能因为未开启显式直连就忽略 Raw 外部池。

6. **配置应该是能力开关，而不是复合语义**
   - 一个配置项只表达一个层面的意图。
   - 组合行为由管线编排决定，不应该藏在某个配置项里。

7. **先选择目标，再执行目标允许的处理**
   - target 是本地凭证时，执行本地 Kiro body pipeline。
   - target 是外部 normalized pool 时，执行外部 normalized body pipeline。
   - target 是外部 raw pool 时，执行 raw body pipeline。
   - 未选中的分支不能提前运行重处理逻辑。

8. **解析 facts 不等于处理 body**
   - 可以从 raw body 做轻量 top-level hints，例如 `model`、`stream`。
   - 可以在需要时解析标准 Anthropic body 得到 `RequestFacts`。
   - 但 facts 提取不能隐式修改 body，也不能触发图片物化、payload guard、schema 修复。

9. **性能热路径必须可控**
   - token 估算、图片尺寸识别、base64 decode、深层 JSON 遍历、payload guard 裁剪、Kiro 序列化都不能默认对所有分支运行。
   - 每个重处理步骤都必须能回答：是谁需要它、在什么配置下运行、无法运行时怎么降级。

## 当前需要修正的耦合点

### 1. 普通 fallback 不能强制 body mode 为 Normalized

当前错误根源是普通外部池 fallback 构造 `ExternalRouteRequest` 时如果强制设置：

```text
body_mode_filter = Normalized
```

那么调度层会过滤掉 `requestBodyMode=raw_passthrough` 的外部池。结果是：

- 外部池启用了。
- Raw 外部池也可用。
- 但显式直连没有开启。
- 本地凭证不可用后尝试 fallback。
- Raw 外部池被过滤。
- 最终报 `external_pool_unavailable: No available external fallback pools`。

正确语义应该是：

- **显式 Raw 直连入口**：只筛 Raw 外部池，因为这个入口要求 byte-level raw passthrough。
- **普通外部池 fallback**：不按 body mode 过滤，选到哪个池就按哪个池自己的 body 配置执行。

### 2. Raw body 透传不能自动禁用 usage 整形

Raw body 模式只表示 body 不进入处理链，不应该隐含 usage 透传。

正确语义：

- `requestBodyMode=raw_passthrough`：控制出站请求体处理。
- `usageProjectionMode=pass_through/current_path_policy`：控制 usage 上报。
- 两者互不覆盖。
- 如果 Raw 请求能旁路提取 `RequestFacts`，usage 就可以按路径整形。
- 如果 Raw 请求不是标准 Anthropic Messages JSON，无法提取 facts，则 usage 整形只能降级为透传并记录原因。

### 3. 模型处理不应该强制绑定 body 处理

Raw body 和模型写回不矛盾，也不应该强制绑定。

合理配置应为：

- `modelMappingMode`：模型映射策略。
- `rawModelMode`：Raw body 下是否只探测模型、是否写回顶层 model。
- `requestBodyMode`：body 处理模式。

示例：

- Raw body + 不写回 model：完全 body 透传，只用 raw 顶层 model 做调度/记录。
- Raw body + 写回顶层 model：只 patch 顶层 `model` 字段，其他 body 原样保留。
- Normalized body + 模型映射：标准链路序列化时写入 outbound model。

模型处理模块应该输出：

```text
ModelDecision {
  original_model,
  processed_model,
  outbound_model,
  source,
  note,
  optional_body_patch,
}
```

Body 模块自己决定是否应用 `optional_body_patch`。

### 4. 外部 direct/fallback 不能依赖同一个 body 预处理阶段

当前代码里 direct policy 和 fallback 都复用了 `ExternalFallbackContext`，这个复用本身没问题，但它不应该意味着所有外部池请求都必须先经过同一套 parsed payload 处理。

正确拆分：

- explicit raw direct：只需要 raw body hints + direct policy + raw pool 调度。
- explicit any direct：先根据 direct policy 选外部目标，再按目标 body mode 决定是否解析和处理 body。
- local preflight fallback：如果本地池已经明确不可调度，应先选外部目标，再按目标配置准备 body；不能先完整构造 Kiro body。
- after-local-attempt fallback：因为已经选择并尝试过本地，前面做过本地 body 处理是合理的；切到外部池后仍必须按外部池自己的 body mode 生成出站 body。

### 5. `RequestFacts` 必须 lazy，不能把 token counting 当作 route 构造副作用

现在 `ExternalFallbackContext::route_request(...)` 会直接 `count_all_tokens(...)`。这在短请求里问题不大，但在长上下文、大并发、多图、多 tool_result 场景会成为热路径问题。

正确语义：

- 构造 route 时只保存 envelope、目标、策略和可选 hints。
- usage projection 需要 input token 时再算。
- payload guard 需要 token 或 byte breakdown 时再算。
- 诊断打开时再做重 breakdown。
- raw passthrough 且 usage pass-through 时，不应该为系统内部估算提前扫描全部 messages。

### 6. 图片处理应该挂在 body pipeline 上，而不是入口 handler 上

图片处理分两类：

- 协议必需处理：本地 Kiro 凭证必须把 Anthropic 图片 block 转成 Kiro `images[].source.bytes`。
- 兼容增强处理：file_id 物化、远程 URL 物化、media type 修正、图片尺寸 token 估算、oversized 检查。

这些处理不能在入口 handler 无条件运行。合理做法是：

- 本地 Kiro body pipeline 默认启用协议必需处理，并按配置启用增强处理。
- 外部 normalized body pipeline 按外部池配置启用增强处理。
- 外部 raw body pipeline 默认不做图片处理，只允许 raw hints 或显式配置的轻量 top-level patch。

### 7. 错误和重试要归属到目标能力，而不是 body 函数内部

例如 too-long payload guard retry 只对 normalized body 有意义。raw passthrough 目标收到上游 too-long 或 invalid request 时，默认不应该自动进入 normalized 裁剪再重试，除非以后显式配置“raw 失败后允许 normalized rescue”。

正确语义：

- retry policy 由 route/target/capability 决定。
- body pipeline 只返回“是否可重试、需要什么 body mode”的建议事实。
- scheduler 决定是否重新选池。
- error pipeline 决定如何对下游统一报错或透传。

## 目标执行分支矩阵

### 本地凭证目标

本地凭证目标的默认计划：

```text
target = LocalCredential
model = local model resolution
body = LocalKiroBodyPipeline
usage = LocalUsagePipeline
retry = local credential retry + cache point retry + payload too long retry
error = local Anthropic-compatible error normalization
```

运行内容：

- 解析标准 Anthropic Messages body。
- thinking 触发和本地兼容处理。
- 多模态 source 处理。
- Anthropic -> Kiro schema 转换。
- Kiro payload guard 和 too-long retry。
- token 估算和 usage 上报记录。
- 本地凭证调度、冷却、429/风控/禁用逻辑。

不应该运行：

- 外部池 usage projection。
- 外部池 raw model rewrite。
- 外部池自动禁用。

### 外部池 normalized 目标

外部 normalized 目标的默认计划：

```text
target = ExternalPool(id, requestBodyMode = normalized)
model = external model pipeline
body = ExternalNormalizedAnthropicBodyPipeline
usage = ExternalUsageProjectionEngine
retry = external retry + optional normalized payload guard retry
error = external error policy
```

运行内容：

- 解析标准 Anthropic Messages body。
- 按外部池 normalized 配置选择图片/source/schema/payload guard 处理。
- 按模型配置写入 outbound model。
- 可以执行 usage projection，与 body mode 解耦。
- 外部池并发 lease、cooldown、auto disable、换池重试。

不应该运行：

- 本地 Kiro schema 转换。
- 本地 profileArn、machineId、Kiro endpoint transform。
- 本地凭证 token refresh。

### 外部池 raw passthrough 目标

外部 raw 目标的默认计划：

```text
target = ExternalPool(id, requestBodyMode = raw_passthrough)
model = external model pipeline with raw_model_mode
body = RawBodyPipeline
usage = ExternalUsageProjectionEngine
retry = external retry without normalized payload guard retry
error = external error policy
```

运行内容：

- 直接使用入口 `raw_body`。
- 可选 top-level model probe。
- 可选 top-level model rewrite。
- 可以基于 raw body hints 或 lazy facts 做 usage projection。
- 外部池并发 lease、cooldown、auto disable、换池重试。

不应该运行：

- `prepare_multimodal_sources`。
- file_id 物化。
- 远程图片下载。
- base64 media type decode/修正。
- payload guard 裁剪。
- schema/tool/thinking 的 normalized body 修复。
- Anthropic -> Kiro 转换。
- Kiro payload guard。

### 外部池 raw + usage projection

raw body 和 usage projection 的组合是允许的：

- 如果 raw body 是标准 Anthropic Messages JSON，可以 lazy 提取 `RequestFacts`，再应用路径缓存策略。
- 如果无法解析标准 facts，外部池响应可以继续透传，并记录 `projection_skipped_reason = facts_unavailable`。
- 路径级“同步请求不整形”是全局上层禁止条件；如果路径禁止非流式整形，外部池不能重新打开。

### 外部池 raw + model rewrite

raw body 和模型处理的组合也是允许的：

- `rawModelMode = none`：完全不改 body，模型只用于调度/日志。
- `rawModelMode = probe_only`：探测 top-level model 并执行映射，但不改 body。
- `rawModelMode = rewrite_top_level`：只改顶层 `model` 字段，不扫描或修改嵌套 JSON。

禁止行为：

- 不能因为选择 raw body 就强制 model rewrite。
- 不能因为要 model rewrite 就进入 normalized body pipeline。
- 不能误改 `messages[].content` 或 tool 参数中的嵌套 `model` 字段。

## 建议模块边界

### 1. `RequestEnvelope`

只表示入口请求的原始事实：

```text
RequestEnvelope {
  request_id,
  endpoint,
  headers,
  raw_body,
  received_at,
}
```

这个结构不可变。后续模块不能直接改它。

### 2. `RoutePlanner` 和 `ProcessingPlan`

`RoutePlanner` 只负责把请求导向一个目标，不做 body 处理。

```text
RouteDecision {
  target: LocalCredential | ExternalPool | Reject,
  selected_credential,
  selected_external_pool,
  route_subtype,
  direct_policy_reason,
  fallback_reason,
  required_capabilities,
}
```

`ProcessingPlan` 由 `RouteDecision` 和目标配置生成。

```text
ProcessingPlan {
  target,
  body_mode,
  model_mode,
  usage_mode,
  image_processing_mode,
  payload_guard_mode,
  schema_compatibility_mode,
  retry_policy,
  error_policy,
  diagnostics_policy,
}
```

关键约束：

- `RoutePlanner` 可以读取轻量 hints，例如 top-level `model`、`stream`，但不能触发深度 body 处理。
- `ProcessingPlan` 必须能解释每个模块为什么运行。
- 配置为 raw body 的外部池，`ProcessingPlan.body_mode` 必须是 `raw_passthrough`，不能先进入 normalized 再决定跳过。
- `ProcessingPlan` 中没有启用的模块不能运行，即使当前代码路径里“顺手能做”也不应该做。

### 3. `RequestFactsExtractor`

负责从 raw body 或 parsed payload 中提取 facts。

```text
RequestFacts {
  model,
  stream,
  max_tokens,
  conversation_id,
  input_tokens,
  has_images,
  has_tools,
  parse_status,
  parse_error,
}
```

特点：

- lazy parse，只有需要 facts 的模块才触发。
- Raw body 模式也可以提取 facts，但提取 facts 不等于 body 被处理。
- 解析失败不一定是请求失败，取决于当前路径是否需要标准协议语义。

### 4. `BodyPipeline`

负责生成出站请求体。

```text
BodyPipelineInput {
  envelope,
  parsed_payload,
  body_mode,
  image_processing_config,
  payload_guard_config,
  optional_model_patch,
  optional_schema_patch,
}

BodyPipelineOutput {
  outbound_body,
  body_mode_used,
  modified,
  diagnostics,
}
```

可实现的 processor：

- `RawBodyProcessor`
  - 默认直接返回原始 bytes。
  - 可选应用顶层 model patch。
  - 不进入图片处理、schema 修正、payload guard。

- `NormalizedAnthropicProcessor`
  - 解析 payload。
  - 图片处理。
  - schema 修正。
  - payload guard。
  - thinking/model 兼容处理。
  - 序列化为标准 Anthropic body。

后续如果需要插件化，可以定义：

```text
trait BodyProcessor {
  fn supports(mode, route, upstream) -> bool;
  fn process(input) -> Result<BodyPipelineOutput>;
}
```

### 5. `ModelPipeline`

负责模型解析、映射、校验。

```text
ModelPipelineInput {
  original_model,
  processed_model,
  mapping_mode,
  mapping_rules,
  require_match,
  fallback_transform,
}

ModelPipelineOutput {
  outbound_model,
  source,
  note,
  body_patch,
}
```

注意：

- `body_patch` 是可选结果，不代表模型模块直接修改 body。
- Raw 模式下可以选择 `probe_only` 或 `rewrite_top_level`。
- Normalized 模式下通常由标准序列化写入 `model`。

### 6. `UsageProjectionEngine`

负责所有 usage 变换和记录口径。

```text
UsageProjectionInput {
  request_facts,
  upstream_usage,
  route_policy,
  external_pool_policy,
  prompt_cache_state,
  pricing_catalog,
}

UsageProjectionOutput {
  raw_usage,
  shaped_usage,
  reported_usage,
  response_usage_patch,
  billing,
  diagnostics,
}
```

规则：

- `requestBodyMode` 不直接参与 usage 是否整形。
- `usageProjectionMode` 才决定外部池 usage 是否整形。
- 路径级“同步请求不整形”是上层拦截，外部池不能重新打开。
- 如果 facts 不足，输出应降级并记录 `projection_skipped_reason`。

### 7. `Scheduler`

负责调度。

```text
ScheduleInput {
  route,
  request_facts,
  direct_policy,
  local_pool_state,
  external_pool_states,
  required_capabilities,
}

ScheduleDecision {
  target_kind,
  selected_credential,
  selected_external_pool,
  fallback_reason,
  direct_policy_reason,
  attempts,
}
```

关键点：

- 普通 fallback 不应该把 `requestBodyMode` 当作硬筛选条件。
- 显式 Raw 直连可以声明 `required_capabilities = RawBodyPassthrough`。
- 如果某个上游不支持某能力，调度模块只负责排除并记录原因，不做 body 转换。

### 8. `ResponsePipeline`

负责处理上游响应。

```text
ResponsePipelineInput {
  upstream_response,
  response_mode,
  usage_projection_context,
  error_policy,
}
```

能力：

- 非流式 JSON usage patch。
- SSE event usage patch。
- SSE error mask。
- 统一错误响应。
- usage capture。

## 配置建议

### 外部池配置

建议配置按职责分组，避免一个字段承担多个语义。

```text
externalPool {
  routing {
    enabled
    priority
    concurrency
    autoDisablePolicy
  }

  requestBody {
    mode: normalized | raw_passthrough
    rawOptions {
      allowTopLevelModelRewrite
    }
    normalizedOptions {
      imageProcessingProfile
      payloadGuardProfile
      schemaCompatibilityProfile
    }
  }

  model {
    mappingMode
    requireMappingMatch
    normalizeVersionDots
    mappingRules
  }

  usage {
    projectionMode: pass_through | current_path_policy
    skipNonStreamProjection
    uplift
  }

  errors {
    publicErrorPolicy
    retryPolicy
    cooldownPolicy
  }
}
```

### 路径级 usage 配置

路径级 usage 设置应该是上层策略。

```text
routeUsagePolicy {
  reportedUsage
  skipNonStreamUsageProjection
}
```

如果路径级禁用同步请求整形，外部池不能重新打开。

### 显式直连配置

显式直连是路由策略，不是 body 策略。

```text
directPolicy {
  enabled
  modelRules
  pathRules
  requiredBodyMode: raw_passthrough | any
}
```

当前建议：

- 原始 raw 直连入口使用 `requiredBodyMode=raw_passthrough`。
- parsed direct fallback 可使用 `any`，让池自己的 body mode 决定如何出站。

## 配置语义推敲

### UI 和配置命名

配置应按能力分组，而不是把所有能力塞进一个“body 处理”下拉。

推荐 UI 分组：

- 路由与调度：启用、优先级、并发、fallback/direct、自动禁用、冷却。
- 请求体出站模式：标准化转发、原始透传。
- 请求体处理选项：只在标准化转发时展示图片处理、payload guard、schema/tool/thinking 兼容。
- 模型处理：模型映射、必须命中、raw 下是否探测、raw 下是否只改顶层 model。
- Usage 上报：透传、按路径策略整形、同步请求禁用整形、价格/放大配置。
- 错误策略：透传、统一包装、冷却、自动禁用。

这里的关键是：

- `raw_passthrough` 是请求体出站模式，不是“关闭整个外部池处理”的总开关。
- 模型处理不是请求体处理选项，即使最终会写入 body，也应该作为独立能力展示。
- usage 整形不是请求体处理选项，不能因为 raw 而隐藏或失效。
- payload guard 是 body pipeline 的一个 processor，必须可以独立关闭；关闭时不能进入该逻辑。

### 同步请求 usage 整形禁用开关

同步请求是否整形要采用“任一层禁止即禁止”的语义。

```text
routeAllowsNonStreamProjection
  && poolAllowsNonStreamProjection
  && requestAllowsProjection
  => 才允许非流式 usage projection
```

也就是说：

- 路径级配置禁止非流式整形时，外部池不能重新打开。
- 外部池配置禁止非流式整形时，即使路径允许，也不整形。
- 以后如果下游 key 或租户级别增加禁用开关，也应该纳入同一条 AND 链。

UI 文案建议用否定式表达这个特定开关，例如“禁用同步请求 usage 整形”。原因是它是上层拦截器，真实语义是“禁止某类请求进入整形”，不是给整形授权。对外部池自身的默认能力则建议用肯定式，例如“启用 usage 按路径整形”。

### Raw passthrough 和必要处理的边界

raw passthrough 的狭义定义是：不进入 body 内容处理链。

允许的必要处理：

- 选择外部池。
- 设置 transport header/path/auth。
- 可选 top-level model probe。
- 可选 top-level model rewrite。
- 可选 lazy facts 提取，用于 usage 或日志。
- 响应 usage projection。
- 错误归一化、冷却、auto disable。

禁止的默认处理：

- 修改 messages、system、tools、thinking、tool_result。
- 物化图片或文件。
- 下载远程 source。
- decode base64。
- payload guard 裁剪。
- schema 修复。
- 重新序列化 normalized body。

### Normalized 和 raw 的降级关系

默认不允许 raw 请求在失败后自动降级成 normalized 裁剪重试。原因：

- raw 的目标就是降低本地处理负担和尽量保持上游兼容行为。
- 自动 normalized retry 会隐式改变 body 语义，可能让用户以为 raw 生效但实际没有。
- 如果未来需要这个能力，应该新增显式开关，例如 `allowNormalizedRetryOnRawTooLong`，并在 usage/log 里记录。

Normalized 请求可以在 too-long 场景下进入 payload guard retry，因为它本来就声明接受 normalized body pipeline。

### 插件化边界

短期不需要把所有 processor 做成动态插件，但代码边界应按插件化预留：

```text
trait RequestProcessor {
  fn name(&self) -> &'static str;
  fn is_enabled(&self, plan: &ProcessingPlan) -> bool;
  fn process(&self, ctx: &mut RequestProcessingContext) -> Result<ProcessorOutcome>;
}
```

第一阶段可以不引入 trait，只要先把函数边界和输入输出拆清楚。后续如果需要高可用、多上游协议、租户插件、自定义 body 变换，再把这些函数收敛到 trait。

## 迁移步骤建议

### 阶段 0：冻结目标语义和现有局部改动边界

- 以本文为准确认目标：先选目标，再按目标配置执行模块。
- 当前 `handlers.rs` / `external_pool.rs` 的局部改动只保留符合该目标的部分。
- 不再继续往 `external_pool_prepare_request(...)` 里堆新能力，除非只是为了短期止血且有后续迁移目标。
- 不发版前必须验证 raw 外部池在显式直连和普通 fallback 下都不被忽略。

### 阶段 1：短期止血和语义修正

- 普通 fallback 不再按 `Normalized` 过滤外部池。
- 显式 raw 直连仍只匹配 Raw 外部池。
- Raw body 不再隐式关闭 usage 整形。
- UI 文案明确 body mode 和 usage mode 独立。
- 外部池 normalized 的 payload guard 只在选中 normalized pool 后运行。
- payload guard retry 只重选 normalized pool。

验收：

- 只配置 raw 外部池，显式直连关闭，本地无可用凭据时，能正常 fallback 到 raw 外部池。
- raw 外部池启用 `usageProjectionMode=current_path_policy` 时，usage 是否整形只受 usage 配置和路径禁用开关控制。
- raw 外部池请求体 byte-for-byte 保持，除非显式启用 top-level model rewrite。

### 阶段 2：引入 `RequestEnvelope`、轻量 hints 和 `ProcessingPlan`

- handler 入口先构造不可变 `RequestEnvelope`。
- 用低成本 probe 提取 top-level `model`、`stream`，不得扫描完整 messages。
- direct policy 和本地 preflight 可以基于 hints 提前生成 route decision。
- `ProcessingPlan` 显式表达本次请求会运行哪些模块。
- `/v1`、`/ha`、`/na`、`/dfcache`、`/cc` 共用同一套 planner，路径差异只进入 route policy。

验收：

- 显式 raw direct 命中时，不解析完整 `MessagesRequest`。
- 本地池 preflight 明确不可调度且外部 raw 池可用时，不先构造 Kiro request body。
- 计划日志能看到 target、body_mode、model_mode、usage_mode、payload_guard_enabled。

### 阶段 3：抽 `RequestFacts`

- 从 `handlers.rs` 和 `external_pool.rs` 中抽出事实提取。
- 所有 usage、model、scheduler 都依赖 `RequestFacts`，不直接各自解析 body。
- Raw 解析失败时只标记 facts 不完整，不直接报错。
- `count_all_tokens(...)` 从 route 构造副作用改成 lazy fact。
- 图片 token 估算只在确实需要 token facts 时运行，不能被 raw body 默认触发。

验收：

- raw + usage pass-through 不触发 token counting。
- raw + current_path_policy 在 body 可解析时触发必要 facts，不修改 body。
- facts 解析失败能记录 skip reason，不把 raw 请求直接打成 400。

### 阶段 4：抽 `UsageProjectionEngine`

- 把 `maybe_project_non_stream_usage`、SSE usage patch、外部池 billing、路径 reported usage 逻辑迁出 `external_pool.rs`。
- 本地凭证和外部池共用 usage 引擎。
- 增加 `projection_skipped_reason` 诊断字段。
- 把“主列表展示下游响应上报字段”和“诊断展示原始/整形/成本口径”固定成同一套输出结构。

验收：

- 非流式和流式 usage projection 口径一致。
- 路径级同步禁用开关能拦截外部池自身允许整形的请求。
- raw body mode 不影响 usage projection mode 的配置生效。

### 阶段 5：抽 `BodyPipeline`

- Raw 和 Normalized 变成两个 processor。
- 图片处理、payload guard、schema 修正成为 Normalized processor 的子步骤。
- Raw processor 只允许显式配置的轻量 patch，例如顶层 model patch。
- 本地 Kiro body pipeline 与外部 normalized Anthropic body pipeline 分开。
- 图片处理配置拆成现有模式和轻量转发模式，并挂在具体 body pipeline 上。

验收：

- raw body 不调用 `prepare_multimodal_sources`。
- normalized body 按配置调用图片处理和 payload guard。
- 本地凭证图片请求仍能转成 Kiro 兼容格式。
- raw 外部池图片请求不做本地 decode、物化、修正。

### 阶段 6：抽 `ModelPipeline`

- 模型映射、别名解析、版本点横转换、必须命中规则集中处理。
- 输出 `ModelDecision`，由 body pipeline 决定是否写回 body。
- raw model rewrite 只允许顶层 `model` patch。
- normalized model rewrite 由 normalized 序列化自然写入。

验收：

- raw + `probe_only` 不改 body。
- raw + `rewrite_top_level` 只改顶层 model。
- normalized + mapping 使用 outbound model。
- local model resolution 不被外部池 mapping 规则污染。

### 阶段 7：调度、错误和响应收口

- Scheduler 只关心候选池和 required capabilities。
- ResponsePipeline 统一处理 usage patch、错误包装和记录。
- ErrorPipeline 统一处理本地凭证错误、外部池错误、cooldown、auto disable、对下游报错。
- RetryPipeline 明确哪些 retry 允许换 body mode，默认 raw 不允许 normalized rescue。

验收：

- 外部池不可用错误能区分禁用、冷却、并发、能力不匹配、无池。
- 429 进入冷却还是自动禁用由错误策略决定，不和 payload too long 混在一起。
- 流式响应头后错误不尝试换池，只记录并释放 lease。

### 阶段 8：性能和压测验证

- 增加针对长上下文、多图、tool_result 深层嵌套、schema 大对象、payload guard 裁剪的 micro benchmark。
- 增加真实外部池上游的流式长占用、首字慢、429/5xx/timeout 混沌测试。
- 增加本地凭证真实请求验证，覆盖图片和 Claude Code CLI 自动压缩相关协议。
- 压测目标至少覆盖 5k RPM 设计方向下的 CPU、内存、FD、队列等待、lease 释放。

验收：

- raw 外部池在相同请求下 CPU 消耗明显低于 normalized 外部池。
- 关闭 payload guard 时不会进入 payload guard 代码路径。
- 长流式占用不会导致外部池 lease 泄漏。
- 高并发下不会因为重复 token counting 或重复 body 序列化造成 CPU 持续攀升。

## 必须覆盖的测试矩阵

### 调度

- 显式直连开启 + Raw 池：必须走 Raw 池。
- 显式直连开启 + 只有 Normalized 池：应走 parsed direct 或明确降级策略，不能误报 Raw 池不可用。
- 显式直连关闭 + 本地失败 + Raw 池：Raw 池不能被忽略。
- 显式直连关闭 + 本地失败 + Normalized 池：仍正常 fallback。
- Raw 池和 Normalized 池共存：按优先级/并发/冷却正常选择。
- 本地 preflight 明确不可调度 + Raw 池：不应提前构造 Kiro request body。
- after-local-attempt fallback + Raw 池：允许前面已做本地处理，但发外部池时必须使用原始 raw body。
- `/v1/messages`、`/ha/v1/messages`、`/na/v1/messages`、`/dfcache/.../v1/messages`、`/cc/v1/messages` 都必须覆盖同一语义。

### Body

- Raw body + 不写 model：body byte-for-byte 保持。
- Raw body + 写顶层 model：只改顶层 model。
- Raw body + 嵌套 model：不能误改嵌套字段。
- Normalized body：图片处理、schema 修正、payload guard 正常生效。
- Raw body + 图片：不调用 file store、不下载远程图片、不 decode base64、不修正 media type。
- Local credential + 图片：仍转成 Kiro 兼容 `images[].source.bytes`。
- Normalized external + payload guard disabled：不进入 payload guard。
- Normalized external + payload guard enabled：只对 normalized branch 生效。
- tool_result.content[] 内图片、文件、大 schema、深层 nested JSON 都要覆盖。

### Usage

- Raw body + `pass_through`：下游 usage 保持上游。
- Raw body + `current_path_policy`：usage 按路径整形。
- Raw body + 路径禁用同步整形：非流式 usage 保持上游。
- Normalized body + `current_path_policy`：usage 按路径整形。
- SSE usage event：流式 usage patch 和 record 一致。
- 主列表展示字段必须只展示下游响应上报 usage。
- 诊断字段可以展示 raw upstream usage、projected usage、internal cost usage、billing split，但不能把字段命名成“用户真实输入”误导。
- facts 缺失时 projection 降级必须记录 `projection_skipped_reason`。
- 价格估算、缓存写入/读取、路径放大配置必须与 body mode 解耦。

### 模型

- Raw + `probe_only`：调度/记录使用映射模型，但 body 不变。
- Raw + `rewrite_top_level`：只写顶层 model。
- Normalized + mapping：序列化 body 使用 outbound model。
- Local credential model resolution 不读取外部池 mapping。
- 外部池 mapping miss 只跳过当前池或按配置报错，不应该污染其他池。

### 错误

- 外部池无可用候选时，应能区分：
  - 真无池。
  - 池被禁用。
  - 池冷却。
  - 并发满。
  - body capability 被要求但无匹配池。
- 不应该把“Raw 池被普通 fallback 过滤”表现成“无外部池”。
- 429 默认应进入 cooldown / retry_next 策略，不应和 payload too long 的裁剪 retry 混为一类。
- raw 外部池收到 too-long 错误时，默认不做 normalized payload guard retry。
- normalized 外部池收到可能 too-long 错误时，可以按配置做 payload guard retry。
- 响应头已返回后的流式错误不能再换池，只能记录并透出连接错误。

### 性能

- raw passthrough 分支不应做完整 JSON parse、图片 decode、payload guard、Kiro serialization。
- normalized 分支关闭 payload guard 时不能进入 payload guard scan/crop。
- token counting 只能在 usage、payload guard、诊断明确需要时运行。
- 多图长上下文、大 tool_result、深层 JSON、慢首字长流式、上游 429/5xx/timeout 都要有针对性压测。
- 压测需要记录 RPM、并发、持续时间、P95/P99、CPU、RSS、FD、队列等待、lease 持有时间、首字延迟。

## 执行前检查清单

进入实现前必须确认：

- 哪些现有局部改动保留，哪些回退或重做。
- `RequestEnvelope` 和 `ProcessingPlan` 放在哪个模块，是否先只服务 Anthropic messages 路径。
- 本地 preflight 要提前到哪个阶段，如何避免破坏当前本地凭证调度行为。
- raw fallback 是否允许在 after-local-attempt 场景使用原始 raw body，同时 usage 记录带上本地尝试诊断。
- payload guard 完全关闭时，是否所有入口都不会调用 payload guard runtime。
- 图片轻量模式和现有模式的配置默认值是什么，是否会改变现网默认行为。
- `/ui` 配置分组和文案是否覆盖对应能力。

## 开放问题

- raw 外部池在 explicit direct policy 未要求 raw，只是 `any` 时，是否允许 normalized 池和 raw 池混选，还是需要策略指定偏好。
- raw body 不是标准 Anthropic Messages JSON 时，usage projection 降级是否只记录日志，还是也要在 usage record 中显式展示。
- 本地 preflight 提前后，如果请求 body 本身非法，是优先返回本地 400，还是在本地不可用且外部 raw 可用时允许 raw 外部池处理。建议按 target-first：如果已明确选 raw 外部池，非法标准 body 不应阻断 raw。
- raw 请求是否需要可选的最大 body bytes 硬限制。这个限制不是 payload guard，但可能是网关级 DoS 保护。
- 是否需要为 normalized external 和 local Kiro 分别配置图片处理 profile，避免一个全局图片配置影响不同上游。

## 当前结论

当前最重要的修复方向是把“调度选择”和“处理模块执行”拆开。

短期应保证：

- Raw 外部池不会因为显式直连关闭而被普通 fallback 忽略。
- 显式 Raw 直连仍只使用 Raw 池。
- usage 整形继续由外部池 usage 设置和路径 usage 设置决定，不被 body mode 隐式覆盖。
- 外部池 normalized 的 payload guard 只在选中 normalized pool 后执行。
- raw 外部池不会提前进入图片、schema、payload guard、Kiro 转换等 body 处理。

中长期应落到 `RequestEnvelope + RoutePlanner + ProcessingPlan + RequestFacts + BodyPipeline + ModelPipeline + UsageProjectionEngine + Scheduler + RetryPipeline + ErrorPipeline + ResponsePipeline` 这组边界。这样配置会更聚焦，也更容易证明“一个配置只影响它所属的模块”，同时避免高并发长上下文场景下未命中分支也提前消耗 CPU 和内存。
