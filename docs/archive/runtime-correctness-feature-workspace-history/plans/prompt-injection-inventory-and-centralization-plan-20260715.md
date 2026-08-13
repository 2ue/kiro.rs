# 上游请求提示词注入点审计与集中化方案

日期: 2026-07-15
范围: `kiro.rs` 网关中所有会把提示词 / 合成文本注入到**发往上游请求**里的位置。
状态: 分析 + 方案(尚未实施集中化改造)。

---

## 1. 背景与目标

网关在把 Anthropic Messages 请求转换 / 转发到上游(Kiro 本地路径或 `/cc` 外部池)时,会注入若干合成文本:
系统级行为规约、控制标签、工具描述后缀、以及各类占位符与裁剪留痕。

当前这些文本分散在 `converter/`、`payload_guard.rs`、`config.rs` 等多个文件,缺少统一入口和统一的"作用说明"注释。

目标:
- 摸清所有注入点(本文件第 3 节清单)。
- 把**真正的提示词文案**集中管理并加统一注释,便于审查与改文案。
- 不破坏文案与 gating 逻辑、payload_guard 裁剪、transcript_sanitizer 检测之间的既有耦合。

---

## 2. 关键区分:两类被注入的文本

这两类性质不同,**不应混在一起管理**:

- **第一类 — 提示词 / 行为规约(改文案会改变模型行为)**
  这是本次集中化的主要对象。包括 thinking 输出策略、tool_choice 策略、分块 Write/Edit 系统策略、Write/Edit 工具描述后缀,以及 `config.rs` 中两个可运行时覆盖的默认提示词。

- **第二类 — 机械占位符 / 运维说明(数据修复与裁剪留痕)**
  例如空消息占位 `Continue`、空结果占位、图片省略说明、文本截断标记等。它们与 `payload_guard` 裁剪逻辑、`transcript_sanitizer` 检测强耦合,**不建议**当作"提示词"集中,以免误导为可随意改文案。

> 耦合示例:占位符 `Continue`(C1)在 `transcript_sanitizer.rs` 有对应的泄漏检测常量配对;一旦改动需两边同步。

---

## 3. 注入点完整清单

除已处理的 `prompt_steering.rs` + `DEFAULT_LANGUAGE_CONSTRAINT_PROMPT` / `DEFAULT_TASK_QUALITY_PROMPT` 外的全部注入点。

### A. 合成控制标签(拼进 system / 转为 history user)

生成逻辑: `src/anthropic/converter/history.rs`。有 system 时按 "thinking 前缀 → tool_choice 前缀 → system 原文 → 分块策略" 拼接,整体转成一对 history `user` + `assistant("I will follow these instructions.")`;无 system 时单独生成一条 history user。

| 编号 | 位置 | 内容摘要 | 语言 | 触发条件 |
|---|---|---|---|---|
| A1 thinking 输出策略 | `converter/thinking.rs:9` `THINKING_OUTPUT_POLICY` | `<thinking_output_policy>...emit concise reasoning inside a <thinking>...</thinking> block...</thinking_output_policy>` | 英文 | `inject_thinking_prefix()` 且 `strict_output_policy` |
| A2 thinking 模式标签 | `converter/thinking.rs:23-36` `generate_thinking_prefix` | `<thinking_mode>enabled</thinking_mode><max_thinking_length>{budget}</max_thinking_length>` 等 | 纯标签 | 同 A1 gating,`req.thinking` 存在 |
| A3 tool_choice 策略 | `converter/tools.rs:259-284` `generate_tool_choice_prefix` | `<tool_choice_policy>Use at least one available tool...` / `Do not call tools in this turn.` 等 | 英文 | `inject_tool_choice_prefix()`,依据 `tool_choice` 字段 |
| A4 分块 Write/Edit 系统策略 | `converter/tools.rs:24-28` `SYSTEM_CHUNKED_POLICY` | `When the Write or Edit tool has content size limits, always comply silently...` | 英文 | `inject_chunked_policy()` |
| A5 合成 assistant 配对回复 | `converter/history.rs:101,118` | `I will follow these instructions.` | 英文 | 注入了 system 或任一合成前缀 |

### B. 工具 description 注入

| 编号 | 位置 | 内容摘要 | 语言 | 触发条件 |
|---|---|---|---|---|
| B1 Write 描述后缀 | `converter/tools.rs:16` `WRITE_TOOL_DESCRIPTION_SUFFIX` | `- IMPORTANT: If the content to write exceeds 150 lines...` | 英文 | `inject_chunked_tool_descriptions()` 且工具名 `Write` |
| B2 Edit 描述后缀 | `converter/tools.rs:19` `EDIT_TOOL_DESCRIPTION_SUFFIX` | `- IMPORTANT: If the new_string content exceeds 50 lines...` | 英文 | 同 B1,工具名 `Edit` |
| B3 空描述占位 | `converter/tools.rs:21,246-257` | `Tool available to the assistant.` / `Tool \`{name}\` available to the assistant.` | 英文 | 工具 description 为空(Kiro 要求非空) |
| B4 历史占位工具描述 | `converter/tools.rs:75` `create_placeholder_tool` | `Tool used in conversation history` | 英文 | 历史引用但 tools 列表缺失的工具 |

### C. 用户 / 工具结果占位符(第二类)

| 编号 | 位置 | 内容 | 语言 | 触发条件 |
|---|---|---|---|---|
| C1 空用户消息占位 | `converter.rs:69,542` | `Continue` | 英文 | current 消息文本为空 |
| C2 空 tool_result 占位 | `converter.rs:70` / `payload_guard.rs:33` | `Tool result content was empty.` | 英文 | tool_result 内容为空 |
| C3 tool_result 图片占位 | `converter/content.rs:14` | `[image attached]` | 英文 | tool_result 含图片 |
| C4 重复/孤立 tool_result 转文本 | `converter/tool_pairing.rs:99,112,123` | `[duplicate output]` / `[previous output]` | 英文 | 兼容模式下转普通文本 |
| C5 guard 孤立 tool_result 转文本 | `payload_guard.rs:2467` | `[trimmed orphan tool_result {id}]` | 英文 | payload guard 移除未配对 tool_result |

### D. Payload 裁剪 / 省略说明(第二类)

| 编号 | 位置 | 内容摘要 | 语言 | 触发条件 |
|---|---|---|---|---|
| D1 历史图片省略 | `payload_guard.rs:1685-1687,2115` | `[Historical image was omitted because it exceeded the upstream 5 MB image size limit.]` | 英文 | 历史图片超 5MB |
| D2 当前图片省略 | `payload_guard.rs:3528-3540,3860-3915` | `[Current image was omitted because it exceeded the request image budget.]` 等 | 英文 | 当前图片超预算 / 超 5MB |
| D3 文本截断标记 | `payload_guard.rs:2679,2690,2722,3752` | `[{label} truncated by proxy: original_chars=..., preserved=...]` | 英文 | 对应内容超 `max_chars` |
| D4 web fetch 裁剪说明 | `payload_guard.rs:2876` | `[Proxy note: web page navigation, repeated links, and image data were trimmed...]` | 英文 | web fetch 正文超限 |

### 已处理(config.rs,可运行时覆盖)

| 位置 | 常量 | 说明 |
|---|---|---|
| `config.rs:200` | `DEFAULT_LANGUAGE_CONSTRAINT_PROMPT` | 已改为英文通用语言规则 |
| `config.rs:210` | `DEFAULT_TASK_QUALITY_PROMPT` | 仍为中文;是否统一为英文待定 |

---

## 4. 排除项(看似注入实则不是)

- `websearch.rs:443` `generate_search_summary` — 构造返回给客户端的 SSE 响应,非上游请求注入。
- `handlers.rs:909` `fallback_question_for_ask_user_question`(`Please choose an option.`)— 回传给 CLI 的响应参数修复,非上游注入。
- `transcript_sanitizer.rs` 的 `user Continue` 等常量 — 响应流泄漏检测/清洗标记,非注入。
- `converter.rs:2082/2111/2144`、`prompt_cache.rs:1417`、`external_pool/tests.rs` 里的 `You are a helpful coding assistant` — 全在 `#[cfg(test)]` 测试代码,不注入生产请求。
- `LEGACY_TASK_QUALITY_PROMPT_V1/V2`(`config.rs`)— 仅用于配置迁移比对,不注入。

---

## 5. 集中化方案

### 5.1 原则

- 只集中**第一类静态文案常量**,不搬第二类占位符/裁剪说明。
- **生成逻辑函数留在原地**(标签拼接、gating 判断不动),仅引用集中后的常量。
- `config.rs` 的两个 `DEFAULT_*` 提示词**保留在 config**,因为它们可被运行时配置覆盖,归属 config 正确。

### 5.2 具体步骤

1. 新建 `src/anthropic/converter/injected_prompts.rs`,收拢静态文案常量:
   - `THINKING_OUTPUT_POLICY`(A1)
   - `SYSTEM_CHUNKED_POLICY`(A4)
   - `WRITE_TOOL_DESCRIPTION_SUFFIX`(B1)
   - `EDIT_TOOL_DESCRIPTION_SUFFIX`(B2)
   - tool_choice 三条 policy 文案(A3,标签拼接逻辑仍留在 `tools.rs`)
2. 每个常量上写**统一格式注释**:注入到哪(system / tool description / history)、触发开关(哪个 `promptSteering` 配置 + gating)、作用。
3. `thinking.rs` / `tools.rs` 的生成函数改为引用集中常量,标签结构与 gating 不动。
4. 在 `injected_prompts.rs` 顶部写**模块级全景索引注释**:列出所有注入点(含 config 里两个 DEFAULT、第二类占位符)分别在哪个文件,形成单一查阅入口。
5. 第二类(C/D)不搬,最多在各自文件补一行用途注释。

### 5.3 收益与保护

- 审查/改"提示词"时有单一入口 + 统一注释。
- 不破坏文案与 gating、payload_guard、transcript_sanitizer 的耦合。
- 避免制造"看起来能改其实不能改"的陷阱。

---

## 6. 待确认事项

1. 集中范围:按 5.2 只集中第一类 + 全景索引注释,还是也要搬第二类占位符?(建议:只集中第一类)
2. `config.rs` 两个 DEFAULT 提示词:留在 config(推荐,可运行时覆盖)还是挪进新模块?
3. `DEFAULT_TASK_QUALITY_PROMPT` 是否统一改为英文?若改,需把当前中文版追加为新的 `LEGACY_TASK_QUALITY_PROMPT_V*` 常量以保证旧配置平滑迁移。
