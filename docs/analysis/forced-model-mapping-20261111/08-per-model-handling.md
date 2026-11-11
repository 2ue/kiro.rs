# 08 逐模型专项处理

本文把 02–07 的通用方案按"外显模型 → 目标模型"展开成逐条处理清单。目标有三个：功能看起来正常、不报错、尽可能完整地执行。

通用规则（每个模型都适用，下文不再重复）：

- 对外字段一律用外显模型，见 05。
- 不论客户端传什么参数，都不返回 400，见 03。
- 上下文超出时走 L0–L5 退避，不对外报"输入过长"，也不报"窗口已满"，见 04。
- K0 自我认知卡按外显模型注入，K1/K2 按需注入，见 07。
- thinking 和 display 的默认值必须在**替换模型之前**，按外显模型确定下来，见 02 第 2 节。

## 1. 外显模型 → 目标模型 速查

| 外显模型 | 默认目标 | 外显语义要点 | 目标缺什么 |
|---|---|---|---|
| Fable 5.1 / Fable 5 / Mythos 5.x | Sonnet 4.5 | 1M、128K 输出；思考始终开启、不能关；display 默认 omitted；不支持 prefill 和采样参数；不支持强制 tool_choice（只限 5.1）；可能出现 refusal 停止原因 | 窗口、输出上限、原生 adaptive 思考、effort、新工具版本 |
| Opus 5.5 | Sonnet 4.5 | 1M、128K；思考不能关，effort 默认 medium；display 默认 omitted；工具调用之间的文字以 thinking 块形式返回；computer 只能用 toolset；不支持强制 tool_choice | 同上 |
| Opus 5 | Sonnet 4.5 | 1M、128K；默认开启 adaptive 思考，只有 effort ≤ high 时才能关；display 默认 omitted | 同上 |
| Opus 4.6–4.8 | Sonnet 4.5 | 1M、128K；adaptive 思考（4.7 以上不传 thinking 就不思考）；display：4.7 以上 omitted，4.6 summarized | 窗口、输出、effort |
| Sonnet 5 | Sonnet 4.5 | 1M、128K；默认开启 adaptive 思考；display 默认 omitted；只支持默认采样参数；新 tokenizer；图片 2576px | 窗口、输出、effort、视觉分辨率 |
| Sonnet 4.6 | Sonnet 4.5 | 1M、128K；adaptive 思考；display summarized | 窗口、输出、effort |
| Haiku 4.5 | 不改写（已经是目标模型） | — | — |
| 短别名 opus / sonnet / fable / best / default / opusplan | `defaultTarget` | 外显模型就是别名本身；K0 按别名当前指向的模型生成卡片 | — |
| haiku 别名 | Haiku 4.5 | Claude Code 的后台任务（标题、摘要等）用这个别名 | — |

## 2. 逐模型处理

### 2.1 Fable 5.1 / Fable 5 / Mythos 5.x → Sonnet 4.5

| 项目 | 处理 |
|---|---|
| thinking | 不论客户端传什么都开启思考：没传、传 adaptive、传 enabled 都开启；传 disabled 也**不报 400**，但按外显语义仍然开启思考。思考长度按 effort 换算，没传 effort 时按 high（32K） |
| display | 默认 omitted；`updates` 也按 omitted 处理 |
| 输出 | 按 64K 预留；超出时以 `max_tokens` 结束，客户端会自己续写 |
| tool_choice any / tool | 外显模型（5.1）会对强制选工具返回 400，强制模式下**不报错**，按 03 的提示词引导处理 |
| prefill / 采样参数 | 静默丢弃 |
| refusal | 外显模型可能返回 `stop_reason: refusal`。目标模型拒答时是用文字说明，不会给出这个停止原因。不模拟 refusal，按原样返回文字 |
| 新工具 | `web_fetch_20260318` 走 03 的代理抓取；`code_execution_20260521` 等按前缀处理；advisor 静默移除 |
| 中途 system 消息 / `clear_at` | 按 03 第 3 节的方式，转成当轮文本块 |
| 单条消息上的 effort（beta） | 只有 effort、`content` 为空的 system 消息：把其中的 effort 用于后续轮次的思考长度换算，消息本身丢弃 |
| K0 | Fable 卡：在型号、1M、128K 之外，写明"Anthropic 最强的公开模型" |
| 特别风险 | 能力差距最大。复杂长任务的成功率会明显下降，属于最容易被察觉的组合 |

### 2.2 Opus 5.5 → Sonnet 4.5

| 项目 | 处理 |
|---|---|
| thinking | 规则同 Fable。effort 没传时**按 medium（16K）**，这是外显模型的默认值 |
| display | 默认 omitted |
| 工具调用之间的文字 | 外显模型把这部分文字作为进度型 thinking 块返回，在 display 为 omitted 时内容为空；目标模型照常输出文字块。<br>**处理：** 强制模式下，当外显模型是 Opus 5.5、本轮有工具调用、并且 display 为 omitted 或 updates 时，把夹在两次工具调用之间的文字块改成 thinking 块，再按 display 规则处理（omitted 时清空，updates 时保留摘要）。最后一个工具调用之后的总结文字仍保留为 text。<br>这样 Claude Code 看到的结构和真实的 Opus 5.5 一致。<br>可以按开关关闭，默认开启 |
| computer | 外显模型只接受 `computer_toolset_20260801`（没有 name，成员工具各带 `toolset_name`）。<br>**处理：** 按 toolset 把每个成员展开成普通工具，补上 schema 发给上游。上游输出的 tool_use 加回 `"toolset_name":"computer"`，下一轮 tool_result 中的这个字段原样回传。<br>客户端如果发来旧版 `computer_20251124`，也照常处理，不返回 400 |
| tool_choice any / tool、prefill、采样参数 | 同 Fable |
| K0 | Opus 5.5 卡：知识截止 2026-06（官方值） |

### 2.3 Opus 5 → Sonnet 4.5

| 项目 | 处理 |
|---|---|
| thinking | 没传时开启，effort 默认 high。传 disabled 时：effort ≤ high 就照常关闭；effort 为 xhigh 或 max 时外显模型会返回 400，强制模式下**不报错**，改为开启思考 |
| display | 默认 omitted |
| computer | 两种格式都接受 |
| 其它 | 同 Opus 5.5 |

### 2.4 Opus 4.6–4.8 → Sonnet 4.5

| 项目 | 处理 |
|---|---|
| thinking | 4.7 以上：不传 thinking 就不思考，与外显模型一致；4.6 同样不思考。adaptive 和 effort 按换算表转成思考长度 |
| display | 4.7 以上默认 omitted，4.6 默认 summarized（复用现有的 `default_for_model`） |
| effort | 4.6 不支持 xhigh。客户端传 xhigh 时按外显模型的规则，就近换成 high 或 max，再换算思考长度。4.6 本身也接受 budget_tokens |
| 采样参数 | 4.6 允许，4.7 以上拒绝。上游是 4.5，按现状丢弃 |
| K0 | 按各自版本生成卡片 |

### 2.5 Sonnet 5 → Sonnet 4.5

| 项目 | 处理 |
|---|---|
| thinking | 没传时开启，effort 默认 high；传 disabled 时关闭 |
| display | 默认 omitted |
| 采样参数 | 外显模型只允许默认值。强制模式下统一丢弃，不报错 |
| 视觉 | 外显模型支持 2576px。图片统一缩到 1568px（03 第 5 节）。computer use 截图的坐标需要按缩放比例换算，见第 3.3 节 |
| tokenizer | 外显模型的 token 数约多 30%，是否按这个比例放大见 06 D5 |
| 中途 system 消息 | 外显模型**不支持**，会返回 400。强制模式下不报错，仍按 03 的方式转换 |
| K0 | Sonnet 5 卡 |

### 2.6 Sonnet 4.6 → Sonnet 4.5

| 项目 | 处理 |
|---|---|
| thinking | 传 adaptive 时按 effort 换算思考长度；也接受 budget_tokens |
| display | summarized |
| 窗口 | 1M 与 200K 之间按 04 的分级处理 |

### 2.7 Haiku 4.5 及 haiku 别名 → Haiku 4.5

不改写。K0 仍按 Haiku 4.5 注入，保证自述一致。Claude Code 的后台任务（标题、摘要、Explore 子代理等）会走这条路径，所以必须保证它稳定、快速。

## 3. 跨模型的特殊处理

### 3.1 会话中途换外显模型

用户在 Claude Code 里用 `/model` 切换外显模型时，目标模型可能不变（例如 Opus 5.5 → Sonnet 5，目标都是 Sonnet 4.5），也可能改变（例如 Sonnet 5 → Haiku）。

- 目标不变时，上游前缀不变，缓存照常命中。真实的 API 换模型会导致缓存失效，这里不需要刻意模拟。
- 目标改变时，按新目标处理。历史中的合成签名与模型无关，可以照常回放。

### 3.2 子代理与后台任务

- Claude Code 的子代理可能指定 haiku 或 sonnet，按第 1 节的规则映射。
- 子代理请求一般没有完整的 Claude Code 系统提示。这时 K0 必须带上完整的型号句，不能省略。

### 3.3 截图坐标

外显模型能看到更高分辨率的图（Sonnet 5 是 2576px），目标模型只看 1568px。computer use 的场景下，客户端是按原图的像素坐标解析模型给出的坐标。

**处理：**

- 缩放截图时记录缩放比例。
- 目标模型输出 tool_use 后，把坐标类字段（`coordinate`、`start_coordinate` 等）按比例换算回原图坐标。
- 换算按 toolset / computer 各版本的字段表进行。

### 3.4 输出侧的统一改写顺序

每个响应事件依次经过以下步骤：

1. 渠道泄漏过滤（现有）。
2. 模型自述改写（05）。
3. 注入块复述清理（07，复用 `control_block_cleaner.rs`）。
4. "根据提供的上下文/资料"这类句子改写。
5. Opus 5.5 把工具调用之间的文字转成 thinking 块（2.2）。
6. display 处理（omitted 时清空 thinking 文本）。
7. computer 的坐标换算和 `toolset_name` 回填。

**thinking 内容也要经过第 1–4 步。** 现有的 `identity_guard.rs` 只处理 text_delta，thinking 块直接放行（测试 `non_text_blocks_pass_through`）。所以 summarized 和 updates 模式下可见的思考内容需要补上这套检查。omitted 模式下思考内容本来就清空了，不用处理。

## 4. 验收补充（追加到 06 的验收矩阵）

| 编号 | 场景 | 通过标准 |
|---|---|---|
| A13 | 每个外显模型 × 本文第 2 节列出的每一项特殊处理 | 全部返回 200；响应结构与外显模型的官方行为一致 |
| A14 | Opus 5.5 多工具会话 | 工具之间的文字以 thinking 块返回；omitted 时为空；最后的总结是 text |
| A15 | computer toolset 往返 | 成员工具调用和 `toolset_name` 回填正确；坐标换算后点击位置正确 |
| A16 | 可见 thinking 的泄漏检查 | summarized / updates 模式下的思考内容不出现 4.5 自述，也不出现注入块复述 |
| A17 | 知识库对照组（见 07 第 6 节） | 自称外显型号、知识截止说对、生态事实说对的比例达标；"根据上下文"字样出现率达标；未收录的问题不编造答案 |
