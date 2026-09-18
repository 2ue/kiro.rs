# Kiro 与 Claude Code 协议互转优化路线

> 分析日期：2026-09-16
> 基线文档：[sub2api-kiro 与当前 Rust 实现的协议互操作对比](./sub2api-kiro-protocol-interop-comparison-20260915.md)
> 分析范围：Claude Code/Anthropic ↔ Kiro 的请求转换、事件解析、流式 SSE、工具调用、thinking、usage/cache、错误传播与重试
> 本文状态：只读分析与候选路线，**未修改 Rust 业务代码、配置或测试，等待确认后再实施**

## 1. 结论先行

当前 Rust 的协议互转主干已经具备较强的结构化能力：

```text
Anthropic / Claude Code request
  -> request facts / compatibility profile
  -> typed converter
  -> Kiro IDE/CLI request envelope
  -> strict AWS EventStream decoder
  -> typed Kiro events
  -> stream state machine / semantic projection
  -> Anthropic JSON or SSE response
```

本轮不建议把 `sub2api-kiro` 的单体 translator、全量 `map[string]any`、宽泛 JSON repair 或 synthetic thinking signature 直接移植到 Rust。更合理的优化方向是：

1. **先把互操作行为变成可回放、可比较、可定位的 contract fixture。**
2. **在 typed parser 之上增加受限的 semantic event adapter，而不是替换 typed boundary。**
3. **优先锁住 trim 后的 tool_use/tool_result 配对、thinking 流顺序和 SSE block lifecycle。**
4. **把 payload guard、schema normalization、错误分类的“阶段和原因”显式记录出来。**
5. **只有真实 Kiro/Claude Code 证据证明存在兼容缺口时，才增加 endpoint-specific fallback。**

推荐的实施顺序是：

```text
P0 证据与回放层
  -> P1 低风险互操作修复
  -> P1 兼容 profile / 观测增强
  -> P2 按真实上游结果决定的局部适配
```

这是一份协议互转路线，不涉及用户收费、余额扣减或成本定价。

## 2. 证据边界

### 2.1 当前已确认的事实

以下结论由当前 Rust 或 `../sub2api-kiro` 源码、测试和已有分析直接支持：

| 结论 | 证据 |
|---|---|
| Rust 使用结构化 EventStream parser，包含 CRC、header type、分片、EOF 和恢复语义 | `src/kiro/parser/frame.rs`、`header.rs`、`decoder.rs` |
| Rust 已将 Kiro 事件解析为 typed event，并区分 metadata、reasoning、tool、error、unknown | `src/kiro/model/events/base.rs`、`additional.rs` |
| Rust 已有工具名确定性映射、collision 检测、reverse map 和完整 payload 约束 | `src/anthropic/converter/tools.rs` |
| Rust 已有递归 JSON Schema normalization，不应被旧调研中的“缺少 schema 处理”结论覆盖 | `src/anthropic/converter/schema.rs` |
| Rust 已有 tool pairing、placeholder、repair、strict/compatibility profile | `src/anthropic/converter/tool_pairing.rs` |
| Rust 已解析 upstream usage/cache，并把 raw、reported、local projection 分开 | `src/kiro/model/events/additional.rs`、`src/anthropic/stream.rs`、`src/anthropic/cache.rs` |
| Rust 已有 strict EOF 和 stream completion 判断，不应把半帧当成功 | `src/kiro/parser/decoder.rs`、`src/anthropic/stream.rs` |
| sub2api 在 fixture 组织、payload 级工具名测试、semantic event fallback、trim 后 tool pair 恢复、WebSearch SSE index 处理方面有可借鉴经验 | `../sub2api-kiro/backend/internal/pkg/kiro/*` |

### 2.2 仍然不能仅靠源码确定的事实

以下事项必须通过真实上游抓包、隔离回放或真实 Claude Code 验收确认：

1. 不同 credential type、endpoint、region 对 `origin` 和 `profileArn` 的精确要求。
2. Kiro 对 payload 超限的真实判据是 JSON bytes、字符数、加权字符数还是动态阈值。
3. Kiro 在不同 model/endpoint 下接受的工具名字符集和长度上限。
4. `metadataEvent` 与 `messageMetadataEvent` 的 usage/cache 字段是否稳定一致。
5. WebSearch server-side block 插入后，客户端实际接受的 SSE `content_block.index` 规则。
6. Kiro 是否接受多个 reasoning block、数组形态或 thinking/tool_use 的交错表达。
7. Kiro 是否容忍工具回合完全省略 thinking/signature。
8. malformed/truncated EventStream 在不同 endpoint 上的 terminal semantics。

源码中看到的兼容分支只能证明“某个实现采取了这个策略”，不能单独证明“上游协议要求这个策略”。

## 3. 优化目标与非目标

### 3.1 优化目标

- 对 Claude Code 长会话、工具调用、agent/MCP 和 thinking 工作流保持稳定的消息语义。
- 让请求转换和响应转换具备可回放的输入/输出证据。
- 让兼容修复是局部、可配置、可观测和可回滚的。
- 将“解析成功”“语义转换成功”“可重试”“可对外输出”分成不同判断。
- 在不牺牲严格协议边界的前提下，覆盖 Kiro 不同 endpoint/event shape 的实际差异。
- 让一次失败能够定位到具体阶段：输入 shape、转换、payload guard、上游 parser、SSE state、tool pairing 或 retry。

### 3.2 非目标

- 不将所有事件退化成 `map[string]any`。
- 不为了兼容而默认丢弃未知字段、upstream cache、thinking 或 tool input。
- 不把本地 synthetic signature 当作真实上游签名。
- 不把所有 HTTP 400 都转换成 failover。
- 不把宽泛 JSON repair 当作协议适配的默认行为。
- 不改变当前计费、usage 存储或余额语义。
- 不在没有真实上游证据前修改 endpoint/origin/profileArn 默认值。

## 4. 当前协议互转边界

### 4.1 请求方向：Claude Code/Anthropic → Kiro

当前 Rust 的主要顺序是：

1. 读取 raw request facts，决定是否可以走 raw external path。
2. 解析 model、route、compatibility profile 和 thinking policy。
3. 处理 system、history、current turn、tools、tool results、images/documents。
4. 建立 tool name mapping、schema normalization 和 tool pair repair。
5. 生成 Kiro IDE 或 CLI envelope，包括 endpoint/profile/origin 等上下文。
6. 运行 payload guard、shaping 和有限的 retry body 重建。
7. 发送至 Kiro provider。

关键实现：

- `src/anthropic/converter.rs`
- `src/anthropic/converter/content.rs`
- `src/anthropic/converter/history.rs`
- `src/anthropic/converter/schema.rs`
- `src/anthropic/converter/tools.rs`
- `src/anthropic/converter/tool_pairing.rs`
- `src/anthropic/payload_guard.rs`
- `src/kiro/endpoint/ide.rs`
- `src/kiro/endpoint/cli.rs`

### 4.2 响应方向：Kiro → Claude Code/Anthropic

当前 Rust 的主要顺序是：

1. EventStream decoder 验证 frame 边界、CRC、header 和 EOF。
2. typed event dispatch。
3. `StreamContext` 维护 text、thinking、tool、usage 和 error 状态。
4. tool input fragments 进入 buffer，必要时只做已证明安全的 repair。
5. WebSearch 走独立 MCP/SSE 生成路径。
6. 终止时生成合法的 Anthropic JSON 或 SSE lifecycle。

关键实现：

- `src/kiro/parser/frame.rs`
- `src/kiro/parser/header.rs`
- `src/kiro/parser/decoder.rs`
- `src/kiro/model/events/base.rs`
- `src/kiro/model/events/additional.rs`
- `src/anthropic/stream.rs`
- `src/anthropic/websearch.rs`

### 4.3 应保持的边界

```text
严格帧解析
  != 兼容字段抽取
  != 语义事件归一化
  != Anthropic SSE 生命周期
  != scheduler/retry/failover 决策
```

任何新兼容能力都应明确属于其中哪一层，不能把上游字段缺失、协议损坏和客户端兼容降级混为同一条 fallback。

## 5. 优化矩阵

| 优先级 | 优化项 | 主要收益 | 风险 | 是否需要真实上游 |
|---|---|---|---|---|
| P0 | 互操作 fixture inventory 与 contract test | 将跨模块兼容性变成可回放证据 | 测试范围扩大，需治理 fixture 版本 | 部分需要 |
| P0 | 完整 payload 工具名合法性扫描 | 防止映射只在局部正确、落到 payload 后失效 | 低 | 否，先用已有样本 |
| P1 | trim 后 tool pair 重新收敛 | 避免孤儿 `tool_result` 或被裁掉的 `tool_use` 发往 Kiro | 中 | 否，可先做本地回放 |
| P1 | semantic event adapter | 兼容 nested/top-level event shape 差异 | 中 | 是，需先有 shape fixture |
| P1 | payload guard 阶段语义统一 | 能区分预检延期、已压缩、仍超限和最终拒绝 | 低 | 阈值校准需要 |
| P1 | endpoint/model-specific schema accepted subset | 只在被证实拒绝时做最小降级 | 中 | 是 |
| P1 | thinking/stream contract matrix | 锁住 signature、交错 thinking、tool 顺序和 EOF 行为 | 中到高 | 是 |
| P2 | WebSearch 局部 SSE index remap | 处理服务端搜索 block 插入/过滤的 index 偏移 | 中 | 是 |
| P2 | 400 message 子分类与消费测试 | 防止 schema 错误误触发 failover/cooldown | 低 | 部分需要 |
| P2 | usage/credits 大整数回放与 alias 兼容 | 防止极端 usage 或字段别名丢失 | 低 | 可先本地回放 |
| 暂缓 | synthetic thinking signature | 只在真实验收证明必要时解决特定客户端问题 | 高 | 必须 |

## 6. P0：先建立互操作证据层

### 6.1 Fixture 分类

建议以“请求 fixture、上游事件 fixture、转换后 SSE fixture、错误 fixture”四类组织：

```text
fixtures/protocol-interop/
  request/
    tool-name/
    schema/
    tool-pairing/
    thinking/
    multimodal/
    endpoint-profile/
  eventstream/
    text/
    tool-fragments/
    reasoning/
    metadata-usage/
    malformed/
    truncated/
  anthropic-sse/
    normal/
    thinking-tool-interleaved/
    websearch/
    terminal-errors/
  errors/
    bad-request/
    auth/
    quota/
    rate-limit/
    protocol/
```

目录只是候选布局，是否落地到仓库应在确认后决定。每个 fixture 至少需要记录：

- 来源：Rust 单元构造、sub2api 样本、真实上游抓包或 Claude Code transcript。
- endpoint：IDE、CLI runtime、外部 Anthropic-compatible pool 或 `/cc/v1`。
- model alias 与 resolved model。
- credential class、region、route policy。
- 是否涉及 tools、thinking、WebSearch、usage/cache。
- 期望的 terminal outcome：成功、客户端错误、上游错误、不可重试。

### 6.2 每个 fixture 的断言分层

不应只断言最终文本。建议至少包含：

1. **结构断言**
   - Kiro payload 能否反序列化；
   - 工具名是否合法；
   - schema 是否在允许的 accepted subset；
   - tool_use/tool_result 是否成对。
2. **事件断言**
   - source event type；
   - nested/top-level shape；
   - fragment 拼接顺序；
   - duplicate、unknown、malformed 的处理。
3. **SSE lifecycle 断言**
   - `message_start`；
   - `content_block_start/delta/stop`；
   - `message_delta`；
   - `message_stop`；
   - index、stop reason、usage 是否一致。
4. **错误语义断言**
   - public error type；
   - failure kind；
   - retryability；
   - scheduler/cooldown/failover 是否被触发。
5. **来源与降级断言**
   - 是否发生 normalization、repair、trim、fallback；
   - 是否保留 raw tail；
   - 是否设置 warning/diagnostic。

### 6.3 P0 工具名扫描

Rust 当前名称映射算法已经比旧调研所描述的状态完整，因此不建议立即改算法。优先补一层“序列化后的完整 payload 扫描”：

- CJK；
- `$`、`.`、`-`；
- MCP server 前缀；
- 超长名称；
- 归一化后碰撞；
- `tool_choice` exact/normalized；
- 历史 tool_use；
- 当前 tool；
- embedded tool 恢复；
- WebSearch 特殊名称。

扫描目标不是只检查 `tools[].name`，而是检查所有承载工具名语义的字段，确认 reverse map 在请求和响应两端都使用同一结果。

## 7. P1：低风险互操作增强

### 7.1 Semantic event adapter

建议增加一个局部适配层，放在 typed event decode 之后、`StreamContext` 之前：

```text
raw frame
  -> typed Event
  -> semantic adapter
  -> StreamContext
```

适配规则：

1. typed event 成功时，typed 字段优先。
2. 只有已知事件类型允许 nested/top-level fallback。
3. fallback 必须记录：
   - `source_event_type`
   - `source_shape`
   - `fallback_reason`
   - 可选 raw tail/hash
4. 字段类型错误不得静默变成空文本。
5. unknown event 只能保留为 unknown/diagnostic，不能猜成 tool call。
6. malformed frame 不能通过 semantic adapter 伪装成正常 terminal event。

建议的 semantic event 类型：

```text
TextDelta
ThinkingDelta
ToolInputFragment
ToolCompleted
UsageObserved
ContextUsageObserved
UpstreamError
UnknownEvent
```

这不是替换 typed Kiro model，而是让“上游 shape 差异”与“协议帧错误”隔离。

### 7.2 Trim 后 tool pair 重新收敛

这是当前最值得先做本地回放验证的功能性项目：

```text
history assistant: tool_use(id=A)
current user:      tool_result(tool_use_id=A)
payload guard trim
```

trim 后必须满足以下之一：

- 恢复最小的 `tool_use(id=A, name=known_name)`；
- 或安全删除对应 `tool_result`；
- 绝不能把 orphan `tool_result` 发往 Kiro。

建议把“裁剪前记录 `tool_use_id -> tool name`，裁剪后重新收敛 pair”做成共享 helper，并让以下路径共用同一边界定义：

- payload guard 的 history trim；
- tool pairing repair；
- usage/diagnostic 中的 history/tool 统计；
- thinking 历史剥离。

这里的“重新收敛”不等于无条件补造完整工具调用。无法恢复名称或输入语义时，应严格失败或删除孤儿结果，并写入结构化诊断。

### 7.3 Payload guard 阶段语义

当前 Rust 已有较丰富的 `PayloadGuardReport`、body hash、serialized byte breakdown 和 warning header。建议只统一概念和报表，不复制 sub2api 的默认阈值：

| 建议语义 | 含义 |
|---|---|
| `DeferredToUpstream` | 本地预检未改写，交给上游判定 |
| `Compressed` | 已执行某个压缩/整形阶段 |
| `Trimmed` | 已删除或裁剪历史/图片/tool result |
| `StillOversized` | 所有允许阶段执行后仍超限 |
| `Rejected` | 本地明确拒绝，未发送上游 |
| `RetriedAfterUpstream400` | 上游 400 后使用独立 retry budget 重建请求 |

每个阶段建议记录：

- stage name；
- before/after byte count；
- affected message/tool/image/reasoning count；
- whether protected current turn was touched；
- repair/trim reason；
- retry attempt。

`ASCII=1 / non-ASCII=8` 只能作为实验指标，不应直接替换当前 Rust 的 JSON byte 判据。

### 7.4 Endpoint/model-specific schema accepted subset

保留 Rust 当前递归、保语义的 schema normalization；新增的是测试和最小降级 profile：

```text
default:
  keep semantically valid keywords

endpoint/model profile:
  remove only a field confirmed to cause upstream rejection
  record normalization diff
  keep profile opt-in or narrowly scoped
```

不建议全局白名单删除 `minimum`、`pattern`、`oneOf`、`allOf`、`additionalProperties` 等字段。每个降级项必须有：

- 原始请求 fixture；
- 上游拒绝证据；
- 最小删除集合；
- 替代表达；
- 非流式和流式验证；
- 回滚开关。

### 7.5 Thinking 与 stream contract matrix

应优先复用 sub2api 的分片测试思路，但保留 Rust 对不透明 signature/redacted data 的严格边界：

- `<thinking>` / `<think>` 跨 chunk；
- UTF-8/partial tag；
- thinking-only；
- reasoning → text → tool；
- thinking → tool；
- 多个 tool fragment；
- tool input EOF；
- duplicate tool ID；
- signed thinking 原样回传；
- redacted thinking 不扫描、不重写；
- message lifecycle 在正常、异常、client disconnect 下均闭合或明确失败。

如果真实 Kiro 证明一个 assistant 消息存在多个交错 reasoning block，必须先确定上游接受的 schema：

- 单 object；
- 数组；
- 位置化内容；
- 或只允许单块。

在此之前，不应通过合并、重排、伪造 signature 或无条件 strip-all 来“猜测兼容”。

## 8. P2：只有实测有缺口才做

### 8.1 WebSearch SSE index map

sub2api 的可借鉴点是把 `content_block.index` 当成状态，而不是逐 chunk 重新假设从 0 开始。当前 Rust 的 WebSearch 是独立 MCP/SSE 路径，因此不建议移植全局 SSE 过滤器。

只有出现以下证据时才增加局部 helper：

- server-side search block 插入后后续 index 偏移；
- `input_json_delta` 跨 chunk 时 index 错配；
- 内部 block 被过滤后客户端收到不连续 index；
- Claude Code 对非连续 index 明确报错。

实现边界应限定在 WebSearch injection context，例如：

```text
BufferedSseIndexMap
adjust_content_block_index(...)
```

不能让普通 tool-use、thinking 或 text stream 共用未经证明的全局 index 重写。

### 8.2 400 message 子分类与 retry/failover 消费

可借鉴 sub2api 的 message 子分类，但必须把分类与实际消费点一起验证：

| 分类 | 默认处理方向 |
|---|---|
| schema/unsupported field | 修正请求或 fail closed，不自动换账号 |
| tool pairing | 先本地 repair/拒绝，不默认跨池重试 |
| payload oversize | 走独立 trim/retry budget |
| invalid model | 重新解析 alias/capability，不盲目重试 |
| auth/profile | credential cooldown 或明确鉴权错误 |
| quota/rate limit | 按 retry-after、region、credential 策略处理 |
| transient upstream | 允许受控 retry/failover |

必须补 call-site test，证明分类结果真的影响：

- `retryable`；
- scheduler reason；
- cooldown；
- region rotation；
- external pool cross-pool retry；
- public error envelope。

### 8.3 Usage/credits 大整数与 alias

Rust 的 serde 整数类型已经比 `float64` 更稳，仍建议补极端值回放：

- 超大 input/output token；
- `totalTokens` 与分项不一致；
- metadata 与 metering 重复；
- 0 值字段；
- credits alias；
- cache read/write 与 uncached input 同时存在。

原则不变：

- upstream usage 先保留；
- compatibility usage 再归一化；
- reported/local projection 单独计算；
- Kiro credits 不直接伪装成 token 或 signature。

## 9. 明确不建议照搬的实现

### 9.1 用单体 translator 替换 Rust 模块边界

单体实现便于快速打通分支，但会把 parser、semantic conversion、SSE lifecycle、usage 和 retry 混在一起，降低错误定位和回滚能力。Rust 应保持 typed parser、semantic adapter、stream state 和 scheduler 的边界。

### 9.2 全量 `map[string]any`

map/fallback 适合局部兼容不同 event shape，不适合作为主数据模型。否则字段缺失、类型错误、unknown event 和 usage/cache 合并错误可能被静默吞掉。

### 9.3 全局窄 schema 白名单

Kiro 的 accepted subset 可能随 endpoint/model 变化。全局删字段会使工具约束语义静默丢失，且无法解释“为什么这个请求被降级”。

### 9.4 宽泛 JSON repair

可修复：

- 已确定边界的单对象尾逗号；
- 明确的控制字符；
- 多 event fragment 拼成一个已知对象。

不可默认修复：

- 多个 JSON 值串接；
- 未知工具；
- 缺失必需字段；
- 无法证明边界的 brace balance；
- 任意 map 覆盖 string fragment。

### 9.5 忽略 upstream cache

“解析字段”和“是否作为对外报告真值”是两层。当前 Rust 分层处理比 sub2api 的无条件忽略更适合保留事实和后续诊断。

### 9.6 Synthetic thinking signature

本地生成 HMAC/protobuf-like 字符串不能证明 Kiro/Claude Code 接受它。默认引入会产生：

- 客户端误以为内容已被上游验证；
- 历史 thinking 污染；
- 跨请求/跨 model 重放或语义混淆；
- 真实 signature 缺失被掩盖。

只有真实上游和真实客户端均证明必要时，才建立独立 compatibility profile，默认关闭。

## 10. 建议实施分批

### 批次 A：P0 证据层

目标：不改协议策略，先建立可回放基线。

- fixture inventory；
- 完整 payload tool-name scan；
- EventStream malformed/truncated/duplicate fixture；
- tool fragment 和 SSE lifecycle fixture；
- error classification → call-site 观测基线；
- 记录当前版本、endpoint、model、credential class、region。

通过标准：

- 每个 fixture 可独立运行；
- 失败能指出 request/event/SSE/retry 的具体层；
- 测试不会把 unknown/malformed 当作成功文本；
- 不产生真实账号或生产数据依赖。

### 批次 B：P1 低风险修复

目标：补齐已知高收益、低到中风险的兼容性缺口。

- trim 后 tool pair 重新收敛；
- semantic event adapter；
- payload guard stage 语义统一；
- thinking/embedded tool 跨 chunk fixture；
- endpoint/model-specific schema fixture；
- 保持 strict EOF 和 typed parser 不变。

通过标准：

- 正常、异常、partial stream 均有终止语义；
- no orphan tool result；
- fallback 可观测且不会静默吞错；
- repair/trim 不触碰受保护的当前 tool/thinking turn；
- 非流式和流式输出语义一致。

### 批次 C：P2 真实上游适配

目标：只处理实测证明存在的 endpoint/client 差异。

- WebSearch index map；
- 400 message 子分类消费；
- weighted payload metric 实验；
- usage/credits alias 极端值回放；
- 需要时建立隔离 compatibility profile。

通过标准：

- 有原始 request、response headers/status、EventStream frame 和转换后 SSE；
- 有前后对比；
- 有回滚开关；
- 不改变不相关 endpoint/model 的默认行为。

## 11. 真实上游验证矩阵

每个需要上游确认的场景至少保留以下上下文：

| 维度 | 记录内容 |
|---|---|
| 请求入口 | `/v1`、`/cc/v1`、外部池或直接 provider |
| endpoint | IDE、CLI runtime、MCP、WebSearch |
| credential | API key、social OAuth、external IdP、BuilderId 等类别 |
| region | 实际 region |
| model | requested alias 与 resolved model |
| body | 原始请求、转换后 Kiro payload、是否 trim/repair |
| response | status、headers、原始 EventStream frame |
| downstream | Anthropic JSON/SSE、block index、stop reason、usage |
| decision | retry、failover、cooldown、最终 public error |

最小抓包/回放集合：

1. 纯文本非流式。
2. 纯文本流式。
3. 单工具调用。
4. 多工具调用与多 fragment input。
5. tool_use/tool_result 跨 history trim。
6. thinking-only。
7. thinking → text → tool。
8. `display:"omitted"` 或 redacted thinking。
9. metadata usage/cache。
10. WebSearch。
11. malformed/truncated EventStream。
12. 429、400 schema、400 tool pairing、401、quota、5xx。

thinking 相关的多块/交错请求必须等真实 Kiro schema 证据后再决定是否支持、拒绝或增加 profile，不能凭 sub2api 的兼容代码推断。

## 12. 需要确认的决策点

在开始改代码前，需要确认以下边界：

### 决策 A：第一批是否只做测试与证据

推荐：**是**。先落地 P0 fixture 和 contract tests，不改变默认协议行为。

原因：

- 当前 Rust 已有较多兼容逻辑，直接改实现容易把已修复路径重新打坏；
- sub2api 的部分策略是兼容经验，不是独立上游规范；
- 真实 Kiro shape 尚有若干待验证项。

### 决策 B：semantic adapter 是否默认开启

推荐：**只对已知 shape 开启，未知 shape 保持 strict failure/diagnostic**。

不要做“任何字段找得到就尝试转换”的全局宽松 fallback。

### 决策 C：tool pair trim 无法恢复名称时如何处理

推荐：**删除孤儿 `tool_result` 或 fail closed，不能猜名称补造 tool_use**。

具体选择可以按当前 `CompatProfile` 区分，但必须记录 reason。

### 决策 D：schema 降级是否全局生效

推荐：**不全局生效**。按 endpoint/model profile 精确开启，并保留 normalization diff。

### 决策 E：是否引入 synthetic thinking signature

推荐：**暂不引入**。除非真实 Kiro 和真实 Claude Code 验收均证明现有逐字透传不足，且能定义隔离、回滚和安全测试。

## 13. 推荐的首批范围

如果确认开始实施，建议首批只包含：

1. P0 fixture inventory 和 contract test 骨架。
2. payload 级工具名合法性扫描。
3. trim 后 tool pair 收敛回归测试。
4. EventStream malformed/truncated/duplicate 回放测试。
5. thinking/tool/SSE lifecycle 的跨 chunk fixture。
6. 错误分类到 retry/failover/cooldown 的 call-site 测试。
7. 不改变默认 endpoint、schema、cache、signature 和 repair 策略。

首批不包含：

- semantic adapter 的宽松 fallback；
- 全局 schema 白名单；
- weighted payload 默认阈值；
- WebSearch 全局 SSE index 重写；
- synthetic thinking signature；
- 任何收费/余额/成本逻辑。

## 14. 当前工作边界

- 已基于 `docs/analysis/sub2api-kiro-protocol-interop-comparison-20260915.md` 收敛协议互转优化路线。
- 参考了当前 Rust 的 typed parser、converter、stream、payload guard、WebSearch 和 retry 边界。
- 参考了 `../sub2api-kiro` 的 semantic event、tool/schema/pairing、WebSearch、payload guard 和错误分类测试经验。
- 未修改 Rust 业务代码、配置、测试或协议实现。
- 未进行真实 Kiro 上游抓包。
- 未运行完整测试套件。
- 等待确认后，再决定是否进入 P0 测试实现或进一步做真实上游验证。
