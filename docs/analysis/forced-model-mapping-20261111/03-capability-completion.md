# 03 能力补全矩阵

原则：

- 对客户端，协议表现一律遵守**外显模型**的规则；发给上游的请求一律按**目标模型**的能力生成。
- 外显模型接受的参数绝不报错。目标模型不支持的，在转换层换算或丢弃。
- 现状细节见 [01-current-state.md](01-current-state.md) 第 4 节。

参考能力（Anthropic 官方模型表）：

| | Opus 5.5 | Fable 5.1 | Sonnet 5 | Sonnet 4.5 | Haiku 4.5 |
|---|---|---|---|---|---|
| 上下文 | 1M | 1M | 1M | 200K | 200K |
| 最大输出 | 128K | 128K | 128K | 64K | 64K |
| thinking | adaptive，不可关闭，effort 默认 medium | adaptive，始终开启 | adaptive，默认开启 | `budget_tokens` | `budget_tokens` |
| effort | low–max | low–max | low–max | 不支持 | 不支持 |
| 采样参数 | 拒绝 | 拒绝 | 拒绝非默认值 | 允许 | 允许 |
| prefill | 拒绝 | 拒绝 | 拒绝 | 允许 | 允许 |
| 视觉 | 高分辨率 | 高分辨率 | 2576px | 约 1568px | 约 1568px |

## 1. 输出上限

- `max_tokens` **不会发给 Kiro**，`KiroRequest` 里没有这个字段。所以客户端即使请求 128K，上游也不会报错。
- 实际上限由目标模型决定。输出超限时上游返回 `ContentLengthExceededException`，代理把它转成 `stop_reason: "max_tokens"`（`stream.rs:2931`）。Claude Code 收到后会自动续写，不报错。
- 入口有一步预算修复（`request_entry.rs:120` 起），会按解析出的模型的输出上限收紧 thinking 预算。强制映射换成目标模型后，这一步会自动按 64K 生效。
- 04 篇的"预留输出"按 min(客户端 max_tokens, 64K) 计算。

## 2. thinking / effort

### 2.1 现状缺口

- 目标模型是 4.5 时没有原生推理字段，只能走提示词思考。
- adaptive 或 effort 现在被原样写成 `<thinking_effort>xhigh</thinking_effort>` 这类文字（`converter/thinking.rs:32`），4.5 并不理解这些档位的含义。
- 下面三项默认值都依赖 `payload.model`，替换模型后会失效。按 02 篇第 2 节的顺序约束，必须在替换**之前**按外显模型固化：
  - `-thinking` 后缀自动开启思考
  - Fable / Opus 5.5 默认始终思考
  - display 默认 omitted

### 2.2 补全规则

强制模式下，发往 4.5 的思考请求统一转换成 `enabled` 加明确的思考长度。也就是走现有的 `<thinking_mode>enabled</thinking_mode><max_thinking_length>N</max_thinking_length>` 分支，不再走 adaptive 的文字档位分支。

| 客户端请求（外显模型） | 是否开启思考 | 思考长度 N |
|---|---|---|
| Fable 系列或 Opus 5.5，未传 thinking | 开启（外显模型语义就是始终思考） | 按 effort 换算；未传 effort 时，Opus 5.5 按 medium，Fable 按 high |
| Sonnet 5，未传 thinking | 开启（官方默认开启 adaptive） | 按 effort 换算；未传时按 high |
| `{type:"adaptive"}` 加 effort | 开启 | 按 effort 换算 |
| 只传 effort | 开启 | 按 effort 换算 |
| `{type:"enabled", budget_tokens:N}` | 开启 | N（经入口预算修复后的值） |
| `{type:"disabled"}` | 关闭。即使外显模型在协议上不允许关闭（Opus 5.5 / Fable），也不返回 400 | — |
| 外显模型名带 `-thinking`，未传 thinking | 开启 | 按 high |

effort 到思考长度的换算表（与现有 `effort_from_budget_tokens` 的区间反向对齐）：

| effort | low | medium | high | xhigh | max |
|---|---|---|---|---|---|
| N | 4K | 16K | 32K | 48K | 48K |

- N 不超过 min(客户端 max_tokens, 64K) 的 3/4，且不低于 1024。
- 换算只影响发给上游的提示词。不改写客户端可见的任何回显。

### 2.3 其它

- **入口协议校验**：现有校验（`request_facts.rs:257-326`）只检查 effort 取值、字段重复、max_tokens 范围这类通用规则，不按模型区分。强制模式下不新增任何按外显模型的拒绝。
- **display**：
  - summarized：思考可见，沿用现状。
  - omitted / updates：复用 `thinking_display.rs`。
  - 默认值的判断（`request_hides_reasoning`）改为看外显模型。这样 Fable、Opus 5.5、Sonnet 5 在客户端未指定时仍然默认 omitted，和真模型行为一致。
- **可见思考输出策略**：目标模型是 4.5，按现状注入（`model_rejects_visible_thinking_policy` 只排除 Opus 5）。
- **签名**：合成签名与模型无关（`signature.rs`）。历史中的签名都由本代理签发，可以原样重放。
- **校准**：4.5 在提示词思考下实际能用到多长的思考，需要用真实请求校准换算表（见 06 第 6 节）。

## 3. 其它协议字段

| 字段 | 现状 | 强制模式处理 |
|---|---|---|
| `temperature` / `top_p` / `top_k` | 本地路径丢弃 | 不变，不报错 |
| assistant prefill（末尾是 assistant） | 静默丢弃，截到最后一条 user（`converter.rs:447`） | 不变。外显模型本身就拒绝 prefill，这里选择容忍并丢弃，不报错 |
| `fallbacks` / `speed` / `inference_geo` / `output_config.task_budget` / `service_tier` / `container` | 结构里没有这些字段，丢弃 | 不变 |
| 对话中途的 `role:"system"` / `developer` | 追加到顶层 system 末尾，丢失原位置（`request_entry.rs:721`） | 改为在原位置转成**当轮 user 文本块**，写法与现有轮次提醒一致。`clear_at:"next_user_message"` 只在当轮生效，之后的请求里不再渲染。理由：外显模型（Opus 5.5 / Fable）的中途 system 消息语义是"从此刻起生效的运营指令"，挪到顶层会让它看起来像是开头就给出的指令，而且每加一条都会改动 system 前缀，导致缓存全部失效 |
| `context_management.edits` | 本地路径丢弃 | `clear_tool_uses_*` 和 `clear_thinking_*` 在本地执行，手段与 04 篇 L1/L2 相同，并且尊重客户端给的 `keep`、`trigger` 参数。`compact_*` 并入 04 篇的退避流程，不返回 compaction 块 |
| `output_config.format` / `output_format` | 转成内部 structured_output 工具（`structured_output.rs`） | 复用 |
| `stop_sequences` | 代理在输出侧截断（`stop_sequence.rs`） | 复用 |
| `anthropic-beta` | 本地路径不转发 | 不变；只读取 thinking-token-count，保持现状 |
| usage 扩展字段（如 `speed`、`inference_geo`） | — | 只有客户端请求了才回显，取值与请求一致 |

## 4. 工具

`Tool` 不拒绝未知字段。所以 `strict`、`eager_input_streaming`、`defer_loading`、`allowed_callers`、`input_examples` 在解析时就被丢弃，不会报错。这本身满足"不报错"的要求，副作用有两个：

- `defer_loading` 失效，工具会被全量发出；
- `strict` 失效，因为 schema 里的 `additionalProperties` 也会被删掉。

| 工具 | 现状 | 强制模式处理 | 工作量 |
|---|---|---|---|
| 自定义工具 | 正常转换 | 不变 | — |
| `tool_choice` any / tool / none | 用提示词引导，事后诊断；`tool` 只保留指定工具；`none` 时用占位工具 | 复用 | — |
| `web_search_*` | 所有 `web_search_YYYYMMDD` 都按原生处理。默认是模板模式（取 query、搜一次、按固定格式输出摘要） | 复用。外显模型是高端模型时，用户期望模型自己决定搜什么、回答带引用，所以建议强制模式的路由默认开启 `modelDrivenWebSearch` | 配置 |
| `web_fetch_*` | 没有专门处理，以空 schema 发出 | 新增代理侧抓取，按 `model_driven_websearch.rs` 的模式实现：模型给出 URL，代理去抓取，再以 `web_fetch_tool_result` 回给模型。抓取失败时返回错误块，HTTP 状态仍为 200 | 新增 |
| `code_execution_*` / `bash_code_execution_*` | 按前缀识别并补 schema；本地执行默认关闭，关闭时客户端会收到它执行不了的普通 tool_use | 强制模式下使用 code_execution，前提是本地执行已开启。未开启时把它当作不可用工具移除，避免客户端收到一个无法执行的调用。结果块类型要与请求的版本一致，例如 20260521 对应 `bash_code_execution_tool_result` | 小 |
| `bash_*` / `text_editor_*` / `memory_*` / `computer_*` / `computer_toolset_*` | 以空 schema 加占位描述发出，模型不知道该传什么参数 | 按 `native_code_execution_contract`（`converter/tools.rs:476`）的写法，为每个版本补上官方的 input_schema 和描述。这些工具仍由客户端执行，输出的 tool_use 保留原来的 name | 中 |
| `tool_search_tool_*` 加 `defer_loading` | 搜索工具以空 schema 发出；`defer_loading` 被丢弃，所有工具实际已全量发送 | 第一期：移除搜索工具，保持全量发送（现状就是全量）。第二期，工具定义过大时：代理在本地实现 regex/BM25 搜索，返回 `tool_search_tool_result`，只发送被选中的工具 | 一期小，二期中 |
| `mcp_toolset` | 没有 name，会变成一个假工具 `toolHash…`；`mcp_servers` 被丢弃 | 无法补齐，因为服务端需要真的去连 MCP。改为静默移除，不报错，也不提示 | 小 |
| advisor | 以空 schema 发出 | 无法补齐，静默移除 | 小 |

## 5. 图片 / PDF

- **图片**：缩放默认关闭。强制模式下不论全局 `resize_enabled` 是否打开，都按目标模型的长边上限 1568px（现有默认值）缩放，避免把 Sonnet 5 级别的高分辨率原图直接发出去。单张 5MB、最多 100 张的上游限制沿用现有 payload guard。
- **PDF**：`document` 块会先转成文本（提取 PDF 文本，`converter/content.rs:288`），不存在页数限制。超长的问题由 04 篇的退避处理，必要时走 L5 截断单块。citations 配置不支持，按现状忽略。

## 6. tokenizer 差异

- 新模型的 tokenizer 对同一段文本会多出 0%–35% 的 token（Sonnet 5 比 Sonnet 4.6 约多 30%），所以上游 4.5 统计出的 token 数偏少。
- 外显模型的 usage 要不要按系数放大，取决于 06 篇的计费口径决策：
  - 默认不放大；
  - 可选：按外显模型的系数放大，让下游看到的 token 量接近真实模型。

## 7. 无法补齐

| 项目 | 原因 | 处理 |
|---|---|---|
| 知识截止 | 训练数据决定 | 不伪造。对时效性强的问题，通过 `modelDrivenWebSearch` 缓解 |
| 推理和编码水平 | 模型能力差距 | 默认规则不把 opus / fable 收敛到 Haiku |
| 原生 1M 上下文 | 目标模型只有 200K | 用 04 篇的退避 |
| MCP connector、advisor | 需要服务端能力 | 静默移除 |
| strict 工具的语法约束 | 上游不支持约束解码 | 依赖模型自身遵循 schema；工具入参不合法时，由客户端按现有流程报工具错误 |
| preserved thinking 的跨轮校验 | 上游没有这个机制 | 不模拟。接受历史编辑，不报错 |
