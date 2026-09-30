# 04 内置工具、权限与 hooks

前置：`$WORK` 是一个 `git init` 过的小项目（几个源文件、一个 README、一个 `.ipynb`、一个 PNG、一个 PDF）。非交互用例默认加 `--permission-mode bypassPermissions` 或 `--dangerously-skip-permissions`，权限类用例除外。

通用判定：每个 `tool_use` 有配对 `tool_result`；工具名在 CLI 侧是原名（`Bash`、`Read`...），不是 `xxxHash<8hex>` 这种哈希名（已知旧现象：代理曾把合法 PascalCase 工具名改写成“首字母小写 + Hash + 8 位十六进制”；预期：原名透传）；`input_json_delta` 拼接后是合法 JSON；第二轮请求正常完成；工具结果内容没被服务端改写（除历史截断策略外）。

说明：
- 「来源」列取值：既有场景（旧测试记录中已有的场景，已并入本分册）/ 新增（用户要求）/ 新增（补充）。
- 「前置」列写“默认”表示使用本文开头的通用前置；「参数」列写“默认”表示使用 `ccp`（见 01-environment-and-observability.md）的默认参数。
- 路由说明：代理有 `/cc` 入口（面向 Claude Code，默认开启部分工具提示词引导）和 `/v1` 等非 `/cc` 入口。涉及路由差异的用例在「前置」中写明。
- 部分场景真实模型难以稳定复现，需配合模拟上游或直连请求（curl/SDK）覆盖，已在「前置」中标明。
- U 组 35–59、H 组 10 为本次并入旧场景时新增的编号，未新开分组。

## U 内置工具

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-U-01 | P0 | 既有场景 | 默认 | `ccp --allowedTools Bash '用 Bash 执行 printf kiro-tool-ok，然后回复结果'`；等价变体：`claude -p --dangerously-skip-permissions --allowedTools Bash --model sonnet "Use Bash to print cli tool ok, then finish."`，以及 `claude --print --allowedTools Bash ...` 输出固定文本（如 tool-ok） | `--allowedTools Bash`；`--model sonnet`；`--output-format stream-json --verbose` | 输出含 `kiro-tool-ok`（或对应固定文本）；exit 0 | 第一轮返回 tool_use（name=Bash，`input_json_delta` 拼出对应命令），CLI 执行后回传 tool_result，第二轮基于工具结果完成；tool_use 与 tool_result 各 1 个且 ID 配对；`num_turns>=2`（典型为 2）；resultSubtype=success；stop_reason 先 `tool_use` 后 `end_turn`；usage 非 0。背景：最基础的 tool_use → tool_result 两回合链路，修改 reasoning/thinking 转换后也要回归 |
| CC-U-02 | P0 | 新增（用户要求） | 默认 | `Read` 读取 README，回答其中的 marker | 默认 | 回答正确 | tool_result 内容完整 |
| CC-U-03 | P0 | 新增（用户要求） | 默认 | `Edit` 修改一个函数名，再 `git diff` 确认 | 默认 | 文件被正确修改 | Edit 的 `old_string/new_string` 含多行、引号、制表符时参数不被破坏 |
| CC-U-04 | P0 | 既有场景 | 默认；分别指向 `/cc` 与 `/v1` 入口 | `Write` 新建 `hello.py` 并用 Bash 运行；再让它“创建一个 300 行的 demo.py”，统计 Write 与 Edit 次数和每次内容行数 | 默认 | 文件内容正确，运行成功；`/v1` 等非 `/cc` 入口 1 次 Write 写完整文件，与官方 API 行为一致 | 大文件写入（>20KB 或 >150 行）时是否被 `chunkedToolPolicy`/提示注入改成分块写，记录行为。已知现象：代理默认对非 strict 路由改写 Write/Edit 工具描述，要求超 150 行时先写 50 行、再每块最多 50 行用 Edit 续写，300 行文件变成约 6 次调用，token 和延迟成倍增加；预期：非 `/cc` 入口不做分块改写 |
| CC-U-05 | P1 | 既有场景 | 默认 | `Glob` 查找 `**/*.py`、`Grep` 搜索某字符串；另跑：`claude -p --model sonnet --output-format stream-json --verbose --allowedTools "Read,Grep,LS" "请搜索某目录中某逻辑，列出三个关键文件和原因。不要改文件。"` | `--allowedTools "Read,Grep,LS"` | 结果正确；多次调用 Grep/Read 后继续回答 | 空结果时模型正常说明；没有 `TOOL_USE_RESULT_MISMATCH` 或 `Expected toolResult`；没有重复的 tool_result、重复工具调用或重复最终段落；正文中没有 `<tool_use>`、`<invoke>` 这类 XML 泄漏。背景：工具调用历史配对被破坏时，上游会返回 400 Improperly formed request |
| CC-U-06 | P1 | 新增（用户要求） | 默认 | `TodoWrite`：要求"列出 4 步计划并逐步完成" | 默认 | todo 状态依次更新 | 多次 TodoWrite 的复杂嵌套数组参数正确 |
| CC-U-07 | P1 | 新增（用户要求） | 默认 | `NotebookEdit` 修改 `.ipynb` 某个 cell | 默认 | notebook 结构有效 | 参数中的 cell_id / edit_mode 正确 |
| CC-U-08 | P1 | 新增（用户要求） | 默认 | `WebFetch` 抓取一个公开页面并总结 | 默认 | 成功总结 | WebFetch 内部的小模型处理请求打到 haiku-4.5 且成功；页面大时 `webFetchTrimEnabled` 截断不导致异常 |
| CC-U-09 | P1 | 新增（用户要求） | 默认 | Bash 后台运行：`运行 sleep 20 && echo done 放到后台，然后先做别的，最后查看输出` | 默认 | 后台任务启动、查询输出、结束 | `run_in_background` 参数与后续读取输出/终止的工具调用成功 |
| CC-U-10 | P1 | 新增（补充） | `BASH_DEFAULT_TIMEOUT_MS=3000` | 执行 `sleep 10` | 默认 | 工具返回超时错误，模型继续 | 错误型 tool_result 回传后下一轮正常 |
| CC-U-11 | P1 | 既有场景 | 默认；打开请求体采样（见 01-environment-and-observability.md） | 工具返回非 0 退出码（`ls /not-exist` 或 `ls /nonexistent`） | `--allowedTools Bash` | 上游 200；模型识别工具失败并据此解释 | `is_error` tool_result 处理正确。已知现象：代理发出的错误工具结果除 `status=error` 外多带一个上游已知形状里没有的 `isError` 字段，上游若收紧校验会变成难以定位的 400；预期：去掉 `isError` 后仍 200 |
| CC-U-12 | P1 | 新增（用户要求） | 默认 | 计划模式下让它规划修改 | `--permission-mode plan` | 输出计划并调用退出计划模式的工具，不实际修改 | plan 工具的 tool_use 正常 |
| CC-U-13 | P2 | 既有场景 | 默认 | `AskUserQuestion` 类交互工具在 `-p` 下；交互模式下也触发一次向用户提问 | `--permission-prompts`（按 CLI 版本） | CLI 按 `--permission-prompts` 语义处理；CLI 不再报 `Invalid tool parameters` | 不导致会话卡死。背景：该工具输入曾不合规，已修复；它与上游返回的 `Invalid tool use format` 不是同一个问题，需分开记录 |
| CC-U-14 | P1 | 新增（补充） | 默认 | Git 工作流：让模型 `git add`、生成 commit message 并提交（在 $WORK 内） | 默认 | 提交成功 | commit message 生成可能走后台小模型，确认成功 |

## 并行与多轮工具

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-U-20 | P0 | 既有场景 | 默认 | `同时读取 a.txt、b.txt、c.txt 三个文件，然后比较内容`；并行多工具调用重复 5 次，记录每个 `content_block_start/stop` 的 index 顺序并对照实际执行顺序 | `--output-format stream-json --verbose --include-partial-messages` | 一个 assistant 消息中 3 个 tool_use | 3 个 tool_use 块 index 连续、各自有 `input_json_delta`；3 个 tool_result 在同一 user 消息回传；stop 按 index 升序且结果确定；块严格串行（上一块 stop 后才 start 下一块），执行顺序与 index 一致。已知现象：上游某工具缺 stop 片段时代理曾允许两个块同时打开，stop 顺序还随哈希表随机变化 |
| CC-U-21 | P1 | 新增（用户要求） | 默认 | 并行 10 个 Read | 默认 | 全部成功 | 并行块数较多时 id 不重复、不丢失；非流式不因“工具名 + input”去重丢调用 |
| CC-U-22 | P1 | 既有场景 | 默认；另需模拟上游覆盖 | 两次完全相同的并行调用：同一文件读两遍；或让模型“并行执行两次完全相同的 date 命令，不要合并”。流式与非流式各跑一次，统计 tool_use 块并查代理日志中的去重记录 | 流式：`--output-format stream-json`；非流式：直连非流式请求 | 两个调用都返回、都执行，两条路径数量一致 | toolUseId 不同但 name 和 input 相同的调用都保留；只有上游用同一 toolUseId 重发时才去重。已知现象：非流式曾按“工具名 + 规范化 input”去重，第二个合法调用被静默丢弃但仍计入 output tokens。真实模型难稳定复现，主要靠模拟上游覆盖 |
| CC-U-23 | P0 | 既有场景 | 默认 | 需要 15 步以上的工具循环（读、改、运行测试、修复）；其中既有一轮内并行调用多个工具，也有顺序执行读、写、改文件 | 默认 | 任务完成；文件操作结果正确 | 长工具循环中 usage 不长期为 0；历史 tool_result 截断后模型仍连贯；无 `Tool results provided.` 等占位文本泄漏；每个工具调用的 ID 与结果一一对应。背景：真实 CLI 长会话矩阵要求覆盖顺序、并行和文件操作 |
| CC-U-24 | P1 | 新增（补充） | 默认 | 并行工具中一个成功一个失败 | 默认 | 模型处理两者 | 混合结果回传正确 |

## 大工具结果

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-U-30 | P0 | 新增（补充） | 默认 | Bash 输出 500KB（`seq 1 100000`），让模型回答最后一行 | 默认 | CLI 自身截断或模型正确回答 | 服务端 payload guard 行为：当前轮工具结果默认不截断（`truncateCurrentToolResults=false`）；超长时走上游 too-long 流程（把错误交给 Claude Code，由客户端自行压缩），而不是静默丢弃 |
| CC-U-31 | P1 | 新增（补充） | 默认 | Read 一个 2MB 文件 | 默认 | CLI 分段读取 | 多次 Read offset/limit 正确 |
| CC-U-32 | P1 | 既有场景 | 默认 | 连续多轮调用 Bash/Read，历史中积累 10 个 60KB 工具结果后继续提问；再构造会触发 payload guard 裁剪的超长工具历史发请求 | 默认 | 成功；后续轮次正常 | 历史工具结果截断（`historicalToolResultMaxChars=8000`，头 80 行尾 40 行）后回答仍合理；tool_use/tool_result 没被污染，没有孤儿或重复；按完整逻辑回合裁剪，当前活跃的工具对保留；无法配对的孤儿 tool_result 删除，不猜名字补造 tool_use；万一还有异常残留就在发送前 fail closed。已知现象：曾出现“最新 assistant 的 tool_use 被裁掉，但当前 user 的 tool_result 还在”，把孤儿配对发到上游。背景：大工具输出最容易触发历史裁剪和配对问题 |
| CC-U-33 | P1 | 新增（补充） | 默认 | 工具结果含二进制/控制字符（`cat /bin/ls \| head -c 2000`） | 默认 | 不报错 | 转换时无 JSON 编码错误 |
| CC-U-34 | P2 | 新增（补充） | 默认 | 工具结果为空字符串 | 默认 | 成功 | 空 tool_result 不导致 400 |

## 工具名与工具调用形态

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-U-35 | P0 | 既有场景 | 默认；开启 debug 或请求体诊断 | 让 CLI 多轮调用 Read、Bash、Edit、Write、Grep/Glob、WebFetch、TodoWrite、Task，查看上游请求中的工具名 | `--debug` 或服务端请求体诊断 | 全部 200；上游工具名与原名一致，tool_use.name 正确 | 模型输出中没有“首字母小写 + Hash + 8 位十六进制”形式的工具名；日志不再为每个工具记录“已规范化/映射”。已知现象：旧行为把合法 PascalCase 名改写成哈希名，与 system prompt 中的“Use the Read tool”对不上，还留下明显代理指纹 |
| CC-U-36 | P1 | 既有场景 | 一个在旧版本代理下产生、文本中出现过哈希工具名的会话 | `--resume` 该会话，继续调用工具 | `--resume <session-id>` | 工具调用正常，无 400 | 旧映射名仍被识别清理。背景：修复后旧映射名只可能出现在历史文本泄漏里，服务端保留了对旧名的识别 |
| CC-U-37 | P1 | 既有场景 | 默认；非 thinking 模式 | 让 CLI 执行“读取三个文件”这类只调工具不输出文字的任务，看首个 `content_block_start`，并在会话记录中搜索空 text 块；再跑一次“先说明再调工具”和 thinking 模式对照 | `--output-format stream-json --verbose --include-partial-messages` | 首块是 index 0 的 tool_use，历史中无空 text 块；“文本加工具”时 text 仍在 index 0；thinking 模式行为不变 | 已知现象：非 thinking 模式下代理总预开 index 0 的空 text 块并写进历史，之后切到官方或严格上游可能触发 `text content blocks must be non-empty` 类 400 |
| CC-U-38 | P0 | 既有场景 | 默认 | 让模型调用带参数的工具（Bash、Read、Edit 等），流式和非流式都测 | 流式：`--output-format stream-json`；非流式：`--output-format json` | 客户端拿到的参数正确，客户端能正确执行工具 | 参数不会变成空对象 `{}`。已知现象：工具输入 JSON 解析失败时曾被静默替换成 `{}`，导致客户端执行失败或走错误的默认行为；预期：解析失败要显式报错而不是静默替换 |
| CC-U-39 | P2 | 既有场景 | 默认 | 观察工具参数很大的回合（如 Write 大文件、Edit 长 old_string） | `--output-format stream-json --verbose --include-partial-messages` | 参数的完整 key/value 形成前可能看不到 `input_json_delta`，之后集中出现，属正常 | 记录首个 `input_json_delta` 延迟与集中输出量；用户体感是“卡住，然后一坨输出”，只有超出超时或最终参数不完整才算问题 |
| CC-U-40 | P2 | 既有场景 | 配置名字带连字符、空格或超长的工具（如通过 MCP 或自定义工具） | 让 CLI 使用这些工具，并在一次请求里用 tool_choice 指定其中之一 | 请求体诊断 | 上游用安全化后的名字，CLI 看到的仍是原名 | 工具名映射在工具定义、历史、tool_choice 中保持一致，不丢失、不冲突 |
| CC-U-41 | P1 | 既有场景 | 模拟上游：既返回结构化工具调用，又在正文泄漏同一个 `<invoke name="Bash">` 文本（如 Bash ls） | 观察 CLI 执行次数和正文 | 默认 | 同一调用只执行 1 次，正文中不出现泄漏的 invoke 文本 | 按内容签名去重只用于“从正文恢复的泄漏调用与结构化调用撞车”；按 ID 去重改造后这层保护必须保留 |
| CC-U-42 | P1 | 既有场景 | 模拟上游：工具输入到流结束仍无 stop 片段（只发 usage 就 EOF） | 让 CLI 发起一次工具调用 | `--output-format stream-json --verbose` | 流结束时按 index 顺序补发 `input_json_delta` 并关闭块，CLI 能执行或给出明确错误，不卡住 | 背景：上游确实存在只发 usage 就 EOF、没有工具 stop 的情况 |
| CC-U-43 | P1 | 既有场景 | 默认；另需直连请求（curl/SDK）对比参数 | 让 CLI“同时读取 a.txt 和 b.txt”，统计单回合 tool_use 数；再用直连请求对比带 `disable_parallel_tool_use:true` 的请求 | `tool_choice.disable_parallel_tool_use`：未设置 / `true` | 未设置时可并行返回多个 tool_use；设为 `true` 时每回合最多 1 个 | 已知现象：上游无 toolChoice 字段，代理曾完全忽略该参数，只能逐个处理 tool_result 的编排器可能卡住或丢结果 |
| CC-U-44 | P1 | 既有场景 | 长会话，历史里出现被重复使用的 tool_use_id（可用直连请求构造） | 继续发送下一轮 | 默认 | 每次出现都按顺序正确配对，上游不报 Improperly formed request | 当前 tool_result 不被当成重复项丢掉或配错位置。已知现象：曾只按集合判断“已配对”，导致当前结果被丢弃，上游拒绝整个请求 |
| CC-U-45 | P0 | 既有场景 | 默认 | 让 Bash 输出一个唯一标记（如随机 UUID），再要求模型复述；direct 请求与 CLI 各跑一次 | `--allowedTools Bash` | 最终回复里有这个标记 | 已知现象：只含 tool_result 的轮次曾用“.”作为内容占位，上游会忽略工具结果；改成“Tool result received.”之后 direct 和 CLI 都能读到结果。同时确认该占位文本不泄漏到模型输出 |
| CC-U-46 | P0 | 既有场景 | 默认 | 只允许 Read、Grep、Glob、LS，让它读取并分析多个大源码文件 | `claude --bare --print --verbose --output-format stream-json --include-partial-messages --allowedTools "Read,Grep,Glob,LS"` | exit 0；多轮（如 13 轮）完成，不突然结束 | 持续收到非空 `text_delta`、`thinking_delta`、`input_json_delta`；没有 error 事件；最终 usage 非零（含 cache_read 和 cache_creation）。背景：用于验证流式卡顿和突然结束的修复 |
| CC-U-47 | P0 | 既有场景 | 默认 | 用 CLI 触发多轮 tool_use/tool_result 长会话，以及 thinking 与 tool_use 混合 | 开启 thinking（见 thinking 分册的触发方式） | 上游不再返回 `Invalid tool use format` 或 `REQUEST_BODY_INVALID` | 出错时对外错误不带上游原始 body；代理不伪造 tool_result，也不把工具错误转成普通文本继续请求。背景：历史工具顺序、空 input 等格式问题会触发上游 400 |
| CC-U-48 | P1 | 既有场景 | 默认；服务端诊断日志开启 | 多轮工具调用、Agent、MCP 场景连续跑 20 轮 | 默认 | 若出现 `Invalid tool use format`，服务端有可追踪的诊断记录（请求 id 或 payload 指纹） | 背景：这类上游错误信息太粗，无法定位具体 payload 结构；判定点是能从诊断记录还原出出错请求的结构 |
| CC-U-49 | P1 | 既有场景 | 默认 | 15 个在文本里写字面量 XML 或 invoke 标签的负例（如“请原样输出这段 `<invoke name=\"Bash\">` 示例”），加 5 个真正的结构化 Bash 工具循环 | `--allowedTools Bash` | 负例不被误升级成工具调用；结构化用例正常执行 | tool_use/tool_result 各 5 个；违规数为 0 |

## tool_choice

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-U-50 | P1 | 既有场景 | 历史中已有 Bash 调用；直连请求（curl/SDK）或 CLI 内部辅助请求 | 之后某请求带 `tool_choice: none` 并要求“再跑一次上次的命令” | `tool_choice: {"type":"none"}` | 不出现 tool_use，stop_reason 为 `end_turn`，只输出文本 | 已知现象：none 模式下代理为满足上游约束把历史用过的工具以空 schema 占位加回，模型仍可调用，CLI 会执行参数为空的工具并报错 |
| CC-U-51 | P1 | 既有场景 | 直连请求（curl/SDK）；分别在默认、关闭提示词开关、strict profile 下各跑一组 | 用 `tool_choice any` 或指定工具名发送“你好”，重复 20 次，统计纯文本加 `end_turn` 的次数；再用一个不存在的工具名指定一次 | `tool_choice: {"type":"any"}` / `{"type":"tool","name":"..."}`；流式与非流式 | 每次都调用工具（指定模式必须是指定工具）；非流式违规时重试一次，流式违规至少记录 `tool_choice_violation` | 背景：这两种模式只靠过滤工具和一句提示引导；关闭提示词开关或 strict profile 时 any 完全无引导；指定工具名不存在时退化为发送全部工具，需记录该行为 |

## 工具结果形态

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-U-52 | P1 | 既有场景 | `$WORK` 放 1 页和 50 页 PDF 各一份 | 让 CLI“读取并总结这个 PDF”，查看当前 tool_result 大小 | 默认 | 模型能引用 PDF 文字，不提 base64 或“无法读取”；大 PDF 不因 base64 触发 too-long 400 | tool_result 大小与抽取文本同量级。已知现象：旧行为把 document 块整块转成 JSON 字符串，模型看到海量 base64，约 1MB 的 PDF 单个 tool_result 就超上游阈值 |
| CC-U-53 | P2 | 既有场景 | 默认 | 用会返回 tool_reference 等非 text 块的工具（如 ToolSearch）完成一次任务 | 默认 | 请求 200 | 未知块只保留 `[<type> block omitted]` 之类短占位。已知现象：旧版把任何非 image、非 text 块原样转成 JSON 文本发给模型 |
| CC-U-54 | P1 | 既有场景 | 默认；MCP 场景需先配置一个测试 MCP server | 让工具分别返回普通文本、错误、空结果、图片或结构化内容，以及一轮多个 tool_result；内置工具与 MCP 工具各跑一次 | 默认 | 都能正确转发；错误结果正确回传 | 多个 tool_result 顺序与上一轮 tool_use 一致；Claude Code 内置工具和 MCP 场景都兼容 |

## 分块写入与提示词引导

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-U-55 | P1 | 既有场景 | CLI 指向 `/cc` 入口 | 让它写 300 行文件；另一轮明确要求“一次写完”或“用 Bash heredoc 写入” | 默认 | 仍可分块，但模型能说明原因；用户明确要求时不被静默压制 | 已知现象：注入文本要求“always comply silently”、不许询问或建议绕过，模型因此不解释分块也不按用户要求换方式 |
| CC-U-56 | P1 | 既有场景 | CLI 指向 `/cc` 入口（交互式） | 分块写大文件过程中按 Esc/Ctrl+C 中止，或触发压缩、网络错误，检查目标文件；另跑一次完整写入 | 默认 | 不留下半截且带占位符的文件；最后一块完成后占位符已删除 | 背景：分块依赖“唯一占位符 + 多次 Edit”，中途打断会留下残缺文件，忘删占位符会进入交付物 |
| CC-U-57 | P1 | 既有场景 | 关闭 promptSteering 总开关，或切到 strict profile | 在 `/cc` 与 `/v1` 路由各重复“写 300 行文件” | 服务端配置：promptSteering 关闭 / strict profile | 只有 1 次 Write | 所有路由都不注入分块、语言、任务质量提示；结构化 tool_choice 过滤不受影响。背景：这些提示词默认开启，只能整体关闭 |
| CC-U-58 | P2 | 既有场景 | 开启分块写入策略；让 Write/Edit 工具描述超过 10000 字符（如通过自定义 system/工具描述构造） | 让 CLI 写大文件 | 请求体诊断 | 截断后描述末尾仍完整保留代理注入的分块策略，模型按策略执行 | 已知现象：描述会被静默截到 10000 字符，以前在追加策略后才截，策略被截掉而悄悄失效；截断没有省略标记和告警，需记录是否已补告警 |

## 缓存

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-U-59 | P1 | 既有场景 | 默认；两次之间改动文件或时间 | 读文件、写文件、工具链、MCP、WebSearch 场景下连续发相同 prompt | 默认 | 每次都实际请求上游，不返回缓存的旧答案 | 服务端日志中每次都有上游请求；答案反映最新文件状态和时间。背景：全量响应缓存会返回过期的文件状态、时间或工具上下文，看起来成功但拿到的是旧答案 |

## H 权限模式与 hooks

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-H-01 | P1 | 新增（补充） | 交互式，默认权限 | 让模型写文件，界面弹权限询问，选"拒绝" | 默认 | 模型收到拒绝结果并调整 | 拒绝产生的 tool_result 回传正确 |
| CC-H-02 | P1 | 新增（用户要求） | 交互式 | 编辑文件 + 执行 Bash | `--permission-mode acceptEdits` | 编辑自动通过，Bash 仍询问 | 纯客户端，确认无异常请求 |
| CC-H-03 | P1 | 新增（用户要求） | `-p` 下 | 执行写文件 + Bash（各模式各一次） | `--permission-mode dontAsk` / `auto` / `manual` | 各模式按 CLI 语义执行 | `auto` 模式可能调用分类模型，确认该后台请求打到可用模型且成功 |
| CC-H-04 | P1 | 新增（补充） | settings `permissions.deny: ["Bash(rm *)"]` | 让模型执行 `rm a.txt` | 默认 | 被拒 | 同 H-01 |
| CC-H-05 | P1 | 新增（补充） | settings `hooks.PreToolUse` 对 Bash 返回 deny（exit 2 + stderr 原因） | 执行 Bash 任务 | 不加 `--bare`；`--include-hook-events` | 工具被阻止，原因回给模型 | `--include-hook-events` 记录事件 |
| CC-H-06 | P1 | 新增（补充） | `hooks.PostToolUse` 追加上下文 | 执行工具 | 默认 | 追加信息出现在下一轮请求 | 服务端正确转换 hook 注入内容 |
| CC-H-07 | P1 | 新增（补充） | `hooks.UserPromptSubmit` 注入额外上下文；`hooks.Stop` 要求继续一次 | 发一条消息 | 默认 | 注入生效；Stop hook 触发额外一轮 | 多出的轮次正常 |
| CC-H-08 | P2 | 新增（补充） | `hooks.SessionStart`、`hooks.PreCompact` | 新会话/手动 `/compact` | 默认 | hook 执行 | 与 CC-L 组联动 |
| CC-H-09 | P2 | 新增（补充） | `statusLine` 配置显示模型与 token | 交互式使用若干轮 | 默认 | 状态栏数值随 usage 更新 | usage 不为 0 |
| CC-H-10 | P1 | 既有场景 | 不显式放行工具（不加 `--allowedTools`、不跳过权限）；另跑一个 Agent 场景 | 直接运行需要工具的任务；再用 `--allowedTools` 放行重跑；Agent 场景中观察工具名 | 无 `--allowedTools` / 有 `--allowedTools` | 未放行的工具被 CLI 权限层拦截，不算协议失败；放行后成功 | Agent 先调用小写的 bash/read/glob 等不存在的工具名、随后改用正确名称恢复，不算代理错误。背景：用来区分 CLI 自身行为和代理协议问题 |
