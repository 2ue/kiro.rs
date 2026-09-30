# 03 Thinking 全组合

本组编号统一为 CC-T-<序号>。CC-T-01～CC-T-35 为原有用例，编号不变；CC-T-36～CC-T-55 为吸收既有场景后新增的用例，按主题放在后面的小节中。

「来源」列取值：既有场景（历史测试中已出现过的场景，本次吸收）/ 新增（用户要求）/ 新增（补充）。

## 背景与判定口径

执行前先理解当前实现，否则会把正常行为误判成问题：

- Claude Code 默认每个请求都带 `thinking: {type: adaptive}` 和 `output_config.effort`（2.1.156 实测默认 `effort=high`；2.1.283 需在 CC-T-01 重新确认）。关键词 `think` / `think hard` / `ultrathink` 历史实测不改变请求体，只是 prompt 文本。CLI 没有"不思考"参数；只有 `--effort` 改 `output_config.effort`，只有显式 thinking 模型名（如 `--model sonnet-thinking`）改请求模型名。
- kiro-rs 服务端在 `thinkingTriggerMode=real_request`（默认）下：
  - 普通 `adaptive`（含各档 effort、含 prompt 关键词）：不强制输出可见 thinking。原因：若所有 adaptive 请求都强制可见 thinking，普通请求会多出 thinking 块和输出 token 成本，adaptive 只作为兼容控制；
  - `thinking.type=enabled`（如 `MAX_THINKING_TOKENS`、`alwaysThinkingEnabled` 可能产生）或模型名带 `-thinking`：按可见 thinking 处理；
  - `thinking.type=disabled`：即使模型名带 `-thinking` 也不输出 thinking。
- 上游不接受带 `-thinking` 后缀的 modelId（返回 `INVALID_MODEL_ID`），服务端把它映射到基础模型，再注入 thinking 控制（提示词让上游输出 `<thinking>`，服务端再转换）。
- sonnet-4.5 / haiku-4.5 不在服务端内置的"原生 reasoning 能力"清单里（该清单只列 opus-4.6/4.7/4.8、sonnet-4.6）。如果模型目录同步（`ListAvailableModels`）没有给出原生能力，thinking 走 XML 提示路径：上游输出 `<thinking>...</thinking>`，服务端转成 `thinking_delta`，**没有 `signature_delta`**，这是预期行为（从文本提取的 thinking 本就无签名）。执行时先在 [01 预检](01-environment-and-observability.md#2-账号与运行配置预检) 记录 native reasoning 能力状态，据此判断签名类用例是否适用。历史记录中 sonnet-4.5 账号只返回无签名 XML thinking，原生签名路径仅被伪造签名和单元测试覆盖过。
- 本次测试账号为免费 plan，只能调度 claude-sonnet-4-5 和 claude-haiku-4-5。既有场景里原本用 Opus 或 sonnet-4.6 执行的，统一改用 sonnet-4.5（必要时再用 haiku-4.5）；依赖原生签名的用例在能力不可用时记为"不适用"并注明原因，不算失败。
- 已知现象：`claude --model sonnet-thinking` 曾在基线和候选版本都没有 thinking block（修复前后版本都如此，原因待查，不算回归）；同一别名在模拟上游下则能产出 thinking。本组 CC-T-10 需要重新确认并给出结论。
- 默认配置下服务端会在转换前丢弃非受保护的历史 thinking（配置项 `discardHistoricalThinking`，默认开启）；受保护的"当前工具续写 assistant"保留原生签名；无签名 thinking 回传时退化成文本。
- 上游每条 assistant 消息只能放一个 reasoning 内容；服务端内部也只有单个 reasoning 表示。多块、交错、签名/无签名混用的历史都需要服务端做取舍，这是本组签名类用例的重点。

**thinking 通过的唯一证据**：`stream-json` 中出现 `content_block_start(type=thinking)` 与 `thinking_delta`，或最终 usage 有 `output_tokens_details.thinking_tokens > 0` 且流里确实出现 thinking。prompt 中含 think、模型文字说"我想了想"、请求返回 200 都不算。

每条用例都要抓包或看服务日志，记录客户端实际发出的 `thinking` 与 `output_config` 字段。

## 主动触发

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-T-01 | P0 | 既有场景 | 基线；准备一次性抓包端口（或服务端请求体采样）记录 CLI 入站请求体 | `ccp '17*23 等于多少？'`，sonnet 与 haiku 各一次。另外按入站形态矩阵抓请求体：不带 `--effort`、`--effort low/medium/high/xhigh/max` 各 5 轮；prompt 写 `think`、`think hard`、`ultrathink`；`--model sonnet-thinking`、`--model opus-thinking`（后者只看请求体，不要求成功） | 默认（adaptive）；effort 各档；关键词；thinking 模型名 | 正常回答；无可见 thinking 符合 real_request 语义。请求体：`thinking` 始终为 `{"type":"adaptive"}`；不带 `--effort` 时默认 `effort=high`；`max` 原样发送，不被压成 `high`；prompt 关键词不改模型名也不改 effort；只有 `--effort` 改 `output_config.effort`，只有 `--model` 改模型名 | 记录请求体 `thinking`/`output_config`；无 thinking 标签泄漏到正文；不出现 `output_config is only compatible with adaptive thinking or an omitted thinking field` 的 400（用户曾遇到，怀疑 effort 被压成 high 或 thinking 被错误保留为 disabled）。本用例用于确认哪些 CLI 操作真正改变请求体，避免把 prompt 关键词误当成 thinking 开关 |
| CC-T-02 | P0 | 既有场景 | 基线 | `ccp 'think: 证明根号 2 是无理数'`、`'think hard: ...'`、`'think harder: ...'`、`'megathink: ...'`、`'ultrathink: ...'`；再对照跑显式 thinking 模型（`--model sonnet-thinking`）与直连 `thinking.type=enabled` | 关键词 | 均成功 | 对比 5 次请求体是否有差异（budget/effort）；若 CLI 因关键词改变字段，则应出现 thinking；若不改变，记录为客户端行为。已知现象：CLI 显示用了 ultrathink，但整轮没有任何 thinking 内容。归因时逐项检查请求体的 model/thinking/effort、下游 SSE 的 thinking `content_block_start`/`thinking_delta`/`signature_delta`、transcript 是否保存 thinking block、usage，区分"没触发 / 触发但上游没返回 / 返回了但没转给下游 / 转了但 CLI 没展示"；不能只因 prompt 有 ultrathink 就判定应有 thinking |
| CC-T-03 | P0 | 新增（用户要求） | 基线 | `MAX_THINKING_TOKENS=4096 ccp '证明根号 2 是无理数'` | 显式 budget | 出现 thinking block 与 `thinking_delta`，随后正文 | 请求体为 `thinking.type=enabled,budget_tokens=4096`；`thinking_tokens>0`；正文不含 `<thinking>` 标签 |
| CC-T-04 | P1 | 既有场景 | 基线 | `MAX_THINKING_TOKENS=1024`、`=31999`、`=0`、`=512`；再让 `MAX_THINKING_TOKENS` 与 `CLAUDE_CODE_MAX_OUTPUT_TOKENS` 取相同值 | 预算边界 | 1024/31999 有 thinking；0 视为关闭；512 与"预算=输出上限"两种情况均 200、不报错 | budget ≥ max_tokens、budget < 1024 等非法组合被服务端静默规范化，不返回 400：预算低于 1024 抬到 1024，大于等于 max_tokens 截到 max_tokens-1；缺省时补默认预算；adaptive/disabled 带的预算被删除。服务端日志应有规范化原因（目前只有 info 日志，没有 warnings 响应头，属已知现状） |
| CC-T-05 | P0 | 新增（用户要求） | settings `"alwaysThinkingEnabled": true` | 交互式 `cci`，问推理题 | settings 开启 | 界面显示思考过程 | 请求体变化被记录；stream 中有 thinking |
| CC-T-06 | P1 | 新增（用户要求） | 交互式 | 会话内用快捷键或 `/config` 切换 thinking 开/关（以当前版本 UI 为准），每次切换后问一题 | 会话内切换 | 开时有 thinking，关时无 | 请求体随切换变化；切换后历史不报错 |
| CC-T-07 | P1 | 既有场景 | 基线；开启发往上游的请求体采样 | `ccp --effort low` / `medium` / `high` / `xhigh` / `max` 各跑推理题；CLI 与直连（IDE 形态）两个入口 × 6 档（含不带 effort）× 5 轮，检查服务端发往上游的请求体 | effort | 均成功，usage 非 0；普通模型名下无 `thinking_delta`、无额外 thinking tokens | `--effort high` 时请求保留 adaptive 配置并被转换进上游 history（普通模型名下 thinking 控制只来自 adaptive 与 effort）。effort 写入上游原生推理字段（或兼容字段），`max` 不被截成 `high`，上游请求保留 `output_config.effort=max`；原生 adaptive 路径不发 `budget_tokens`；不凭空添加客户端没声明、上游也没声明的 thinking 字段；不支持的 effort 被映射而不是 400；违规数为 0。已知问题：max 被悄悄截成 high，或 adaptive/预算/别名/开关没进入最终上游请求体 |
| CC-T-08 | P1 | 既有场景 | 基线 | `MAX_THINKING_TOKENS=4096 ccp --effort max ...`；再构造 thinking 预算大于输出上限的组合（如 max_tokens 32000、预算 100000），分别带与不带 `--betas interleaved-thinking-2025-05-14` | budget + effort 叠加；interleaved 大预算 | 成功，有 thinking；期望带 interleaved beta 时预算不截断，发往上游的 effort 保持 `max` | 两个字段同时存在时服务端取舍合理。已知现象（未修复）：预算被截成 31999，effort 从 max 降为 high。官方语义下 interleaved thinking 预算是整轮总量，可大于 max_tokens；上游本就不接收 max_tokens，截断只会让思考档位无故下降。复现时记为问题 |
| CC-T-09 | P1 | 既有场景 | 运行配置 `thinkingTriggerMode=always`（先备份，结束恢复） | 先在默认 real_request 下跑：普通 sonnet、prompt 含 think/ultrathink、`--effort low`/`--effort max`、`--model sonnet-thinking`；再切到 always 跑普通 `ccp '17*23'` | 服务端强制 | real_request 下：普通 adaptive（含关键词和各档 effort）不输出可见 thinking；`-thinking` 模型名或 `thinking.type=enabled` 输出 `thinking_delta` 且有 thinking tokens。always 下：普通请求也出现可见 thinking，usage 非 0 | 结束后 GET 运行配置确认已恢复 `real_request` |
| CC-T-10 | P0 | 既有场景 | 基线 | `--model claude-sonnet-4-5-thinking`、`--model claude-haiku-4-5-thinking`、`--model sonnet-thinking`，统一用 `--output-format=stream-json --include-partial-messages --effort high`，prompt 如 `"Think briefly, then reply with exactly: OK"`；另用直连 API 测 `thinking.type=enabled` 以及 adaptive + effort=max | 模型名后缀 | 有可见 thinking，或清晰报错：出现 thinking 块和 `thinking_delta`，随后 `text_delta`；`result.usage` 与 `modelUsage` 非 0，`thinking_tokens>0`，计费不为零 | 不得静默退化为普通模型；只有真的拿到 thinking 输出才算通过，不能因为请求成功就判通过；记录 upstream model（应为基础模型，不带 `-thinking`）。对照"已知现象"中该别名无 thinking block 的历史记录给出结论 |
| CC-T-11 | P1 | 新增（用户要求） | 基线 | 直接 curl：`thinking:{type:"enabled",budget_tokens:2048}` + `stream:false` | 非流式 | content 含 `thinking` + `text` 块 | `extractThinking` 生效；usage 含 thinking_tokens；正文中不残留 `<thinking>` 标签（旧行为：非流式下标签原样泄漏到正文） |

## 被动触发与 thinking 混合

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-T-20 | P1 | 既有场景 | 基线（adaptive）；另一组开启 thinking | 复杂任务：`ccp --allowedTools 'Bash,Read' '分析当前目录的代码结构并给出重构建议'`（$WORK 放若干源文件）。开启 thinking 时：先做几轮只调用工具的对话，再多次问普通问题；检查 stream-json 中 thinking/text 块内容和 `message_delta.stop_reason`。主动（显式开启）与被动（不声明 thinking、由模型或配置触发）各跑一次 | 模型自发思考 | 若上游自发输出 `<thinking>`，服务端应转成 thinking 块而不是正文；回答出现在主区域而非 thinking 内；`stop_reason=end_turn` 而非 `max_tokens`；CLI 不会自动发"继续"；两种触发方式下 SSE 生命周期都完整，无协议错误 | 正文中不得出现裸 `<thinking>` 标签；也不得把正文误吞进 thinking；下一轮历史中没有"整段内容包在 thinking 里 + 一个空格"的污染。已知旧行为：`</thinking>` 后只有单换行、直接接正文或紧贴句末标点时不被识别为结束，整段回答进了 thinking，主区域只剩空格并误报 max_tokens |
| CC-T-21 | P0 | 既有场景 | `MAX_THINKING_TOKENS=4096`（或 `--model sonnet-thinking`） | `ccp --allowedTools Bash '先思考，再用 Bash 执行 date，再根据结果思考并回答今天星期几'`；另跑一次"先说一句『我来读取文件』再调用 Read" | thinking + tool_use；thinking → text → tool 顺序 | 顺序为 thinking → tool_use → (tool_result) → thinking → text；"我来读取文件"这句话在 text 块中，其后是 tool_use 块 | 第 1 轮 thinking 在 tool_use 之前关闭；`</thinking>` 紧贴工具调用、或后跟单换行正文再接 tool_use 时也能正确关闭（旧版此时 thinking 不关闭，正文留在 thinking 里）；上游不返回 `Invalid tool use format`（旧链路在工具调用轮直接进入 tool_use，没有稳定输出 thinking_delta）；SSE 块顺序、index 和终态合法，CLI 能正常继续工具回合；第 2 轮请求历史里的 thinking 处理不致 400 |
| CC-T-22 | P0 | 既有场景 | 同上，`--betas interleaved-thinking-2025-05-14`（或 CLI 默认） | 需要连续 3 次工具调用的任务；让一条 assistant 消息里出现多个 thinking/redacted 块并夹着 tool_use | interleaved thinking | 每次工具调用之间都可有 thinking；上游支持时多块保序传递，不支持时明确 fail closed | 所有 tool_use/tool_result 配对；stop_reason 依次为 tool_use…end_turn；不能伪造、合并或静默丢弃块。上游是否接受一条消息多块尚未被真实上游证实，结论需记录 |
| CC-T-23 | P1 | 既有场景 | 同上；多签名子项需原生签名能力，否则记"不适用" | 让模型一次并行调用 3 个工具（如同时 Read 三个文件）；另让模型在同一回复里同时 Read 两个文件，流式和 `-p` 各跑一次（原场景用 Opus，本次用 sonnet-4.5） | thinking + 并行工具 | thinking 后连续多个 tool_use 块；返回 200，无本地 400、无重试 | 块 index 连续、关闭顺序正确。若受保护轮里有多个签名 thinking：上游请求中只有一个 reasoning（优先保留最后一个 tool_use 前最近的那块），签名与原块一致，所有 tool_use 都保留。已知旧行为：直接 400 `multiple or mixed native reasoning blocks` |
| CC-T-24 | P0 | 既有场景 | 有原生签名能力时适用；否则记录"不适用"并用 XML 路径跑 | 开启 thinking 的会话，让模型用 Read 和 Bash 完成任务，退出后在同一服务端上 `--resume` 继续 3 轮，每轮带工具。原生 reasoning 能力可用后补测真实原生签名多轮回放 | 签名回传 | 3 轮均成功，退出码 0，UI 仍显示 thinking | 若有 `signature_delta`：历史回传后上游校验通过，无 `thinking.signature` 相关 400，真实原生签名多轮回放全部 200；若无签名：无签名 thinking 回传后退化成文本，不报错；无本地转换错误 |
| CC-T-25 | P1 | 既有场景 | 交互式，thinking 开启 | 第 1 轮 sonnet 带 thinking + 工具，`/model` 切到 haiku 继续问，再切回 sonnet | 切换模型带历史 thinking | 全部成功，行为稳定 | 历史 thinking 在不同模型间回放不报错，不因旧模型签名触发签名错误（签名绑定模型而不是会话，上游在这种情况下的行为尚未确认，需记录结论）；haiku 轮是否输出 thinking 与其请求字段一致 |
| CC-T-26 | P1 | 新增（用户要求） | 第 1 轮开 thinking，第 2 轮 `-c` 时 `MAX_THINKING_TOKENS=0` 或关闭 alwaysThinking | 开 → 关 | 历史有 thinking，当前禁用 | 第 2 轮无 thinking 且成功 | 服务端不因历史 thinking + 当前 disabled 组合报错 |
| CC-T-27 | P1 | 新增（用户要求） | 与 T-26 相反：第 1 轮关，第 2 轮开 | 关 → 开 | 历史无 thinking，当前开启 | 第 2 轮有 thinking | 带工具续写时同样成功 |
| CC-T-28 | P1 | 既有场景 | curl | `thinking:{type:"disabled"}` + `--model claude-sonnet-4-5-thinking`；再构造 `thinking.type=disabled` + `output_config.effort=high` 的请求（普通模型名），以及不带 effort 的 disabled 请求 | 显式禁用 + thinking 模型名；disabled + 显式 effort | 无 thinking；disabled + effort 请求成功 | 与当前实现语义一致；发往上游时去掉不兼容的 thinking 字段、只保留 output_config；没有显式 effort 时不偷偷生成推理字段。某入口若没走同一套归一化逻辑，这种组合会被原样送到上游导致 400 |
| CC-T-29 | P2 | 既有场景 | curl；或真实会话中换账号/换模型后 `--resume`，或使用过期会话，使历史带上游不再接受的签名 | 历史中带伪造/无效 `signature` 的 thinking 块、`redacted_thinking` 块；长会话历史带已签名 thinking 且上游返回 `THINKING_SIGNATURE_INVALID` | 无效签名回放 | 服务端丢弃或降级后成功，最多一次签名重试；CLI 不显示错误 | 非受保护历史签名不触发首发 400，只有 1 次 attempt（旧版首发把所有历史签名原样发出，一个旧签名就多一次失败请求）；受保护工具轮签名被拒时，同一凭据剥离历史 reasoning 后只重试一次；重试再失败要按新的错误类型如实上报，不向客户端暴露 signature 相关内部错误。曾出现"签名 400 → 剥离重试 → too-long → 最终 502"的链路，需确认不再复现。注意：用户反馈过"第一次签名失败，第二次成功"，第二次成功只是因为重试体里没有历史签名，不代表签名被修好 |
| CC-T-30 | P1 | 新增（补充） | `MAX_THINKING_TOKENS=4096`，`CLAUDE_CODE_MAX_OUTPUT_TOKENS=2048` | 推理题 | thinking 占满输出 | 被截断时 `stop_reason=max_tokens` | thinking 块被正确关闭，没有悬空块；CLI 显示合理 |
| CC-T-31 | P1 | 既有场景 | thinking 开启，非 strict profile | 让模型"在回答里用反引号解释 `<thinking>` 标签的用法"；让模型"给出包含 `<thinking>` 标签的提示词模板，放在 xml 代码块里"；在工具调用后输出这类正文 | 正文含标签字面量 | 正文完整显示字面量，整段模板作为 text 显示，不被折进 thinking；`stop_reason=end_turn` | 不被误判为 thinking 开始/结束。已知旧行为：模型未思考时整轮都在探测开始标签，正文中出现未被引号包住的 `<thinking>` 就会开 thinking 块，后续正文全部藏进 thinking 并可能报 max_tokens；修复后只在响应开头（前面只有空白）识别开始标签 |
| CC-T-32 | P2 | 新增（补充） | thinking 开启 | 中文推理题、含 emoji 与代码块的推理 | 多字节内容 | thinking 与正文无乱码 | 多字节字符跨 chunk 边界不损坏 |
| CC-T-33 | P1 | 新增（补充） | thinking 开启，交互式 | thinking 输出过程中按 ESC 中断，再发新问题 | 中断 thinking 流 | 下一轮正常 | 被中断轮次的半截 thinking 不导致下一轮 400 |
| CC-T-34 | P1 | 新增（用户要求） | thinking 开启 | 子代理任务（见 CC-G-01）+ `--forward-subagent-text` | 子代理 thinking | 子代理 thinking 块带 `parent_tool_use_id` 转发 | 子代理请求字段记录；无错误 |
| CC-T-35 | P1 | 新增（补充） | thinking 开启 | 长会话接近压缩阈值后触发压缩（见 CC-L 组） | 压缩 + thinking | 压缩成功后继续有 thinking | 压缩请求本身的 thinking 字段处理不导致失败 |

## XML thinking 解析边界

适用于 XML 提示路径（sonnet-4.5 / haiku-4.5 无原生能力时的默认路径）。

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-T-36 | P1 | 既有场景 | thinking 开启 | 让模型在思考中讨论"`</thinking>` 标签的作用"（行内提及或用引号包裹） | thinking 内含结束标签字面量 | 行内或引号中的标签不被当作结束，思考内容不会提前变成可见正文 | 放宽结束标签识别规则（见 CC-T-20）后仍需防误判；检查 thinking 块与 text 块的分界位置 |
| CC-T-37 | P1 | 既有场景 | thinking 开启，非 strict profile | 让模型输出"代码如下："后换行接一段 HTML/XML 代码；检查 CLI 渲染和 `text_delta` | 正文空白与 `<` 前缀 | 文本与模型原文逐字节一致，冒号和代码之间的空行保留，Markdown 结构完整 | 已知旧行为：chunk 为"空白 + 疑似 thinking 开始标签前缀"（如 `\n\n<`）时，探测会丢掉前面的空白，导致正文粘连 |
| CC-T-38 | P1 | 既有场景 | thinking 开启 | 让模型先思考再回答（响应以空行加 `<thinking>` 开头） | 响应开头 XML thinking | thinking 块内容正确，正文块只有回答，开头不多出空 text 块 | 修复空白保留与正文误判后的回归保护，和 CC-T-31、CC-T-37 一起跑 |

## 签名与历史 thinking 回放

依赖原生签名的子项在 01 预检显示原生能力不可用时记为"不适用"，但 XML 路径（无签名）的对应行为仍需验证。

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-T-39 | P1 | 既有场景 | 开启可见 thinking（显式 `-thinking` 模型） | 用 `--output-format=stream-json --include-partial-messages` 查看 thinking 块的 `content_block_start` 与 delta 序列 | 文本提取的 thinking | 从上游文本 `<thinking>` 提取的块不伪造 `signature_delta`；`content_block_start` 带 `"signature":""`；带签名的原生块在 `content_block_stop` 前发 `signature_delta` 且值为原始签名 | 已知现状：文本提取块完全没有 signature 字段，与官方 ThinkingBlock 中 signature 必填不一致，复现即记为问题 |
| CC-T-40 | P1 | 既有场景 | 原生签名能力可用 | 开启原生 thinking 跑任务，检查每个 thinking 块在 `content_block_stop` 前是否都有 `signature_delta` | 多段原生 reasoning | 每个 thinking 块都有自己的签名，下一轮回放不出现无签名 thinking | 已知旧行为：同一响应出现第二段 reasoning 时第二个块缺签名，下一轮回放触发"签名与无签名混用"的 400 |
| CC-T-41 | P0 | 既有场景 | 开启 thinking；开启请求体采样 | 让模型先思考再调用工具（如 Read），工具返回后继续；显式 thinking 模式下连续多轮工具调用；查看请求体采样和 attempts | 工具续写轮签名回放 | 当前工具续写那条 assistant 的签名 thinking 原样发给上游并一次成功；上游报签名错误时同账号剥离 reasoning 重试后成功；历史 thinking 被移除或截断后请求仍成功，不报签名无效 | 丢弃历史 thinking 时必须保护当前工具续写轮，否则违反"tool_use 前的 thinking 必须原样回传"的要求；最新工具回合的签名 thinking 不被无条件剥离 |
| CC-T-42 | P1 | 既有场景 | 多轮 tools + thinking 的长会话 | 持续工具调用直至历史超限，触发 payload 裁剪 | 裁剪 + 工具回合 | 与当前 tool_result 对应的最新 assistant 里的签名 thinking 保留；更旧的 thinking 可按配置丢弃；不出现 `THINKING_SIGNATURE_INVALID` | 已知旧行为：按"最后一条消息"划定当前窗口，工具回合最后一条通常是 user 的 tool_result，结果最新的 assistant 被当成历史，签名被剥掉 |
| CC-T-43 | P1 | 既有场景 | 开启 thinking 的多轮工具会话 | 中途制造流中断（断网或杀连接）后重试，或输入"继续"，使历史出现两条相邻 assistant 且各带签名 thinking | 相邻 assistant 各带签名 thinking | 不出现 `API Error: 400 consecutive assistant messages contain multiple native reasoning blocks and cannot be merged losslessly`；会话可继续；上游请求中没有旧轮 thinking，两段可见回答都保留 | 已知旧行为：服务端在本地转换阶段直接拒绝，请求没发到上游，同一历史每次回放都 400，重试和"继续"都无效 |
| CC-T-44 | P1 | 既有场景 | 长会话 | 让同一条 assistant 历史同时含签名 thinking 和无签名 thinking（如先出现文本 thinking 后出现原生 reasoning，或回放中某块丢了签名；可用 curl 构造），继续下一轮 | 签名与无签名混用 | 返回 200，无签名 thinking 被丢弃，CLI 不显示 API Error | 已知旧行为：直接报 `invalid_request_error`（`assistant history mixes native signed/redacted reasoning with unsigned thinking` 或 `cannot be represented losslessly`），且不在兜底重试触发列表中，会话卡死 |
| CC-T-45 | P1 | 既有场景 | 历史带 reasoning 的会话 | 继续提问，观察服务端日志是否出现 `Improperly formed request` 及"同凭据剥离历史 reasoning 后重试" | malformed 400 自动恢复 | 命中时剥离后重试一次成功，CLI 看不到 400；重试仍失败返回 502 `api_error` 且不进入外部池 fallback；原因不是 reasoning（如空 content）时只多一次失败请求 | 上游对历史 reasoning/signature 不兼容时返回泛化 malformed 400，不能当作普通坏请求直接暴露给 CLI |
| CC-T-46 | P2 | 既有场景 | curl 构造或长会话积累历史 | 历史包含：同一 assistant 多个连续 thinking/redacted_thinking；thinking → tool_use → thinking → tool_use 交错；会话中途切换模型；display=omitted 或空 thinking 带签名。对比 CLI 回传内容与发往上游的内容 | 不透明块回传 | signature 和 redacted data 原样回传，不拼接、不重算、不改写、不重排、不伪造，也不被 transcript 清洗逻辑扫描修改；无法无损表达时明确失败，而不是合并成一个伪造块或静默成功 | 按官方规则这些是不透明块，客户端唯一正确做法是原样回传；服务端对多块或混合块保守处理，这些场景尚缺真实上游验证 |
| CC-T-47 | P1 | 既有场景 | 可触发外部池回退的配置 | 用 thinking/ultrathink 进行长会话，并触发回退到外部池 | 外部池回退 + 历史签名 | 不出现 `THINKING_SIGNATURE_INVALID` | 已知现象：历史 assistant 的 thinking block 带签名被转发到外部池后被判签名无效；预期所有外部池路径都清理 signature 和 redacted_thinking |
| CC-T-48 | P2 | 既有场景 | 运行配置可切换 `discardHistoricalThinking`（先备份，结束恢复） | 开启 thinking 做长会话，分别在 `true` 与 `false` 下跑同一任务脚本，对比回答质量与连贯性 | 历史 thinking 丢弃开关 | `false` 时历史 reasoning 原样发送（全部发送 + 冲突兜底）；`true` 时无明显质量下降 | 需人工判断；默认丢弃后模型看不到自己之前几轮的推理，影响尚未实测，结论写入结果汇总 |
| CC-T-49 | P1 | 既有场景 | 开启 thinking | 用 sonnet-4.5 与 haiku-4.5 各跑一次 20 轮以上、Read/Bash/Edit 交替并多次 `--resume` 的任务（原场景用 Opus 和 Sonnet） | 长 thinking 工具任务综合回归 | 全程 200，无本地 `unsupported_content` 400，无上游签名校验 400 | 多块 reasoning 相关诊断采样为零；签名重试次数明显低于历史水平；覆盖多块转换、签名回放、历史 thinking 丢弃三处行为 |

## effort 与推理能力兼容

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-T-50 | P0 | 既有场景 | 上游推理能力发现不完整、运营方 prompt steering（兼容 thinking 提示注入）总开关关闭（先备份，结束恢复） | `claude --print --effort high "..."`；`claude --print --model sonnet-thinking --effort high "..."`；同模型普通请求对照 | 显式 reasoning + prompt steering 关闭 | exit 0，最终文本正常；sonnet-thinking 有 thinking block 且 thinking tokens 非零；普通请求也正常 | 不再出现 `API Error: 400 reasoning was requested, but both native reasoning fields and compatible thinking prompt controls are unavailable`。已知现象：普通请求 200，reasoning 请求在发往上游前被本地拒绝；原因是能力状态未知时不生成原生推理字段，兼容 thinking 提示又被总开关关掉。预期客户端显式 reasoning 走专用的 thinking 兼容传输 |
| CC-T-51 | P0 | 既有场景 | 上游模型目录只声明基础模型、没有原生 reasoning effort 字段 | `ccp --model sonnet '...'` 普通请求 | 默认 adaptive + effort | 请求正常成功，不返回 400 | 已知问题：CLI 默认带 adaptive 和 effort，服务端因上游模型没声明原生 reasoning schema 就回 400 `does not advertise a native reasoning effort field`；预期降级处理 |

## 流式节奏、展示与跨上游

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-T-52 | P2 | 既有场景 | 原生签名能力可用，否则"不适用" | 开启原生 thinking，给需要长时间推理的任务，观察 thinking 显示节奏和 `thinking_delta` 数量 | 长推理 | 原生 reasoning 在完整缓冲后一次性下发（1 个 `thinking_delta` 加 `signature_delta`），首个 thinking 可见延迟约等于整段推理时长，属有意取舍；不截断、不报错 | 上游 reasoning 是累计快照，服务端为保证签名内容安全会原子缓冲（上限 1 MiB）后才下发；超过上限的行为需记录 |
| CC-T-53 | P1 | 既有场景 | thinking 模型；可模拟上游首字慢或长时间无事件（如限速代理） | 多轮长会话，中间制造上游首字慢、thinking 首包后长时间无事件 | 流式稳定性 | XML 路径的 thinking delta 实时透传不被截留；首个 thinking 与首个可见文本的延迟可分开观测；空闲超时后连接中止并释放账号占用 | thinking 首包之后长时间无事件也要按空闲超时处理；检查账号占用计数恢复 |
| CC-T-54 | P2 | 既有场景 | 支持 `thinking.display` 的客户端配置，或 curl 构造 | 请求带 `thinking.display=omitted`；再带非法 display 值；观察 UI 和 stream-json | display 选项 | 期望：omitted 被接受，暂不执行时给出 `thinking-display-ignored` 告警；非法值返回 400 `thinking.display must be one of: summarized, omitted`。若真正执行 omitted：只有 thinking 块的开闭和签名，没有 `thinking_delta`，属协议行为 | 已知现状：服务端不解析客户端 display，仍返回完整 thinking，非法值也 200，上游 display 只由运营配置决定；上游是否接受 omitted 未验证。用户在 omitted 下会看到"状态在转但没有内容"，不算故障 |
| CC-T-55 | P2 | 既有场景 | 另有可用的官方 Anthropic API 凭据，否则"不适用" | 在服务端上完成 thinking + 工具调用后，把 `ANTHROPIC_BASE_URL` 改为官方 API 再 `--resume` | 跨上游 resume | 已知限制：官方返回 400，指向历史 thinking 签名缺失或无效；需跨上游时先 `/clear` 或使用 strict profile | 经服务端产生的 thinking 没有官方签名，任何方案都无法被官方接受；本用例只确认限制与规避方式，不记为缺陷 |

## 结果汇总要求

本组执行完输出一张表：场景 × 模型（sonnet-4.5 / haiku-4.5） × 请求体 thinking 字段 × 是否出现 thinking 块 × 是否有签名 × 结果。用它对照上面的实现语义，任何"请求要求可见 thinking 但没有 thinking 块"或"正文出现 thinking 标签"都记为问题。标为"不适用"的用例需写明原因（如原生签名能力不可用、缺官方凭据）。CC-T-48 的质量对比结论单独附在表后。
