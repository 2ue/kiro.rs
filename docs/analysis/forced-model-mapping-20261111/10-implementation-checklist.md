# 10 实现清单（实施基准）

本篇是实现的唯一基准。决策项全部按推荐方案定稿（见第 1 节）。每个任务都写明改哪个文件、怎么改、怎么验收。打勾表示已实现并通过验收。

## 1. 决策定稿

| 编号 | 决策 | 定稿 |
|---|---|---|
| D1 | 生效粒度 | 全局开关，加上按路由的 `pathOverrides`（最长前缀匹配）。不做按账号映射 |
| D2 | 是否作用于外部号池 | 不作用。外部池的出站请求、原始直通、兜底均保持现状 |
| D3 | 默认规则 | haiku → `claude-haiku-4-5`；opus / fable / mythos / sonnet 及其它 Claude 模型 → `claude-sonnet-4-5`。规则可配置 |
| D4 | 对外 input 口径 | 按客户端原始请求估算（注入和退避之前），之后仍走路由整形策略 |
| D5 | tokenizer 系数放大 | 不放大 |
| D6 | 下游计费口径 | 本服务不参与。对外 usage 字段只给外显模型 |
| D7 | L3 锚定摘要 | 本期不做，配置项保留，默认关闭 |
| D8 | 知识库范围 | 本期内置 K0（每个外显模型一张卡）和 K1 生态条目；K2 提供存储与检索能力，默认为空，运营审核后再补充 |
| D9 | Opus 5.5 与 Fable / Mythos 5 系工具调用之间的文字转成 thinking 块 | 默认开启，可配置；omitted 时为空，updates 时保留文字 |

## 2. 配置（T1）

**文件**：`src/model/config.rs`、`src/anthropic/handlers.rs`（RequestRuntimeConfig）、`src/admin/types.rs`、`src/admin/service.rs`

```rust
pub struct ForcedModelMappingConfig {
    pub enabled: bool,      // 默认 false
    pub rules: Vec<ForcedModelRule>,      // 默认见 D3
    pub default_target: String,    // 默认 "claude-sonnet-4-5"
    pub path_overrides: BTreeMap<String, ForcedModelRouteOverride>, // { enabled: Option<bool> }
    pub context_backoff: ForcedContextBackoffConfig,
    pub identity: ForcedIdentityConfig,   // knowledge_enabled、self_rewrite_enabled 等
    pub interleaved_text_as_thinking: bool, // D9，默认 true
    pub knowledge: Vec<ForcedKnowledgeEntry>, // K1/K2 条目，内置 K1 默认值
}
pub struct ForcedModelRule { pub families: Vec<String>, pub target: String }
```

- [x] T1.1 结构体、默认值，以及 `normalized()`（去空白、小写、去重）。
- [x] T1.2 `Config.forced_model_mapping` 字段带 `#[serde(default)]`，旧配置可以无缝加载。
- [x] T1.3 在 RequestRuntimeConfig 中接入，提供 `forced_mapping_for(endpoint) -> Option<&ForcedModelMappingConfig>`，按路由覆盖算出是否生效。
- [x] T1.4 管理端 GET/PUT 接口：响应中增加字段，更新请求中增加可选字段。保存时校验目标模型存在于上游目录（目录为空时放行并记录 warn）。
- [x] T1.5 单测：默认关闭；旧 JSON 能加载；路由覆盖遵循最长前缀；保存时校验目标模型。

## 3. 外显模型与目标模型（T2）

**新文件**：`src/anthropic/forced_model.rs`

- [x] T2.1 `fn resolve_forced_target(display: &str, cfg) -> Option<ForcedTarget>`：
  - 去掉 `[1m]`、`-thinking` 后缀和日期后缀后，识别模型族（opus / sonnet / haiku / fable / mythos，以及短别名）。
  - 非 Claude 模型返回 None。
  - 目标与外显规范化后相同时返回 None（不需要改写）。
- [x] T2.2 `struct ForcedTarget { display_model, target_model, display_family, display_requested_thinking }`。
- [x] T2.3 `struct DisplayModelSpec`：外显模型的官方规格，包括窗口、输出、思考默认值（always / default_on / off）、effort 默认值、display 默认值、是否支持中途 system、知识截止、显示名。数据内置一张表，覆盖：
  - Fable 5.1 / 5、Mythos 5.1 / 5
  - Opus 5.5 / 5 / 4.8 / 4.7 / 4.6 / 4.5
  - Sonnet 5 / 4.6 / 4.5
  - Haiku 4.5
  - 短别名
- [x] T2.4 单测：每个模型族、带后缀、带日期、短别名、非 Claude 模型、相同模型。

## 4. 入口改写（T3）

**文件**：`src/anthropic/handlers.rs`（post_messages_inner）、`src/anthropic/handlers/request_entry.rs`、`src/anthropic/types.rs`

- [x] T3.1 收敛结果 `ForcedTarget` 挂在 `ModelResolution.forced`（`#[serde(skip)]`，不序列化）上，随模型解析结果贯穿转换、usage 上下文与响应组装；转换选项 `ConverterOptions.forced` 与之同源。`MessagesRequest` 有一百多处字面量构造，不加字段。
- [x] T3.2 在 `post_messages_inner` 中、`parsed_body_pipeline::prepare` **之后**、注入与 `resolve_request_model` **之前**，按顺序执行：
  1. 计算 ForcedTarget。如果为 None，后续流程与现状逐字节一致。
  2. 按外显模型固化 thinking 默认值（第 5 节 T4.1）。
  3. 固化 display 默认值：`payload.thinking.display` 为空时，写入外显模型的默认值。
  4. 记录 `display_requested_thinking`。
  5. 统计"原始请求 input tokens"（按外显模型，在注入之前）。
  6. `payload.model = target_model`，并把 `payload.forced` 设为 `Some(..)`。
- [x] T3.3 `maybe_local_pool_unavailable_fast_fail_response` 在强制模式下改用目标模型。目标模型由 raw probe 的 model 经 `resolve_forced_target` 计算得出。
- [x] T3.4 `request_hides_reasoning` 的请求模型参数优先使用 `payload.forced.display_model`。由于 T3.2 第 3 步已经固化了 display，这里保留作为兜底。
- [ ] T3.5 单测：开关关闭时，payload 与现状一致；开启后 model 被替换、thinking 与 display 已固化、forced 已填充。

## 5. thinking 补全（T4）

**文件**：`src/anthropic/forced_model.rs`、`src/anthropic/converter/thinking.rs`

- [x] T4.1 `fn materialize_display_thinking(payload, spec)`，按外显模型的规则执行：
  - **没传 thinking**：
    - always（Fable / Mythos / Opus 5.5）、default_on（Opus 5 / Sonnet 5）：写入 `adaptive`。
    - off（Opus 4.6–4.8 / Sonnet 4.6）：保持没有 thinking。
    - 外显模型名带 `-thinking`：写入 `adaptive`，effort 为 high。
  - **disabled**：
    - always 类：改为 `adaptive`，不报错。
    - Opus 5 且 effort 为 xhigh 或 max：改为 `adaptive`。
    - 其余情况保持 disabled。
  - **effort**：没传且外显模型有默认值时，写入 `output_config.effort`（Opus 5.5 为 medium，其余为 high）。
- [x] T4.2 在 `converter/thinking.rs` 中，当 `req.forced.is_some()` 且目标模型没有原生推理字段时：
  - adaptive 或只传了 effort：改为 `enabled` 分支，`max_thinking_length = effort_to_length(effort)`。对应关系为 low 4000、medium 16000、high 32000、xhigh 48000、max 48000，并且不超过 min(max_tokens, 64000) × 3/4，不低于 1024。
  - enabled：沿用 budget_tokens，同样受上面的限制。
- [x] T4.3 可见思考策略里判断 `-thinking` 的逻辑，改为读取 `forced.display_requested_thinking`。
- [ ] T4.4 单测：外显模型 × thinking 四种形态 × effort 五档，检查上游提示前缀里的 `max_thinking_length`。所有组合都不能返回错误。

## 6. 协议字段（T5）

**文件**：`src/anthropic/handlers/request_entry.rs`（normalize_message_roles）、`src/anthropic/converter/history.rs`、新文件 `src/anthropic/context_edits.rs`

- [x] T5.1 中途 system 消息：强制模式下，`normalize_message_roles` 不再把这类消息挪到顶层，而是改写成 `role:user` 的文本块，内容用 `<system-reminder>…</system-reminder>` 包裹，放回原位置。
  - 包裹格式与 Claude Code 自己的提醒块一致，能被模型识别为运营指令，又不会显得像注入。
  - 连续的 user 消息由现有逻辑合并。
  - `clear_at` 字段丢弃，只在当前位置渲染。之后的请求中它属于历史，按原样回放。
- [x] T5.2 context_management：在 `MessagesRequest` 中补上 `context_management: Option<Value>` 字段，只在强制模式下读取。
  - `clear_tool_uses_*`：按 `keep`（默认 3）保留最近的 tool_result 原文，更早的替换成 `[Old tool result content cleared]`，这与官方被清理后的占位文字一致。
  - `clear_thinking_*`：清理历史中的 thinking 块。
  - 其它类型：忽略。
- [x] T5.3 单测：中途 system 的位置与内容；clear 两种策略的结果；未知 edit 类型被忽略。

## 7. 上下文退避（T6）

**文件**：新文件 `src/anthropic/forced_backoff.rs`、`src/anthropic/handlers.rs`（错误映射、PayloadTooLongRetryRequest）、`src/anthropic/stream.rs`、`src/anthropic/handlers/local_body_pipeline.rs`

- [x] T6.1 本地预检：在转换之前，按目标模型窗口（catalog 的 `max_input_tokens_for(upstream)`，取不到时用 200K）计算预算 = 窗口 − min(max_tokens, 64K)（下限 8K）− 5%。注入之后的 payload 超出预算时，进入 L1。
- [x] T6.2 L1：去掉最后一条 assistant 之前所有 assistant 的 thinking 文本；去掉重复的 system-reminder 文本块。
- [x] T6.3 L2：保留最近 K=8 个 tool_result 原文，更早的替换成 `[Old tool result content cleared]`。边界按步长 50K tokens 推进：被清理的 tool_result 数量 = floor(超出量 / 50K + 1) × 步长对应的数量，保证相邻请求的结果稳定。
- [x] T6.4 L4：从最早的轮次开始成对删除 user/assistant，保证 tool_use/tool_result 配对完整，保留第一条 user 消息里的文字。
- [x] T6.5 L5：对仍然超出的最大单个块（tool_result 或 text）做中间截断，保留首尾，中间加上 `[... content truncated ...]`。
- [x] T6.6 每一级处理后重新估算，满足预算就停止。
- [x] T6.7 上游仍返回输入过长时：强制模式下 `too_long_retry_enabled_for` 返回 true（不再走 client_compaction）；重试前把预算降到 85%，再从 L1 开始。
- [x] T6.8 错误映射：强制模式下不对外返回 prompt-too-long。重试用尽后，用 L4+L5 的最小形态再试一次；仍失败就返回通用的临时错误（529 overloaded），不暴露窗口信息。
- [x] T6.9 `stream.rs`：强制模式下 `contextUsageEvent` 达到 100% 时，不设置 `model_context_window_exceeded`。
- [x] T6.10 压缩请求的窗口预算改为按 catalog 计算（A5）。
- [x] T6.11 单测：构造 300K / 600K / 1M 的请求，检查最终估算 ≤ 预算；配对完整；同一输入两次的结果完全一致。

## 8. 对外字段（T7）

**文件**：`src/anthropic/handlers.rs`、`src/anthropic/websearch.rs`、`src/anthropic/handlers/model_driven_websearch.rs`、`src/anthropic/usage.rs`、`src/anthropic/envelope.rs`

- [x] T7.1 新增 `fn display_model(payload) -> &str`：有 `forced` 时取 forced.display_model，否则取 payload.model。
- [x] T7.2 以下位置改用 display_model：
  - 流式模板：`handle_stream_request` 的 model 参数（来自 `&payload.model`）拆成两个参数，`display_model` 用于响应，目标模型用于调度。
  - 非流式响应的 `"model"`。
  - usage_context.model（`handlers.rs:6277`）以及 RequestRejectionUsageContext 的 model。
  - websearch 的模板路径和模型驱动路径。
  - `model_not_found_message`。
- [x] T7.3 调度、冷却、劣化计分仍使用目标模型。这些地方现在拿到的是 `payload.model`，改写后自然就是目标模型。拆参数时要逐个确认。
- [x] T7.4 对外错误文案统一处理：在 envelope 出口做一次替换，把目标模型 ID 和 Kiro 点号 ID 换成 display_model（只在强制模式下生效）。
- [x] T7.5 usage 记录新增 `forced_mapping=true`，`model_resolution_source` 记为 `forced`。数据库若无对应列就放进现有 JSON 或 note 字段，不迁移表结构。
- [x] T7.6 对外 input：usage 的 input 以 T3.2 第 5 步算出的原始请求 token 为基准（替换 `input_tokens`），之后再走整形。
- [x] T7.7 单测和集成测试：流式与非流式的 model 字段、usage.model、错误文案中都不出现目标模型 ID。

## 9. 模型列表与 count_tokens（T8）

**文件**：`src/anthropic/handlers.rs`（models_list_response、model_retrieve_response、count_tokens_for_endpoint）、`src/anthropic/model_capabilities.rs`

- [x] T8.1 强制模式（全局开关打开时）下，`/v1/models` 用 DisplayModelSpec 表补齐并覆盖窗口值，确保官方外显模型都在列表里，且 `max_input_tokens` / `max_tokens` 为官方值。
- [x] T8.2 `/v1/models/{id}`：同样按 DisplayModelSpec 返回。
- [x] T8.3 count_tokens：强制模式下不做 system 注入，按客户端原始请求计数。
- [x] T8.4 单测：开关打开后，模型列表中包含 `claude-opus-5-5`，窗口为 1M、输出为 128K。

## 10. 身份、知识库与输出改写（T9）

**文件**：`src/model/prompt_injection/catalog.rs`、`src/anthropic/prompt_injection.rs`、`src/anthropic/converter.rs`、新文件 `src/anthropic/forced_knowledge.rs`、`src/anthropic/identity_guard.rs`、`src/anthropic/control_block_cleaner.rs`、`src/admin/public_pool.rs`

- [x] T9.1 K0 卡：强制模式下，在 system 首块注入外显模型的卡片（型号、ID、窗口、输出、知识截止，以及"之后的事情可能不知道"）。
  - 如果客户端 system 里已经出现 "You are powered by the model named"，省略型号句。
  - 卡片用身份 marker 包裹，避免重复注入。
- [x] T9.2 K0 卡也替代原有身份提示：强制模式下自动启用身份能力（不依赖 `identity.enabled`），默认文案换成卡片。
- [x] T9.3 K1/K2 检索：只在本轮用户有文字时执行，对用户文字做关键词匹配（topics），按语言选取条目，最多 3 条，以 `<system-reminder>` 包裹后追加到当前轮 user 文本之后。历史轮次不重新注入。
- [x] T9.4 输出侧改写：在 identity_guard 中新增"模型自述"规则。
  - 句子中出现第一人称，并且出现 `Claude (Sonnet|Haiku) 4(.|-| )5` 或 `claude-(sonnet|haiku)-4[-.]5` 时，把型号替换成外显模型的显示名或 ID。
  - 强制模式下 guard 自动开启（包括流式）。
- [x] T9.5 guard 覆盖 thinking_delta：summarized 模式下，可见思考也走同一套规则。
- [x] T9.6 删除后的兜底回复改为带上外显模型名。
- [x] T9.7 注入块复述清理：K0/K1 块被模型原样复述时，由 control_block_cleaner 删除。"根据提供的上下文/资料"这类句子由 guard 改写。
- [x] T9.8 公开号池页的冷却 model：强制模式下映射为外显族名。目标模型冷却时，显示为"该目标承接的外显族"。在强制模式下直接不显示 model 字段即可（更简单，也更稳）。
- [x] T9.9 单测：卡片注入与省略；检索命中、不命中、工具轮次不注入；自述改写（中英文）；thinking 改写；复述清理。

## 11. 工具补全（T10）

**文件**：`src/anthropic/converter/tools.rs`、`src/anthropic/handlers.rs`（工具预处理）、新文件 `src/anthropic/forced_tools.rs`

- [x] T10.1 Anthropic 定义的无 schema 工具：按名称和类型前缀补全官方 schema 与描述，包括：
  - `bash_*`（command、restart）
  - `text_editor_*`（command、path、file_text、old_str、new_str、insert_line、view_range）
  - `memory_*`（command、path 等）
  - `computer_*`（action、coordinate、text 等）
  - `web_fetch_*`（url）
  
  这些工具在强制模式和非强制模式下都补全，因为补全对现状本身也是修复。
- [x] T10.2 `computer_toolset_*`：展开成成员工具（screenshot、left_click、type、key、mouse_move、scroll、zoom 等），每个成员带上自己的 schema。响应中这些成员的 tool_use 补上 `"toolset_name":"computer"`。
- [x] T10.3 强制模式下静默移除以下工具：`mcp_toolset`（以及顶层的 `mcp_servers`）、advisor、`tool_search_tool_*`。代码执行未开启时，同时移除 `code_execution_*`。
- [x] T10.4 web_fetch：本期以补全 schema 的方式作为客户端工具发出（模型会输出 tool_use）。客户端如果不认识它，会返回工具错误，但不会让请求失败。代理侧抓取留到下一期。
- [x] T10.5 强制模式下默认启用模型驱动的 websearch（路由特性没有显式配置时）。
- [x] T10.6 单测：各工具补全后的 schema；toolset 展开与回填；移除逻辑。

## 12. 逐模型特殊处理（T11）

**文件**：`src/anthropic/stream.rs`、非流式响应组装、`src/anthropic/forced_model.rs`

- [x] T11.1 工具调用之间的文字（D9）：外显模型为 Opus 5.5 或 Fable / Mythos 5 系、display 为 omitted 或 updates、并且本轮存在 tool_use 时，处理 tool_use 之前的 text 块（updates 时 thinking 块保留原文字，签名按文字生成）：
  - 非流式：把 tool_use 之前的 text 块改为 thinking 块，thinking 内容清空，并补上合成签名。
  - 流式：tool_use 之前的 text_delta 先缓冲；遇到 tool_use 时，以 thinking 块的形式下发（内容为空），并补上 signature_delta；没有遇到 tool_use、直接结束时，按 text 正常下发。

  按配置可以关闭。
- [x] T11.2 截图坐标换算：图片被缩放时记录缩放比例，按请求 id 保存在内存中。computer 类 tool_use 输入中的 coordinate、start_coordinate 按比例换算回原图尺寸。
- [x] T11.3 单测：Opus 5.5 的流式与非流式结构；坐标换算。

## 13. 管理端 UI（T12）

**文件**：`ui/src/features/runtime/*`

- [x] T12.1 在"模型解析"旁边新增"强制模型收敛"配置区：开关、规则表（模型族多选 + 目标模型下拉，选项来自上游目录）、默认目标、路由覆盖、退避参数、D9 开关。
- [x] T12.2 知识库条目管理：列表、新增、编辑、删除，字段为 topics、locale、fact、enabled。条目的启停用 enabled 开关管理，不设有效期字段。
- [x] T12.3 保存时提示"可承接目标模型的账号数"。
- [x] T12.4 使用现有 UI 组件，与运行时配置页保持一致的风格。

## 14. 验收（T13）

- [x] T13.1 `cargo test --bin kiro-rs` 全量通过；fmt、clippy 基线通过。
- [x] T13.2 开关关闭的回归：现有测试全部通过，抽样请求的上游请求体与改动前一致。
- [x] T13.3 mock 上游 + 隔离库的真实调度：外显模型 × 规则，确认账号筛选、冷却、计分都按目标模型进行。
- [x] T13.4 泄漏扫描（06 A3）：流式、非流式、错误、count_tokens、模型列表、单个模型查询、OpenAI 兼容、公开页。
- [x] T13.5 真实 Claude Code 端到端：长会话、多工具、压缩、子代理；不报错；不出现 4.5 / 注入 / 上下文等字样；任务能够完成。
- [x] T13.6 06 和 08 的验收矩阵逐条执行，并在本文末尾记录结果。

## 15. 验收记录

（实现完成后填写：每个任务的状态、验证方式、关键证据）
