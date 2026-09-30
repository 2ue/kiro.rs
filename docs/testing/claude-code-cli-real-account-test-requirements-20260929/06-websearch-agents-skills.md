# 06 WebSearch、子代理、Skills 与插件

本分册包含 W（WebSearch / WebFetch）、G（子代理与后台代理）、S（Skills、斜杠命令、插件与输出样式）三组用例，未新开分组。

「来源」列含义：既有场景 = 以往测试中已出现过的场景（含已知现象）；新增（用户要求）= 与本轮测试要求直接对应；新增（补充）= 为覆盖完整性补充。

## W WebSearch / WebFetch

背景：Claude Code 的 `WebSearch` 工具会另发一个一次性子请求，`tools` 只含一个服务端工具 `web_search`（`type: web_search_20250305` 等版本）。kiro.rs 在 `tools` 仅含一个原生 `web_search` 时走内置 WebSearch 转换（调用 Kiro MCP `web_search`），返回 `server_tool_use` + `web_search_tool_result` + 文本；与其他工具混在一起时走 mixed 分支。CLI 主会话使用客户端 WebSearch 工具，原生搜索只在一次性子请求中出现，回放历史时保留文本摘要。

已知现状（测试时需逐项确认是否已修复）：

- 代理不跑真实的搜索推理循环：从最后一条 user 文本取 query，调用一次 MCP，用模板拼结果（片段截到 200 字符），`encrypted_content` 放明文片段，不返回 citations。
- `allowed_domains`、`blocked_domains`、`user_location` 在解析时被静默丢弃；`max_uses` 只解析不使用。
- 为兼容下游中转平台，刻意删除了 `usage.server_tool_use.web_search_requests`，CLI 看不到搜索次数。
- 历史 assistant 消息中的 `server_tool_use` / `web_search_tool_result` 块以前被整块丢弃，现已渲染为有上限的文本。
- 隔离的 CLI 环境下（CLI 2.1.280）曾出现未形成原生 tool_use 的情况：MCP 调用 0 次，模型输出 XML/function-call 文本或计划类文本；直接用 curl 请求原生 web_search 可以工作。

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-W-01 | P0 | 既有场景 | 基线；服务端可观察 MCP 调用次数与 usage | `ccp --allowedTools WebSearch '搜索 Rust 1.80 的发布日期并给出来源链接'`；另用 `claude --print --tools=WebSearch --allowedTools=WebSearch --output-format stream-json --verbose '<需要联网搜索的问题>'` 对照 | `--tools=WebSearch`、`--allowedTools=WebSearch`、`--output-format stream-json --verbose` | 返回答案和来源；工具调用名是 WebSearch，结果数为 1；CLI 正常结束（exit 0） | 只有服务端出现真实 MCP 调用，且有配对的 `server_tool_use` 与 `web_search_tool_result` 才算通过，仅有 `server_tool_use` 标记不算；服务端 usage 中 `web_search_requests` 大于 0（若已开启计数）且记录为本地成功；主会话最终 usage 非 0；无内部术语泄漏。已知现象：CLI exit 0，但未通过代理发起原生 WebSearch，工具列表只见 telemetry，模型只输出计划类文本或 XML/function-call 文本；这属于 CLI 工具协商或模型配置的兼容问题，需单独记录与修复 |
| CC-W-02 | P0 | 新增（用户要求） | 同 W-01 | haiku-4.5 为主模型跑 W-01 | `--model claude-haiku-4-5` | 同上 | WebSearch 子请求的模型字段与服务端映射一致 |
| CC-W-03 | P1 | 既有场景 | 同 W-01 | 1) 中文查询、带引号/特殊字符查询、很长的查询；2) 非流式请求（curl `stream:false`）；3) 长历史多轮后再要求搜索新内容；4) 只有搜索前缀、没有实际查询内容的请求；5) 当前 user 消息取不到 query、只能取到历史里的旧 query | 流式 / 非流式 | 1) 成功；2) 非流式返回 JSON 而不是 SSE；3) 搜的是当前 query 而不是历史旧 query；4)/5) 在调用 MCP 前就给出确定性 400 拒绝，不消耗搜索，不拿旧 query 去搜；原生搜索失败后能走外部回退 | query 提取正确，不取错消息；usage 与 attempt 有记录；原始 query 和请求 body 不写入日志。已知问题：曾搜到旧 query、非流式返回 SSE、usage 缺失、日志泄漏隐私、原生搜索绕过外部回退；空 query 直接交给真实 MCP 会返回参数错误且没有可用搜索内容 |
| CC-W-04 | P1 | 新增（用户要求） | 同 W-01 | 一轮中连续多次搜索（"分别搜索 A、B、C 并对比"） | — | 多次搜索均成功 | 每次子请求独立成功；结果不串 |
| CC-W-05 | P1 | 新增（补充） | 同 W-01 | 搜索无结果的查询（随机字符串） | — | 模型说明没有结果 | 空结果不导致错误 |
| CC-W-06 | P1 | 既有场景 | 可在上游偶发失败时观察，或在 fake 环境让搜索后端返回 429/5xx/超时/断连/JSON-RPC 错误/无结果 | 触发 WebSearch，分别以「HTTP 200 + `web_search_tool_result_error`」和「HTTP 502」两种形态返回给 CLI 做对照 | stream-json 观察 `apiRetryCount`、`terminalReason` | 可恢复失败返回 HTTP 200，assistant 消息里带 `web_search_tool_result_error`，模型继续，CLI 正常结束本轮：`apiRetryCount=0`、`terminalReason=completed`、`resultIsError=false`、收到 `message_stop`；502 对照时 CLI 反复 api_retry，最终 `error_during_execution` / `aborted_streaming` | 错误码合理，内部原因不泄漏；可恢复失败不能被 CLI 当成 API 层失败；坏包、ID 不匹配、结果超限、超大或非 UTF-8 响应等协议损坏仍应作为 API 错误硬失败，不能包装成成功 |
| CC-W-07 | P1 | 既有场景 | 同 W-01 | 执行一次 WebSearch 后，`-c` 连续追问两轮："第二条结果的标题和 URL 是什么"、"展开说说" | `-c`、`--output-format stream-json --verbose` | 块顺序为：决策文本、`server_tool_use`（id 以 `srvtoolu_` 开头）、`web_search_tool_result`（`tool_use_id` 配对）、摘要文本；追问能准确答出标题、片段和 URL | 历史中的 web_search 块回放不报错，模型仍能引用上一轮搜索内容；无 citations 为已知情况，不判失败 |
| CC-W-08 | P1 | 既有场景 | curl 直连服务端 | 1) `tools` 同时包含 `web_search_20250305` 与普通客户端工具；2) 构造 `max_uses: 0`、负数、`1`、`5` 的 web_search 工具定义 | 流式 / 非流式 | 1) 包含原生 web_search 时走服务端搜索（MCP）分支，不把 `web_search` 当成普通 tool_use 发给没有执行器的客户端；本轮普通客户端工具被忽略（已知边界）；2) 修复后 `max_uses<=0` 本地返回 400，`>=1` 固定只搜 1 次 | mixed 分支行为；现状 `max_uses` 只解析不使用；CLI 的 WebSearch 子请求一般只带 web_search 一个工具，主路径不受影响 |
| CC-W-09 | P2 | 既有场景 | curl 直连；另观察较新版本 CLI 的请求 | 1) curl：未知版本 `web_search_20991231`、名字不是 `web_search` 的 web_search 类型；2) 观察较新 CLI 是否发送 `web_search_YYYYMMDD` 服务端工具（包括更新的版本号，以及与普通工具混用） | 流式 / 非流式 | 1) 清晰处理（接受或明确 400）；2) 若触发，返回 `server_tool_use` 和 `web_search_tool_result` 各 1 个，stream / non-stream 都是 200；混用时走服务端搜索分支，不退化成普通 tool_use | 未列出的原生 web_search 版本、错误命名的原生 web_search 工具均有确定行为；已知：真实 CLI 2.1.280 未触发原生 WebSearch，原生路径在 CLI 下的完整验证仍缺，需在报告中注明 |
| CC-W-10 | P1 | 既有场景 | 账号支持 WebFetch | WebSearch + WebFetch 组合：先搜索再抓取第一个链接并总结，然后继续追问一轮 | `--allowedTools WebSearch,WebFetch` | 成功返回并继续对话 | 两个工具链路都正常；作为 WebFetch / WebSearch 完整回归的必跑项 |
| CC-W-11 | P1 | 新增（补充） | settings `permissions.deny: ["WebSearch"]` | 要求搜索 | settings 权限 | 被拒，模型说明 | 不发 WebSearch 子请求 |
| CC-W-12 | P1 | 既有场景 | 同 W-01 | 提示"只在 docs.rs 上搜索 tokio spawn_blocking"，让 CLI 带 `allowed_domains` 调用 WebSearch；查看返回链接和服务端记录的 MCP 请求 | `allowed_domains: ["docs.rs"]` | 修复后返回链接都在 docs.rs 或其子域，CLI 正常完成；全部被过滤时按合法零结果处理 | 已知现象：`allowed_domains` 在解析时被静默丢弃，结果中会出现不在范围内的网站；预期：过滤生效 |
| CC-W-13 | P1 | 既有场景 | 同 W-01；另用 curl 构造互斥请求 | 1) 让 CLI 搜索并排除 github.com；2) curl 构造 `allowed_domains` 与 `blocked_domains` 同时出现的工具定义 | `blocked_domains: ["github.com"]` | 1) 修复后结果不含 github.com 及其子域；2) 两字段同时出现返回 400 `invalid_request_error`，且不调用 MCP | 已知现象：`blocked_domains` 不生效；官方规定两字段互斥 |
| CC-W-14 | P1 | 既有场景 | 同 W-01；准备开启/关闭搜索计数的两种服务端配置 | 执行 WebSearch 后在 `message_delta.usage` 或最终 usage 中查找 `server_tool_use`，并查看 CLI 的搜索计数与费用显示（`/cost`） | 服务端搜索计数配置开 / 关 | 修复后开启配置时 `web_search_requests=1`，工具出错时为 0；关闭配置时与现状一致 | 已知现象：为兼容下游中转平台刻意删除了 `usage.server_tool_use.web_search_requests`，CLI 看不到搜索次数 |
| CC-W-15 | P2 | 既有场景 | fake 环境让搜索后端超时 | 触发 WebSearch，观察服务端返回与 CLI 提示 | — | 修复后返回 504 `timeout_error` | 已知现象：返回 504 `api_error`，未按官方错误类型表对齐 |
| CC-W-16 | P1 | 既有场景 | 准备一段由其他实现产生的会话：历史 assistant 含服务端搜索调用、搜索结果块与网页抓取结果块，后续请求不再声明原生搜索工具 | 继续该会话（`--resume` 或 curl 回放），追问"你找到的 URL 和页面标题是什么" | 历史含 `server_tool_use` / `web_search_tool_result` / 抓取结果 | 答案包含 URL 与标题；上游 assistant 文本中有渲染后的搜索内容，无需配对的工具调用；`encrypted_content` 等密文字段不进入上下文；单块有长度上限（最多 8000 字符、最多 20 条结果）；响应 warnings 头中有服务端块转文本的计数 | 已知现象：以前这些块被整块静默丢弃，模型丢失搜索结果与查询词，网页抓取结果也丢失，相邻文本块直接粘连；现已改为有上限的文本渲染，需回归 |
| CC-W-17 | P1 | 既有场景 | 同 W-01；可抓取原始上游 SSE 与转换后 SSE | 流式 WebSearch 回合中，搜索后继续输出文本或工具调用；对比原始 SSE、转换后 SSE 与 CLI 消费结果 | `--output-format stream-json --verbose` | `content_block` 的 index 连续递增；插入搜索块后后续块不偏移、不重复从 0 开始；CLI 不报错 | 服务端插入搜索块或过滤内部块可能导致 CLI 收到不连续的 index；目前是否需要偏移尚未证实有问题，需以真实 CLI 结果为准 |
| CC-W-18 | P2 | 既有场景 | 同 W-01；可在 fake 上游让模型输出伪 XML | 让模型输出类似 `<search_web><query>...</query></search_web>` 的文本 | 流式 / 非流式 | stream 和 non-stream 下都被识别并恢复成搜索调用，不作为普通文本显示给用户 | Claude Code 风格的伪 XML 搜索需要兼容恢复；恢复后仍应有配对的 `server_tool_use` / `web_search_tool_result` |

WebFetch 其余用例见 [04 CC-U-08](04-builtin-tools-permissions-hooks.md#u-内置工具)。

## G 子代理 Agent/Task 与后台代理

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-G-01 | P0 | 既有场景 | `--agents '{"quick":{"description":"Returns SUBAGENT-OK","prompt":"Reply with exactly SUBAGENT-OK"}}'` | `ccp '调用 quick 子代理，然后回复 AGENT-FINAL-OK'`；用 `--print` + stream-json 检查 `subagent_stats.spawned` | `--agents`、`--print`、`--output-format stream-json --verbose` | 子代理返回 SUBAGENT-OK，主会话返回 AGENT-FINAL-OK；子代理真实派发（`spawned>0`） | stream-json 中 Task/Agent 的 tool_use 与 tool_result 配对，两段标记文本都出现；子代理请求独立成功；主会话与子代理 usage 都非 0。已知现象：`--print` 下多次尝试中出现过模型用文本模拟 function_calls 或只调 Bash、`spawned=0` 的情况，此时不能据此宣称 agent 能力通过 |
| CC-G-02 | P0 | 新增（用户要求） | 内置 general-purpose / Explore 子代理 | `'用子代理探索 $WORK 并总结目录结构'` | — | 成功 | 子代理内部的工具调用（Glob/Read/Grep）全部成功 |
| CC-G-03 | P1 | 新增（用户要求） | 子代理 `model` 字段分别为 `haiku`、`sonnet`、`inherit`；并设置 `CLAUDE_CODE_SUBAGENT_MODEL=claude-haiku-4-5` 做对照 | 调用子代理 | 子代理 `model`、`CLAUDE_CODE_SUBAGENT_MODEL` | 子代理模型按配置落到 haiku-4.5 / sonnet-4.5 | 服务日志中子代理请求的 model；`model: opus` 时清晰报错 |
| CC-G-04 | P0 | 既有场景 | 同 G-01 | `'并行启动 3 个子代理，分别统计 a/b/c 目录的文件数'` | — | 3 个子代理并行执行并汇总；子请求各自完成，主会话拿到全部子代理结果 | 同时有 3 个以上 in-flight 请求；调度和并发控制正常，免费账号数量少时排队而不失败；结果正确。真实会话矩阵要求覆盖多代理触发的协议调用，Claude Code 子代理是并发来源之一 |
| CC-G-05 | P1 | 新增（用户要求） | 子代理带工具限制 `tools: ["Read"]` | 让子代理执行 Bash | 子代理 `tools` | 子代理无法执行 Bash | 工具集合正确下发 |
| CC-G-06 | P1 | 新增（补充） | `--forward-subagent-text` + stream-json | 同 G-01 | `--forward-subagent-text` | 子代理文本/thinking 带 `parent_tool_use_id` 转发 | 事件结构正确 |
| CC-G-07 | P1 | 新增（补充） | 同 G-01 | 子代理嵌套：让子代理内再调用子代理（若 CLI 允许） | — | 按 CLI 语义执行或拒绝 | 无异常 |
| CC-G-08 | P1 | 新增（用户要求） | `$WORK/.claude/agents/reviewer.md` 定义项目级代理 | 交互式 `/agents` 查看并调用 | — | 调用成功 | 文件定义的代理加载正确 |
| CC-G-09 | P1 | 新增（用户要求） | 基线；先在交互式会话中完成权限免责声明和初始化（`--bg` 依赖交互式初始化） | `claude --bg '长任务：逐个读取 20 个文件并总结'`，然后 `claude agents`、`claude logs <id>`、`claude attach <id>`、`claude stop <id>`、`claude rm <id>` | `--bg` | 后台会话运行、可查看、可附着、可停止 | 后台会话的请求正常；停止后服务端 in-flight 回落；测试结束后清理 idle 的后台进程，确认无残留后台会话 |
| CC-G-10 | P2 | 既有场景 | G-08 的 reviewer；另准备 `--agents '{"auditor":{"description":"...","prompt":"你只分析，不修改文件"}}'`（可附加很长的 agent prompt） | 1) `--agent reviewer` 以指定代理作为主会话；2) `claude -p --model sonnet --agents '<auditor 定义>' --agent auditor --allowedTools "Read,Grep,LS" "审计某些文件，输出三个风险点"` | `--agent`、`--agents`、`--model sonnet`、`--allowedTools` | 成功，exit 0；子代理发起工具调用并成功完成；最终回答不重复 | 主会话 system/工具按代理定义；很长的 agent prompt 不触发请求体裁剪异常；若 CLI 内部自动选了 Haiku，只记为测试环境限制，不算协议失败 |
| CC-G-11 | P1 | 新增（用户要求） | 交互式，同 G-04 | 子代理运行中按 ESC 中断主会话 | — | 子代理停止，可继续对话 | 中断的子代理请求在服务端记录为 client drop，不遗留 in-flight |
| CC-G-12 | P1 | 既有场景 | 同 G-01；保留 CLI transcript 与服务端请求时间线 | 一次派出多个 Agent（如 9 个），执行耗时差异较大的任务，观察长时间无输出后集中输出 | — | 能通过 transcript 算出各 Agent/工具的等待时间，并与服务端请求延迟时间线区分 | 等待发生在本地 Agent 上时不判为代理卡住；代理不伪造 assistant 文本制造进度感。已知现象：派出 9 个 Agent，最慢的几分钟后才返回，用户误以为流卡住 |

## S Skills、斜杠命令、插件与输出样式

这些大多是客户端能力，重点确认它们产生的请求（额外 system 段、注入的 user 消息、Skill 工具调用）经服务端转换后正常。不要加 `--bare`。

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-S-01 | P1 | 新增（补充） | `$WORK/.claude/skills/hello/SKILL.md`（description 明确触发条件，正文要求输出 SKILL-OK） | 交互式/非交互触发该 skill | — | 模型调用 Skill 工具并输出 SKILL-OK | Skill 工具的 tool_use/tool_result 正常 |
| CC-S-02 | P1 | 新增（补充） | 同上 | 直接输入 `/hello` | — | skill 执行 | 同上 |
| CC-S-03 | P1 | 新增（补充） | `$WORK/.claude/commands/greet.md`（带 `$ARGUMENTS`） | `/greet 张三` | `$ARGUMENTS` | 参数替换正确 | 展开后的消息发送成功 |
| CC-S-04 | P1 | 新增（补充） | 交互式 | 内置命令：`/help`、`/status`、`/cost`、`/context`、`/model`、`/config`、`/memory`、`/init`、`/review`（若存在）、`/doctor` | — | 各命令正常 | 触发 API 请求的命令（`/init`、`/review` 等）成功；`/cost`、`/context` 的数值合理（见 CC-K 组） |
| CC-S-05 | P1 | 新增（补充） | 交互式 | `#` 快捷写入记忆或 `/memory` 编辑 CLAUDE.md，下一轮验证生效 | — | 记忆生效 | CLAUDE.md 内容进入请求 |
| CC-S-06 | P1 | 新增（补充） | `$WORK/CLAUDE.md` 写入规则"回答必须以 OK: 开头" | 非 bare 运行 A-01 | — | 回答遵守规则 | CLAUDE.md 注入未被服务端裁剪 |
| CC-S-07 | P2 | 新增（补充） | `--plugin-dir` 加载一个含命令、代理、hook、MCP 的本地测试插件 | 使用插件中各组件 | `--plugin-dir` | 均正常 | 插件 MCP 与 hook 联动 |
| CC-S-08 | P2 | 新增（补充） | 交互式 | `/output-style` 切换输出样式（如 explanatory / learning） | — | 回答风格变化 | system 变化后请求正常 |
| CC-S-09 | P2 | 新增（补充） | `--disable-slash-commands` | 输入 `/hello` | `--disable-slash-commands` | 作为普通文本发送或被拒 | 无异常 |
| CC-S-10 | P2 | 新增（补充） | `--safe-mode` | 运行 A-01、U-01 | `--safe-mode` | 成功 | 定制关闭后基础能力正常 |
