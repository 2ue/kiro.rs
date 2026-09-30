# Claude Code CLI 真实账号测试轮次记录（2026-09-29 ～ 09-30）

测试标准见同目录 `claude-code-cli-real-account-test-requirements-20260929/`。本文记录这一轮改了什么、没改什么、以及每个决策的依据。

## 测试环境

- 实例：本地 `127.0.0.1:19023`，最终验证构建为集成分支（二进制 SHA-256 `dbf31b9e…`）。
- 客户端：Claude Code CLI 2.1.283，隔离 HOME / CLAUDE_CONFIG_DIR，剥离操作者自身会话继承的 `ANTHROPIC_*` / `CLAUDE*` 变量。
- 账号：1 个 KIRO STUDENT（可用 opus 等全部模型）、3 个 KIRO FREE（本轮耗尽）；测试库另有 2 个 mock 账号（假 key）。

## 判定原则（用户确认）

1. 兼容优先：Claude Code 合法、但 Kiro 不直接支持的输入（`-thinking` 后缀、同族模型、工具名、参数取值、content 形态等）由代理拆解、映射或改写后正常服务，并在日志 / 诊断中留痕。
2. 只对明确错误报错：完全不存在的模型名、非法 role、当前轮超大图片；错误类型与文案对齐官方并带 `x-should-retry: false`。
3. 所有路由处理一致，差异只来自通用的按路由配置项。
4. 不改 usage 的计算与记录口径。
5. 不注入模型身份，保持自称 Kiro。
6. 不压缩图片（内存消耗大）。

## 已修改

| 范围 | 改动 | 依据 / 效果 |
| --- | --- | --- |
| 工具 | `tool_choice=none`、或本轮无 tools 但历史有工具时，占位工具标记为本轮不可用、注入强约束，并在响应侧丢弃对它们的调用 | 原来模型会带真实参数调用，CLI 真的执行 |
| 工具 | 实现 `disable_parallel_tool_use`：注入约束，响应侧只保留第一个 tool_use | 原来完全被忽略 |
| 工具 | `tool_choice` any/指定工具：强化提示，违规写入 `latency.toolChoiceViolation` / `droppedToolUses` 诊断 | 原来 26/26 违规且无记录；首输出前重试暂不做，先看统计 |
| 工具 | 工具名宽容匹配：模型返回去掉 Hash 后缀、大小写或分隔符不同的名字时唯一匹配回原工具；正文中的 Hash 名还原 | 原来 CLI 报 No such tool，且泄漏内部名 |
| 转换 | 同一 assistant 消息相邻 text 块以换行分隔 | 原来拼成 "First.Second." |
| 转换 | `claude-opus-5*` 不再注入"可见 thinking"策略 | 实测该策略会让 opus-5.5 只回 usage 即结束 |
| 流式 | 零输出的 usage-only 结束视为上游失败，首输出前去掉 reasoning 降级重试；仍为空时返回带 `x-should-retry: false` 的错误 | 原来返回 200 空内容；企业号带 tool 的 usage-only 兼容场景保持不变 |
| 流式 | 开 tool_use / text 块前先关闭未闭合的 XML thinking 块 | 原来块交错（S0 S1 E1 E0） |
| 流式 | 不再预开空 text 块，首段真实文本到达时才开 | 原来空块写入 CLI 会话历史 |
| 流式 | thinking 块统一带 `signature` 字段（无签名为空字符串） | 对齐协议形态 |
| 流式 | 不再按 token 数推断 `max_tokens`，内容完整时报 `end_turn` | Kiro 请求无 max_tokens 参数，误报会触发 CLI 自动续写 |
| 流式 | 流错误时不把 input 不完整的 tool_use 作为完整调用下发 | 原来 CLI 用 `{}` 执行工具后陷入循环 |
| 流式 | 实现 `stop_sequences`（跨 chunk 匹配、只作用于 text），格式非法时宽容解析 | 原来被忽略 |
| 流式 | 心跳间隔改为运行配置 `streamKeepaliveIntervalSecs`（1–300 秒，默认 5，两个管理页面可配）；空增量心跳只发给 ≥2.1.193 的 Claude Code 且仅在有打开的块时，其余发 ping；不再临时开文本块；重试后保持配置间隔 | 原来版本判断未生效，重试后间隔被误设为 180 秒 |
| thinking | 实现 `thinking.display=omitted`，非法值按默认处理 | 原来不解析 |
| thinking | 输出侧清洗与注入模板逐字一致的控制块 | 原来模型会把注入的控制提示原样输出 |
| 错误 | 不可重试的 4xx 统一带 `x-should-retry: false`；401 写 WARN 日志 | 原来 CLI 对 401 重试 11 次 |
| 错误 | 上游 INVALID_MODEL_ID 返回 404 `not_found_error`（`model: <name>`），error id 与 request id 一致 | 原来显示"请求体无效" |
| 错误 | 非法 role 返回精确的 400；标量 content 转文本 | 按报错口径 |
| 错误 | 未注册路径 / 方法返回 Anthropic 风格 JSON 404 / 405 并带 request-id；`/api/hello` 探测返回 200 | 原来空 body 404 |
| 错误 | 当前轮图片超过上游限制返回官方格式 400；历史图片保持原策略 | 原来静默丢弃，模型编造图片内容 |
| 模型 | 已知但所有账号都不可用的模型映射到同族最接近的可用模型；`-thinking` / `[1m]` 按基础模型目录调度 | 兼容优先 |
| 调度 | 有账号目录明确列出模型时，目录为空的账号不参与选号，invalid-model 在预算内继续换号；sticky 会话在模型被拒后迁出空目录账号 | 原来 mock 账号被优先选中 |
| 诊断 | 本地拒绝记录的 errorDetail 写入阶段 / 原因 / 类别（仅标识符类取值） | 原来为 null |
| models | `/v1/models` 按 Claude Code 协议返回：只列连字符 id，不列 Kiro 点号 id、别名和 `-thinking` 变体；支持 `/v1/models/{id}` 与 `limit`/`after_id`/`before_id` | 用户决策 |
| WebSearch | `allowed_domains` / `blocked_domains` 在结果侧过滤（可同时生效）；`max_uses` ≤ 0 按未设置；超长 query 按词边界截断到 200 字符 | 原来被静默丢弃；200 为保守取值 |

## 未修改

| 问题 | 原因 |
| --- | --- |
| usage 计算类（client_dropped 输出为 0、contextUsage 推算 input 偏大、响应头前断开无记录、WebSearch 子请求缺 conversationId / 搜索次数） | 用户要求不动 usage 计算与记录口径 |
| 模型自称 Kiro | 用户决定不注入身份 |
| 旧模型 ID 静默映射到同族 | 有意的兼容设计 |
| `message_start` 等上游响应头才发出 | 为首输出前换号重试保留 |
| 负例被上游变成真实工具调用 | 调用来自上游 toolUseEvent，代理不拦截合法调用 |
| `tool_choice` 违规的首输出前重试 | 成本与延迟高，先看违规统计 |
| 分块写入策略（/v1 也注入、中断残留占位符） | 改变模型写文件方式，待决定 |
| `x-kiro-rs-warnings` 不包含被忽略字段与提示词注入 | 本轮未涉及 |
| 运行配置校验错误文案带"凭据无效:"前缀 | 既有问题，所有运行配置校验共用该错误类型 |

## 验证结果

- 静态门禁：`cargo fmt --check`、`cargo test --all-targets`（2320 + 31 通过）、Clippy 基线（760 / 849）、`--no-default-features` 编译、两个前端类型检查与构建、前后端契约检查全部通过。
- 协议级：上表修改项 20/20 通过；心跳（2 秒间隔、按版本区分、页面可配、非法值拒绝）通过；WebSearch 域名过滤 4/4 通过。
- 真实 CLI 端到端：从零实现一个 Markdown 目录工具，覆盖 ultrathink、WebSearch、WebFetch、MCP 读写、文件与 Bash 工具、并行子代理、git 提交与交互式 ESC 中断，全部完成，无 API 重试。
- 长上下文：10 轮逐步增长后回忆全部正确；上下文超过 200k 时上游返回 `prompt is too long: N tokens > 200000 maximum`，CLI 自动压缩后继续并正确作答；手动 `/compact` 后信息保留；首轮单条超窗消息给出明确错误且不重试。
- 并发：3 个会话各 3 轮带工具并行全部成功；两个进程同时 `-c` 均成功；结束后 in-flight 回落为 0。
- 运行配置热更新：保活间隔、`exposeProxyWarnings`、`thinkingTriggerMode=always`、关闭 prompt steering 均即时生效，测后恢复并逐字段一致。

## 未覆盖

- F 组（fake 上游异常形态）未用真实 CLI 执行，由 handler 故障注入单测覆盖。
- 外部池相关用例：测试环境未配置外部池。
- 多账号跨账号行为：免费账号耗尽后只剩一个真实账号。
