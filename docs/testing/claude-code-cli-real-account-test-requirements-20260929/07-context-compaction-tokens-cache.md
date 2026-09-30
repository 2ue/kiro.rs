# 07 长上下文、压缩、count_tokens、缓存与 usage

本组消耗额度最多，放在最后执行，每个用例限制轮数和块大小，必要时只选 sonnet 跑完整版、haiku 跑精简版。免费 plan 只能调度 claude-sonnet-4-5 与 claude-haiku-4-5，涉及 opus 或真实 1M 窗口的场景只记录“当前账号不可测”，不视为失败。

本分册沿用 L / K / R 三组编号，未新开组。「来源」列含义：既有场景 = 旧测试文档中已有的场景（已并入或新写）；新增（用户要求）= 与用户原始要求直接对应；新增（补充）= 为覆盖完整而补充。

## 背景

- 输入过长的对外错误：Kiro 判定输入过长时，代理对外返回 Anthropic 协议格式的 400，message 以 `prompt is too long: N tokens > M maximum` 开头，且不带“contact the administrator with error ID”之类后缀。Claude Code CLI 只有收到 message 含 `prompt is too long` 或 `input is too long for requested model` 的 400 时才会被动压缩，并用正则解析 N 与 M。已知旧现象：代理曾返回 “Request input content length exceeded...” 或 “Context window is full...”，CLI 当普通 API Error 处理，每轮失败、从不压缩，会话卡死。
- 过长处理配置 `payloadTooLongHandling`，默认 `client_compaction`，所有路由一致：
  - 上游 too-long 时不再静默裁剪重试，直接返回上述 400，由 CLI 自动压缩后重试；
  - 例外：Claude Code 自己的压缩请求（最后一条 user 含 `create a detailed summary of the conversation`）仍走服务端裁剪重试，允许截断当前轮 tool_result，并把加权上限封顶为模型窗口的 90%；
  - too-long 重试请求的 requested model 从请求 payload 推导，而不是沿用路由默认值；
  - `trim_retry` 保留旧行为（静默裁剪历史后重试并返回 200）。
- 服务端主动压缩信号曾经上线（usage 达到窗口 80% 时提前返回 prompt is too long），会覆盖客户端的窗口配置，已撤销。当前 usage 字段不因压缩逻辑而改变。
- CLI 2.1.283：默认不做主动压缩检查，只靠被动 400；配置 `autoCompactWindow` / `CLAUDE_CODE_AUTO_COMPACT_WINDOW` / `--autocompact <tokens>` 时按 usage 主动压缩；`autoCompactEnabled=false` / `DISABLE_AUTO_COMPACT` / `DISABLE_COMPACT` 时收到 400 也不压缩。
- 主动压缩阈值 = min(配置窗口, 模型窗口) − min(max_output, 20k) − 13k，可用 `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` 调整；已用量取 usage 中 input、cache_creation、cache_read 之和。上下文窗口完全由客户端判定（普通模型名按内置表 200k，带 `[1m]` 后缀或 context-1m beta 按 1M），服务端收不到客户端窗口配置。
- `/cc` 路由的 usage 按 `reportedUsage.pathOverrides["/cc"]` 整形（放大），客户端配置主动窗口时会比设定值更早压缩，这是已知且可接受的行为。
- 两套独立限制：上游对请求体字节有阈值（`CONTENT_LENGTH_EXCEEDS_THRESHOLD`，“Input is too long.”，约 754KB 的请求体会被拒，此时 CLI 可能显示上下文只用了约 13%），与 token 上下文窗口不是一回事。代理本地的请求体保护（payload guard）按加权长度计算：ASCII 计 1、非 ASCII 计 8，默认上限约 1,300,000。

增长会话脚本（可复用，`TURNS`、`CHUNK` 可调），示意：

```bash
# 每轮生成 CHUNK 字节带 marker 的文本，用 -c 续聊 TURNS 轮
TURNS=${TURNS:-10}; CHUNK=${CHUNK:-40000}
for i in $(seq 1 "$TURNS"); do
  body=$(head -c "$CHUNK" /dev/zero | tr '\0' 'x')
  flag=$([ "$i" -eq 1 ] && echo "" || echo "-c")
  claude $flag -p "MARKER-$i: 记住这个标记。$body 只回复 ok-$i" --model claude-sonnet-4-5 --debug
done
```

## L 长上下文与压缩

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-L-01 | P0 | 既有场景 | 基线 `client_compaction`，CLI 默认（自动压缩开启、不设压缩窗口），打开 CLI debug log | 逐步增长：sonnet-4.5 每轮加入约 40KB，10 轮（`-c` 续聊）；或不断 Read 大文件 | 渐进增长 | 10 轮全部成功，约第 7 轮自动压缩（一次或多次）；CLI 先收到 400，message 以 `prompt is too long: N tokens > M maximum` 开头，无“contact the administrator with error ID”后缀，压缩后自动重试成功 | debug log 有 `trigger=auto`；400 文案匹配 `prompt is too long[^0-9]*(\d+) tokens? > (\d+)`；服务端 usage 记录的公开错误与响应一致；压缩后回答仍能引用关键 marker。已知旧现象：返回“Request input content length exceeded...”时 CLI 从不压缩，会话卡死 |
| CC-L-02 | P0 | 既有场景 | 同上 | 大幅增长：每轮约 150KB，4 轮 | 快速增长 | 第 2–4 轮都是先收到 prompt is too long、自动压缩（如约 158,749 降到 38,947），再返回正确结果 | 不出现“从第 2 轮起每轮都报错、会话卡死”（旧行为） |
| CC-L-03 | P0 | 既有场景 | 同上 | 单轮工具循环中途超长：让模型一轮内并行 Read 10 个约 60KB 的文件，重复 2 次 | 当前轮工具结果过大 | 中途超限后自动压缩并继续，10 次工具调用全部完成且答案正确 | 压缩请求会把当前轮工具结果放在当前消息里，代理需走裁剪重试成功，不出现 `exhausted`。已知旧现象：读到第 3 个文件后报错、工具调度中断 |
| CC-L-04 | P0 | 既有场景 | settings `autoCompactEnabled=false`（或环境变量 `DISABLE_AUTO_COMPACT` / `DISABLE_COMPACT`，逐一测） | 同 L-02 让上下文超限，再手动 `/compact` | 关闭自动压缩 | 超限时 CLI 显示 `Prompt is too long` 并提示 `/compact`，不自动压缩；手动 `/compact` 后会话可继续 | 与官方行为一致，服务端不替客户端决定压缩 |
| CC-L-05 | P1 | 既有场景 | `CLAUDE_CODE_AUTO_COMPACT_WINDOW=100000`、settings `autoCompactWindow` 或 `--autocompact 100k`（分别测） | 每轮 30KB，连续 6 轮增长会话，查看 debug log | 客户端主动窗口；可叠加 `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` | 日志显示 effectiveWindow=80000、阈值 67000，按 usage 主动压缩，6 轮全部成功，无 400 | 记录压缩发生的轮次，与整形后的 usage 对照；usage 被放大只会让客户端更早压缩，可接受 |
| CC-L-06 | P1 | 新增（用户要求） | 交互式 | 手动 `/compact`、`/compact 只保留关于 X 的信息` | 手动压缩 | 压缩成功，后续对话连贯 | 压缩请求本身成功；自定义指令生效 |
| CC-L-07 | P1 | 既有场景 | 运行配置 `payloadTooLongHandling=trim_retry`（或预先裁剪模式；备份/恢复配置） | 同 L-02 重复增长会话，长历史里带大工具结果；之后问早期对话细节 | 旧行为对照 | 请求成功（200）但上下文被静默裁剪；上游 too-long 后正好 2 次推理请求，重试请求体大幅缩小（如约 554KiB 降到约 37KiB）；tool_use/tool_result 无孤立项；无内部 transcript 标记 | 记录与 `client_compaction` 的差异，确认切换可用。已知现象：裁剪后返回 200 无任何标记，CLI 不读自定义警告头、感知不到上下文已满、不压缩，模型遗忘早期内容；usage 按裁剪后上下文反推，可能低估 CLI 压缩阈值使裁剪状态持续。目标（未修）：对话内有省略标记，或提供直接拒绝模式 |
| CC-L-08 | P1 | 既有场景 | 基线 | 新会话首轮就发送超出窗口的单条消息（如 sonnet 读入约 900KB 纯 ASCII 文本） | 首轮超长 | 返回 `prompt is too long: N tokens > M maximum`（如 225933 > 200000）；首轮无可压缩历史，CLI 给出清晰错误、不会恢复 | 与官方一致，不算缺陷；不在服务端无限重试。上游阈值更接近按 token 计的上下文上限，ASCII 大请求靠体积阈值裁剪基本救不回来。原场景中的 1M opus 读入约 1.15MB 直接 200 的对照，免费账号不可测，1M 相关见 L-11 |
| CC-L-09 | P1 | 既有场景 | 基线；另在 `trim_retry` 模式下，在 system 或 CLAUDE.md 写口令或规则（如回复末尾带口令、固定用中文） | ① 超长 system（`--append-system-prompt` 200KB）+ 正常消息；② 裁剪模式下把会话拉到超限后问口令 | system 过大 / 裁剪保留 system | ① 清晰错误或成功；② 裁剪后仍能说出口令并遵守 system 约束 | system 转成的合成对不被最先删除，只有其余可删轮次删完仍超限时才删。已知旧现象：从最旧轮开始裁剪，最先删掉 system 合成对，长会话后半段模型突然不守项目规则；已有约 4.5MB、45 轮历史裁剪后仍答对口令的验证 |
| CC-L-10 | P1 | 新增（用户要求） | haiku-4.5 | 同 L-01 精简版（5 轮） | haiku 窗口 | 成功压缩 | haiku 窗口与 sonnet 相同为 200k，服务端上限按模型取 |
| CC-L-11 | P1 | 既有场景 | `--model 'claude-sonnet-4-5[1m]'`（或 context-1m beta）；对照组用普通模型名 | 每轮 150KB，3 轮；检查响应 `model` 字段和 CLI 开始 compact 的时机 | 客户端认为 1M 窗口 | 普通模型名按 200k、带 `[1m]` 按 1M 判定，压缩时机随之变化；上游真实上限触发时仍能被动压缩；响应 `model` 保留 `[1m]` 之类 CLI 可识别的标识，CLI 不在约 160k 就提前 compact | 服务端不下发窗口；客户端/服务端窗口认知不一致时不卡死；标识缺失时 CLI 会按 200k 过早压缩。代理不应故意夸大 count_tokens，也不应返回模糊错误诱导 compact。若账号不支持 1M，记录为不可测 |
| CC-L-12 | P1 | 既有场景 | 压缩后（`/compact` 或自动压缩），有 system 与无 system 各一次 | 问“刚才提到的文件和行号是什么”；继续工具调用、thinking、MCP 调用各一次；记录触发压缩时的 token 水位 | 压缩后能力 | 返回 200，模型能复述摘要内容；工具、thinking、MCP 全部正常；工具配对和语言跟随不出问题 | 压缩后首条消息是 assistant 摘要，代理应在其前插入占位 user，保证 user 开头且严格交替，不导致 400。已知旧现象：无 system 时首条 assistant 被静默删除，有 system 时出现连续两条 assistant |
| CC-L-13 | P1 | 新增（用户要求） | 压缩后 | `-c` / `--resume` 继续 | 压缩后续聊 | 成功 | 会话文件中压缩边界正确 |
| CC-L-14 | P1 | 新增（补充） | 基线 | 历史中有大图片（多张 1MB PNG）的长会话 | 图片占上下文 | 超限时按 too-long 流程处理 | `oversizedImageHandling=drop-with-placeholder`、`currentImagesMaxBytes` 行为记录 |
| CC-L-15 | P2 | 新增（补充） | `hooks.PreCompact` | 触发自动压缩 | hook | hook 执行后压缩成功 | 联动 CC-H-08 |
| CC-L-16 | P0 | 既有场景 | 基线；会话已接近上限 | 执行 `/compact` 或等自动压缩，使压缩请求本身超过上游上限 | 压缩请求自身超长 | 第一次 400 后代理按模型窗口上限裁剪（含截断当前轮工具结果，加权上限封顶为窗口的 90%）再重试，返回 200 并生成摘要 | 服务端记录显示该请求被识别为压缩请求（最后一条 user 含 `create a detailed summary of the conversation`）。已知旧现象：默认上限远高于上游真实 token 上限，重试后仍超长 |
| CC-L-17 | P0 | 既有场景 | 默认配置 | 触发一次上游 too-long（普通请求），检查服务端记录；在各路由各测一次 | 默认不静默裁剪 | 普通请求直接收到 prompt is too long，服务端无“先 400 再裁剪重试并返回 200”的记录，所有路由表现一致 | 旧行为：静默删历史重试并返回 200，CLI 不知道也不压缩，下一轮又带着完整超长历史来，每轮多一次失败的上游调用 |
| CC-L-18 | P1 | 既有场景 | 默认配置 | 让 usage 显示很高（如 19 万甚至 98 万 token）但上游未真实超限，继续对话 | 服务端不主动发压缩信号 | CLI 不压缩，也不收到 prompt is too long；只有上游真实超限才返回该 400 | 曾有版本在达到窗口 80% 时提前返回 prompt is too long，覆盖了客户端窗口配置，已撤销，需确认不回归 |
| CC-L-19 | P1 | 既有场景 | 长会话推到接近 100% 上下文 | 同时让模型发起 Read 等工具调用，查看 `stop_reason` 与工具是否执行；再做一个纯文本回合对照 | 上下文占满 + tool_use | 本轮已输出 tool_use 时 `stop_reason` 保持 `tool_use`，CLI 正常执行工具并继续；纯文本回合才报 `model_context_window_exceeded` | 已知旧现象：上游上下文占用达到 100（含 99.995 四舍五入）会强制改 stop_reason 并覆盖 tool_use，CLI 可能不执行工具或提示上下文已满 |
| CC-L-20 | P2 | 既有场景 | CLI 开启 context-management beta | 跑长会话产生大量工具结果，对比 CLI 请求与代理上游请求中 tool_result 总字节数，观察压缩与“输入过长”出现时机 | `context_management`（如 `clear_tool_uses_20250919`） | 现状两者相等（未清理），会更早触发超长；修复后（开关默认关闭、灰度开启）旧工具结果替换为中性占位，tool_use/tool_result 严格配对，响应带 `applied_edits`，400 不增加 | 已知现象：本地路径整个丢弃 `context_management`，依赖服务端清理的长会话更快撑满；记录当前是否已修复 |

### L（续）请求体保护、裁剪与错误分类

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-L-21 | P1 | 既有场景 | 基线 | 持续积累历史和大工具结果，直到超过本地请求体阈值，观察服务端日志和 CLI | 长会话触发请求体保护 | 先裁剪最早的历史，裁剪后 tool_use/tool_result 配对完整，CLI 继续流式输出并完成，对话仍理解当前任务；仍超限时返回上游真实错误，不在本地静默截断当前输入 | 已知现象：上游以 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`（“Input is too long.”）拒绝约 754KB 的请求体，而 CLI 显示上下文只用了约 13%，这是字节阈值而非 token 窗口；修过头会破坏工具调用连续性 |
| CC-L-22 | P1 | 既有场景 | 基线 | 构造加权大小接近上限（ASCII 计 1、非 ASCII 计 8，默认约 1,300,000）的会话，分别让它刚好不触发和刚好触发 payload guard；中文为主与 ASCII 为主各一组 | guard 边界 | 两种情况下 CLI 都能继续；服务端报告里有计算口径、原始和最终权重、上限等字段 | 中文等非 ASCII 内容多的会话更容易超过上游真实阈值，本地与上游算法要一致 |
| CC-L-23 | P1 | 既有场景 | 基线 | 构造超大当前输入（大 Read、大网页、大 PDF、大图、大工具结果，约 420K 以上） | 请求体超阈值 | 返回明确的“请求内容长度超过阈值”类 `invalid_request_error`，说明与上下文窗口无关，而不是泛化的 Improperly formed request；不换凭据、不重复发送同一请求体；上下文用量到 100% 时才映射为 `model_context_window_exceeded` | 请求字节上限与上下文 token 上限是两套独立限制；换账号不会改变请求体，只会制造额外失败 |
| CC-L-24 | P1 | 既有场景 | 带大量历史签名 thinking 的超长会话（上千条历史、数 MB 请求体） | 继续提问，观察上游尝试次数与错误记录 | 签名无效后又超长 | 第一次 `THINKING_SIGNATURE_INVALID` 后去掉历史推理重发；重发体基于剥离后的请求体重建并走 payload guard 裁剪；仍超限则按统一的输入过长分类返回（能让 Claude Code 正确处理，如触发压缩），不打第三次上游；错误记录同时展示首次原因（签名无效）和最终原因（超长）；同一坏请求体短时间内不被反复放大发送 | 已知现象：同一条记录里两次上游尝试，第二次报 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`，但被包在签名重试分支里、笼统报成 invalid_request 或签名重试，没有经过统一的过长分类和出站准入，CLI 同一请求反复失败 |
| CC-L-25 | P2 | 既有场景 | 长历史里有大工具结果或病态混合内容 | 检查发往上游的文本（服务端请求日志） | 历史回放占位符 | 不出现 `[previous output]`、`[trimmed output]`、`[duplicate output]` 这类占位；只有工具结果的消息用 `.` 作占位；被拒绝的工具结果不被转成文本 | 防止裁剪或修复逻辑把内部占位带进上下文 |

## K count_tokens 与 /context

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-K-01 | P1 | 新增（补充） | 交互式 | `/context` | 无 | 显示上下文占用分解 | CLI 是否调用 `/cc/v1/messages/count_tokens`；返回 200 且数值合理，不为 0 或明显离谱 |
| CC-K-02 | P1 | 既有场景 | 基线 | curl `POST /cc/v1/messages/count_tokens`（纯文本 / 含工具 / 含图片 / 含 thinking 历史）；用 `/context` 查看占用，再与下一次请求的 `usage.input_tokens` 对比；碎片多的会话与大块文本会话各测一次；同样对话在 `/cc` 与 `/v1` 各测一次 | 口径一致性 | 估算接近真实值，不故意夸大；修复后两者用同一套估算，差值合理（有校准时 15% 以内）；`/cc` 计数大于 `/v1`，差值来自注入的提示词 | messages 与 count_tokens 使用同一份注入后 payload；CLI 依靠该接口判断何时 compact。已知现象：count_tokens 用“字符数/4 × 分段系数”，逐片段相乘且不单调（396 字符算 148，400 字符反而 130），碎片会话被高估、大块文本被低估；messages 路径有上游百分比时改用“百分比 × 窗口” |
| CC-K-03 | P2 | 既有场景 | 基线 | 观察 CLI 发出的 count_tokens 请求；分别对 `/v1`、`/na/v1`、`/ha/v1` 调 count_tokens | 各入口 | 各入口都能正确返回 token 计数，不报错 | 各内置路由上的本地 count_tokens 行为一致；与 `/cc` 差异符合路由策略 |
| CC-K-04 | P2 | 既有场景 | 配置了远程计数 `countTokensApiUrl`（若有），并让该服务延迟 10 秒响应 | CLI 并发发起多个对话，观察首包与健康检查延迟，并查看该服务收到的内容 | 远程 token 计数 | 修复后远程计数只用于 count_tokens 端点，秒级超时后回退本地估算不报错，推理请求不外发 prompt | 已知现象：推理热路径被同步阻塞（发送与读 body 各最长 300 秒、每次新建连接），每个请求额外外发完整 prompt 1 到 3 次；涉及隐私，需记录现状 |
| CC-K-05 | P1 | 新增（补充） | 交互式 | `/cost` 与 `/status` | 无 | 显示费用与模型 | 与服务端 usage 记录一致；模型显示为 sonnet-4.5/haiku-4.5 |
| CC-K-06 | P2 | 既有场景 | 基线 | 让 count_tokens 收到缺 Content-Type、JSON 语法错误或字段错误的 body；观察 CLI token 计数是否降级且不崩溃 | 请求体异常 | 修复后统一返回 400 `invalid_request_error` JSON 信封 | 已知现象：返回纯文本的 415/400/422，与 messages 接口不一致 |

## R Prompt caching 与 usage

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-R-01 | P0 | 既有场景 | 基线；`--output-format stream-json --verbose` | 先用固定 `--session-id` 跑首轮，再用 `-c` / `--resume` 续接相同长前缀（另测自定义 Agent 模式）；查看 `message_start.message.usage` 和最终 `message_delta.usage` | 同会话缓存 | 首轮只有 `cache_creation_input_tokens` 大于 0、`cache_read_input_tokens` 为 0；续接轮 `cache_read_input_tokens` 非零（实测如 8802），按 `/cc` 策略报告；升级到“工具名透传、丢历史 thinking”的版本后，进行中会话首轮允许一次缓存 miss，之后恢复命中 | usage 字段齐全（input、output、cache_creation、cache_read），不互相矛盾；`message_start` 是估算、`message_delta` 是最终值；只有真的发生缓存读写时才体现，CLI 最终 usage 不为 0。已知旧问题：usage 口径转换曾把首轮 input 的差值计入 cache_read，首轮未命中也显示读了缓存；工具名和 tools 数组字节变化会让前缀缓存失效一轮 |
| CC-R-02 | P1 | 新增（用户要求） | `DISABLE_PROMPT_CACHING=1` | 普通对话 | 关闭缓存 | 请求中没有 `cache_control` | 服务端 usage 报告仍合理 |
| CC-R-03 | P1 | 新增（补充） | 基线 | 同一 prompt 分别打 `/cc`、`/v1`、`/na`、`/ha`（改 `CC_BASE`） | 路由对比 | 都成功 | `/na` 使用原始 usage 不做缓存模拟；其它路由按各自策略；记录 4 条 usage 对比 |
| CC-R-04 | P1 | 既有场景 | 基线；stream-json | 长工具循环（CC-U-23）中观察每轮 assistant 的 usage：`message_start`、assistant partial message、最终 `message_delta.usage` 与 `result.usage` | 中间事件与最终 usage | 中间事件可能显示 `output_tokens=0`，但最终 `message_delta.usage` 与 `result.usage` 必须非 0，不长期为 0 | 长时间显示 0 tokens 若发生在最终结果之前属于 CLI 聚合时机；最终 `result.usage` 仍为 0 才说明代理漏发 usage |
| CC-R-05 | P1 | 既有场景 | 基线 | 每一轮检查最终 `message_delta.usage`，并与 Admin usage 记录对照；包含 fallback 或重试路径 | 服务端记录 | 每个 request id 都有记录；输入和输出 token 非零且合理，与服务端记录一致；fallback 或重试路径下不重复计、不丢失 | raw usage 与 reported usage 都在，writer 无丢弃（`/api/admin/usage-writer-stats`）；终态 usage 事件是客户端视角的权威值 |
| CC-R-06 | P2 | 既有场景 | 运行配置开启 cachePoint 试验能力（`kiroCachePointEnabled`，仅工具）；记录当前默认值（旧场景对默认开启/关闭的描述不一致，以实际配置为准） | 发送带工具 `cache_control` 的请求，检查上游请求；再模拟上游拒绝 cachePoint | cachePoint 注入与回退 | 上游请求中插入 cachePoint，下游仍收到正常 tool-use 流；上游拒绝时自动去掉 cachePoint 只重试一次并成功 | 下游不暴露第一次上游原始错误；未开启时不改变上游请求体；上线前需经过长会话、工具调用和 CLI 真实验证 |
| CC-R-07 | P1 | 既有场景 | 基线 | 压缩前后各轮查看 `message_start` / `message_delta` 中的 usage | 压缩前后 usage | usage 与计费记录一致，不被压缩逻辑改动 | usage 是按路由整形的计费字段，压缩方案不能改它 |
| CC-R-08 | P1 | 既有场景 | 基线 | 连续多轮相同前缀的会话，每轮导出 `cache_creation_input_tokens` 和 `cache_read_input_tokens` | 缓存字段语义 | 没有前缀时只报 creation，不凭空报 read；前缀命中后才报 read；上游有真实缓存元数据时以上游为准；input 和缓存字段不异常超过 1M | 已知线上现象：input_tokens、cache_creation_input_tokens、cache_read_input_tokens 曾超过 1M，后来加了 cache_creation 上限等保护，需确认不回归 |
| CC-R-09 | P2 | 既有场景 | 基线 | 用不同会话、不同模型（sonnet 与 haiku；可用时 sonnet 与 sonnet[1m]）、切换上游账号分别测缓存命中 | 缓存隔离 | 按当前策略：不同会话不共享，跨模型或跨凭据不共享；若改为“只按会话”策略，则同会话跨凭据、跨模型也应命中；每轮变化的计费 header 文本不导致未命中 | 官方规则里模型是缓存 key 的一部分，“只按会话”是产品策略而非完整复刻官方行为；记录实际采用的策略 |
| CC-R-10 | P1 | 既有场景 | 基线 | 让某一轮在流中失败或被中断（如 Ctrl+C），下一轮用同样前缀请求 | 失败不提交缓存 | 失败那轮不更新缓存状态，下一轮缓存 usage 不受失败轮影响；失败记录里标准 usage 字段为 0 | 只有成功完成的请求才能更新缓存 tracker |
| CC-R-11 | P2 | 既有场景 | haiku-4.5 | 发低于 4096 token 的请求（Haiku 3.5 为 2048，免费账号不可测，仅作规则说明） | 最小缓存长度 | 本地不模拟 cache creation/read | 本地缓存模拟按实际上游模型的最小可缓存长度判断 |
