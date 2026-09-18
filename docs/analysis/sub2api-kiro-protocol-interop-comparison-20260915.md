# sub2api-kiro 与当前 Rust 实现的协议互操作对比

> 调研日期：2026-09-15
> 调研目的：对比 `../sub2api-kiro` 在 Kiro 上游协议解析、Claude Code/Anthropic ↔ Kiro 协议互转方面的实现，判断是否有值得当前 Rust 项目借鉴的设计、测试和运维经验。
> 本文只做源码分析，**未修改业务代码，等待确认后再决定是否实施任何候选动作**。

## 1. 调研基线与证据纪律

### 1.1 双方基线

| 项目 | 路径 | 语言 | 当前 HEAD | 最近提交 |
|---|---|---|---|---|
| 当前项目 | `/Users/yuanfeijie/Desktop/procode/2ue_kiro.rs` | Rust | `08751f28590013d95ae9c2c34ad00e8f1527c1fa` | `2026-09-15 17:47:13 +0800 style: satisfy rustfmt in warmup batch regression test` |
| 对比项目 | `../sub2api-kiro` | Go | `7a4b4fe999c71a24a6b018d977081ef57a1a43bb` | `2026-09-15 01:25:44 +0800 feat(cursor): 纯文本导入支持 refresh token，并修正账号命名` |

两个工作区在本次核对开始时均为干净状态。本文结论只针对上述 commit，不自动代表两个仓库未来版本。

### 1.2 证据标记

| 标记 | 含义 |
|---|---|
| `[Rust]` | 当前 Rust 仓库源码或测试直接证明 |
| `[sub2api]` | `../sub2api-kiro` 源码或测试直接证明 |
| `[推断]` | 根据两个实现的差异推导出的工程建议，尚未由真实上游单独证明 |
| `[待验证]` | 需要真实 Kiro/Claude Code 抓包、回放或线上行为确认 |

必须区分两件事：

1. “某实现这样写”只能证明该实现的兼容策略，不等于 Kiro 上游协议就要求这样做。
2. 当前 Rust 与 sub2api 可能使用了共同社区经验；同一行为同时出现，不自动构成独立上游证据。

### 1.3 对旧调研的修正

`../sub2api-kiro/docs/kiro-protocol-ecosystem-analysis.md` 的基线是 2026-09-13，不能直接作为当前 Rust 缺口清单。至少以下两条已被当前 HEAD 追平或反超：

- **工具名字符清洗不再是 Rust 缺口。** 当前 `src/anthropic/converter/tools.rs:102-199` 已有非法字符清洗、camelCase 归一化、长度限制、SHA-256 确定性映射、collision 检测和 reverse map。
- **upstream cache usage 不再是 Rust 缺口。** 当前 `src/kiro/model/events/additional.rs:33-75`、`src/anthropic/stream.rs:2420-2478` 和 `src/anthropic/cache.rs` 已解析、合并并输出 metadata/cache usage。相反，sub2api 当前仍明确只消费本地 cache emulation，忽略上游 cache fields（`translator.go:4592-4594`）。

因此，本文重点不是重复列出“谁功能更多”，而是识别：

- sub2api 在**兼容矩阵、端到端故障样本、阶段化降级和可观测性**方面的经验；
- 当前 Rust 在**类型边界、解析严谨性、状态机、错误语义和安全修复边界**方面已经更强的地方；
- 哪些差异值得形成测试或小型适配层，哪些不应该照搬。

## 2. 总体结论

### 2.1 一句话结论

**有值得学习的地方，但主要是测试组织、语义事件适配、payload guard 的阶段化记录和 WebSearch 的 SSE 索引处理；不建议把 sub2api 的单体 translator、全量 `map[string]any`、宽泛 JSON repair、合成 thinking signature 或“无条件忽略 upstream cache”直接移植到当前 Rust。**

### 2.2 能力分层判断

| 领域 | 当前 Rust | sub2api | 判断 |
|---|---|---|---|
| AWS EventStream 底层帧 | CRC、边界、分片、恢复、EOF 完整性均有结构化实现 | 单体读取，主要抽取 event type，不校验 CRC | Rust 明显更强；借鉴上层 shape 兼容，不替换底层 parser |
| Kiro typed event | typed enum + typed payload，unknown/error/exception 有独立分支 | 宽松 JSON map + nested/top-level fallback | Rust 主路径更稳；可借鉴 semantic adapter |
| endpoint/profileArn | endpoint trait、credential-aware resolver、unicode key 防绕过 | translator 中统一处理 origin/history/profile 语义 | Rust 结构更清晰；借鉴 endpoint fixture 和联合断言 |
| 工具名 | 确定性映射、collision 拒绝、旧名称兼容 | 清洗/缩短，端到端合法性测试样本丰富 | 借鉴 sub2api 的故障样本和 payload legality test |
| JSON Schema | 递归、保语义、支持多种 legacy/combinator 归一化 | Smithy 兼容白名单，简单且保守 | Rust 保留主策略；借鉴“accepted subset”契约测试 |
| tool pairing | strict/repair、历史占位工具、当前轮校验 | 同样完整，另有 trim 后配对恢复经验 | 优先检查 Rust 是否覆盖 trim 后恢复场景 |
| thinking | native reasoning、真实/伪造签名边界、安全过滤和 retry | 模型 alias、tag buffer、合成 signature 覆盖广 | 借鉴 stream fixture；不直接移植 signature |
| embedded tool/XML | 当前已支持多种 tag、partial buffer、代码围栏和已知工具约束 | `[Called tool with args: ...]` 解析器和跨 chunk pending tail 清晰 | 借鉴 fixture 与 pending tail 语义 |
| WebSearch | MCP 路径和 Anthropic server blocks 已有独立实现 | buffered SSE 分析、过滤和 index remap 更通用 | 借鉴 index helper，但限制作用域 |
| usage/cache | Rust 解析 upstream cache 并与本地投影分层 | 大整数 `UseNumber`、credit alias 有小优点；无条件忽略 upstream cache | 借鉴数值保真与 alias；不照搬 cache policy |
| payload guard | 双方向 guard、报告、repair、shaping、retry pipeline 已完整 | 加权阈值、阶段名、deferred/still oversized 语义很明确 | 借鉴阶段化观测和真实阈值实验方法 |
| error/failover | failure kind 与 retry/scheduler/cooldown 已连通 | 400 细分和诊断理由较丰富，但需逐调用链确认消费情况 | 借鉴 message 子分类和 call-site test，不复制 status policy |

### 2.3 最值得确认的候选项

按“收益/风险/改动范围”排序：

1. **P0：建立互操作 fixture inventory 和 contract tests。**
2. **P1：增加 semantic event adapter 的兼容矩阵，不破坏 typed parser。**
3. **P1：核对 payload trim 后 tool pair 是否始终重新收敛，并补回归测试。**
4. **P1：把 payload guard 的阶段、deferred、still oversized 和真实阈值记录统一成可观测字段。**
5. **P2：为 WebSearch buffered SSE 建立局部 index remap helper。**
6. **P2：把具体 400 message 的诊断分类与 retry/failover 消费关系用测试锁住。**
7. **单独评估：synthetic thinking signature。默认不启用，除非有真实上游/Claude Code 证据。**

## 3. 架构与边界对比

### 3.1 当前 Rust 的边界

当前 Rust 大体分为四层：

```text
Anthropic request
  -> converter/content/history/tools/schema/tool_pairing/thinking
  -> Kiro typed request + endpoint/profileArn policy
  -> payload guard / shaping / retry
  -> Kiro HTTP + AWS EventStream decoder
  -> typed Kiro Event
  -> StreamContext/SSE state manager/transcript sanitizer
  -> Anthropic response
```

证据：

- 请求转换总流程：`src/anthropic/converter.rs:430-550`
- Kiro EventStream parser：`src/kiro/parser/frame.rs:63-153`、`src/kiro/parser/decoder.rs:154-310`
- typed event dispatch：`src/kiro/model/events/base.rs:81-208`
- 响应事件处理：`src/anthropic/stream.rs:2395-2544`

这个边界的优点是：协议解析错误、语义转换错误、SSE 生命周期错误和 scheduler/retry 错误可以分别观测，不需要让一个巨大函数同时承担所有责任。

### 3.2 sub2api 的边界

sub2api 主要以 `backend/internal/pkg/kiro/translator.go` 为中心，统一处理：

- Anthropic request → Kiro payload；
- Kiro EventStream → 中间语义事件；
- 非流式 response；
- streaming SSE；
- embedded tool text；
- thinking tags/signature；
- usage/cache projection。

优点是改一个兼容策略时可以快速贯通请求、响应和测试。缺点是：

- 上游 shape 兼容和严格协议解析容易混在一起；
- `map[string]any` fallback 过多时，schema 错误可能被静默转成普通文本或空值；
- 单体 translator 的调用链不容易证明某个分类、重试或过滤逻辑真的被消费。

**建议：** 当前 Rust 保留现有模块边界；如果需要 sub2api 的灵活性，新增的是“语义事件适配层”或“特定 endpoint compatibility profile”，而不是把现有 typed model 改成无类型 map。

## 4. Kiro 上游协议解析

### 4.1 EventStream 底层帧解析

#### 当前 Rust 的实现

`src/kiro/parser/frame.rs:23-30,63-153` 实现：

- total length、header length；
- prelude CRC；
- message CRC；
- 16 MB 最大帧；
- header 边界；
- `Ok(Some(frame)) / Ok(None) / Err` 三态返回。

`src/kiro/parser/header.rs:8-24,122-257` 支持 AWS EventStream 的全部 10 类 header value type。`src/kiro/parser/decoder.rs:87-310` 负责：

- 分片 feed；
- buffer 上限；
- 连续错误计数；
- prelude 错误逐字节恢复；
- data 阶段错误按帧跳过；
- `pending_bytes()`、`is_clean_eof()`、`frames_decoded()`。

这些设计对 streaming 很重要：EOF 时有残余半帧不能被当作正常完成，CRC 错误也不能被误判为上游返回了一个空事件。[Rust]

#### sub2api 的实现

`backend/internal/pkg/kiro/translator.go:3816-3928` 的 `readEventStreamMessage`：

- 读取 12 字节 prelude；
- 校验 total/header length 的基本边界；
- 读取剩余帧；
- 扫描 `:event-type`；
- 提取 payload。

但当前实现不校验 prelude CRC 和 message CRC。`parseEventStream`（`translator.go:3233-3357`）将读取、JSON decode、语义处理和 output assembly 放在同一条路径。[sub2api]

#### 可借鉴点与不可照搬点

| 判断 | 结论 |
|---|---|
| 不建议移植 sub2api 的 parser | 当前 Rust 的 CRC、header type、partial EOF 和 recovery 更成熟。把严格 parser 替换成只读长度的实现会降低协议错误可见性 |
| 值得借鉴上层 fallback | sub2api 能处理不同版本事件的 nested/top-level shape，说明“帧解析”和“语义抽取”之间需要一个兼容层 |
| 值得借鉴测试形态 | 用原始 frame fixture 测 malformed JSON、trailing bytes、重复 event、tool fragment、EOF 截断，而不是只测最终文本 |

### 4.2 typed event 与 semantic event

当前 Rust 的 `EventType`/`Event` 已覆盖：

- `assistantResponseEvent`
- `toolUseEvent`
- `reasoningContentEvent`
- `metadataEvent`
- `meteringEvent`
- `codeEvent`
- `contextUsageEvent`
- `messageMetadataEvent`
- `invalidStateEvent`
- error/exception/unknown

证据：`src/kiro/model/events/base.rs:8-120,135-208`、`src/kiro/model/events/additional.rs:15-165`。[Rust]

sub2api 的 `extractSemanticEvents`（`translator.go:3970-4105`）会：

- 优先读取嵌套 `eventType` 对象；
- 缺失时回退到顶层字段；
- 把 string input、map input、无 input 统一为 semantic tool event；
- 把 usage/metering/message metadata 统一成 semantic usage event；
- 对未知事件保留 raw event。

这不是 typed model 的替代品，而是一个兼容适配器。[sub2api]

**建议：**

```text
raw frame
  -> typed Event（严格主路径）
  -> semantic adapter（只做已知 shape fallback）
  -> StreamContext
```

适配器应遵守四条约束：

1. typed parse 成功时优先使用 typed 字段；
2. fallback 必须记录 source shape；
3. malformed event 不得静默变成成功文本；
4. unknown event 保留诊断尾部，但不能任意猜成 tool call。

### 4.3 endpoint、origin 与 profileArn

当前 Rust 已将 endpoint 和 credential policy 拆开：

- `src/kiro/endpoint/mod.rs:1-220`
- `src/kiro/endpoint/cli.rs:1-221`
- `src/kiro/endpoint/ide.rs:1-175`
- `src/kiro/protocol.rs:92-161`

并且区分：

- header/query API 的 `resolve_profile_arn`；
- streaming body 的 `resolve_streaming_profile_arn`；
- API key、social、external IdP、BuilderId 的不同 fallback；
- origin 和 unicode escaped JSON key 的防绕过。

sub2api 在 `translator.go:438-594,1864-1897` 中统一构建 payload、处理 system history、origin 和 profile 语义，并将 `KIRO_CLI`/`AMAZON_Q` 与 `KIRO_AI_EDITOR`/`KIRO_IDE` 归一到 CLI/AI_EDITOR 侧。[sub2api]

**值得借鉴：**

- 为每个 endpoint 建立 request legality fixture：`origin`、query、User-Agent、profileArn、body path 必须联合断言；
- 对 credential type × endpoint × region 建立矩阵测试；
- 把“真实 profileArn”“请求体 fallback ARN”“不可发送 placeholder”写进测试名称。

**不应直接下结论：**

- origin 字符串差异是否真的影响上游行为仍是 `[待验证]`；
- 不应只因为两个实现使用不同 origin，就直接改当前 Rust 的 endpoint policy。

## 5. Claude Code → Kiro 请求转换

### 5.1 请求模型、system/history 和兼容 profile

当前 Rust 在 `src/anthropic/converter.rs:430-550` 先确定：

1. 末尾 user turn；
2. conversation/continuation id；
3. chat trigger；
4. current message；
5. tools；
6. history；
7. tool pairing；
8. history placeholder tools；
9. current content/tool results；
10. additional model request fields。

并通过 `CompatProfile`、`KiroConverterPlan` 控制 strict/repair 行为。[Rust]

sub2api 在统一 translator 中完成相同的 payload 生成，同时针对 Claude Code 版本、model alias 和 endpoint origin 做更多兼容分支。[sub2api]

**借鉴方向：** 不是复制分支，而是给 Rust 的 compatibility profile 增加“可观测的 plan summary”：本次请求是否发生 tool mapping、schema normalization、placeholder、history trim、thinking fallback。当前 Rust 已有 `ProxyWarnings` 和 `PayloadGuardReport`，可以继续沿这个方向保持结构化。

### 5.2 工具名映射

#### 当前 Rust

`src/anthropic/converter/tools.rs:102-199,300-371`：

- 非 ASCII、分隔符和非法字符归一化；
- 首字符规则；
- 最大长度 `TOOL_NAME_MAX_LEN = 63`；
- SHA-256 确定性短名；
- duplicate/collision 拒绝；
- tool_choice exact/normalized matching；
- reverse map 只在整个分配成功后提交；
- 旧版本 overlong name 提供兼容映射。

#### sub2api

`backend/internal/pkg/kiro/translator.go:2048-2125`：

- `sanitizeToolNameCharset`；
- `shortenToolNameIfNeeded`；
- `mapKiroToolName`；
- 非法字符替换为 `_`；
- 必要时追加原始名称 hash。

`tool_name_charset_test.go` 进一步做 payload 级合法性检查，覆盖 CJK、MCP、`$WEB_SEARCH`、`$MUTLI_1.N.1-Read` 等真实故障样本，并扫描 payload 中所有承载工具名语义的字段。[sub2api]

#### 判断

旧调研把“当前 Rust 缺少工具名清洗”列为缺口，针对当前 HEAD 已不成立。两者现在都是“有算法”的实现，Rust 的 collision/atomic reverse map 还更严谨。

**值得借鉴的只有测试方式：**

- 不只测试 mapper 输出；
- 要把完整 Kiro payload 序列化后再次扫描；
- 覆盖 `$`、`.`、`-`、CJK、MCP server 前缀、超长名、归一化后碰撞；
- 检查历史 tool_use、当前 tool、tool_choice、嵌入式恢复路径是否使用同一个映射。

**建议优先级：P0/P1，低风险。** 不改当前名称算法，先补齐端到端合法性 fixture。

### 5.3 JSON Schema

当前 Rust 的 `src/anthropic/converter/schema.rs:1-715` 支持：

- 根 schema object/properties 保底；
- `definitions` → `$defs`；
- `$ref` 路径迁移；
- nullable/type alias；
- legacy `dependencies`；
- `required`、`items`、`prefixItems`；
- `oneOf`/`anyOf`/`allOf`；
- enum、annotation、validation keywords；
- properties、patternProperties、`$defs`、dependentSchemas 递归处理。

sub2api 的 `translator.go:2126-2246` 使用 Kiro Smithy 兼容白名单，保留：

```text
type
description
properties
required
items
enum
title
```

并删除可能导致 400 的字段，清理空 required，把 `const` 转成 `enum`。对应 `schema_whitelist_test.go`。[sub2api]

#### 判断

- Rust 方案更适合保留 JSON Schema 语义；
- sub2api 方案更适合应对“上游实际拒绝的字段集合”；
- 全局采用极窄白名单会无条件丢失 `minimum`、`pattern`、`oneOf` 等有效约束，不建议直接照搬。

#### 建议

采用“两层策略”：

1. 保留 Rust 的完整、递归、保语义 normalization；
2. 增加按 endpoint/model 的 `accepted schema subset` contract test，只有真实被拒绝且确认无替代表达时才降级字段。

同时记录 normalization diff，例如：

```text
removed_keywords: ["additionalProperties"]
rewritten_keywords: ["const -> enum"]
root_shape_repaired: true
```

这样可以知道“上游 400”究竟来自原始 schema、归一化规则还是模型/endpoint 差异。

### 5.4 tool_use/tool_result 配对和历史占位工具

当前 Rust 的 `src/anthropic/converter/tool_pairing.rs:8-235` 已支持：

- 历史 orphan tool_result 安全删除；
- duplicate tool_result 删除；
- 当前轮只接受最后一个 assistant tool_use 的结果；
- orphan tool_use 清理；
- strict profile 直接报错；
- repair profile 进行兼容修复；
- 空 user content 和空 tool result 占位；
- 历史中引用但当前 tools 缺失时生成 placeholder tool。

sub2api 的 `translator.go:2247-2539` 具备相同主能力，但有一个值得单独关注的经验：**payload trim 后重新收敛 tool pair**。它在裁剪前记录被移除的 toolUse ID → name；如果当前轮 toolResult 对应的 toolUse 被裁到历史外，优先补回最小 toolUse，只有无法恢复名称时才删除 toolResult。相关测试在 `payload_guard_orphan_toolresult_test.go`。[sub2api]

当前 Rust 的 payload guard 在 trim 后会调用 `repair_request`/`repair_anthropic_messages`（`src/anthropic/payload_guard.rs:556-643,1159-1233`），并且已有大量 pairing/shaping 测试，但仍应明确核对这一特定场景：

```text
history 中最后一组 assistant tool_use 被裁掉
current user 仍带对应 tool_result
trim 后是否：
  A. 能恢复最小 tool_use；
  B. 或安全删除 tool_result；
  C. 绝不会把孤儿 pair 发给 Kiro。
```

**这是本次比较中最值得优先确认的功能性候选项之一。**

### 5.5 image/document/multimodal

sub2api 在 `translator.go:2541-2800` 一带体现出阶段化策略：

- remote image 下载；
- inline image 保留；
- 非 PDF document 转文本；
- 历史图片晚于文本/tool result 才丢弃；
- 大工具结果 head/tail compact。

当前 Rust 在 `src/anthropic/body_processing.rs`、`src/anthropic/payload_guard.rs` 和相关 pipeline 中已有：

- inline/remote image shaping；
- 5 MB source limit；
- document shaping；
- current/history 分开统计；
- oversized image error；
- payload report 和 warning header。

因此不建议复制 sub2api 的具体实现。值得借鉴的是**组合行为矩阵**：

| 组合 | 应确认的性质 |
|---|---|
| image + tool_result | image 降级不能破坏当前 tool pair |
| document + history trim | document 转文本/截断后仍可序列化 |
| image + retry | 第一次 payload guard 与 400 retry 不重复损坏内容 |
| image + WebSearch | server-side search block 不应被普通历史图片策略误删 |

### 5.6 thinking/reasoning

sub2api 的请求侧思路包括：

- `thinking.type=adaptive/enabled`；
- `output_config.effort`；
- `reasoning_effort`；
- `Anthropic-Beta: interleaved-thinking`；
- model alias 驱动默认 thinking；
- `additionalModelRequestFields`；
- system prompt 的 thinking tags；
- `translator.go:1468-1680` 的 directive 解析。

当前 Rust 已有：

- native reasoning model：`src/kiro/model/events/additional.rs:15-30`；
- request compatibility retry：`src/kiro/model/requests/conversation.rs:75-111`；
- thinking converter：`src/anthropic/converter/thinking.rs`；
- stream state、redacted/native reasoning、signature、XML thinking、污染过滤：`src/anthropic/stream.rs` 相关路径。

sub2api 的流式 tag buffer（`translator.go:1048-1204`）和测试覆盖很有价值：

- `<thinking>`/`<think>` 跨 chunk；
- UTF-8/partial tag；
- thinking-only；
- reasoning → tool 的顺序；
- 非流式/流式语义一致。

但 `signature.go:1-110` 生成的是本地 HMAC/protobuf-like synthetic signature。它可以作为某些客户端兼容尝试，却不能证明 Kiro 或 Claude Code 接受该签名，也不应被当成真实上游 integrity metadata。

**建议：**

- 借鉴 tag buffer 和 fixture 矩阵；
- 保持当前 Rust 对真实 signature、redacted content、协议污染的安全边界；
- 如果未来必须支持 synthetic signature，单独定义 compatibility profile，默认关闭，要求：
  - 明确的上游/客户端验收证据；
  - 非流式/流式一致性测试；
  - 重放、跨请求、跨 model、空内容和伪造 signature 的安全测试。

### 5.7 payload guard

sub2api 的 `payload_guard.go:12-181,186-304,307-491` 有三个特别值得学习的点：

1. **阶段化降级顺序写得非常明确：**
   1. 压缩历史 tool results；
   2. 剥离历史 thinking；
   3. 压缩工具定义；
   4. 丢弃历史图片；
   5. 最后裁整轮历史。
2. **体积口径是 weighted payload，不把字节数误当上游计价：**
   - ASCII = 1；
   - non-ASCII = 8；
   - 默认阈值来自 2026-09-14 黑盒实测，且允许环境变量覆盖。
3. 明确区分：
   - `DeferredToUpstream`：`on_upstream_400`，预检故意不改；
   - `StillOversized`：压缩/裁剪跑完仍超限；
   - `Compressed`、`Trimmed`、`Rejected`；
   - stage counters。

当前 Rust 的 `src/anthropic/payload_guard.rs:37-176,418-665,1044-1255` 已更完整地支持：

- Kiro 和 Anthropic 两种 body；
- serialized byte breakdown；
- current/history/tool/image/reasoning 统计；
- tool format diagnostics；
- history/tool result/thinking/document/image shaping；
- trim 后 repair；
- warning header；
- body SHA-256；
- external pool body pipeline 复用；
- 400 too-long 单次重试。

`src/anthropic/handlers.rs:7352-7385,9572-9602` 和 `src/external_pool/retry_pipeline.rs:3-52` 也已把 payload retry 作为独立预算处理。[Rust]

#### 判断

当前 Rust 不缺 payload guard。sub2api 值得借鉴的是：

- 把阶段顺序写进稳定文档和诊断；
- 记录 `DeferredToUpstream` 与 `StillOversized`；
- 以真实上游实验校准阈值；
- 每个阶段有独立 counters；
- trim 后 pairing 重新收敛。

不建议把 `ASCII=1/non-ASCII=8` 直接作为 Rust 默认值：当前 Rust guard 的主判据是实际 JSON bytes，两个路径的上游限制不一定相同。若采用 weighted metric，应配置化并用当前 endpoint/model/credential 的真实样本验证。

## 6. Kiro → Claude Code 响应转换

### 6.1 semantic event fallback

sub2api 的 `extractSemanticEvents` 会对 nested/top-level event 做 fallback，并将多个原始 shape 统一成：

- assistant content；
- reasoning；
- tool input/tool use/tool stop；
- usage；
- raw unknown。

当前 Rust 先解析成 typed `Event`，再由 `StreamContext::process_kiro_event` 分发：

- assistant/code → text；
- reasoning → thinking block；
- toolUse → tool block；
- metadata/messageMetadata/metering/contextUsage → usage/state；
- invalid/error/exception → stream error。

证据：`src/anthropic/stream.rs:2395-2544`。[Rust]

**建议：** 如果实测存在某些 endpoint 将字段放在顶层而不是嵌套对象，增加局部 semantic adapter，保留：

- `source_event_type`；
- `source_shape`；
- raw tail；
- fallback warning。

不建议把所有事件退化为 map，也不建议 malformed event 自动转成空文本。

### 6.2 streaming tool input 与 JSON repair

sub2api 的 `translator.go:4107-4385` 支持：

- 多个 `toolUseEvent` fragment 拼接；
- string/map input；
- `UseNumber`；
- 控制字符转义；
- trailing comma 删除；
- brace balance repair；
- empty input `{}`；
- required field 检查；
- truncated tool 丢弃；
- duplicate tool suppression；
- response tool name restore。

当前 Rust 的 `src/anthropic/stream.rs` 已有：

- block lifecycle；
- tool input buffer；
- tool schema key reverse；
- duplicate suppression；
- EOF closure；
- `repair_tool_use_input_for_cli`；
- transcript sanitizer；
- known tool name gate。

sub2api 的经验值得用于**测试覆盖**，但宽泛 repair 需要分级：

| repair 等级 | 例子 | 建议 |
|---|---|---|
| 可安全修复 | 字符串中的控制字符、明确尾逗号、明确未闭合的单一对象边界 | 可修复，并记录 repair |
| 条件修复 | JSON 字符串被分成多个 event fragment、map snapshot 覆盖 string fragment | 只有状态机能证明边界时修复 |
| 不可安全修复 | 多个 JSON 值串接、字段缺失、未知结构、工具名未知 | 丢弃/报错，不制造伪造参数 |

当前 Rust 的“strict by default, repair only within proven cases”更适合作为安全默认。

### 6.3 embedded XML/tool text

sub2api 的 `translator.go:4397-4523` 解析：

```text
[Called tool with args: {...}]
```

并支持：

- JSON bracket matching；
- JSON string 内括号；
- 完整调用与 pending tail 分离；
- 跨 event fragment；
- 未闭合尾部不直接泄漏；
- 非流式/流式共用解析语义。

当前 Rust 的 `src/anthropic/stream.rs:970-1125` 已支持：

- `<function_calls>`；
- `<antml:function_calls>`；
- `<invoke>`；
- `<parameter>`；
- partial tag buffer；
- markdown fence/quoted tag detection；
- known tool name gate；
- search web literal protocol；
- stray token flood guard。

旧调研中“当前 Rust 缺少 XML parsing”的说法对当前 HEAD 已大部分不成立。

仍值得借鉴的不是再增加 marker，而是 fixture 矩阵：

1. 单 event 完整调用；
2. 跨 event 分片；
3. JSON string 内含 `{}`；
4. 未闭合 tail；
5. 普通正文中出现类似 marker；
6. markdown/code fence 中出现 marker；
7. unknown tool；
8. 已知 tool 的 duplicate call；
9. thinking → text → tool 的顺序。

### 6.4 WebSearch 与 SSE content-block index

sub2api：

- `websearch.go:140-326` 识别多种 web search name/type，替换 tool description，注入 `server_tool_use`/`web_search_tool_result`；
- `websearch_stream.go:82-165` 缓冲 SSE，跨 chunk 拼接 `input_json_delta`；
- `websearch_stream.go:167-206` 计算最大 content block index；
- `websearch_stream.go:209-297` 过滤内部 message lifecycle、过滤 search block、调整 `indexOffset`。

这套实现最有价值的思想是：**当代理在原始 stream 中插入或过滤 server-side search block 时，必须把 content block index 当作状态，而不是假设每个 chunk 都从 0 开始。**[sub2api]

当前 Rust 的 `src/anthropic/websearch.rs:306-488,539-704,802-1030` 已将原生 WebSearch 走独立 MCP 路径，并生成完整非流式/SSE response；SSE state manager 也统一维护 block lifecycle。[Rust]

当前实现与 sub2api 并不完全同构，因此不应直接移植 sub2api 的全局过滤器，特别是其对 `message_start/message_delta/message_stop` 的无条件抑制只适合 buffered WebSearch path。

**建议 P2：**

- 先确认当前 Rust 是否存在“插入 WebSearch block 后后续 block index 偏移”或多 chunk `input_json` 边界问题；
- 只有实测有问题时，增加局部 `BufferedSseIndexMap`/`adjust_content_block_index` helper；
- helper 只接收明确的 WebSearch injection context，不做全局 SSE 过滤。

### 6.5 usage、credits 与 cache

sub2api：

- `translator.go:4574-4767` 解析 `uncachedInputTokens`、`outputTokens`、`totalTokens` 和 Kiro credits；
- JSON decoder 使用 `UseNumber`，避免大整数先转 float；
- 明确忽略 upstream cache fields；
- 最终通过 `mergeKiroCacheEmulationUsage` 使用本地模拟值覆盖 response usage。

当前 Rust：

- typed `MetadataTokenUsage`：`src/kiro/model/events/additional.rs:33-75`；
- metadata/messageMetadata 正值合并：`src/anthropic/stream.rs:2420-2478`；
- metering fallback；
- cache read/write 映射；
- reported/raw/local cache policy 分层；
- 防止零值覆盖和双算。

**结论：**

- 借鉴 sub2api 的 `UseNumber` 思维：所有 token/credit 字段都应避免无意的浮点精度损失；
- 可借鉴 credits alias 扫描；
- 不照搬“无条件忽略 upstream cache fields”；
- upstream cache 解析和对外计费/报告策略必须分层：字段存在不等于账单真值，但字段存在也不应被静默丢弃。

### 6.6 EOF、重复事件和错误传播

sub2api 在 `parseEventStream` 中：

- 对重复 toolUse ID 去重；
- EOF 时尝试 finalize 当前 tool；
- 使用 stop reason fallback；
- 无 usage 时按文本和 tool input 估算 output token。

当前 Rust 除了事件去重/生命周期外，还通过：

- `EventStreamDecoder::is_clean_eof()`；
- `StreamContext::upstream_eof_without_completed()`；
- `upstream_status_indicates_incomplete()`；
- `record_stream_error`；
- SSE state manager；

把“没有正常完成”与“只是没有显式 status”区分开。[Rust]

**建议：** 保持当前 Rust 的 EOF 严格性；可以借鉴 sub2api 的测试样本，但不应把半帧/半个 tool 默认当成功。

## 7. 错误分类、retry 与 failover

### 7.1 sub2api 的细分经验

`kiro_error_classifier.go:40-232` 把错误细分为：

- auth；
- monthly request；
- profile；
- quota；
- rate limited；
- suspended；
- usage forbidden；
- upstream transient；
- bad request schema；
- bad request tool pairing；
- bad request invalid model；
- bad request auth；
- bad request quota；
- bad request oversize；
- bad request unknown。

这比只按 HTTP status 记录更利于诊断，尤其是 Kiro 多种 400 都可能共用模糊文案。[sub2api]

### 7.2 当前 Rust 的错误语义

当前 Rust 在 `src/kiro/provider.rs:162-233` 定义：

- InvalidRequest；
- Auth；
- RateLimit；
- Quota；
- RiskControl；
- Server；
- Timeout；
- ResponseTooLarge；
- BodyRead；
- Protocol；
- Unknown。

并把它们映射到：

- public status；
- scheduler reason；
- `TransientFailureKind`；
- retryability；
- credential cooldown/rotation。

调用点和测试分布在 `src/kiro/provider.rs:673,935,7013-7062,7783-7837,11463-11580` 等路径。429/region rotation 另有 `src/kiro/retry_pipeline.rs` 的账号 × region × round 策略。[Rust]

### 7.3 值得借鉴与风险

值得借鉴：

1. 对 `400 Improperly formed request`、`Invalid tool use format`、payload oversize 等具体 message 做子分类；
2. 分类结果写入 structured diagnostic；
3. 测试“分类结果实际被哪个 call site 消费”，而不是只测 classifier 函数；
4. 将 schema、tool pairing、oversize 与 retryability 分开，不要因为都是 400 就统一 failover。

不建议直接复制 sub2api 的 status-code failover policy。sub2api 的分类很细，但某个分类是否真的进入 failover/cooldown，必须沿 `gateway_forward`、pool scheduler 和调用链逐一确认。[推断]

## 8. 值得借鉴矩阵

| 优先级 | 主题 | sub2api 证据 | 当前 Rust 状态 | 建议 |
|---|---|---|---|---|
| P0 | 互操作 fixture inventory | `translator_test.go`、`tool_name_charset_test.go`、`schema_whitelist_test.go`、`websearch_stream_test.go` 覆盖具体故障样本 | Rust 测试很多，但跨模块矩阵仍可集中整理 | 建立请求/事件/SSE/EOF/错误的 contract fixture 清单 |
| P0 | payload 级工具名合法性 | `tool_name_charset_test.go:117-163` 扫描完整 payload | Rust 已有算法与 mapping test | 只补端到端扫描，不改算法 |
| P1 | semantic event adapter | `translator.go:3970-4105` nested/top-level fallback | Rust typed event 主路径更强 | 增加局部 fallback 层，保留 source shape/raw trace |
| P1 | trim 后 tool pair 收敛 | `payload_guard_orphan_toolresult_test.go` | guard 有 repair，但需核对“被裁 toolUse + 当前 toolResult” | 补一个明确回归测试；不能制造孤儿 pair |
| P1 | payload 阶段观测 | `payload_guard.go` 的 stages、DeferredToUpstream、StillOversized、weighted threshold | Rust 有丰富 report，但语义命名不完全同构 | 对齐概念和日志字段，不直接复制 weighted 默认值 |
| P1 | schema accepted subset | `schema_whitelist_test.go` | Rust schema normalization 更完整 | 增加 endpoint/model-specific accepted subset fixture |
| P1 | thinking stream matrix | partial tag buffer、thinking-only、reasoning/tool 顺序测试 | Rust 已有 parser/sanitizer/security tests | 复用测试样本，保持 strict repair 边界 |
| P2 | WebSearch SSE index | `websearch_stream.go:167-297` | Rust 有独立 WebSearch SSE 生成器和 state manager | 先实测，再增加局部 index remap helper |
| P2 | error reason consumption | 400 子分类和 oversize message matching | Rust failure kind 已接入 retry/scheduler | 补分类 → retry/cooldown 的 call-site test |
| P2 | 大整数 usage | `json.Decoder.UseNumber` | Rust serde integer 类型天然较稳 | 对 credit/token 极端值补回放测试 |
| 需单独批准 | synthetic signature | `signature.go:1-110` | Rust 已区分真实/兼容签名和污染过滤 | 默认不引入；仅在真实客户端验收后建立隔离 profile |

## 9. 当前 Rust 已更强、不要照搬的项目

### 9.1 不要用 sub2api 的单体 parser 替换 Rust EventStream parser

CRC、header value type、partial EOF、recovery 和 max frame 都是协议边界能力。当前 Rust 的 parser 结构更容易做安全审计和单元测试。

### 9.2 不要把所有事件退化为 `map[string]any`

sub2api 的 map/fallback 对兼容不同 event shape 很有效，但如果作为主模型，会丢掉：

- schema 约束；
- unknown/error 的可见性；
- 字段缺失和类型错误的区分；
- usage/cache 的正值合并语义。

应增加 semantic adapter，而不是抛弃 typed boundary。

### 9.3 不要全局采用窄 schema 白名单

Kiro Smithy 的 accepted subset 可能按 endpoint/model 变化。全局删除关键字会让 Claude Code/MCP 工具的约束语义静默丢失。

### 9.4 不要直接复制宽泛 JSON repair

控制字符、尾逗号、明确单对象闭合可以修；未知结构、多个 JSON 值串接、必需字段缺失不能靠猜测修复。

### 9.5 不要无条件忽略 upstream cache usage

“解析字段”和“是否作为计费/对外报告真值”是两层。当前 Rust 已有更合理的分层模型。

### 9.6 不要默认引入 synthetic thinking signature

本地 HMAC 只能是兼容试探，不等于上游认可。错误使用可能：

- 让客户端误以为内容已被上游验证；
- 污染历史 thinking；
- 造成跨请求/跨 model 的重放或语义混淆；
- 掩盖真实 signature 缺失。

## 10. 真实上游验证清单

以下项目不能仅凭两个仓库源码决定：

1. CLI/runtime 与 IDE/Q endpoint 对 `origin` 的真实接受范围；
2. credential type × endpoint × region 下 `profileArn` 的真实要求；
3. `metadataEvent`/`messageMetadataEvent` 的 cache fields 是否在所有 model/endpoint 都稳定存在；
4. upstream payload threshold 是字节、字符、加权字符还是动态模型阈值；
5. 工具名最终合法字符集和长度是否随 endpoint/model 变化；
6. WebSearch `server_tool_use`、`web_search_tool_result` 和 SSE index 是否符合当前 Claude Code 客户端实际消费方式；
7. synthetic thinking signature 是否被任何真实上游/客户端接受；
8. malformed/truncated EventStream 在不同 endpoint 上是否有统一 terminal semantics。

验证时应保留：

- 原始 request body；
- endpoint、region、credential class；
- response headers/status；
- 原始 EventStream frame；
- converted Anthropic SSE；
- usage/cache fields；
- retry/failover/cooldown decision。

## 11. 确认后的建议实施顺序

本节只是候选路线，不代表本次已实施。

### P0：测试和证据层

1. 汇总 sub2api 的工具名、schema、tool pairing、thinking、WebSearch、EOF fixture；
2. 在当前 Rust 增加跨模块 contract test；
3. 每个 fixture 同时断言：
   - Kiro payload 合法性；
   - SSE block lifecycle；
   - tool name/schema reverse map；
   - usage/cache；
   - error/retry reason。

### P1：低风险兼容增强

1. 增加 semantic event adapter，严格限制 fallback；
2. 补 trim 后 tool pair 收敛测试；
3. 把 payload guard 的阶段、deferred、still oversized 语义统一到 report/log；
4. 增加 endpoint/model-specific schema accepted subset fixture；
5. 增加 thinking tag/embedded tool 的跨 chunk fixture。

### P2：按实测结果决定

1. WebSearch buffered SSE index remap helper；
2. 400 message 子分类和实际 failover/cooldown 消费测试；
3. weighted payload metric 的配置化实验；
4. credits alias/大整数回放；
5. synthetic signature compatibility profile。

## 12. 证据索引

### 当前 Rust

- EventStream frame：`src/kiro/parser/frame.rs:23-153`
- EventStream headers：`src/kiro/parser/header.rs:8-257`
- EventStream decoder/recovery/EOF：`src/kiro/parser/decoder.rs:87-310`
- Typed events：`src/kiro/model/events/base.rs:8-208`
- Metadata/reasoning/metering events：`src/kiro/model/events/additional.rs:15-165`
- Endpoint/profile resolver：`src/kiro/endpoint/mod.rs:1-220`、`src/kiro/endpoint/cli.rs:1-221`、`src/kiro/endpoint/ide.rs:1-175`、`src/kiro/protocol.rs:92-161`
- Anthropic → Kiro main flow：`src/anthropic/converter.rs:430-550`
- Tool mapping：`src/anthropic/converter/tools.rs:102-199,300-397`
- Schema normalization：`src/anthropic/converter/schema.rs:1-715`
- Tool pairing：`src/anthropic/converter/tool_pairing.rs:8-235`
- Embedded protocol extraction：`src/anthropic/stream.rs:970-1125`
- Kiro event → Anthropic SSE：`src/anthropic/stream.rs:2395-2544`
- Native WebSearch：`src/anthropic/websearch.rs:306-488,539-704,802-1030`
- Payload guard/report：`src/anthropic/payload_guard.rs:37-176,418-665,1044-1255`
- Provider failure semantics：`src/kiro/provider.rs:162-233,7783-7837`
- Retry/region rotation：`src/kiro/retry_pipeline.rs:1-240`
- Payload retry route：`src/external_pool/retry_pipeline.rs:3-52`

### sub2api-kiro

- EventStream read/semantic parse：`backend/internal/pkg/kiro/translator.go:3233-3357,3816-4105`
- Tool input repair/embedded tool text：`translator.go:4107-4523`
- Usage/cache projection：`translator.go:4574-4767`
- Thinking directive and stream tags：`translator.go:1048-1204,1468-1680`
- Tool name mapping/schema/pairing：`translator.go:2048-2539`
- WebSearch response injection：`backend/internal/pkg/kiro/websearch.go:140-326`
- WebSearch buffered SSE/index：`backend/internal/pkg/kiro/websearch_stream.go:82-297`
- Payload guard and weighted threshold：`backend/internal/pkg/kiro/payload_guard.go:12-181,186-491`
- Error classifier：`backend/internal/service/kiro_error_classifier.go:40-232`
- Synthetic thinking signature：`backend/internal/pkg/kiro/signature.go:1-110`
- Tool-name end-to-end tests：`backend/internal/pkg/kiro/tool_name_charset_test.go:117-163`
- Payload trim/tool pair test：`backend/internal/pkg/kiro/payload_guard_orphan_toolresult_test.go`
- Schema whitelist tests：`backend/internal/pkg/kiro/schema_whitelist_test.go`
- WebSearch stream tests：`backend/internal/pkg/kiro/websearch_stream_test.go`
- Thinking/stream compatibility tests：`backend/internal/pkg/kiro/translator_test.go` relevant cases around model alias, thinking tags, tool fragments and EOF

## 13. 本次工作边界

- 已完成双方当前 HEAD 的源码静态对比。
- 本次只新增本文档。
- 未修改 Rust 业务代码、配置、测试或协议实现；工作区中另有 `src/kiro/provider.rs` 和 `src/kiro/retry_pipeline.rs` 的既有修改，本次未触碰并予以保留。
- 未进行真实 Kiro 上游抓包，也未运行完整测试套件。
- 等待确认后再决定是否实施 P0/P1/P2 候选项。
