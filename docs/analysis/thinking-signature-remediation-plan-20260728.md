# Thinking Signature 官方机制与改造方案

日期：2026-07-28
范围：只读源码分析 + 两轮 codex CLI（`codex exec --sandbox read-only`）多 agent 头脑风暴与对抗式自检，**未修改任何代码，未运行 `cargo test`**。
方法：主分析给出官方机制与项目缺陷断言 → 4 个 codex agent 分层拆解（A 阻断项修复 / B HIGH 项 / C 架构与配置 / D 红队风险与测试）→ 第二轮 codex 对 A 的设计做对抗式自检 → 主分析交叉核对全部引用的 file:line 与当前代码一致。
关联文档：
- 现状（签名怎么处理、session 是否一致）见 [`kiro-upstream-signature-and-fingerprint-analysis-20260727.md`](./kiro-upstream-signature-and-fingerprint-analysis-20260727.md)
- 早期兼容性设计（**部分已过时**，见本文 §6）见 [`../archive/request-and-protocol-history/anthropic-tools-signature-compatibility-analysis.md`](../archive/request-and-protocol-history/anthropic-tools-signature-compatibility-analysis.md)

> 本文只做分析与方案设计。任何代码改动前需再次确认。

---

## 1. 官方机制（详细）

### 1.1 signature 是什么

thinking 块上的 `signature` 是**对整段 thinking 内容的加密表示（encrypted representation）**，不是哈希、不是 HMAC、不是校验和。（F1）

- 由 Anthropic 服务端在生成 thinking 时签发；客户端把它原样带回，服务端用它验证这段 thinking 确实由 Claude 生成且未被篡改。
- 因此它是**不透明的（opaque）**：客户端不允许扫描内容、改写、断言长度、重算。（F1/F2）
- `redacted_thinking.data` 同理，也是不透明加密块，规则完全一致。（F2/F12）

### 1.2 绑定维度：模型绑定，不是会话绑定（F3）

signature 与**模型**绑定，不与**会话**绑定，可在 Claude API / Bedrock / Vertex 之间互换使用，只要模型一致。

这纠正了一个常见误问："每个 session 是否一样"这个问法本身是错的——正确维度是"模型是否一致"。会话隔离不是官方机制；跨会话复用同模型的 thinking 块是合法的。（现状层面的 session 分析见 07-27 文档 §5）

### 1.3 客户端唯一正确行为：逐字回传（verbatim round-trip）

对 signature / redacted data 的唯一合法操作是原样带回。禁止：伪造、占位、合并、重排、重算、补长度、改 base64 规范化。（F2/F12/F14）

### 1.4 保留规则的三档强度（F14）

| 场景 | 规则 |
| --- | --- |
| 工具调用回合内（tool-use turn） | **必须（REQUIRED）** 逐字回传最新 assistant 的 thinking+signature |
| 跨回合（cross-turn） | **建议（RECOMMENDED）** 保留 |
| 非工具场景 | **允许（ALLOWED）** 省略 |

特殊合法态：`display:"omitted"` → `thinking:""` + signature 仍在，这是合法状态，不能因 text 空就丢 signature。（F14）

### 1.5 交错思考：一条 assistant 消息可含多个 thinking 块（F13）

- 开启 interleaved thinking 时，一条 assistant 消息可以有多个 thinking 块，每块各自带 signature，可与 text、tool_use 交错排列，例如 `[thinking(sigA), text, thinking(sigB), tool_use]`。
- 工具回合内，最新 assistant 消息的连续 thinking 块不能被重排、编辑、部分丢弃，否则服务端 400。redacted_thinking 同受此约束。

### 1.6 事实编号（Fact，anchor 用）

- F1：signature = 对整段 thinking 的不透明加密表示；非 HMAC/hash；不扫描/改写/断言长度。
- F2/F12：signature 与 `redacted_thinking.data` 都不透明；逐字回传；无内容/长度断言。
- F3：signature 模型绑定，非会话绑定；跨 Claude API/Bedrock/Vertex 可互换。
- F13：一条 assistant 消息可含多个交错 thinking 块，各带 signature；工具回合内最新 assistant 连续 thinking 块不得重排/编辑/丢弃。
- F14：工具回合内必须逐字回传；跨回合建议保留；非工具允许省略；`display:omitted` → `thinking:"" + signature` 合法。

---

## 2. 项目缺陷全景（已核对 file:line，与当前代码一致）

| 编号 | 位置 | 违反的官方事实 | 严重度 |
| --- | --- | --- | --- |
| P5 | `payload_guard.rs:1983`（同模式还在 1060/1235/1262/2116/2307/2656）+ `discard_anthropic_history_thinking`（`payload_guard.rs:2229`）；默认 `discard_historical_thinking=true`（`config.rs:2374`、`5640`） | F13/F14：工具回合内最新 thinking 被裁进删除区并剥离 | **BLOCKER** |
| P2 | `set_native_reasoning_content`（`converter/history.rs:397`，第二块报错文案在 403 行）；合并路径 `merge_assistant_messages_with_known_tools`（`history.rs:431`） | F13：第二个 reasoning 块直接返回 Err → 交错多块本地被拒，请求发不出去 | **BLOCKER** |
| F12-违规 | `types.rs:83` `MAX_REDACTED_THINKING_DECODED_BYTES=768*1024`；`validate_redacted_thinking_data`（`types.rs:85`）空检查+长度上限+base64 解码+规范再编码 | F2/F12：对不透明块做内容/长度断言，严于"不透明" | HIGH |
| HIGH2 | `transcript_sanitizer.rs:461` `thinking_sanitizer.push(signature)`（所在函数 `sanitize_assistant_run`，块处理 428-474） | F1/F2：把不透明 signature/redacted data 送进 sanitizer 扫描，可能删掉已签块 | HIGH |
| P9 | 重试剥离：`provider.rs:10986` `thinking_signature_retry_body_builder.take()`；`handlers.rs:6104` `build_thinking_signature_retry_body`（调用点 6042/6087/6333/6381）；`AssistantMessage`（`conversation.rs:345`）无 model 字段 | F3：无模型维度，重试无脑剥离全部 thinking | HIGH |
| P6 | `clear_history_reasoning_content`（`conversation.rs:91`）无差别 `.take()` 每个 assistant 的 reasoning | 配合 P5 修复，否则会毁掉受保护的最新 thinking | 配合修复 |
| P1 | `ReasoningContent` 单值无标签联合（`conversation.rs:374`）；`AssistantMessage` 持单个 `reasoning_content`（345）+ 独立 `tool_uses`（342） | **未知——需抓包**，阻塞 BLOCKER 2 分支决策 | 阻塞 |

> P1-unknown：Kiro 上游是否只接受单值 `reasoningContent`、是否校验 thinking↔tool_use 的交错顺序，目前**没有抓包证据**。整个 BLOCKER 2 的 Branch A/B 取舍都卡在这里。

---

## 3. BLOCKER 1 — 裁剪窗口丢弃最新 assistant thinking

### 3.1 根因（已确认）

`payload_guard.rs:1983` `history_end = messages.len().saturating_sub(1)` 只把数组最后一个元素当"当前"。工具回合里最后一个元素通常是 `user(tool_result)`，于是最新 assistant（携带刚产出的已签 thinking）落进 `messages[..history_end]`，被 `discard_anthropic_history_thinking`（`payload_guard.rs:2229`）无条件 `retain` 掉每个 thinking/redacted_thinking 块。默认 `discard_historical_thinking=true`（`config.rs:2374`）使其每次请求都触发。违反 F13/F14 → 400。

### 3.2 推荐修复：结构化受保护边界（不是"沿角色连跑回退"）

> 第二轮 codex 对抗式自检**推翻了**第一轮"沿连续 assistant 角色回退"的写法。F13 保护的是"工具续写里那一条最新 assistant 消息内部的连续 thinking 块"，不是任意同角色连跑。沿角色回退会把更老、可能不同模型（F3）的 assistant 拖进保护区，反而钉住一个自己就会 400 的陈旧 signature，让请求更难修复。

正确边界是**结构化且带条件**的：

```text
end = messages.len() - 1                              // 默认:只有最后一个元素算"当前"
last_asst = messages.rposition(role=="assistant")     // 无 → 返回 end
if is_tool_use_continuation(messages, last_asst):     // 靠 tool_use id ↔ 后续 tool_result id 匹配判断
    end = last_asst                                   // 只保护这一条 assistant,不往前走
// 非工具续写:F14 允许跨回合省略 → 故意不强制保护(否则可能钉住模型不匹配签名→400)
discard_anthropic_history_thinking(&mut messages[..end])
```

- `is_tool_use_continuation` 用**结构**（tool_use id ↔ 后续 tool_result id 配对）判断，而非相邻位置——因为 Kiro 把 `tool_uses` 建成独立顶层字段（`conversation.rs:342`），光看 role 分不清工具回合和普通 assistant 回合。该函数当前**不存在**，是待新增的。
- 只保护一条 assistant（续写来源），不往前走。

边界与边界情形：
- 无 assistant → `end = len-1`（不变，安全）。
- 最后一个元素已是 assistant（无尾随 user）→ `rposition` 命中它，continuation 判否 → 行为同现状。
- 非工具 assistant 回合 → 有意不保护（F14 允许跨回合省略；强制保护会有模型不匹配 400 风险）。

### 3.3 必须一起改的调用点（否则修复静默漂移）

`len-1` 这个边界在多处重复，且 `clear_history_reasoning_content`（`conversation.rs:91`，P6）会无差别清掉所有历史 reasoning——若在工具续写上被调用，会毁掉受保护的最新 thinking。**必须抽一个共享 helper，所有"历史 vs 受保护最新"的判断都走它**：

- `payload_guard.rs:1060`（`original_history_entries`）
- `payload_guard.rs:1235`（`final_history_entries`）
- `payload_guard.rs:1262`（`anthropic_history_reasoning_content_stats`）
- `payload_guard.rs:1983`（P5 主位置）
- `payload_guard.rs:2116`、`2307`（同模式）
- `conversation.rs:91`（P6 `clear_history_reasoning_content`）

### 3.4 图片裁剪保持独立

历史图片丢弃（`payload_guard.rs:1991-1998` 附近）与当前图片丢弃（`2022-2033` 附近）是硬上游约束，**不要复用改过的 protected 边界**，否则会误伤既有测试 `anthropic_guard_drops_oversized_historical_images_even_when_body_fits`。

### 3.5 状态

结构化边界 + 共享 helper 集中化，**现在就能安全落地**，是低风险止血项。唯一前置是抓包场景 #8（确认"只保护最新"是否充分，见 §7）。

---

## 4. BLOCKER 2 — Kiro `reasoningContent` 单值；第二块报错

### 4.1 根因（已确认）

`set_native_reasoning_content`（`history.rs:397`）在遇到第二个 reasoning 块时返回 Err，文案（403 行）："assistant history contains multiple or mixed native reasoning blocks; Kiro accepts one reasoningContent union value per assistant message"。合并路径 `merge_assistant_messages_with_known_tools`（`history.rs:431`，调用 `set_native_reasoning_content` 在 470）同样报错。

后端建模：`ReasoningContent` 是单值无标签枚举（`conversation.rs:374`），`AssistantMessage`（`conversation.rs:334-359`）持有一个 `reasoning_content: Option<ReasoningContent>`（345）**加一个独立** `tool_uses: Option<Vec<ToolUseEntry>>`（342）。因此一条合法 F13 assistant（`thinking, tool_use, thinking, tool_use`）无法被表示。

### 4.2 决定性发现（第二轮对抗自检，最严重的洞）

**即使走 Branch A（把 `reasoning_content` 改成 Vec）也解决不了 F13。** 两个并行数组 `reasoningContent:[A,B]` + `toolUses:[1,2]` **丢失了 thinking 与 tool_use 之间的位置交错**。如果 Kiro 校验原始 `thinking→tool_use→thinking` 顺序，任何字段基数改动都无效——需要**位置化内容模型**，一个更深的 schema 变更。Kiro 是否校验交错顺序**未抓包验证**。因此 Branch A vs B 凭事实无法选定。

### 4.3 Branch A — 数组/位置化（仅当抓包证明 Kiro 接受）

- 最小：`reasoning_content: ReasoningContents(Vec<ReasoningContent>)` newtype。恰好 1 块时序列化为单 OBJECT（保留今天已验证的线格式），>1 块时为 ARRAY。
- 内容相关的形状意味着一个请求里不同 assistant 消息序列化不同（object vs array），仅当上游字段 schema 是真正的 per-value 联合时安全；若 schema 是统一/生成式的则失败。
- **必须同时实现自定义 Deserialize** 以在两种形状下 round-trip Kiro **响应**（第一轮只说了 serialize）。检查 reasoning 结构体的 `deny_unknown_fields` 不会拒绝上游不透明字段（F2/F12）。
- 两条转换路径（`history.rs:397` 与 `431`）都必须按源顺序 push；若跨两条 assistant 消息合并无法保序，须 **fail closed**，绝不静默重排。
- 若交错顺序重要且 Kiro 无位置化形态 → Branch A **不可能靠字段微调实现**，需真正的 schema/模型改动。

### 4.4 Branch B — 单值 fail-closed（若 Kiro 只接受单值）

- 当受保护的最新 assistant 有 >1 个已签/redacted 块 → 返回 `ConversionError`（绝不合并/丢弃/伪造——F1/F2/F13）。
- 仅在 `messages[..protected_history_end]` 内按配置丢弃 thinking。
- 阻止 `THINKING_SIGNATURE_INVALID` 重试（P9：`provider.rs:10986`、`handlers.rs:6104`）剥离受保护的最新块。
- **死角**：strip-all 不是安全兜底。F14 要求工具回合内必须回传 thinking，一个无 thinking 的工具回合很可能**同样 400**——把一个非法请求换成另一个。**Kiro 是否容忍无 thinking 的工具回合是关键未知**（抓包场景 #5），在证实之前不能声称有降级路径。

### 4.5 取舍小结

Branch A = 广义正确，前提是上游支持数组/位置化且接受 schema 变更风险；Branch B = 改动小、诚实失败，但对真正的多块最新回合是死角。**两者都无法在抓包前选定。**

---

## 5. 横切问题（对抗轮浮现，两个 BLOCKER 都适用）

### 5.1 模型溯源缺失（F3，P9）

边界保护不够：一个由**不同模型**产生的受保护块，即使原样重发，仍会 `THINKING_SIGNATURE_INVALID` 400。今天没有"当前模型 vs 块模型"的维度。重试必须从"按边界"改成"按分类"：

```text
{ 受保护/当前模型, 受保护/模型不匹配, 历史/当前模型, 历史/模型不匹配, 未知 }
```

- 省略历史的不同模型块；绝不动工具续写里受保护的最新块；受保护块模型不匹配时 fail closed 报具体错，除非抓包证明有无 thinking 降级路径。
- 需在创建时按块存模型溯源（记录解析后的真实上游模型，不是 alias，不是 `UserInputMessage.model_id`）。

### 5.2 redacted-thinking 校验（F2/F12）

任何对 `redacted_thinking.data` 的长度/内容/可解码/结构断言都是非法的。把 `validate_redacted_thinking_data`（`types.rs:85`）降为**纯语法校验**：只要求 `type=="redacted_thinking"` 且 `data` 是 JSON 字符串 + 逐字保留字节。移除空检查、长度上限（`MAX_REDACTED_THINKING_DECODED_BYTES`）、base64 解码、规范再编码、以及只用于调试日志的 `usize` 返回。资源防护改用**全局入口 body 大小限制**（路由已有）。

> 风险分离：内容校验（F12 违规）与资源风险（不透明字节无界）是两回事。只删长度上限却保留 decode+re-encode 会在并发下放大内存，所以要**同时删掉 decode/re-encode**，用全局 body 限额兜底资源。

### 5.3 transcript_sanitizer 扫描不透明块（HIGH2，F1/F2）

`transcript_sanitizer.rs:461` 把 signature 送进 `thinking_sanitizer.push()`。改法：signature 与 redacted data **永不被扫描**；删除该 push；把已签 thinking + 全部 redacted 视为协议原子；仅**非工具窗口外**的未签 thinking 可清洗，未知情形 fail closed。

### 5.4 可观测性漂移

§3.3 的 helper 集中化同时覆盖此项：`payload_guard.rs:1262` 等处的陈旧 `len-1` 统计会掩盖修复是否生效。

---

## 6. 与早期兼容性文档的对账（[`../archive/request-and-protocol-history/anthropic-tools-signature-compatibility-analysis.md`](../archive/request-and-protocol-history/anthropic-tools-signature-compatibility-analysis.md)，日期 2026-05-31）

该文档整体方向仍成立（§3.1 签名不能伪造、§7 签名过检、§11.3 payload guard 影响签名），但有一处描述已**过时**，以本文为准：

- 该文 §4.7 "assistant 历史中的 thinking signature 当前会丢失"，描述的是把 thinking 拼进 `<thinking>{}</thinking>` 普通文本、`signature` 不保留的旧状态。**当前代码已改为**在 `converter/history.rs:397` 把历史 `thinking.signature` 提取进 native `reasoningContent`（见 07-27 文档 §4）。因此"完全不保留"已不准确；真正的现存缺陷是本文 P2（多块被拒）、P5（裁剪窗口剥离）、P9（重试无脑剥离），而非"拼文本丢弃"。

其余章节（工具链路要求、模式设计、验收清单）不冲突，可继续参考。

---

## 7. 抓包前置矩阵（BLOCKING）

第一轮的 3 请求测试（object / 1 元素数组 / 2 元素数组）只能回答"Kiro 是否接受数组"，不够。所需矩阵，除注明外均用**同模型、逐字捕获**的块：

| # | 请求形状 | 解决的问题 |
| --- | --- | --- |
| 1 | 单 object `reasoningContent` | 基线（今天可用） |
| 2 | 1 元素数组 | 数组是否被接受 |
| 3 | 2 元素数组 | 多块是否被接受？Branch A 可行？ |
| 4 | **交错** thinking→tool_use→thinking→tool_use（分离的 `reasoningContent`+`toolUses` 数组） | Kiro 是否校验交错顺序？（定 Branch A 可行性，§4.2） |
| 5 | 最新工具回合 thinking **完全省略** | strip-all 是有效降级还是也 400？（定 Branch B 死角，§4.4） |
| 6 | 最新 thinking **重排** | 确认 F13 重排→400 在 Kiro 上成立 |
| 7 | **模型不匹配**：块来自模型 X，请求模型 Y | 确认 F3 → 400；量化溯源工作量（§5.1） |
| 8 | 更老工具回合 thinking 省略、最新保留 | "只保护最新"是否充分？（定 BLOCKER 1 充分性，§3.5） |
| 9 | `display:"omitted"` → `thinking:""` + signature | 确认 F14 合法性在 Kiro 上成立 |
| 10 | 捕获 Kiro **响应**形状（object vs array；1 元素是否归一化） | Branch A Deserialize 需求（§4.3） |

抓包端点：IDE `POST /generateAssistantResponse`（`kiro/endpoint/ide.rs`）或 CLI runtime（`kiro/endpoint/cli.rs`），全新 `conversationId`、同模型、同头部凭据。检查 HTTP 状态、AWS event-stream 帧头、`assistantResponseEvent`/`reasoningContentEvent`/`metadataEvent`/`invalidStateEvent`（`kiro/model/events/base.rs`）、reasoning 负载 `text`/`signature`/`redactedContent`（`kiro/model/events/additional.rs`）。

判定预期：F13/F14 下只有逐字/同模型通过；#4/#5/#8 任一通过都是 Kiro 特有容忍，必须显式 compat flag 门控，绝不默认假设。

---

## 8. 配置与默认值治理

- `discard_historical_thinking=true`（`config.rs:2374`、`5640`）**不应是默认值**。
- 建议策略枚举：
  - `thinking_history_policy`（默认 `preserve_all`）
  - `redacted_thinking_validation`（默认 `opaque`）
  - `thinking_model_policy`（默认 `pin_required/reject`）
  - `signature_invalid_retry`（默认 `strip_all_once`，且仅在首发已保留后）
- 推荐默认组合：首发 `preserve_all` + `opaque` 校验 + 模型要求回合 pin/reject + `strip_all_once` 重试并打观测标记。

社区参考：可借 LiteLLM 的保序/保基数/保块边界/零不透明字段改写/工具回合保护最新 assistant 连续 thinking/模型维度纳入策略（对照 issue LiteLLM #20698、#23047 的失败教训）；**不可照搬**其双 provider 原生透传——本项目上游无 Anthropic-native 回退通道。

---

## 9. 落地优先级

**BLOCKING（动任何代码前解决）：**
1. 抓包 #3/#4/#10 → 决定 Branch A vs B（数组支持 + 交错校验 + 响应形状）。
2. 抓包 #5 → 决定 Branch B 是否有降级路径或是死角。
3. 抓包 #8 → 决定 BLOCKER 1"只保护最新"是否充分。
4. 抓包 #7 → 量化模型溯源工作量（F3）。
5. 用**结构化**（工具续写检测）而非角色连跑定义受保护单元。

**现在可安全落地（低风险，不依赖抓包）：**
- BLOCKER 1 结构化边界 + 跨 `payload_guard.rs:1060/1235/1262/1983/2116/2307` 与 `conversation.rs:91`（P6）的单一共享 helper。这一项就能止住默认配置下剥离最新 thinking 的血。
- redacted 校验降为语法/存在性 + 逐字（F2/F12）。

**抓包后高优先级：**
- 按抓包结论落 Branch A（数组+位置化+双形状 Deserialize）或 Branch B（fail-closed）。
- 用基于分类的重试替换无脑 strip-all（P9）；按块加模型溯源。

**后续：**诊断错误归因（多块 vs 模型不匹配 vs 数组不支持 vs 无效历史）；回归 fixture（单块/多块/交错/redacted/`omitted`/模型切换/历史丢弃）；区分省略原因的可观测性计数器。

---

## 10. 结论

- **BLOCKER 1** 有安全、可交付的结构化修复（结构化边界 + helper 集中化 + redacted 校验放松），可现在就做。
- **BLOCKER 2** 无法仅靠代码关闭：Branch A / Branch B / "需要更深位置化模型"三者凭事实无法选定，须先抓 #3/#4/#5/#8/#10。
- 模型溯源缺口（F3）意味着即使边界修得完美，模型切换时仍会 400——这是独立于两个 BLOCKER 的横切硬伤。
- 全程无任何 signature 被伪造/合并/占位。

### 附：本轮方法记录

- 两轮 codex CLI 均 `--sandbox read-only`，未改代码、未跑 `cargo test`。
- 第一轮 4 agent（A 阻断项 / B HIGH 项 / C 架构配置 / D 红队测试）；第二轮对 A 的边界与分支设计做对抗式自检，推翻了"角色连跑回退"并暴露了 §4.2 交错丢失、§4.4 strip-all 死角、§5.1 模型溯源三处硬洞。
- 本文所有 file:line 已由主分析回读当前代码交叉核对一致（截至 2026-07-28）。

