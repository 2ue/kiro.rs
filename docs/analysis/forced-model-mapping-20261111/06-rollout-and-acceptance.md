# 06 实施分期、验收与待决策项

## 1. 实施分期

| 期 | 内容 | 依赖 |
|---|---|---|
| P1 核心映射 | 配置结构、入口改写、`display_model` 贯穿所有对外字段、保存配置时校验目标模型、开关关闭时路径保持不变 | 无 |
| P2 协议补全 | 03 篇第 1–3 节：替换模型前，按外显模型固定 thinking 和 display 的默认值；effort 换算成 enabled 和思考长度；中途 system 消息保留原位置，转成一对指令 / 确认消息；在本地执行 context_management 的 clear 类操作 | P1 |
| P3 上下文退避 | 04 篇：本地 token 预检；L0–L2、L4、L5；上游拒绝后内部重试；强制模式下不走 client_compaction，也不对外返回 prompt too long；`model_context_window_exceeded` 改为 `end_turn` | P1 |
| P4 隐藏补全 | 05 篇：错误文案替换、`/v1/models` 口径、公开页、模型自述的提示词与输出改写 | P1 |
| P5 工具补全 | 03 篇第 4 节：给 Anthropic 定义的工具补 schema；mcp_toolset 和 advisor 静默移除；tool_search 移除；本地执行未开启时移除 code_execution；强制模式下默认开启模型驱动的 web search | P2 |
| P6 进阶 | L3 锚定摘要；web_fetch 由代理抓取；tool_search 本地实现；usage 按 tokenizer 系数放大 | P3、P5 |
| P7 知识库 | 07 篇：K0 自我认知卡（与 P4 的身份提示合并，**K0 必须随 P4 一起上线**）；K1/K2 条目库、关键词检索、当轮注入、复述清理；管理端维护与导入导出 | P4 |
| P8 逐模型专项 | 08 篇：Opus 5.5 把工具调用之间的文字转成 thinking 块；computer toolset 展开与 `toolset_name` 回填；截图坐标换算；单条消息上的 effort；可见 thinking 泄漏检查 | P2、P4、P5 |

上线的最低集合是 P1–P4，再加上 P7 的 K0 和 P8 中的"可见 thinking 泄漏检查"。缺任何一项都可能出现报错或暴露。

## 2. 改动清单（预估）

| 区域 | 文件 | 改动 |
|---|---|---|
| 配置 | `src/model/config.rs` | 新增 `ForcedModelMappingConfig`，加入 runtime config 与路由覆盖 |
| 入口 | `src/anthropic/handlers/request_entry.rs`、`handlers/parsed_body_pipeline.rs` | 先按外显模型固定 thinking 和 display 的默认值，再改写模型并保存 `display_model`；`normalize_message_roles` 在强制模式下保留中途 system 消息的原位置 |
| 对外字段 | `src/anthropic/handlers.rs`（流式模板、非流式响应、usage、错误） | 所有对外字段里用到 `payload.model` 的地方，统一改成 `display_model`。OpenAI 兼容出口已经用的是客户端模型，不需要改 |
| thinking | `src/anthropic/converter/thinking.rs`、`handlers.rs`（`request_hides_reasoning`） | 强制模式下把 adaptive / effort 转成 enabled 加思考长度；display 默认值改为看外显模型 |
| 历史 | `src/anthropic/converter/history.rs` | 保留原位置的中途 system 消息，渲染成一对指令 / 确认消息；处理 `clear_at` |
| 工具 | `src/anthropic/converter/tools.rs`、`websearch.rs`、`code_execution.rs` | 补 schema、匹配版本前缀、静默移除 |
| 上下文 | `src/anthropic/payload_guard*.rs`、`handlers/local_body_pipeline.rs`、`handlers.rs`（错误映射、压缩请求预算）、`stream.rs`（`model_context_window_exceeded`） | 本地预检，按级退避；强制模式下不对外返回 prompt too long，也不对外返回"窗口已满" |
| 隐藏 | `src/anthropic/envelope.rs`、`model_capabilities.rs`（模型列表）、`src/admin/public_pool.rs`、`identity_guard.rs`、`prompt_injection` | 文案替换、模型列表口径、公开页、自述改写 |
| 知识库 | `src/model/prompt_injection/*`（K0 并入身份注入）、新增知识库模块、`converter.rs`（当轮注入）、`control_block_cleaner.rs`（复述清理） | K0/K1/K2 的存储、检索和注入 |
| 逐模型 | `stream.rs` 及非流式响应组装（工具调用之间的文字转 thinking、`toolset_name` 回填、坐标换算）、`identity_guard.rs`（覆盖 thinking） | 08 篇第 2、3 节 |
| 管理端 | `ui/src/features/runtime/*` | 新增"强制模型收敛"配置区，放在现有"模型解析"旁边，二者明确区分；另加知识库条目管理 |

## 3. 验收矩阵

| 编号 | 场景 | 通过标准 |
|---|---|---|
| A1 | 开关关闭，跑全量现有测试 | 全部通过；抽样请求的上游请求体和改动前逐字节一致 |
| A2 | 外显模型 {opus-5-5, fable-5-1, sonnet-5, opus-4-8, haiku-4-5, opus, sonnet, haiku, default} × 规则 | 上游 `modelId` 等于规则目标；账号按目标模型筛选、冷却、计分 |
| A3 | 泄漏扫描：流式、非流式、错误、count_tokens、`/v1/models`、`/v1/models/{id}`、OpenAI 兼容出口、公开页 | 响应体和响应头都不含 `4.5`、`4-5`、`sonnet-4`、`haiku-4`（排除用户内容回显） |
| A4 | thinking：{未传, adaptive, enabled+budget, disabled} × effort {无, low…max} × display {无, summarized, omitted, updates} × 外显模型 | 全部返回 200；响应格式与外显模型一致；不出现 400 |
| A5 | 协议字段：temperature/top_p、prefill、fallbacks、speed、中途 system、context_management、structured outputs | 全部返回 200；中途 system 的指令能被遵从 |
| A6 | 工具：所有 Anthropic 定义工具的各版本 + mcp_toolset + advisor + tool_search/defer_loading | 全部返回 200；可补齐的工具能被正确调用 |
| A7 | 上下文：构造 150K、300K、600K、1M 历史，各 20 轮 | 零报错；对外从不出现 prompt too long；缓存命中率达到现有目标；L3 摘要命中缓存 |
| A8 | 上游拒绝：注入估算偏差 | 内部重试后成功，对外不报错 |
| A9 | Claude Code 自身压缩请求在 1M 附近触发 | 压缩成功 |
| A10 | 身份：有/无 Claude Code 系统提示 × 中英文 × 问型号、问知识截止 | 自称外显模型的比例达标，用对照组对比；可见 thinking 中不含 4.5 自述 |
| A11 | 真实 Claude Code 端到端：长任务编码会话 | 无报错、无暴露，任务能完成 |
| A12 | 外部号池：`applyToExternalPool=false` | 外部池出站行为不变 |

## 4. 决策项

以下决策已全部按推荐方案定稿，定稿结果见 [10-implementation-checklist.md](10-implementation-checklist.md) 第 1 节。

| 编号 | 问题 | 推荐 |
|---|---|---|
| D1 | 生效粒度 | 全局开关 + 按路由覆盖；不按账号映射（理由见 02 第 2 节） |
| D2 | 是否作用于外部号池 | 默认不作用 |
| D3 | 默认规则 | opus/fable/sonnet → Sonnet 4.5；haiku → Haiku 4.5。不建议全部收敛到 Haiku |
| D4 | usage 输入口径 | 报客户端实际发送的量，以保证客户端能正常压缩（04 第 4 节） |
| D5 | tokenizer 系数放大 | 默认不放大 |
| D6 | 下游计费口径 | 见第 5 节风险，由业务方决定 |
| D7 | 退避 L3 摘要是否启用 | 第一期不做；P6 再加 |
| D8 | 知识库 K2 时事条目的收录范围与审核责任人 | 先只收录 Claude/Anthropic 生态（K1），K2 按需求逐步增加；每条入库前都要人工审核来源 |
| D9 | Opus 5.5 与 Fable / Mythos 5 系工具调用之间的文字转 thinking 块 | 默认开启，与真模型结构保持一致 |

## 5. 风险

- **商业与合规风险（需要业务方明确决策）。** 对外显示高端模型、实际由 4.5 提供服务，同时刻意隐藏这一点。如果下游按高端模型的价格付费，就构成对付费方的不实陈述，有合规和信誉风险。另外，质量、速度、知识截止的差异，用户迟早会发现（见 05 第 3 节）。
  - 建议：外显模型名保持一致，这是协议兼容上的需要；计价按实际服务的档位，或者明确作为"经济档"对外提供。
  - 本文只提供技术方案，不对商业口径下结论。
- **长会话质量。** 退避一定会丢失早期上下文，长任务后期的表现会比真 1M 模型差。
- **复杂任务能力。** 4.5 和 5.x 的能力差距无法靠协议补全弥补，agent 编码类任务的成功率会下降。
- **维护成本。** Anthropic 新增模型、工具版本或协议字段时，补全矩阵需要跟进。未识别的新字段默认被 serde 丢弃，所以不会报错；但新工具类型需要补 schema，否则按普通工具处理，可能无法调用。外显模型阵容变化时，K0 卡和 K1 模型阵容条目也要同步更新，否则模型会说出过时的阵容。
- **知识库错误。** 收录了错误事实，模型会自信地说错，比不知道更糟糕。所以入库前必须人工审核。

## 6. 实现前需要核实

已核实，结论写进了 01–05 篇：

- max_tokens 不会发给 Kiro，输出上限靠上游截断后返回 `max_tokens`。
- document/PDF 会先转成文本，没有页数限制。
- 请求和工具结构都不拒绝未知字段，新字段会被静默丢弃。
- 中途出现的 system 消息会被并入顶层 system；prefill 会被丢弃。
- context_management 在本地不实现；本地也没有 token 预检。
- web_fetch、memory、bash、text_editor、computer、tool_search、advisor 都以空 schema 发出；mcp_toolset 会变成一个假工具。
- `-thinking` 后缀和 display 默认值都按 `payload.model` 判断。

仍然待核实：

- OpenAI Responses 出口回写 `model` 的位置（chat completions 已核实，回写的是客户端模型）。
- 4.5 在提示词思考模式下实际用了多少思考长度，用来校准 03 篇第 2 节的换算表。
- `code_execution_20260521` 的结果块类型，现有实现是否按请求的版本输出。
- 下游 sub2api 同步 usage 时用的是哪个模型字段（参考现网 sub2api usage 的查法）。
- 本地输入 token 估算与 4.5 上游实际计数的偏差分布，用来校准 04 篇的安全余量。
