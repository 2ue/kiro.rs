# 11 第二阶段：缺口补全（总览与任务清单）

日期：2026-11-12　分支：`feat/forced-model-mapping`（已合入 main @ `9335e987`）

第一阶段（01–10 篇）把强制收敛做通了。第二阶段对照三份新资料，补齐剩下的缺口：

- Anthropic 官方的最新模型表与 API 协议全集（2026-11-09 发布 Fable 5.2 后的状态）；
- Claude Code 作为网关客户端的实际请求形态（本机 2.1.283 抓包，对照 2.1.322 changelog）；
- 本分支代码的逐项审计。

资料摘要见 [14-reference-2026-11.md](14-reference-2026-11.md)，上下文方案见 [12-phase2-context.md](12-phase2-context.md)，知识库与身份见 [13-phase2-knowledge-identity.md](13-phase2-knowledge-identity.md)。

## 1. 目标（用户需求）

| 编号 | 需求 | 判定标准 |
|---|---|---|
| R1 | 协议无法转换的不要报错，保证能顺畅进行 | 官方会接受的请求，强制模式下都不返回 4xx/5xx；转换不了的内容降级成模型能看懂的说明，而不是静默丢掉 |
| R2 | 能支持的尽量支持（协议、tools、MCP、agent） | Claude Code 的工具、MCP、子代理、tool search 全链路可用；API 客户端常用的新字段与新块有对应处理 |
| R3 | 上下文问题要解决 | 长会话不报错、不暴露 200K；早期目标和决策在裁剪后仍可追溯；缓存前缀稳定 |
| R4 | 外挂知识库增强对外的模型认知 | 自述、阵容、知识截止、价格、API 新特性回答正确，并与官方 2026-11 状态一致；注入不可察觉、不破坏缓存 |

## 2. 审计发现（按需求归类）

### R1 仍会报错的路径

| # | 位置 | 触发 | 处理 |
|---|---|---|---|
| E1 | `converter.rs` reasoning unavailable | 强制固化了思考，而路由是 strict 或关了 ThinkingControls | 强制模式下不报错：没有可用的思考控制就按不思考发出 |
| E2 | `converter/content.rs` document | PDF 抽不出文字（扫描件、加密）时，当前轮和历史都 400，会话永久坏掉 | 官方接受这类 PDF。改为插入说明文字，让模型知道文档不可读。官方本来就拒绝的（不支持的 media_type、base64 损坏）：当前轮照旧报错，历史降级成说明 |
| E3 | `handlers.rs` websearch 分支 | 搜索路径在退避之前就 return 了，长会话加搜索会确定性 529 | 把强制退避移到搜索分支之前 |
| E4 | `handlers.rs` 搜索桥接 | strict 路由下，强制模式默认开启的搜索桥接把子请求变成原生 web_search，随后 400 | 只在路由支持 websearch 时才默认桥接 |
| E5 | `converter/tools.rs:337/401` | tool_choice 名字规范化后有歧义；同名工具只差 cache_control | 歧义时保留全部工具；比较定义时忽略 cache_control |
| E6 | 上游 IMPROPERLY_FORMED | 退避重试后仍 400 | 强制模式下多一次降级重试：历史里的工具调用扁平化成文字，再发 |
| E7 | 超长重试 | 字节上限被误封到 180KB；只重试一次；用尽后返回 529，客户端会空转重放 | 不论 payload guard 怎么配置都重试一次：按窗口 75% 裁剪（含当前轮），历史工具调用转成文字；字节上限没有配置时按窗口 × 4。之前已经有摘要检查点，正常情况下不会走到这一步 |
| E8 | 非流式 tool_use 输入损坏 | 502 | 强制模式下尽力修复 JSON，修不了用 `执行时会返回工具错误，模型能看到并重试 |
| E9 | `max_tokens: 0` | 400 | 官方用它做缓存预热。强制模式下直接返回空响应（`stop_reason: max_tokens`），不请求上游 |
| E10 | 上下文满时的停止原因 | 强制模式报 `end_turn`，客户端以为回答已完整 | 改报 `max_tokens`，Claude Code 会自动续写 |

### R2 支持缺口

| # | 项目 | 现状 | 处理 |
|---|---|---|---|
| S1 | 外显模型规格表 | 缺 Fable 5.2 / Mythos 5.2 / Sonnet 5.5 / Haiku 5.5；别名过时；发布日期和知识截止大多错或空；未收录的旧型号一律按同族最新处理（如 claude-3-5-haiku 被当成 1M 的 Haiku 5.5） | 按官方表重写（见 14 篇）；未收录的版本按"版本号最接近、且不高于请求版本"的规格处理 |
| S2 | 工具间文字转 thinking | 只覆盖 Opus 5.5 和 Fable/Mythos | 按官方名单补 Sonnet 5.5，排除 Mythos 5 |
| S3 | tool search / `defer_loading` | `defer_loading` 被丢弃，所有工具全量发送；Claude Code 2.1.304+ 的占位工具 `DeferredToolPlaceholder` 会被当成真工具发给模型；`tool_reference` 渲染成 `[tool_reference block omitted]` | 强制模式下按官方语义处理：带 `defer_loading` 且对话里没有被 `tool_reference` 引用过的工具不发；`tool_reference` 渲染为"工具 X 已可用" |
| S4 | 历史中的服务端块 | code_execution / bash / text_editor 结果、tool_search 结果、mcp_tool_use/result、compaction 块被静默丢弃；web_search 结果只剩标题和 URL | 全部渲染成文字；compaction 块按官方语义处理（之前的消息丢弃，摘要作为上下文）；web_search 结果的摘要用带前缀的 `encrypted_content` 存下来，回放时还原 |
| S5 | context_management | 不认 trigger / exclude_tools / clear_tool_inputs / clear_at_least；`clear_thinking` 不认 `keep`。Claude Code 每个请求都发 `keep: "all"`，结果被当成全部清除 | 按官方参数执行；`keep: "all"` 不清 |
| S6 | 服务端 compaction（`compact_20260112`、顶层 `compaction`） | 忽略 | 复用 12 篇的摘要引擎：按需压缩返回 `compaction` 块与 `stop_reason: compaction`；阈值压缩由服务端检查点透明完成，不返回 `compaction` 块 |
| S7 | MCP connector（`mcp_servers` + `mcp_toolset`） | 静默移除 | 本期不做。Claude Code 的 MCP 是客户端工具，已完整可用；connector 需要代理去连远程 MCP 服务器，只有直接调 API 的客户端用得上，投入产出低。见第 4 节 |
| S8 | advisor / 服务端 code_execution / web_fetch 服务端执行 | 移除或当作客户端工具下发 | advisor 和 code_execution 保持现状（移除，或在开启本地执行时执行）；web_fetch 本期保持客户端工具（Claude Code 用的是自己的 WebFetch，不受影响） |

### R3 / R4

见 12、13 篇。要点：

- 输出预留过大，有效输入只有约 129K → 改成按上限 32K 预留。
- 硬删早期轮次会丢失目标 → 换成摘要压缩（L3），并在后台预先生成。
- 知识块不进历史，导致缓存前缀变化 → 改为在每条用户消息上确定性注入。
- 自述改写会叠字（"Claude Sonnet 5 5"）→ 改成单遍替换。
- 身份过滤在强制模式下误删正常句子 → 强制模式改用精简规则集。
- 知识内容过时 → 按 2026-11 官方状态重写。

## 3. 决策

| 编号 | 决策 | 理由 |
|---|---|---|
| P2-D1 | 新行为只在强制模式下生效。下列明显的协议缺陷例外，全局修复：历史服务端块的渲染、`tool_reference` 渲染、web_search 摘要回放、document 历史降级 | 控制影响面；全局修复的这几项对任何路由都只会减少报错和信息丢失 |
| P2-D2 | 对外 input 继续按客户端原始请求报（沿用 D4） | Claude Code 按它计算上下文百分比；报小了，客户端永远不会自己压缩 |
| P2-D3 | 摘要压缩（L3）默认开启，失败就回退到硬裁剪 | 这是解决上下文问题的核心；任何失败都不允许变成报错 |
| P2-D4 | 摘要调用占用同一账号池，内部成本另记，不计入用户 usage | 与 C4 一致 |
| P2-D5 | MCP connector、advisor、web_fetch 服务端执行本期不做 | Claude Code 用不到，投入大；需要时另立项 |
| P2-D6 | `auto` 不是官方别名，但保留为兼容别名，指向 Sonnet 5.5 | 已有客户端可能在用；不在模型列表中展示 |
| P2-D7 | 知识内容只写官方可核查的事实，每条标注来源日期 | 写错比不知道更糟 |

## 4. 不做的项与原因

| 项目 | 原因 | 现状表现 |
|---|---|---|
| MCP connector | 需要代理用 Streamable HTTP/SSE 去连用户的远程 MCP 服务器，并跑服务端工具循环；只有直接调 API 的客户端用。Claude Code 的 MCP 走客户端工具，不受影响 | 工具被移除，模型看不到这些工具，不报错 |
| advisor | 需要另一个更强的模型当顾问；目标模型给自己当顾问没有意义 | 移除 |
| web_fetch 服务端执行 | 代理去抓网页有带宽和内存成本（与"远程资源不下载"的约定一致） | 作为客户端工具下发 |
| `stop_details` / refusal 模拟 | 目标模型的拒答以文字形式出现，不需要模拟 | 原样返回 |
| tokenizer 系数放大 | 沿用 D5 | — |

## 5. 任务清单

每项都写明文件、做法和验收方式。打勾表示已实现并通过验收。

### T20 规格表（S1、S2）

- [x] T20.1 `forced_model.rs` 规格表按 14 篇第 1 节重写：新增 Fable 5.2、Mythos 5.2、Sonnet 5.5、Haiku 5.5；补全知识截止、发布时间、思考默认值、effort 默认值、display 默认值、可关闭思考的 effort 上限。
- [x] T20.2 别名：`opus` / `opusplan` / `best` / `default` → Opus 5.5，`sonnet` / `auto` → Sonnet 5.5，`haiku` → Haiku 5.5，`fable` → Fable 5.2。
- [x] T20.3 未收录版本的回退：解析版本号（`claude-haiku-4` → 4.0，`claude-3-5-haiku` → 3.5，`claude-opus-6` → 6.0），取同族中不高于该版本的最高版本；比所有已知版本都低时取最低版本。
- [x] T20.4 `returns_interleaved_text_as_thinking`：Fable 全系、Mythos 5.1/5.2、Opus 5.5、Sonnet 5.5。
- [x] T20.5 单测：别名、回退、旧型号不再被当成 1M、thinking 组合矩阵按新表重算。

### T21 不报错（R1）

- [x] T21.1 E1：converter 在 `forced.is_some()` 时不返回 reasoning unavailable，改为不带思考控制发出并记 warn。
- [x] T21.2 E2：document 转换失败的降级。PDF 抽不出文字时，当前轮和历史都插入 `[document: text could not be extracted (scanned or encrypted PDF, N KB)]`；不支持的 media_type 或 base64 损坏，在历史里降级，当前轮维持 400。
- [x] T21.3 E3/E4：强制退避块移到搜索分支之前；搜索桥接的默认开启要求路由支持 websearch。
- [x] T21.4 E5：tool_choice 名字有歧义时保留全部工具；同名工具比较定义时忽略 cache_control。
- [x] T21.5 E6：强制模式下，上游 IMPROPERLY_FORMED 时做一次扁平化重试：历史中的 tool_use/tool_result 转成文字，当前轮保持原样。
- [x] T21.6 E7：强制模式下上游报超长时必做一次重试：目标为窗口的 75%，当前轮也裁进预算，历史工具调用转成文字；字节上限沿用 payload guard 配置，没有配置时按窗口 × 4（12 篇第 3 节）。
- [x] T21.7 E8：非流式 tool_use 输入修复失败时用 `{}`，不返回 502（仅强制模式）。
- [x] T21.8 E9：`max_tokens: 0` 在强制模式下合成空响应。
- [x] T21.9 E10：强制模式下上下文满时 `stop_reason` 报 `max_tokens`。
- [x] T21.10 单测：每一项各一个用例；合成"会报错的请求"清单（见 06 A5 扩充版），全部返回 200。

### T22 协议与工具（R2）

- [x] T22.1 S3：在入口原始请求体上读取 `defer_loading` 和对话中所有 `tool_reference`，丢掉没被引用的延迟工具，包括 `DeferredToolPlaceholder`。
- [x] T22.2 S3：`tool_reference` 渲染为 `[tool loaded: NAME]`。
- [x] T22.3 S4：`render_server_tool_block` 补齐 code_execution / bash_code_execution / text_editor_code_execution / tool_search_tool_result / mcp_tool_use / mcp_tool_result / container_upload 的文字渲染。
- [x] T22.4 S4：compaction 块。请求里出现 `compaction` 块时，丢弃它之前的消息，把摘要作为首条用户上下文（形态与 12 篇的摘要块相同）。
- [x] T22.5 S4：web_search 结果的 `encrypted_content` 写成 `krs1:` + base64(snippet)；回放时识别前缀，渲染出摘要。
- [x] T22.6 S5：context_management 按官方参数执行（trigger、keep、clear_at_least、exclude_tools、clear_tool_inputs；`clear_thinking` 的 keep 支持 `all` 和 thinking_turns）。
- [x] T22.7 S6：顶层 `compaction` 和 `compact_20260112` 接到摘要引擎上（12 篇 2.7 节）。
- [x] T22.8 单测：各块的渲染；延迟工具的筛选；context_management 各参数。

### T23 上下文（R3）→ 12 篇

### T24 知识库与身份（R4）→ 13 篇

### T25 管理端

- [x] T25.1 退避配置新增：最大预留输出、摘要开关、摘要触发比例、摘要后目标比例、摘要超时。
- [x] T25.2 知识库：查看内置条目（Dialog，可一键复制成可编辑条目）；命中预览（输入一段文字，显示命中条目和注入块）。
- [x] T25.3 "工具间文字转 thinking"开关的说明改为 Opus 5.5、Sonnet 5.5、Fable、Mythos 5.1/5.2。
- [x] T25.4 提示：强制模式下会默认开启模型驱动搜索。

### T26 验收

- [x] T26.1 `cargo test --bin kiro-rs` 全量、fmt、clippy 基线、前端 `tsc -b`。
- [x] T26.2 真实链路：147 个可用 free 账号；协议矩阵（每个 E*/S* 各一例）、身份与知识对照组、长上下文（300K/600K/1M，带早期事实回忆题）、泄漏扫描。
- [x] T26.3 真实 Claude Code：opus / sonnet / haiku / fable 四个别名；多工具编码任务、子代理、MCP + tool search（`ENABLE_TOOL_SEARCH=true`）、`/compact`、长会话。
- [x] T26.4 在本篇末尾逐条记录验收结果。

## 6. 验收记录（2026-11-13）

环境：本地 PG + Redis，147 个可用 free 账号，Kiro 客户端版本 1.4.154，强制模式开启，外显模型映射到 Sonnet 4.5 / Haiku 4.5。

| 项 | 结果 |
|---|---|
| T26.1 单测 | `cargo test` 3155 + 31 通过，18 个忽略（与 main 相同）；`cargo fmt --check` 通过；clippy 461 条，main 基线 465 条，没有新增；前端 `tsc -b`、`npm run build` 通过 |
| T26.2 协议矩阵 | 冒烟 18 例全部 200：history 内的 thinking / server tool 块、`max_tokens: 0` 预热、`between_tools`、tool search（`defer_loading` + `tool_reference`）、browser toolset、context_management、请求里的 compaction 块、按需压缩（返回 `compaction` 块与 `stop_reason: compaction`）等 |
| T26.2 身份与知识 | 69 题评测（身份、知识截止、阵容、价格、对比、API、Claude Code、截止后事件、无关问题、泄漏）：开启 68/69，唯一失败是上游 429，重试通过；关闭知识库的对照组 38/69。身份题按别名和完整 ID 每题重复 10 次：40/40 |
| T26.2 长上下文 | 300K / 600K / 1M 全部 200，上游 input 分别为 215K / 383K / 654K；首条消息、早期用户约束、第 4 个工具结果末尾的根因三条事实都答对（3/3） |
| T26.2 泄漏扫描 | 评测 69 题和真实 CLI 全部回复中，Kiro、AWS、Amazon、system-reminder、assistant_identity、"根据提供的上下文"出现 0 次。4.5 只出现在阵容和退役问题的回答里（旧版模型列表、Sonnet 4.5 退役日期），属于正确内容，没有一处是自述 |
| T26.2 中途 system | 语言指令遵从 47/48（含工具结果之后） |
| T26.3 真实 Claude Code | 临时 `CLAUDE_CONFIG_DIR` 注入，不读也不改本机配置。身份（opus）、阵容（sonnet）、编码（haiku：Read / Edit / Write / Bash 共 10 次调用，5 个测试通过）、子代理（opus：Agent + 后台任务，结果正确）、MCP（opus：调用 `mcp__orders__lookup_order`）、tool search（`ENABLE_TOOL_SEARCH=true`：ToolSearch 后再调用 MCP 工具）、7 轮会话加 `/compact`（压缩后仍记得代号和 `kx:` 约束）全部成功，`api_errors` 为 0 |

