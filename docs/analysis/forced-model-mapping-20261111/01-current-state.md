# 01 现状

代码基线：`main` @ `acfdd10c`。

## 1. 模型解析链路

客户端传入的 `model`，在入口处由全局 `ModelCapabilitiesCatalog` 解析成 `upstream_model`，然后写进 Kiro 请求的 `modelId`。账号选好之前，`modelId` 就已经定了。

| 步骤 | 位置 | 说明 |
|---|---|---|
| 入口能力预查 | `src/anthropic/handlers/request_entry.rs:125` | 用解析出的模型查 max output tokens，修 thinking 预算 |
| 正式解析 | `src/anthropic/handlers.rs:7476` `resolve_request_model` | 调 `resolve_model_with_mapping`；结果为 `Unsupported` 时返回 400 |
| 可用目录重解析 | `src/anthropic/handlers.rs:7547` `remap_model_for_usable_catalog` | 所有可用账号的显式列表都不含已解析模型时，按这些列表的并集再解析一次 |
| 转换 | `src/anthropic/converter.rs:423` | 直接用 `resolution.upstream_model` 作为 `modelId` |

`resolve_model_with_catalog_mapping_and_mode`（`src/anthropic/model_capabilities.rs:1165`）按以下顺序解析：

1. `modelMapping.enabled=false` 时，只做精确匹配，未命中就透传。
2. 显式规则。三种 kind 都参与，`source` 必须和请求模型或其 base（去掉 `[1m]`、`-thinking`）**精确相等**。不支持通配符，也不能按模型族匹配。
3. 上游目录精确命中。
4. 自动规则：去后缀、点号/横杠等价、短别名对应模型族。
5. fallback 规则。
6. 同族就近版本（`allowVersionFallback`，默认开）。可选跨族（`allowCrossFamilyFallback`，默认关）。
7. 透传。

配置只有一份全局的（`src/model/config.rs:4105` `ModelMappingConfig`，另有 `modelResolutionMode`）。没有按路由、按 API Key、按账号的映射。外部号池另有一套自己的映射（`src/external_pool.rs:616`），只作用于外部池出站请求。

**和本需求的差距**：现有机制回答的是"请求的模型不存在时怎么找替代"。`claude-sonnet-5` 会落到同族最近的版本，目录里有 4.6 就是 4.6，不一定是 4.5。`fable` 族没有可用模型时直接透传。现有机制无法表达"不管请求什么，一律发到指定模型"，规则只能逐个精确列出。

## 2. 账号模型过滤

- 账号自身的 `supported_models`（`src/kiro/model/credentials.rs:183`）优先；没有时用订阅等级列表 `subscription_model_lists`；两者都没有就不限制（`credentials.rs:849`）。
- 匹配只认写法变体：点号/横杠、日期后缀、`-thinking`、`[1m]`（`src/model/model_support.rs:221`）。不做跨版本兜底。
- `src/kiro/token_manager/degrade.rs` 是"账号 × 模型"的劣化计分，用来暂时把有问题的账号移出调度，与模型降级无关。

**已知风险**：调度时用来筛选账号的是**客户端原始模型名**，不是解析后的上游模型。

- 调用链：`handlers.rs:8881` 传入 `&payload.model` → `handle_stream_request` 的 `model` 参数 → `call_api_stream_maybe_fail_fast(..., Some(model))`（`handlers.rs:9899`）→ provider 的 `dispatch_model_filter` → `capacity.rs:20` 的 `supports_model_with_tier_lists(&[model])`。
- 后果：如果账号列表是 `[claude-sonnet-4.5, claude-haiku-4.5]`，请求 `claude-sonnet-5` 时，即使解析已经映射到 4.5，这些账号仍会被判定为 `ModelNotSupported`。
- 结论：强制映射必须改写"内部请求模型"本身，不能只改 `modelId`，否则账号筛选、冷却、劣化计分仍按外显模型进行。见 [02-design.md](02-design.md)。

## 3. 响应与计费口径

| 项目 | 现状 | 位置 |
|---|---|---|
| 流式 `message_start.message.model` | 客户端请求模型 | `src/anthropic/stream.rs:2743`，来源 `handlers.rs:10395` |
| 非流式响应 `model` | 客户端请求模型 | `handlers.rs:15038` |
| usage 记录 | `model` 记客户端模型，`upstream_model` 记上游模型 | `handlers.rs:6277` |
| 计费 | 按上游模型估价，写入 `pricing_model` | `handlers.rs:5495` |
| 上下文窗口 | 按上游模型取 | `handlers.rs:6306` |
| `/v1/models` | 由上游目录加静态列表生成，窗口取上游值 | `model_capabilities.rs:492` |

## 4. 现有能力处理（可复用）

本地 Kiro 路径**没有"按目标模型降级"这一层**。下面列出的处理逻辑，依据各不相同：有的看客户端请求模型名，有的看上游模型，有的与模型无关。

### 4.1 请求结构与未知字段

`MessagesRequest` 只建模了以下字段：model、max_tokens、messages、stream、system、tools、tool_choice、thinking、output_config、output_format、metadata、stop_sequences（`types.rs:731-765`）。

`Tool` 只建模了：type、name、description、input_schema、max_uses、allowed/blocked_domains、cache_control（`types.rs:847-872`）。

两者都**不拒绝未知字段**。因此在本地路径上，下面这些字段都会被静默丢弃：

- temperature、top_p、top_k、service_tier、container
- context_management、mcp_servers、fallbacks、speed、inference_geo
- 工具上的 strict、eager_input_streaming、defer_loading、allowed_callers、input_examples
- 内容块上的 cache_control

协议层面这满足"不报错"。但 `defer_loading` 被丢弃，意味着工具会全量发送。

### 4.2 上下文与输出

| 项目 | 现状 | 位置 |
|---|---|---|
| 每个请求的窗口值 | 先取上游模型的 `max_input_tokens_for`；取不到时，名字含 `[1m]` 记 1M，否则 200K | `handlers.rs:6307-6317`、`converter/model.rs:42` |
| 窗口值的用途 | 只有三处：<br>1. 把 `contextUsageEvent` 百分比换算成 input_tokens，到 100% 时置 `model_context_window_exceeded`<br>2. 生成 `prompt is too long: N > M` 文案<br>3. Claude Code 压缩请求重试时，预算封顶为窗口的 90% | `stream.rs:2833`、`handlers.rs:6804`、`handlers.rs:1489` |
| 超长检测 | 本地不按 token 预检，过长靠上游报错识别。payload guard 按字节/加权计量：加权上限 1.3M，max_bytes 450KiB | `handlers.rs:7176`、`payload_guard.rs:36` |
| 超长处理 | 默认 `OnTooLong` + `ClientCompaction`：直接把 prompt too long 返回给客户端，由客户端自行压缩。只有识别为 Claude Code 自己的压缩请求时，才在服务端裁剪后重试 | `config.rs:4148`、`config.rs:4169`、`handlers.rs:1280`、`handlers.rs:1456` |
| 裁剪手段 | 丢最老的历史；截断 tool_result 和 web_fetch 内容；删历史 thinking；压缩工具定义；丢图片 | `payload_guard.rs:448-722` |
| max_tokens | **不发给 Kiro**（`KiroRequest` 没有这个字段），只在 thinking 预算修复时按模型上限封顶。输出侧只有收到 `ContentLengthExceededException` 才报 `max_tokens` | `kiro/model/requests/kiro.rs:30`、`request_facts.rs:456`、`stream.rs:2931` |
| `[1m]` / `context-1m` | `[1m]` 在解析时剥掉；本地路径完全不读 `context-1m` beta 头 | `model_capabilities.rs:1128` |
| context_management | 本地路径没有实现，直接丢弃。外部池在请求没有对应 beta 时删掉该字段 | `external_pool.rs:12425` |

### 4.3 thinking / effort

| 项目 | 现状 | 位置 |
|---|---|---|
| 入口规范化 | effort 为 none 或未知值时删除；`between_tools` 改成 adaptive；budget 小于 1024 时抬到 1024，过大时扩大 max_tokens 或收紧 budget | `request_facts.rs:365-734` |
| `-thinking` 后缀 | **按请求模型名**补默认值：4.6+ 和 5.x 补 adaptive，旧模型补 enabled + 20000 | `handlers.rs:15078` |
| 原生推理字段 | catalog 支持时发 `additionalModelRequestFields`，形式为 adaptive + output_config.effort，或者 reasoning.effort。effort 取最近一档；budget 按 ≤4K low、≤16K medium、≤64K high、其余 xhigh 换算成 effort。硬编码表只覆盖 Opus 4.7+ 和 Opus/Sonnet 4.6，**4.5 不在其中** | `converter/model.rs:54-237` |
| 提示词思考（4.5 走这条路） | enabled 时注入 `<thinking_mode>enabled</thinking_mode><max_thinking_length>N</max_thinking_length>`。adaptive 或只带 effort 时注入 `<thinking_mode>adaptive</thinking_mode><thinking_effort>xhigh</thinking_effort>`，**effort 原样写成文字，不会换算成预算**。另外注入可见思考输出策略（Opus 5 不加），响应侧解析 `<thinking>` XML | `converter/thinking.rs:26-52`、`catalog.rs:87` |
| display | Opus 5、Sonnet 5、Fable 5、Mythos 默认 omitted。请求模型或上游模型任一命中就隐藏思考内容；`updates` 按 omitted 处理 | `handlers.rs:10784`、`types.rs:199`、`thinking_display.rs` |
| 合成签名 | 与模型无关，可以跨模型重放 | `signature.rs` |

### 4.4 工具

| 工具 | 现状 | 位置 |
|---|---|---|
| web_search | 所有 `web_search_YYYYMMDD` 都按原生处理。默认是模板模式：取最后一条 user 文本作为 query，搜一次，返回固定格式的摘要。模型驱动模式按路由开启，默认关 | `websearch.rs:23`、`websearch.rs:469`、`model_driven_websearch.rs` |
| code_execution / bash_code_execution | 按前缀识别，补上入参 schema。本地执行默认关闭；关闭时客户端收到的是普通 tool_use | `code_execution.rs:23`、`converter/tools.rs:476`、`config.rs:347` |
| web_fetch、memory、bash、text_editor、computer、tool_search、advisor | **没有专门处理**。作为空 schema（`{type:object, properties:{}}`）加占位描述的普通工具发出，模型不知道入参约定 | `converter/schema.rs:8`、`converter/tools.rs:459` |
| mcp_toolset | 没有 name 字段，会变成一个名为 `toolHash…` 的假工具 | `converter/tools.rs:115-217` |
| 历史中的服务端工具块 | server_tool_use 和各类 *_tool_result 渲染成文本 | `converter/content.rs:897` |
| tool_choice | `tool`：只保留指定工具。`any` / `tool`：用 system 前缀加轮次提醒来引导，事后做违规诊断，不是硬约束。`none`：历史里用到的工具改成"不可调用"的占位 | `converter/tools.rs:307-650`、`tool_use_policy.rs` |
| schema | 删掉 `additionalProperties`、`$schema` 等字段，strict 语义因此丢失。工具名超过 63 字符时改写成映射名，描述截断到 10000 字符 | `converter/schema.rs:148`、`converter/tools.rs:95` |

### 4.5 其它字段

| 项目 | 现状 | 位置 |
|---|---|---|
| messages 里的 system / developer | 内容追加到顶层 system 末尾，**原来的位置丢失**；其它未知角色直接丢弃 | `request_entry.rs:721` |
| prefill（末尾是 assistant） | **静默丢弃**，截到最后一条 user | `converter.rs:447` |
| 结构化输出 | 转成内部 `structured_output` 工具并强制调用，响应时还原成 JSON 文本 | `structured_output.rs` |
| stop_sequences | 代理在输出侧检测并截断 | `stop_sequence.rs` |
| document / PDF | 转成文本：PDF 提取文本，附带 title/context。不支持 citations 配置 | `converter/content.rs:288`、`:836` |
| files | 本地实现 Files API：单文件 50MB，最多 128 个，总量 256MB。请求里的 file 源会物化成 base64 | `files.rs:19` |
| 图片 | 缩放默认关闭。长边默认 1568，可配到 8000；单图 5MB，最多 100 张；不区分模型 | `config.rs:112`、`payload_guard.rs:32` |
| anthropic-beta | 本地路径不转发，只读取 `thinking-token-count-2026-05-13` | `handlers.rs:10801` |
| 模拟缓存的最小可缓存长度 | 按模型名区分：haiku 和 opus 4.5–4.7 为 4096，其余 1024 | `prompt_cache.rs:1941` |

### 4.6 身份

| 项目 | 现状 | 位置 |
|---|---|---|
| 身份提示 | 默认关闭。文案为 "You are Claude, made by Anthropic, not Kiro…"，不带型号。开启后插入 system[0]，轮次提醒只在有用户文字的轮次追加 | `catalog.rs:67`、`prompt_injection/mod.rs:96` |
| 渠道泄漏过滤 | `channelLeakGuard` 默认关闭。按句删除 Kiro/AWS 自述；整段被删光时，兜底回复 "I'm Claude, an AI assistant made by Anthropic." | `identity_guard.rs`、`prompt_injection/mod.rs:658`、`handlers.rs:7902` |
| 日期 / 知识截止 | 不注入 | — |

## 5. 现状缺口（本需求要补的）

1. 没有"一律改发到指定模型"的开关。规则只能逐个精确匹配，而且调度时按外显模型筛选账号（见第 2 节）。
2. 发往 4.5 时，adaptive + effort 只是以文字形式写进提示词，没有换算成思考长度。`-thinking` 后缀按外显模型补参数，强制模式下要保留这个语义。
3. max_tokens 不发给上游，输出上限靠上游自己截断后再报 `max_tokens`。客户端请求 128K、实际只有 64K 时不会报错，但会提前停下。
4. 客户端认定的上下文窗口（1M）和实际窗口（200K）不一致。现有兜底（client_compaction）会报错，与"不能报错"冲突，而且错误文案里的 `200000 maximum` 会暴露真实窗口。
5. prefill 被丢弃；中途的 system 消息丢失了原来的位置。
6. Anthropic 定义的工具（memory、bash、text_editor、computer、web_fetch）以空 schema 发出，模型无法正确调用；mcp_toolset 变成假工具；tool_search 的延迟加载失效。
7. 公开号池页冷却记录里的 `model`（`src/admin/public_pool.rs:157`）是内部模型名，会直接显示 4.5。
8. 模型自述和知识截止会暴露 4.5，而身份相关的防护默认全部关闭。
