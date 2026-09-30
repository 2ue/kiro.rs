# 02 对话、会话、模型与参数

命令封装 `ccp` / `cci` / `ccenv` 见 [01](01-environment-and-observability.md#4-命令封装)。未注明模型时为 `claude-sonnet-4-5`。

本分册分组：A 普通对话与输出格式、B 多轮交互与会话恢复、M 模型别名与混用、P 参数，以及本分册新开的 Q 组（入口路由与服务端注入，编号 `CC-Q-<序号>`）。

「来源」列取值：
- 既有场景：此前测试中已出现过的场景，步骤、预期和已知现象直接写入本表。
- 新增（用户要求）：直接对应本轮用户要求（普通对话、多轮交互、thinking、各种参数、MCP、WebSearch、tools、agent）。
- 新增（补充）：为覆盖边界或配置路径补充的用例。

## A 普通对话与输出格式

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-A-01 | P0 | 既有场景 | 基线环境 | `CASE=CC-A-01 ccp 'Reply with exactly: pong'`；非交互基线冒烟另跑 `ccp '请用三点列出当前目录这个项目是什么，不要改文件。'`，以及 `ccenv claude --print 'Reply with exactly: cli-pong'` | stream-json + partial；`--print` 文本 | 输出 `pong` / `cli-pong` / 三点概述，exit 0 | `message_start` → `content_block_start(text)` → `text_delta` → `message_delta(stop_reason=end_turn, usage)` → `message_stop`；`result.subtype=success`；usage 非 0；有 request id；没有重复段落；没有 `server_error`、`bad_request`、`assistant-prefill`、`Improperly formed request`。背景：发版前的基础连通性检查 |
| CC-A-02 | P0 | 既有场景 | 同上 | `CASE=CC-A-02 CC_MODEL=claude-haiku-4-5 ccp 'Reply with exactly: pong'`；另用 `--model haiku` 发一条普通文本请求 | haiku | 同 A-01，成功返回 | usage 记录 upstream model 为 `claude-haiku-4.5`，usage 非零。背景：需要覆盖多个模型族 |
| CC-A-03 | P0 | 新增（用户要求） | 同上 | `ccenv claude -p --output-format json 'Reply with exactly: pong'` | json | 单个 JSON 结果 | `result`、`usage`、`total_cost_usd`、`session_id` 字段齐全，`is_error=false` |
| CC-A-04 | P1 | 新增（用户要求） | 同上 | `ccenv claude -p 'Reply with exactly: pong'` | text | 纯文本 `pong` | stdout 无多余前后缀 |
| CC-A-05 | P1 | 新增（用户要求） | 同上 | `ccp` 不加 `--include-partial-messages` | stream-json 无 partial | 只出现完整 `assistant` 与 `result` | 无 `stream_event`，内容与 A-01 一致 |
| CC-A-06 | P0 | 新增（用户要求） | 同上 | `cci` 启动交互式，输入 `你好，用一句话介绍你自己`，等待完成后 `/exit` | 交互式 | 流式逐字显示，结束后可继续输入 | 首字时间、无卡顿/重复段落；屏幕无错误提示；状态栏 token 计数更新 |
| CC-A-07 | P1 | 新增（补充） | 同上 | `echo '解释什么是 CAP 定理' \| ccenv claude -p` | stdin 输入 | 正常回答 | 管道输入等价于参数输入 |
| CC-A-08 | P1 | 新增（用户要求） | 同上 | `ccp --input-format stream-json` 从 stdin 连续写入 3 条 user 消息 | stream-json 输入 | 3 轮依次回答，共享上下文 | 每轮一个 `result`；第 3 轮能引用第 1 轮内容 |
| CC-A-09 | P1 | 新增（补充） | 只在 settings.json 的 `env` 中配置 BASE_URL/模型，shell 不设这些变量 | 运行 A-01 | settings 配置路径 | 同 A-01 | 服务端收到请求，模型正确 |
| CC-A-10 | P1 | 新增（用户要求） | 同上 | `ccp --json-schema '{"type":"object","properties":{"city":{"type":"string"},"temp":{"type":"number"}},"required":["city","temp"]}' '北京今天大概多少度？随便编一个'` | 结构化输出 | 输出合法且符合 schema 的 JSON | CLI 结构化输出实现依赖工具调用或约束，确认服务端正确转换；`result` 中 structured 字段存在 |
| CC-A-11 | P2 | 新增（补充） | 同上 | `ccp --prompt-suggestions true 'Reply with: ok'` | prompt 建议 | 回答后出现 `prompt_suggestion` 消息 | 额外的后台请求模型为 haiku-4.5 或主模型，均成功 |
| CC-A-12 | P1 | 新增（补充） | 同上 | 空白 prompt：`ccp '   '`；超短 prompt：`ccp '?'` | 边界输入 | CLI 自身拒绝或模型正常回应，无 400 | 若服务端返回 400，错误文案可读、带 error id |
| CC-A-13 | P0 | 既有场景 | 基线环境，MCP 按 03 分册配置好 | 发版前最小组合冒烟：依次跑简单问答（A-01）、单个工具调用（如让模型用 Bash 执行 `pwd`）、thinking（见 CC-T 组任一 P0 用例）、多轮对话（B-02）、MCP（任一 P0 MCP 用例） | 组合冒烟 | 全部成功 | tool_use 与 tool_result 正确配对；最终 usage 不为零；没有内部标记泄漏。背景：这是发版前的最小 CLI 冒烟集 |
| CC-A-14 | P1 | 既有场景 | 基线环境 | `ccp '写一篇约 1500 字的文章，介绍分布式一致性的发展历史'` | 中等长度输出 | 内容完整，`stop_reason=end_turn` | 结尾完整、没有静默截断；有 `message_delta` 和 `message_stop` 终态。背景：流式长输出容易暴露截断或终态缺失 |
| CC-A-15 | P2 | 既有场景 | 基线环境 | 分别在普通散文、流式输出、工具结果总结、长历史续聊、歧义提示下观察输出；再要求"输出一段包含 `<br>` 的 HTML 片段" | 输出清洁度 | 普通散文中不出现无故的 `<br>`；明确要求 `<br>` 或网页内容时原样输出 | 已知现象：曾怀疑输出被 `<br>` 污染，多种场景下都未复现，因此服务端没有加过滤；预期：服务端不改写模型文本 |

## B 多轮交互与会话恢复

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-B-01 | P0 | 新增（用户要求） | 交互式 | `cci`；第 1 轮让模型记住 `kiwi-river-714`；第 2 轮纠正"其实是 kiwi-river-715"；第 3 轮问"总结我们前两轮说了什么，最后的代号是什么" | 交互式多轮 | 回答 715 且能总结纠正过程 | 每轮请求 messages 持续增长；无 user/assistant 角色交替错误（已知风险：历史转换时角色顺序被打乱会导致上游 400） |
| CC-B-02 | P0 | 既有场景 | 临时 HOME；固定 `--session-id`；不要带 `--no-session-persistence` | 第 1 轮 `ccp --session-id <uuid> '记住 kiwi-river-714，回复 TURN1-OK'`（或让模型读 README 并总结）；之后 `ccp --resume <uuid> ...` 续接至少 3 轮：复述代号、"基于上一轮上下文继续，不要重复上一轮原文" | resume 多轮 | 每轮成功、exit 0；准确复述 `kiwi-river-714`；后续轮承接上下文，不重复上一轮大段内容 | 服务端后续请求包含前几轮历史；usage 非 0；`terminal_reason=completed`，`api_error_status` 为 null；不出现 assistant-prefill 或工具配对错误。已知现象：命令带 `--no-session-persistence` 时会话无法恢复，去掉后多轮正常；CLI 提示 session id "already in use" 属本地 CLI 会话状态问题，换新 UUID 即可，不算上游失败 |
| CC-B-03 | P0 | 新增（用户要求） | 同一 `$WORK` | 连续 `ccp 'turn1'`、`ccp -c 'turn2'`、`ccp -c 'turn3 引用 turn1'` | `--continue` | 链式继续 | 3 次共享同一会话 |
| CC-B-04 | P1 | 新增（用户要求） | B-02 会话 | `ccp --resume <uuid> --fork-session '在分叉里改代号为 X'`；再 `--resume <uuid>` 原会话问代号 | fork | 分叉不影响原会话 | 新 session_id；原会话仍为 714 |
| CC-B-05 | P1 | 新增（用户要求） | 交互式 | 交互式中 `/resume` 选择 B-02 会话后继续对话 | 交互式恢复 | 历史完整显示，可继续 | 历史中的工具块与 thinking 块回放后不触发 400（已知风险：历史 tool_use/tool_result 和 thinking 签名回放时格式不符会被上游拒绝） |
| CC-B-06 | P0 | 既有场景 | 含工具调用的会话 | 第 1 轮让模型用 Bash 执行 `ls`，第 2 轮 `-c` 追问结果；第 3 轮 `-c` 再调用工具；之后继续用 sonnet 做多轮加工具的长会话，扫描 CLI debug 日志和服务端日志 | 带工具历史的续聊 | 均成功 | 历史 tool_use/tool_result 配对；历史工具结果被 `payloadShaping` 截断时模型仍能正常回答；日志中不出现 `assistant-prefill final message is not supported`、`last message must be user`、`profileArn is required`、`server_error`、`bad_request`，没有重复的 final message。背景：历史上出现过最后一条消息是 assistant 预填、重复输出等问题 |
| CC-B-07 | P1 | 既有场景 | 交互式 | 3 个独立交互式 CLI 会话，每个至少 20 轮用户/助手对话（工具和 MCP 子请求不算轮次；可用脚本 `tmux send-keys`），覆盖 thinking、顺序和并行工具、Bash/文件操作、搜索、MCP、图片/文档、子代理；每个会话用唯一的合成标记验证模型确实读到了内容 | 多轮稳定性 | 每个会话 20 轮以上无错误 | 每轮 TTFT 无明显劣化；usage 正常增长；每轮结果和请求数可审计；缺轮或交互失败按失败算；不能用单次冒烟代替。背景：既往证据只说明 CLI 用例通过，没有持久记录三会话、20+ 轮和完整功能矩阵 |
| CC-B-08 | P1 | 既有场景 | 交互式 | 一轮中途按 ESC 打断，然后立即输入新问题（制造当前消息前紧邻另一条 user 的情况）；另用 hook 在 user 消息后追加 user 内容再跑一轮；让模型复述上一轮它看到的内容，并抽样上游请求 | 中断后继续 / 连续 user | 新问题正常回答；连续 user 合并为一个回合 | 被打断轮次在历史中的形态不导致下一轮 400；服务端记录 client drop 而不是错误；上游历史中不出现凭空插入的 assistant `OK`。已知现象：代理会给前一条 user 自动配一条伪造的 assistant("OK")，不符合官方同角色合并语义；预期：修复后合并连续 user |
| CC-B-09 | P1 | 新增（用户要求） | 交互式 | 输入 `/clear` 后问前文代号 | 清空上下文 | 模型不知道代号 | 新请求不含旧历史 |
| CC-B-10 | P2 | 新增（补充） | 交互式 | `/rewind`（或双击 ESC 回到之前的消息）后编辑重发 | 回退重发 | 从回退点继续 | 请求历史被截断到回退点 |
| CC-B-11 | P1 | 新增（补充） | 服务重启 | 会话中途重启 19023 实例（本用例明确覆盖重启），然后 `-c` 继续 | 跨服务重启续聊 | 继续成功 | 不依赖服务端内存状态；thinking 签名、缓存等不因重启失效导致 400 |
| CC-B-12 | P2 | 新增（补充） | `--no-session-persistence` | `ccp --no-session-persistence 'hi'`，再 `ccp -c 'hi again'` | 不持久化 | 第二次是新会话 | 符合 CLI 语义即可 |
| CC-B-13 | P2 | 新增（补充） | 交互式 | `-n my-session` 命名会话，`/resume` 按名称查找 | 会话命名 | 可找到 | 纯客户端行为，确认无额外异常请求 |
| CC-B-14 | P1 | 既有场景 | 基线环境 | 分别做：短会话中文提问；中途从中文切换英文；长历史（20 轮中文）后切换英文；`/compact` 压缩后切换语言；两个相反语言的并发会话 | 回复语言 | 回复始终跟随最新一条用户消息的语言 | 服务端没有锁定首轮语言的状态；并发会话互不影响。已知现象：用户反馈过疑似"锁死第一次使用的语言"，目前未复现，服务端也没有加强制语言覆盖 |
| CC-B-15 | P2 | 既有场景 | 基线环境 | 让会话中产生含多个 text 块的 assistant 消息（如文本 → 工具调用 → 文本），下一轮 `-c` 让模型原样复述上一条回复，并抽样上游请求中的历史 | 多 text 块拼接 | 块之间有空行分隔 | 不出现 `First.Second.` 式粘连。已知现象：同一 assistant 的多个 text 块直接拼接，而 user 侧用换行、连续 assistant 之间用空行，三处拼接规则不一致；预期：修复后统一用空行分隔 |
| CC-B-16 | P1 | 既有场景 | 真实 Kiro 上游；每个会话独立 `$WORK` | 5 个隔离会话，每个先用 `--session-id` 开始，之后只用 `claude --continue` 续接，每个会话 20 轮（加压版 100 轮），每轮触发 Bash 和 Read 工具 | 长会话工具循环 | 所有轮次成功；历史和请求体线性增长 | tool_use 与 tool_result 数量一致、无孤立项；泄漏标记命中为 0；每轮 usage 稳定非零；单轮 p99 约 650ms 以内（不含模型生成时间，按代理自身耗时统计）。背景：该场景在模拟上游下已通过，真实上游下需补测；长会话里工具历史和工具名映射的往返必须一致，不能污染回答 |
| CC-B-17 | P1 | 既有场景 | 交互式 | 多轮会话中途追加新要求（例如"要记录目的，以及实现后如何配置各项参数"），逐轮记录：最新要求、assistant 下一步动作、是否明确承接、工具或 Agent 介入量、最终是否覆盖 | 最新指令承接 | assistant 的下一步和最终回复都明确回应这条新要求，而不是接着做原任务 | 核对上游请求体中确有这条用户消息、位置正确、没有被大量历史或工具结果淹没。已知现象：对话"断感很强"，模型像没收到最新指令；此问题与 thinking 无关 |
| CC-B-18 | P1 | 既有场景 | 完成 B-01、B-06、B-07 后 | 人工抽查多轮回答质量 | 回答质量 | 回答完整、不中途断裂 | 不答非所问、不无视工具结果、不只输出模板话术。背景：为规避上游报错做的兼容修复可能让模型"变笨" |

## M 模型别名与混用

目的：确认免费 plan 下 Claude Code 常见模型写法能落到 sonnet-4.5 / haiku-4.5，且不可用模型给出清晰错误。

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-M-01 | P0 | 既有场景 | 基线 | `--model claude-sonnet-4-5`、`--model claude-sonnet-4-5-20250929` 各跑 A-01；有外部池时同样命令经外部池路由再跑一次；有支持 opus 的账号时补跑 `--model claude-opus-4-8` 等完整横杠 ID | 全名/带日期/横杠写法 | 成功 | 本地路径：支持点号版本的账号被选中，实际发给 Kiro 的是点号模型（upstream model 为 `claude-sonnet-4.5`）；外部池路径：出站保持横杠 ID；usage 里的请求模型记为横杠 ID。背景：Claude Code 用横杠 ID，Kiro 用点号 ID，命名不一致会导致账号匹配不上或出站模型错误 |
| CC-M-02 | P0 | 新增（用户要求） | 基线 | `--model claude-haiku-4-5`、`--model claude-haiku-4-5-20251001` | haiku 全名 | 成功 | upstream 为 `claude-haiku-4.5` |
| CC-M-03 | P0 | 既有场景 | 设置 `ANTHROPIC_DEFAULT_SONNET_MODEL/HAIKU_MODEL` | 用默认模型（不传 `--model`）、`--model sonnet`、`--model haiku`、完整 ID（如 `claude-sonnet-4-5`）各发一条简单 prompt | CLI 别名 | CLI 把别名展开成环境变量指定的全名，都返回预期文本、exit 0 | 抓包确认 CLI 实际发送的 model 字段；stream-json 中的模型与服务端记录一致；横线与点号写法等价解析，不降级 |
| CC-M-04 | P0 | 既有场景 | 不设置 `ANTHROPIC_DEFAULT_SONNET_MODEL` | `ccenv claude --print --model sonnet 'Reply with exactly: pong'` | 别名直达服务端 | 本地账号路径应解析到免费 plan 可用的 `claude-sonnet-4.5` 并成功 | 记录实际结果：CLI 发出的名字、服务端映射（既往记录为 `claude-sonnet-4.5`，当前实现可能映射到 `claude-sonnet-4.6`，以实测为准）、免费 plan 是否失败；usage 里上游模型与请求一致，输出和缓存 usage 非零。失败时必须是清晰的公开错误（模型不可用），不得是 5xx 或内部术语；若映射到账号不可用的模型导致失败，记为问题。背景：别名解析错会导致模型不可用或按错误模型计费 |
| CC-M-05 | P0 | 既有场景 | 不设置 OPUS 变量 | `--model opus`、`--model opusplan`、`--model best`、`--model default`、`--model claude-opus-4-5` | 别名映射 + 免费 plan 不可用模型 | 别名映射到对应系列的当前版本；免费 plan 不可用时给出清晰错误，CLI 显示可读信息并正常退出 | 从日志确认映射结果；不认识的新 Sonnet/Opus ID 不会被静默降级到旧版本；错误为 4xx、带 error id；不在多个账号间无限重试；账号不因此被冷却或禁用。已知现象：旧逻辑把不认识的新 ID 映射到已有旧版本，用户以为在用新模型实际被降级 |
| CC-M-06 | P1 | 新增（用户要求） | 交互式 | 会话中 `/model` 在 sonnet-4.5 与 haiku-4.5 间来回切换，每次切换后问一句并引用前文 | 会话内切模型 | 切换后都能继续且记得上下文 | 请求 model 字段随切换变化；历史不因模型变化报错 |
| CC-M-07 | P1 | 新增（补充） | `CC_NONESSENTIAL_OFF=` 空（允许后台请求），不加 `--bare`，交互式 | 新会话首条消息后观察服务端 | 主 sonnet + 后台 haiku | 标题生成/话题判断等后台请求打到 haiku-4.5 并成功 | 服务日志中出现 haiku 请求且 200；后台请求失败不影响主对话 |
| CC-M-08 | P1 | 新增（补充） | 只设旧变量 `ANTHROPIC_SMALL_FAST_MODEL`，不设 `ANTHROPIC_DEFAULT_HAIKU_MODEL` | 同 M-07 | 旧变量兼容 | 记录后台模型实际取值 | 用于确定推荐配置 |
| CC-M-09 | P1 | 新增（用户要求） | 基线 | `--model claude-sonnet-4-5 --fallback-model claude-haiku-4-5`；再用 `--model opus --fallback-model claude-sonnet-4-5` | fallback | 主模型可用时用主模型；主模型不可用时 CLI 回退并成功 | 回退依赖服务端返回的错误类型能被 CLI 识别为"不可用/过载"，记录 CLI 是否真的回退 |
| CC-M-10 | P1 | 既有场景 | 基线 | 分别用 `--model 'claude-sonnet-4-5[1m]'`，以及不带后缀但 CLI 发送 context-1m beta 头（`--betas context-1m-2025-08-07`）的模型跑长上下文会话 | 1M 后缀 / 1M beta 头 | 成功（剥离后缀）或清晰错误；带 `[1m]` 后缀按 1M 窗口处理 | 服务端剥离 `[1m]` 后缀后再发上游；CLI 的上下文窗口判断随之变为 1M，需结合 CC-L 组注意压缩阈值；观察 usage、上下文统计和压缩时机。已知现象：本地路径不读取 `anthropic-beta` 和 `anthropic-version` 头，只有 `[1m]` 后缀或上游目录中的 maxInputTokens 能把窗口变成 1M；预期：修复后 context-1m beta 与 `[1m]` 后缀等价 |
| CC-M-11 | P1 | 新增（用户要求） | 基线 | `--model claude-sonnet-4-5-thinking`、`--model claude-haiku-4-5-thinking` | thinking 后缀 | 成功并有可见 thinking（见 CC-T-10） | 不得静默退化为普通模型 |
| CC-M-12 | P2 | 既有场景 | 基线 | 大小写与空白：`--model Claude-Sonnet-4-5`、`--model ' claude-sonnet-4-5 '`；不存在的模型：`--model claude-foo-9-9`；构造错误参数（如 curl 发送 `max_tokens: -1`） | 异常写法 / 非法请求 | 成功或规范化的 4xx 错误，CLI 显示清楚的错误 | 不产生 5xx；错误中不出现号池、凭据等内部字样；普通请求错误不能被当成可重试错误跨账号放大 |
| CC-M-13 | P1 | 既有场景 | 基线 | `curl /cc/v1/models`、`/v1/models`，重复请求两次；在 CLI 中用 `/model` 或 `--model` 选择 opus、sonnet、haiku、default 及带 `-thinking` 后缀的 ID，各发一句 | models 接口 / 模型选择 | 列出可用模型；别名和 `-thinking` 变体都能正常对话（本质是换别名或切换 thinking 开关，不是另一个模型） | 结构与 Anthropic 一致：每项 `type="model"`、RFC3339 格式 `created_at`，顶层有 `has_more`/`first_id`/`last_id`；重复请求时间字段稳定；只包含免费 plan 实际可用模型或能标出可用性。已知现象：该端点原为 OpenAI 风格（`type="chat"`、整数秒 `created`、未知模型的 `created` 每次都变），列表混入别名和 `-thinking` 变体；单模型查询与分页尚未实现。推理本身不依赖该端点，主要影响模型选择器和 SDK |
| CC-M-14 | P2 | 既有场景 | 账号池中只有少数账号的模型目录包含某个模型（免费 plan 账号池通常不满足，需混合账号池时执行，否则记为跳过） | `ccenv claude -p --model sonnet-thinking 'Reply with only: alias-think-ok'`，再用 `--model sonnet` 重复多次；查看 usage 的上游模型和 attempts | 部分账号支持的模型 | 别名请求全部派到支持该模型的账号并返回 200；当前缓解为失败后换一个账号重试一次（attempts 为 2），并对拒绝的账号按模型记冷却，CLI 尽量不看到 400 | 已知现象：别名解析使用全池模型目录并集，派发时不按账号目录过滤，未配置模型列表的账号被当作支持所有模型；修复前 CLI 直接显示 `API Error: 400 The request body is invalid` 且不换号；支持该模型的账号很少时前几个请求仍可能失败 |
| CC-M-15 | P1 | 既有场景 | 基线 | `--model claude-3-5-sonnet-20241022`、`--model claude-opus-4-1-20250805` 各发一轮；另用 `--model claude-sonnet-4-20250514` 回归；查看 stream-json 中 `message_start.message.model`、响应头和代理日志里的上游模型 | 旧版/带日期模型 ID | 发生重映射时响应头带 `x-kiro-rs-model-resolved` 标出实际模型；显式指定 4.1 不跨 minor 升级到 4.5（无显式规则时透传，由上游报真实错误）；3.x 是否升级由配置开关决定；`claude-sonnet-4-20250514` 仍解析为 `claude-sonnet-4` | 免费 plan 下上游可能报模型不可用，重点观察映射结果与错误是否清晰。已知现象：3.5 Sonnet 被换成 sonnet-4.5，opus-4-1 被换成 opus-4.5，响应 model 字段却回显请求值，用户以为在用旧模型，实际能力、thinking 支持、输出上限和计费都不同 |
| CC-M-16 | P2 | 既有场景 | 已配置外部池路由（未配置则记为跳过） | 同一个旧模型 ID（如 `claude-3-7-sonnet-20250219`、`claude-sonnet-4-20250514`）分别走本地路由和外部池路由 | 路由间模型解析一致性 | 两条路由解析结果一致 | 对比上游实际收到的 model、响应头和 usage 记录。已知现象：外部池路由用另一套宽松的子串匹配改写模型，名字含 sonnet 且含字符 4（包括日期中的 4）就映射为 sonnet-4.5，3-7-sonnet 反而不映射 |

## P 参数

### CLI 参数

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-P-01 | P0 | 新增（用户要求） | 基线 | `ccp --append-system-prompt '所有回答必须以"喵"结尾' '1+1=?'` | `--append-system-prompt` | 回答以"喵"结尾 | 追加内容出现在 system 中，未被服务端请求体大小保护裁剪 |
| CC-P-02 | P1 | 新增（用户要求） | 基线 | `ccp --system-prompt '你是一个只会说英文的助手' '你好'` | `--system-prompt` | 英文回答 | 替换默认 system；工具说明也随之消失，确认服务端不注入冲突提示（/cc 入口的注入见 CC-Q-02） |
| CC-P-03 | P1 | 新增（用户要求） | 基线 | 超长 append-system-prompt（约 50KB） | 超长 system | 正常回答 | system 未被静默裁剪；若超限，应走上游 too-long 流程（返回让 CLI 可识别并触发压缩的错误），而不是静默截断 |
| CC-P-04 | P0 | 新增（用户要求） | 基线 | `ccp --max-turns 1 --allowedTools Bash '用 Bash 执行 ls 然后总结'`（隐藏参数，以当前 CLI 实测为准） | `--max-turns` | `result.subtype=error_max_turns` | CLI 在工具调用后停止，服务端无异常 |
| CC-P-05 | P1 | 新增（用户要求） | 基线 | `ccp --max-turns 3` 做需要 5 步的任务 | `--max-turns` | 停在第 3 轮 | 同上 |
| CC-P-06 | P0 | 新增（用户要求） | 基线 | `ccp --tools '' '只回答文字：天空为什么是蓝色'` | `--tools` 空 | 无 tools 字段的请求成功 | 请求体 `tools` 为空或不存在，服务端不注入工具 |
| CC-P-07 | P1 | 新增（用户要求） | 基线 | `ccp --tools 'Bash,Read' ...` | `--tools` 子集 | 只暴露两个工具 | 请求体 tools 数量为 2 |
| CC-P-08 | P0 | 新增（用户要求） | 基线 | `ccp --allowedTools 'Bash(git *)' --disallowedTools Write '执行 git status，然后尝试写文件 a.txt'` | 工具白/黑名单 | git 执行成功，Write 被拒 | 被拒的工具以 `is_error` tool_result 回传，下一轮正常继续 |
| CC-P-09 | P1 | 既有场景 | 基线 | `ccenv claude --print --model sonnet --effort high 'Reply with exactly: pong'`；再 `ccp --effort low/medium/high/xhigh/max`（各一次） | `--effort` | 均成功，effort 被正确传到上游 | 抓包记录 `output_config.effort`，确认在代理链路中不丢失；sonnet-4.5/haiku-4.5 不在原生 reasoning 表中时，服务端对不支持的 effort（如 xhigh/max）应降级或忽略，不得报 400 |
| CC-P-10 | P1 | 新增（用户要求） | 基线 | `ccp --betas context-1m-2025-08-07 ...`、`--betas interleaved-thinking-2025-05-14 ...` | `--betas` | 成功 | `anthropic-beta` 头被接受；记录是否被静默忽略（已知现象：本地路径不读取 `anthropic-beta` 头） |
| CC-P-11 | P2 | 新增（用户要求） | 基线 | `ccp --max-budget-usd 0.0001 '写一篇 2000 字文章'` | `--max-budget-usd` | CLI 按预算停止 | 预算基于服务端返回的 usage 计算，确认计费估算合理 |
| CC-P-12 | P1 | 新增（用户要求） | 准备 `/tmp/kiro-cc-extra-$RUN_ID/x.txt` | `ccp --add-dir /tmp/kiro-cc-extra-$RUN_ID '读取额外目录中的 x.txt'` | `--add-dir` | 能读到 | 纯客户端能力，确认工具结果正常回传 |
| CC-P-13 | P2 | 新增（用户要求） | 基线 | `ccp --exclude-dynamic-system-prompt-sections ...` | 动态 system 段落 | 成功 | 动态段落移入首条 user 消息后，服务端缓存模拟与 usage 仍合理 |
| CC-P-14 | P2 | 新增（用户要求） | 基线 | `ccp --system-prompt-snapshot off` 连续两轮 | system 快照 | 成功 | 每轮 system 重新渲染不影响续聊 |
| CC-P-15 | P1 | 既有场景 | 基线；另准备只含 `env.CLAUDE_CODE_MAX_OUTPUT_TOKENS=512` 的临时 Claude Code settings 文件 | `CLAUDE_CODE_MAX_OUTPUT_TOKENS=512 ccp '写一篇 3000 字的文章'`，观察服务日志里的 `max_tokens`；再改用临时 settings 文件（`--settings <file>`）重跑 | 输出 token 上限 | 上限生效时 `stop_reason=max_tokens`，CLI 提示输出被截断或自动继续 | 流式与非流式的 `stop_reason` 一致；不得变成 error 事件。已知现象：该环境变量没有覆盖用户 settings，日志仍显示 `max_tokens=32000`；需要限制时改用临时 settings 文件。背景：CLI 默认 32000 输出上限会让免费账号测试成本很高 |
| CC-P-16 | P1 | 新增（用户要求） | 基线 | `CLAUDE_CODE_MAX_OUTPUT_TOKENS=64000`（超过模型上限） | 超上限 max_tokens | 成功或服务端规范化 | 不产生 400（已知风险：超出模型上限的 max_tokens 原样转发会被上游以请求无效拒绝） |
| CC-P-17 | P2 | 新增（补充） | 基线 | `API_TIMEOUT_MS=5000` 下让模型输出长文 | 客户端超时 | 超时后 CLI 重试或报超时 | 服务端记录 client drop，账号 in-flight 回落 |

### API 参数（Claude Code 不直接暴露，用 curl 对 `/cc/v1/messages` 补齐）

这些参数 Claude Code 在某些路径会发送（抓包确认），或其他兼容客户端会用到。用 curl 验证服务端行为。已知缺口：代理只建模了 model、max_tokens、messages、stream、system、tools、tool_choice、thinking、output_config、metadata，其余字段（temperature/top_p/top_k、stop_sequences、context_management、service_tier 等）反序列化时直接丢弃，不报错也不告警；`tool_choice` 的 `any`/`tool` 目前靠提示词约束而非原生支持。

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-P-20 | P1 | 既有场景 | 基线；分别配置 `missingMaxTokens` 为自动补全与拒绝两种策略 | curl 发送 `max_tokens` = 1 / 16 / 8192 / 64000 / 缺失；再发 JSON 无效和 `max_tokens` 非法值的请求 | `max_tokens` | 小值 `stop_reason=max_tokens`；超上限被规范化；缺失时自动补全策略补成默认值并正常响应，拒绝策略返回 400 `invalid_request_error`；JSON 无效或 max_tokens 非法时始终 400 | 流式与非流式一致；被拒请求能在 usage 里按 request id 查到。已知现象：以前入口直接报 `missing field max_tokens` 的 400，且 usage 页面查不到这条记录 |
| CC-P-21 | P1 | 新增（用户要求） | 基线 | curl 分别发送 `temperature` 0 / 1、`top_p` 0.5、`top_k` 10 | 采样参数 | 请求成功 | 记录是否透传或静默丢弃，丢弃需在文档说明 |
| CC-P-22 | P1 | 新增（用户要求） | 基线 | curl 发送 `stop_sequences: ["END"]`，prompt 要求输出 `A B END C` | `stop_sequences` | 理想为 `stop_reason=stop_sequence`、`stop_sequence=END` | 当前已知被忽略，记录实际行为 |
| CC-P-23 | P1 | 新增（用户要求） | 基线 | curl 分别发送 `tool_choice` = `auto` / `any` / `{"type":"tool","name":"x"}` / `none`；再加 `disable_parallel_tool_use:true` | `tool_choice` | 分别不强制 / 必须调工具 / 必须调指定工具 / 不调工具 | `any`/`tool` 靠提示词约束，统计命中率；`disable_parallel_tool_use:true` 时是否仍并行 |
| CC-P-24 | P2 | 既有场景 | 基线 | curl 发送 `metadata.user_id`、`service_tier`、`context_management`；再用 CLI 正常对话和工具调用各跑几轮（CLI 会带 temperature、context_management 等字段和 `anthropic-beta` 头），对比 CLI 发出的请求与代理发给上游的请求 | 未建模字段 | 请求成功，CLI 不出现 400，对话正常完成 | 不因未知字段 400；修复后被丢弃的字段至少在 warnings 或日志中有记录 |
| CC-P-25 | P1 | 既有场景 | 基线 | `stream:false` 与 `stream:true` 各跑同一 prompt：普通文本、含工具、含 thinking；同时用 CLI 发同一普通问答对照 | stream / non-stream | HTTP 200；内容结构一致 | 流式只有一个 `message_start`，内容块 start/delta/stop 顺序正确，最终 `message_delta` 的 usage 非零；非流式正确提取 thinking 块，重复的工具调用被去重。背景：这是所有其它场景的基线 |
| CC-P-26 | P2 | 新增（补充） | 基线 | curl 发送 `system` 为字符串 / 数组（含 `cache_control`） | system 格式 | 均成功 | 缓存标记不引起 400 |

## R 入口路由与服务端注入

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-Q-01 | P1 | 既有场景 | 各入口已启用；外部池未配置时跳过对应路径 | 把 `ANTHROPIC_BASE_URL` 分别指向 `/cc`、`/v1`、`/ha`、`/na` 以及外部池的 raw/normalized 路径，各跑 A-01、B-02、B-06 | 多入口 | 各入口都正常完成 | 缓存和 usage 上报行为符合该入口当前配置。背景：各入口共用本地转换语义，只在缓存和上报策略上有区别 |
| CC-Q-02 | P1 | 既有场景 | 基线 | 分别经 `/cc` 和 `/v1` 入口，先中文提问再切换英文提问；再给出需要工具的任务和要求"验证后告诉我结果"的任务；最后关闭提示词总开关或使用 strict profile 重跑 | 服务端提示词注入 | `/cc` 按用户最新消息语言回复并遵守任务质量规则；`/v1` 不注入这些内容；关闭总开关或 strict profile 后两者一致 | 对比回复语言、"声称已验证时是否给证据"、"需要工具时是否同一轮发 tool_use"。已知现象：代理只在 `/cc` 的 system 最前面插入语言约束和约 20 行中文任务质量规则，文档和响应都未披露，同一 CLI 接不同入口行为不同 |
