# Claude Code CLI 真实账号调度测试要求（2026-09-29）

Role: 用真实 Claude Code CLI + 真实本地 Kiro 账号验证 kiro-rs 代理的测试要求与用例矩阵
Status: requirements / 尚未执行
Verified-against: Claude Code CLI 2.1.283（编写时本机版本）

本目录自包含：执行测试所需的背景、已知现象、预期行为都写在各分册正文里，不依赖其它文档。

## 目标

验证 Claude Code CLI 在各种真实使用方式下，经 kiro-rs 调度到真实本地 Kiro 账号时，行为与直连 Anthropic 官方 API 一致，或偏差可解释、可接受。每个发现的问题都要走完"测试 → 记录 → 修复 → 回归"闭环（见 [09](09-issue-record-and-fix-loop.md)）。

判断标准是"用户用 Claude Code 干活是否正常"，而不是"接口返回 200"。以下任一情况都算失败：会话中断、工具调用失配、thinking 被声称但没有真实 delta、usage 长期为 0、上下文静默丢失、内部术语泄漏、错误无法被 CLI 正确呈现或恢复。

### 兼容优先原则

Claude Code 协议与 Kiro 上游协议并不完全对应，代理的职责是在转换层尽量包容差异，让会话顺畅、输出内容切题，而不是把差异直接变成错误。测试时按以下口径判定：

- 能兼容的必须兼容：Claude Code 合法、但 Kiro 不直接支持的输入（模型名变体、`-thinking` 后缀、同族模型、工具名格式、参数字段、内容块类型、指纹等）应由代理拆解、映射或改写后正常服务。直接报错、静默丢弃内容、让模型"看不到"输入，都算失败。
  - 例：`claude-opus-4-5-thinking`、`sonnet-thinking` 这类名字必须拆成"基础模型 + thinking 模式"调度；某账号不支持的已知模型应换到支持它的账号，没有账号支持时映射到同族可用模型。
  - 例：Claude Code 支持而 Kiro 不接受的工具名，必须映射后可用，且下游始终看到原名。
- 能自动恢复的必须自动恢复：上游偶发空响应、格式错误、单账号不可用等，应在首个输出前重试或换号，而不是让 CLI 会话中断或进入重试循环。
- 例外：content 类型非法（数字、布尔等标量，或数组中的非对象元素）按兼容处理，标量转成文本，null 丢弃并留痕。
- 参数值宽容：WebSearch `max_uses` ≤ 0 按未设置处理；`allowed_domains` 与 `blocked_domains` 同时出现时两者都生效；`thinking.display`、`stop_sequences` 等取值不合法时按默认或可用部分处理。均需留痕，不报错。
- 该报错的必须报错：完全不存在的模型名、非法 role、当前轮图片超过大小限制（代理不压缩图片，避免大量内存消耗）等明确错误，返回错误，不做宽容改写、不静默占位。错误类型、状态码和文案与官方一致，并带 `x-should-retry: false`，避免 CLI 无谓重试。
- 区分口径：兼容针对"Claude Code 合法发出、但 Kiro 不直接支持"的内容；报错针对"按官方协议本身就不合法"的输入。
- 兼容改写要可追溯：映射、降级、丢弃都要在服务日志或 usage 诊断中留痕，便于排障。
- 模型身份：模型自称 Kiro 属于预期行为，代理不注入身份说明，不记为问题。

## 被测系统速览

- kiro-rs 对外提供 Anthropic Messages 协议，把请求转换成 Kiro 的 `conversationState` 发给 Kiro 上游，再把 Kiro 的 event-stream 转回 Anthropic SSE 或 JSON。
- 对外入口：
  - `/cc/v1`：Claude Code 专用入口（high-cache 缓存模拟，usage 按 `/cc` 策略整形），本测试的主入口；
  - `/v1`：标准入口（high-cache）；
  - `/na/v1`：no-cache 入口，usage 为原始值；
  - `/ha/v1`：high-cache，usage 按 `/ha` 策略整形；
  - `/dfcache/{route}/v1`：只有显式定义的路由可用，未定义时返回 404。
- 每个入口都有 `/messages`、`/messages/count_tokens`、`/models`、`/files`。
- 认证方式：`x-api-key` 或 `Authorization: Bearer`。
- 管理接口在 `/api/admin/*`，健康检查为 `/healthz`、`/readyz`。

## 范围

- 客户端：本机安装的 `claude` 二进制，必须真实执行。curl 只用于辅助定位和补齐 CLI 不暴露的参数，不计入 CLI 通过。
- 服务：项目唯一的长期测试实例 `127.0.0.1:19023`。
- 上游：真实本地 Kiro 账号。外部池默认关闭，只有 CC-C-07 做对照。
- 辅助层：fake upstream 或 fake Anthropic server，只用于真实账号难以稳定制造的异常（F 组），不消耗真实额度。
- 覆盖面：
  - 对话与会话：普通对话、多轮与会话恢复；
  - 模型与参数：模型与别名、CLI 参数与 API 参数；
  - 能力：thinking 全组合、内置工具、MCP、WebSearch、子代理，以及 Skills、斜杠命令、hooks、插件；
  - 输入与上下文：多模态、长上下文与压缩；
  - 统计：count_tokens、usage、缓存；
  - 可靠性：错误与异常、并发、文本编码与输出边界。

不在范围：模型智商主观评价，以及真实上游的大规模压测。

## 硬约束

1. **模型限制**：本地账号是 Kiro 免费 plan，只能调度 `claude-sonnet-4.5` 和 `claude-haiku-4.5`。
   - CLI 侧必须显式指定 `claude-sonnet-4-5` / `claude-haiku-4-5`，并设置 `ANTHROPIC_DEFAULT_SONNET_MODEL` / `ANTHROPIC_DEFAULT_HAIKU_MODEL`。
   - 原因：kiro-rs 当前把别名 `sonnet` 映射到 `claude-sonnet-4.6`，把 `opus`/`default`/`best`/`opusplan` 映射到 `claude-opus-4.7`，免费 plan 调这些模型会失败。别名行为只在 M 组作为用例单独验证。
   - 后台小模型（标题生成、摘要、WebFetch 处理、部分子代理）必须显式指向 haiku-4.5。
2. **配额**：免费 plan 额度有限。
   - 执行前后用 Admin API 记录账号余额，按下文顺序先跑低成本用例，长上下文和压缩类最后跑，并限制轮数。
   - 触发上游限流后停止当前批次，记录后等待恢复，不要连续重试打爆账号。
3. **凭据安全**：不读取、不打印、不复制任何凭据文件、环境文件和测试实例配置里的 key 值。文档、问题记录、日志摘录中的 API key、refresh token、profileArn 一律写 `<redacted>`，只保留 request id、error id、账号 id。
4. **实例与隔离**：
   - 只用 19023 实例。测试前检查服务是否已在运行，按规则决定复用或替换；测试后停止本轮启动的所有临时进程。
   - CLI 一律使用隔离的 `HOME` / `CLAUDE_CONFIG_DIR` 和临时工作目录，不得触碰本机真实 `~/.claude`。本机全局 settings 可能配置了别的 `ANTHROPIC_BASE_URL`，不隔离会打到别的服务。
5. **阶段分离**：执行测试阶段只记录问题。修复在闭环的修复阶段单独进行，每个修复单独提交，回归通过后才关闭问题。

## 文档结构

| 文件 | 内容 |
| --- | --- |
| [01 环境准备与观测](01-environment-and-observability.md) | ENV 组：预检、隔离、命令封装、日志与抓包观察点、证据与清理 |
| [02 对话、会话、模型与参数](02-conversation-session-model-params.md) | A 普通对话与输出格式、B 多轮交互与会话恢复、M 模型别名与混用、P CLI/API 参数、Q 入口路由与服务端注入 |
| [03 Thinking 全组合](03-thinking.md) | T 组：主动触发、被动触发、interleaved、签名回传、切换模型、禁用 |
| [04 内置工具、权限与 hooks](04-builtin-tools-permissions-hooks.md) | U 内置工具、并行、大结果、工具名保真；H 权限模式与 hooks |
| [05 MCP](05-mcp.md) | MCP 组：stdio/http/sse、多 server、参数与 env、复杂 schema、资源与 prompt、错误与超时 |
| [06 WebSearch、子代理、Skills 与插件](06-websearch-agents-skills.md) | W WebSearch/WebFetch、G 子代理与后台代理、S Skills/斜杠命令/插件 |
| [07 长上下文、压缩、count_tokens、缓存与 usage](07-context-compaction-tokens-cache.md) | L 长上下文与压缩、K count_tokens 与 /context、R 缓存与 usage |
| [08 错误、异常、并发、多模态与文本边界](08-errors-concurrency-multimodal.md) | E 错误与异常、C 并发与调度、F fake 辅助、I 图片/PDF/Notebook、X 文本边界 |
| [09 问题记录与修复闭环](09-issue-record-and-fix-loop.md) | 问题记录模板、严重级别、闭环流程、回归与验收标准 |

## 用例约定

- 编号：`CC-<组>-<序号>`。
- 优先级：P0 必跑（发布门禁），P1 应跑，P2 选跑（额度充足时）。
- 列：编号、优先级、来源、前置、步骤/命令、参数、预期结果、判定/观察点。
- "来源"取值：
  - `既有场景`：以往 Claude Code CLI 回归、协议问题复现与验收中做过的场景，本矩阵吸收并按当前约束改写；
  - `新增（用户要求）`：本次需求明确点名的场景；
  - `新增（补充）`：为全面覆盖而补充的场景。
- 命令里的 `ccp`、`cci`、`ccenv`、`cccurl` 定义在 [01 命令封装](01-environment-and-observability.md#4-命令封装)。

## 全局判定规则

对每条用例，除各自的预期外，统一检查：

- CLI 退出码与 `result.subtype`（`success` / `error_max_turns` / `error_during_execution` 等）符合预期。
- 最终 `result.usage` 与 `modelUsage` 非 0，有输出时 `output_tokens > 0`。中间 `message_start` 的估算值不作为判定依据。
- 每个 `tool_use` 都有对应的 `tool_result`，id 配对；CLI 看到的工具名是原名，不是 `xxxHash<8位十六进制>` 这类改写名。
- 下游可见内容（正文、错误、工具结果）不含以下内部术语：`credential`、`凭据`、`fallback`、`external pool`、`外部池`、`scheduler`、`bashHash`、`Tool results provided.`、`user Continue`。
- 服务端日志无 panic；ERROR/WARN 能对应到本用例的 request id，并有合理解释。
- 模型映射可追溯：usage 记录里的 requested model / upstream model 与预期一致（sonnet-4.5 或 haiku-4.5）。

## 执行顺序与配额预算

1. ENV 组预检：服务、账号、余额、模型目录、CLI 版本。
2. P0 冒烟：CC-A-01、CC-A-02、CC-M-01、CC-M-02、CC-T-03、CC-U-01、CC-MCP-01、CC-W-01、CC-G-01、CC-B-02、CC-E-01。任一失败先修，不继续跑大矩阵。
3. 按组跑 P0 → P1，每组结束记录余额变化。
4. F 组（fake 辅助）可以在任何时候穿插执行，不消耗真实额度。
5. 高成本组放在最后：CC-L（长上下文/压缩）、CC-C（并发）、CC-G 并行子代理。
6. 额度充足时补跑 P2 用例。
