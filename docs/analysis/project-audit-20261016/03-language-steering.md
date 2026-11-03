# 03 语言跟随失效：根因、调研与方案

## 1. 现象

59137 上 `promptSteering.languageConstraint` 已开启，规则是"跟随用户语言"。用户用中文提问，主回复基本是中文，但长 agent 会话中工具调用之间的进度说明、子任务 prompt 会变成英文。用户确认：之前有版本是正常的，后来才变成英文；上一次的改造发布后仍然一样。

用户的要求（2026-10-15 明确）：

- **不是**强制简体中文。
- 如果用户在 Claude Code 里设置了全局语言（`language` 设置），就把它转换成 Kiro 侧的语言约束。
- 没有设置时，注入一段"跟随用户语言"的提示：用户用中文问就答中文，用英文问就答英文。

## 2. 远端只读检查结论

- 59137 运行版本与本地 `origin/main` 的差异没有涉及占位符、合成 ack、prompt_steering 和历史转换。
- `runtime_config`：`runtimeConfigMigrationVersion = 11`；`promptSteering.enabled = true`，`routeMode = allow_list`，`routeRules = ["/cc"]`；`languageConstraint.enabled = true`，prompt 为 v11 新版英文规则文本。
- 近 2 小时日志里没有 payload guard 裁剪语言约束的记录。
- 结论：问题**不是**协议没有转换成 Kiro 格式，转换本身是正常的。

## 3. 根因（按影响排序）

### 3.1 工具结果轮次被插入英文占位符（主因）

- `TOOL_RESULTS_PROVIDED_PLACEHOLDER = "Tool result received."`（`src/anthropic/converter.rs:90`），用于当前消息（`:588`）和历史（`converter/history.rs:227`）。
- 由 e33f5e7（v0.0.126，2026-08-01）引入，之前是 `"."`。
- 在 Kiro 协议里它是普通的 user 文本，不是 tool_result，所以规则中"排除工具结果"那一条排除不了它。
- 长 agent 会话里，模型看到的"用户消息"绝大多数是这句英文，于是推断用户在用英文。

### 3.2 内置语言提示从"强制中文"变成"规则推断"

- V1 原文是"面向用户的自然语言叙述默认使用简体中文"。
- v0.0.114（b528ead，07-23）改了默认值，但 PG 中已保存的 V1 没有迁移，所以线上一直还在用中文版。
- v0.0.184（c7f9edb，10-02）的迁移 v11（`CURRENT_RUNTIME_CONFIG_MIGRATION_VERSION = 11`，`src/model/config.rs:5004`）把保存的 V1/V2 替换成新英文规则，并强制关闭一次开关。
- 时间上与"之前正常，后来直接变英文"吻合。新规则本身语义正确，但它把判断交给模型，而模型能看到的"用户消息"已经被 3.1 污染了。

### 3.3 客户端 system 被降级为历史第一对消息（放大因素）

- `converter/history.rs:107,124` 把客户端 system 转成 user，再配一条合成的 assistant `"I will follow these instructions."`。这个做法从 7f849cd（2025-12）就有，同类项目都这么做，因为 Kiro 协议没有 system 字段。
- prompt_steering 的语言约束注入在 `system[0]`（`src/anthropic/prompt_steering.rs:109`），因此语言约束、Claude Code 系统提示、CLAUDE.md 都排在会话最早的位置，离当前轮越来越远。
- Kiro 上游自带英文系统提示（"You are Kiro…"），不在本仓库，由 vibe 模式注入。

### 3.4 已排除

- payload guard 裁剪丢失语言约束：ce2fad6（v0.0.178 起）保留系统对，只在最后才删除，当前版本可以排除。

## 4. 业内做法调研

| 来源 | system 处理 | tool_result-only 轮占位 | 语言处理 |
|---|---|---|---|
| 上游 hank9999/kiro.rs | 并入首条 user + `"I will follow these instructions."` | 空字符串 | 无 |
| ZyphrZero/kiro.rs、TsinHzl/kiro2cc-proxy | 同上 | 空 / `"OK"` | 无 |
| Yaocool/kiro-proxy、AIClient2API | 并入首条 user | `Tool results provided.` / `Continue` | 无 |
| caidaoli/kiro2api | 并入首条 user | 中文 `执行工具任务`（仅特定条件） | 无 |
| mikeyobrien/pi-provider-kiro | — | `Please proceed with the task.` | 无；注释指出 `Continue` 填充会让模型复读 |
| new-api、one-api、claude-code-router 等 | — | — | 只有错误信息的 i18n，不碰对话 |

Anthropic 官方文档：

- Multilingual support：模型会从对话推断回复语言，但生产环境应显式指定目标语言。
- Prompting best practices：指令放在靠近末尾、贴近当前轮的位置，遵从效果更好。长对话建议把提醒注入到 user turn。
- Claude Code settings：`language` 设置会原样传给模型，作为"始终用该语言回复"的指令。

结论：模型的语言自适应只在上下文以用户语言为主时可靠。本链路的长会话上下文中英文占绝大多数（上游英文 system、英文 ack、英文工具输出、英文占位符），需要特别处理。同类项目都没有处理，这不代表不需要，只是它们没有解决这个问题。

## 5. Claude Code 语言设置的实际形态

从本机 Claude Code 2.1.283 安装包中确认：

- **设置了语言**：注入一条 meta user 消息，以最后一条为准；会话开始、压缩后、修改设置时都会重新发送：
  ```
  # Language
  Always respond in {X}. Use {X} for all explanations, comments, and communications with the user. Technical terms and code identifiers should remain in their original form.
  Maintain full orthographic correctness for {X}, ...
  ```
- **清除了设置**：发送 `The language preference was cleared. Match the user's language.`
- **从未设置**：不发送任何内容。

Kiro 请求结构没有 language 或 locale 字段，语言要求只能通过提示词表达。

## 6. 方案

### 6.1 识别

在转换之前（payload guard 裁剪之前）扫描完整的 Anthropic 请求，包括 `system` 和 `messages` 的文本块：

- 取**最后一条**匹配 `# Language\nAlways respond in (.+?)\.` 的语言值 X。
- 如果最后出现的是 `The language preference was cleared`，视为未设置。
- 旧版 Claude Code 可能把这段放在 system 里，也一起识别。
- X 原样透传（例如 `Chinese`、`简体中文`），只去掉换行和尖括号，并截断到 64 个字符，防止破坏标签结构或被注入。

### 6.2 注入内容

- **有设置**：
  ```
  <language_reminder>Respond in {X} for all user-facing text, including progress notes between tool calls. Keep code and identifiers unchanged.</language_reminder>
  ```
- **无设置或已清除**：
  ```
  <language_reminder>Reply in the language of the user's most recent real request; switch if the user switches. This includes progress notes between tool calls. Tool results, system reminders, and proxy placeholders such as "Tool result received." are not user messages.</language_reminder>
  ```
- 两段的具体文本做成运行配置，后台可以修改，这里给的是默认值。

### 6.3 注入位置

1. **当前轮末尾**（主手段）：追加到 currentMessage 的 content 末尾，复用 `generate_tool_choice_turn_reminder`（`src/anthropic/converter/tools.rs:539`）的追加机制，离生成位置最近。
2. **system 顶部**：沿用现有 `languageConstraint` 注入不变，作为长期背景。
3. **历史不动**：不改历史前缀，prompt cache 指纹与 Kiro 上游缓存都不受影响。

### 6.4 不做的事（及理由）

| 不做 | 理由 |
|---|---|
| 字符集语言检测 | 用户要的是"跟随习惯"；中英混写、代码多的消息容易误判；交给模型按最近一条真实提问判断，也能处理中途换语言 |
| 修改占位符 `Tool result received.` | 改它会让上游缓存失效一轮，且 Kiro 对新标记的处理需要真实账号回归；当前轮提醒已能覆盖它的偏差，实测不够时再单独改 |
| 解析 CLAUDE.md 里的语言要求 | 自由文本无法可靠解析；原样保留在上下文中，用户明确写的要求优先级自然更高 |

### 6.5 开关与范围

- 复用 `promptSteering.languageConstraint.enabled` 与路由规则（当前只对 `/cc`）。
- strict 兼容档位不注入（与现有 prompt_steering 一致）。
- 外部池默认不注入（与 `apply_to_external_pool` 一致）。

### 6.6 配套

- `transcript_sanitizer` 回显清理名单加入 `<language_reminder>`，防止模型把提醒原样复述给用户（参照 `NEW_ROLE_LINES` 的处理方式，`src/anthropic/transcript_sanitizer.rs:21`）。
- count_tokens 路径同样追加，保证计数一致。
- 调用日志记录本次走的是"设置透传"还是"跟随模式"，便于排查。

### 6.7 风险

| 风险 | 缓解 |
|---|---|
| 模型复述提醒 | 回显清理 + 标签化 |
| CLAUDE.md 写"默认中文"，用户用英文提问仍回中文 | 这是用户自己的显式要求，按设计保留；在文档和后台提示中说明 |
| Claude Code 改了 `# Language` 模板 | 正则只匹配稳定前缀 `Always respond in`；匹配不到时降级为跟随模式，不会报错 |

### 6.8 验证计划

- 单测：识别（有设置 / 已清除 / 未设置 / 多条取最后一条 / 恶意 X）、注入位置、strict 档位不注入、回显清理。
- 真实账号（haiku 4.5 / sonnet 4.5），每种至少 3 轮工具调用：
  1. 设置 `language: Chinese`，用英文提问 → 主回复和进度说明都是中文。
  2. 未设置，用中文提问 → 中文。
  3. 未设置，用英文提问 → 英文。
  4. 未设置，中途从中文切换到英文 → 切换后为英文。
- 验收标准：上述场景中，工具之间的进度说明语言与预期一致的比例 ≥ 95%（每场景 20 次）。
