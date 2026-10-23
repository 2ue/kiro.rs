# 02 开关清单：默认值、关闭影响、建议

每个注入项都有自己的开关，开关之间互不影响。只有两种情况会互相关联，都写在"依赖"列：

- 一项依附在另一项的内容上（例如 B2 拼在 B1 后面）。
- 注入项属于某个功能（例如 D1 只在 WebSearch 模型驱动开启时才有意义）。

被依赖的一项关闭时，页面把依赖它的项显示为"不生效"，并说明原因，但不会改写它的开关值。

建议等级：

- 建议开：关闭会导致报错或可见的功能退化。允许关，页面上标红提示。
- 用户决定：开和关都合理。

"新装默认"指没有旧配置时的初始值。旧配置升级时按 [03 第 5 节](03-design.md#5-迁移) 迁移，结果与升级前等价。

## 1. 注入项（14 个，每项都可按路由覆盖）

| ID | 名称 | 新装默认 | 外部池 | 文本 | 依赖 | 关闭后的影响 | 建议 |
|---|---|---|---|---|---|---|---|
| `taskQuality` | 任务质量（A1） | 默认关，`/cc` 显式开 | 可选，默认关 | 可编辑 | 无 | 模型更容易只说一句"我先看看"就结束、不发 tool_use，Claude Code 任务停住；"声称已完成要给证据"的约束也没了。规则是中文写的，对非 Claude Code 客户端可能影响回复语言（未实测） | Claude Code 路由建议开；其他路由用户决定 |
| `languageConstraint` | 语言约束（A2） | 关 | 可选，默认关 | 可编辑 | 无 | 回复语言完全由客户端 system 决定（Claude Code 自己会声明语言） | 用户决定 |
| `languageTurnReminder` | 每轮语言提醒（A3） | 关 | 仅本地 | 可编辑，两个变体 | 无 | 长会话里工具输出多时，回复语言容易漂到工具输出的语言 | 用户决定；开了 A2 时建议一起开 |
| `custom` | 自定义（A4） | 关 | 可选，默认关 | 可编辑，默认为空 | 文本为空时不注入 | 无 | 用户决定 |
| `identityPrompt` | 身份声明（A5） | 关 | 可选，默认关 | 可编辑 | 无 | 上游 Kiro 在服务端注入了自己的身份，模型可能自称 Kiro 或提到 AWS | 需要隐藏渠道的路由开 |
| `identityTurnReminder` | 每轮身份提醒（A6） | 关 | 仅本地 | 可编辑 | 无 | 单靠 A5 压不住上游身份，小模型尤其明显（`model/config.rs:391` 注释记录的实测结论） | 开了 A5 时建议一起开 |
| `chunkedWritePolicy` | 分块写入策略（A7） | 开 | 仅本地 | 可编辑 | 请求里声明了 Write 或 Edit 工具才注入 | 模型可能建议改用 Bash 写文件，或先问用户要不要换方式 | 用户决定，问题见 [05 第 3 条](05-known-issues.md) |
| `chunkedWriteToolSuffix` | Write/Edit 描述后缀（A8） | 开 | 仅本地 | 可编辑，Write/Edit 各一段 | 只作用于名为 Write/Edit 的工具 | 模型会一次写入大文件。上游对大输入的截断阈值没有实测证据 | 用户决定 |
| `thinkingControls` | thinking 控制标签（B1） | 开 | 仅本地 | 只读 | 只在模型没有原生 reasoning 字段、且请求要求 reasoning 时注入 | **请求 reasoning 但模型没有原生字段时直接 400**（`converter.rs:686`） | 建议开 |
| `thinkingOutputPolicy` | 可见 thinking 输出策略（B2） | 开 | 仅本地 | 只读 | 依赖 B1；opus-5 系列自动跳过 | 代理拆不出 thinking 块，客户端看不到思考过程，推理可能混进正文（未实测） | 建议开 |
| `toolChoicePrefix` | tool_choice 前缀（B3） | 开 | 仅本地 | 只读，有 any/tool/none/并行四种变体 | 依赖结构化 tool_choice 处理（见第 2 节） | `any`/`tool` 时模型更容易只回文本；`none` 时模型去调用历史工具，响应侧会丢掉这些调用，可能得到空回复 | 建议开 |
| `toolChoiceTurnReminder` | tool_choice 每轮提醒（B4） | 开 | 仅本地 | 只读 | 同 B3；只在 any/tool 时注入 | `any`/`tool` 的遵从率进一步下降（`converter/tools.rs:538` 注释：只靠 B3 时几乎总是只回文本） | 建议开 |
| `webSearchGuidance` | WebSearch 引导（D1） | 开 | 仅本地 | 可编辑 | 依赖功能"WebSearch 模型驱动" | 模型不知道要按编号引用，响应侧按 `[n]` 生成的 citations 会大量缺失 | 开了 WebSearch 时建议开 |
| `emptyTurnNudge` | 空轮续写提示（E1） | 开 | 仅本地 | 可编辑 | 依赖空轮重试（`streamRetry` 允许 UsageOnly） | 空轮重试只去掉 reasoning 字段后重发。线上记录：基本相同的请求重发几乎总是再次为空（`empty_turn_nudge.rs:3`），Claude Code 会静默停下 | 建议开 |

文本校验，前后端都执行：

- 单项不超过 8000 字符，所有可编辑文本合计不超过 20000 字符（沿用 `admin/service.rs:6463` 的总量上限）。
- A3、A6 必须保留各自的 `<language_reminder>`、`<identity_reminder>` 标签。控制块清洗（F1）靠这两个标签识别模型复述的内容。缺标签时拒绝保存。
- A3 的"客户端声明语言"变体必须包含 `{language}`。
- 文本清空等于恢复内置默认。A4 例外，A4 清空就是不注入。
- D1 文本不包含 `[` 时，页面给出警告，但允许保存。
- 修改 A1、A2、A4、A5 会改变缓存模拟的前缀。保存时提示"所有会话的缓存前缀会失效一次"。

## 2. 拆出去的非注入功能

这些功能原来和提示词开关绑在一起，现在拆出来，各自单独控制。

| 功能 | 原来的耦合 | 拆分后 | 位置 | 新装默认 |
|---|---|---|---|---|
| `-thinking` 模型名解析（D5） | 受提示词总开关控制（`parsed_body_pipeline.rs:32`）。总开关一关，`claude-xxx-thinking` 就失效 | 独立开关，可按路由覆盖 | 路由 → 功能 | 开 |
| thinking 触发方式（D6） | 受提示词总开关控制（`handlers.rs:13659`） | 取值改为 `real_request` / `always` / `off`，可按路由覆盖。新增 `off`，用于等价迁移"总开关关闭" | 路由 → 功能 | `real_request` |
| 强制可见 thinking | 受提示词总开关控制（`handlers.rs:13778`），同时决定发给 Kiro 的 `display` 字段 | 跟随 D6：`off` 时不强制，其余取值行为不变 | 随 D6 | 随 D6 |
| 渠道信息输出过滤（F2） | 挂在身份隐藏总开关下面 | 独立功能。不开身份声明也能单独开，因为 Kiro 自己的系统提示同样会泄露（`cctest-protocol-compat-20261020/02-evidence.md` 的 echo_tag 用例） | 路由 → 功能，含"流式也过滤" | 关 |
| 模型列表只显示 Claude | 放在身份配置里（`identity.claudeModelsOnly`） | 独立的全局开关。`/v1/models` 的 handler 拿不到路由，所以暂不按路由覆盖 | 运行配置 → 模型 | 关 |
| 结构化 tool_choice 处理 | `bodyConversion.toolChoiceSteering` 同时控制工具过滤、响应侧拦截和 B3/B4 提示词 | 只保留过滤和响应侧拦截（none=0、any=N、named=1，`disable_parallel_tool_use`），提示词由 B3/B4 自己控制 | 运行配置 → 协议转换（全局） | 开 |
| WebSearch 模型驱动 | 已经按路由控制，放在 `bodyConversion` | 移到路由功能，D1 引导单独控制 | 路由 → 功能 | 关 |
| 空轮重试 | 重试和续写提示绑在一起，提示没有开关 | 重试仍由 `streamRetry` 控制，提示由 E1 控制 | 不变 | 不变 |

下面这两个旧开关删除，它们唯一的作用就是给对应注入项多加一道开关：

- `bodyConversion.chunkedToolPolicy`：只控制 A7、A8（`converter.rs:162-178`）。
- `bodyConversion.thinkingPromptControls`：只控制 B1、B2（`converter.rs:180-186`）。

## 3. 去掉的全局开关和规则

| 旧设置 | 处理 | 原因 |
|---|---|---|
| `promptSteering.enabled` 总开关 | 删除，页面改为批量操作"全部注入项默认值设为关" | 总开关压在所有开关上面，各项就没法单独控制；原来它还连带关闭了 D5/D6 |
| `scope` / `routeMode` / `routeRules`，以及身份的同名字段 | 删除 | 注入项不再自带路由规则，和路由的关联只在路由上显式设置 |
| `explicit_reasoning_request` 特例（`converter.rs:182`） | 删除 | B1 不再受总开关控制，这个特例就没有存在的必要了 |
| `compatProfile=anthropic-strict` | 保留，作为全局协议模式，覆盖范围保持现状 | strict 表示不做兼容改写。现状是 A1–A4、A7、A8、B1–B4 在 strict 下不注入（B1 在强制可见 thinking 时例外）；A5、A6、D1、E1 不受 strict 影响。页面在受影响的项上标"strict 下不生效"，见 [05 第 7 条](05-known-issues.md) |
| `applyToCountTokens` | 保留，作为全局开关 | 这是计数口径设置，不属于注入项 |

## 4. 不提供开关，只在页面展示

| 项 | 原因 |
|---|---|
| C1 system 确认句 "I will follow these instructions." | Kiro 没有 system 字段，history 必须是 user/assistant 交替。payload guard 也靠这句识别合成的 system 消息对（`payload_guard.rs:4471`） |
| C2 空内容占位（`.`、"Tool result received." 等） | Kiro 拒绝空 content。语言检测也靠这几句识别非用户文本（`language_preference.rs:26`） |
| C3/B5 历史工具占位定义 | Kiro 要求历史中引用的工具必须有定义。已有开关 `bodyConversion.historyPlaceholderTools`，保持不变 |
| C4 媒体替换文本、C5 空工具描述 | 是"内容被省略"的告知，删掉等于静默丢弃内容 |
| D2/D3 搜索工具描述和结果提示、D4 结构化输出工具 | 由功能或请求参数触发，属于功能实现的一部分 |
| F1 控制块清洗 | 只删除和注入内容逐字一致的块；请求本身提到这些标签时自动关闭 |
