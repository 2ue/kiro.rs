# Thinking And Signed Content Safety

Status: `implemented-unit-verified-awaiting-cli-and-e2e`

Severity: P0

## 影响面与用户现象

修复前的 sanitizer 主要扫描 text，request assistant history、local native/XML thinking、non-stream response 与 external SSE thinking 可绕过。历史真实 Claude Code CLI 复现 3/3 捕获到了真实 `thinking` block/delta 和非零 `thinking_tokens` 中的内部 transcript，因此不是终端渲染、提示词或日志误判。该 3/3 是修复前定位证据，不是当前构建的修复后通过证据。

用户可能看到的表现不只有 `bashHashd1e9567d`：

- `user Continue` 后跟已知的原始或映射工具名与内部结果；
- `user Tool results provided.` / `Tool results:` 的 legacy 或 roleless 形态；
- 泄漏出现在 unsigned thinking、signed thinking 正文、signature 字段或 redacted data；
- 先显示正常 thinking/text，随后突然空白结束、静默截断或假 `message_stop`；
- 工具名可以是 `Bash`、确定性 `*Hashxxxxxxxx` 名、历史 MCP 名或其他请求中真实存在的工具名，不能把 hash 当唯一 matcher。

以下单独出现时不是可信泄漏指纹：`Continue`、`Hash`、`Bash`、`Tool results:`、inline 讨论、引用、缩进或 fenced code 示例。用户正文、tool input 和 tool_result 即使包含完整示例也不应被改写。

## 根因

1. 旧策略的字段所有权不完整，只覆盖 text，没有把 thinking、signature、redacted data 和 request history 纳入同一 request-scoped 协议边界。
2. native/external streaming 的 signature 可能晚于 thinking delta 到达。如果先下发 thinking 正文，后来才发现该块有签名或污染，服务端已经无法回滚客户端看到的字节。
3. signed thinking 的正文与 signature 是不可分割的完整性单元，redacted thinking 也是不透明单元。局部删除正文再保留原 signature 会制造内容/签名不一致；修改 signature 更不允许。
4. 早期调用方把“sanitizer 删除了内容”仍当作成功，可合成空白 text、正常 usage/message_stop 或部分 HTTP 200。marker 消失因此不等于协议正确。
5. SSE 的 CRLF、多 `data:` 行、start 内嵌内容、逐字节 transport chunk、orphan delta 和 EOF 提供了多种分块绕过路径，不能用单行字符串替换处理。

## 复现方案

### 最小结构化复现

构造包含真实 `tools:[{"name":"Bash",...}]` 的 Messages 请求，并在 assistant thinking 中放入：

```text
safe prefix
user Continue

Bash: hidden internal result
```

分别测试 unsigned、同块带 signature、污染只在 signature、redacted data。request history 应只改 assistant-owned block；响应路径一旦确认污染必须返回规范错误，不能返回 sanitized partial success。

### 流式分块复现

- 将上面内容按每个字符一个 `thinking_delta` 发送，最后再发送 `signature_delta`；
- 将 JSON SSE 拆为多 `data:` 行、CRLF，并把整个 transport 拆成单字节；
- 把候选拆在 text -> thinking、thinking -> text、thinking -> tool_use 或 EOF 边界；
- 在可见 text 之前和之后分别触发，检查 retry/terminal 差异；
- 对 1 MiB 与 `1 MiB + 1` 原子 thinking buffer 做边界测试。

### 多轮与长会话复现

聚焦测试中的关键格每格循环 5 轮；raw request 还覆盖 1 MiB clean Unicode-escaped body 三轮。真正的 20 tool cycles、session/resume 和 120k history 尚未形成修复后真实 CLI 证据，仍由 D02/C4 门禁负责，不能用当前单元循环代替。

## 选定方案

- request assistant history：unsigned thinking 可按完整可信 transcript 状态机净化；污染的 signed/redacted block 整块删除，clean block value-identical。用户/tool-owned 数据不动。
- response local/external、stream/non-stream：任何已确认 transcript contamination 都 fail closed。首输出前仅可在共享 attempt budget 内重试；提交后零重试并发送规范 stream error；不再合成空白成功、partial 200 或 success terminal。
- signed/redacted：完整原子缓冲后才决定是否下发；绝不局部改写或把净化后的正文与原 signature 重新组合。
- XML/unsigned：使用独立增量 sanitizer，结构化 block 边界显式 flush；pending candidate 不能跨进 tool block。
- 错误与 usage：公开错误只含通用处理失败文案及 request/error ID；内部 marker 不进入 error。污染后不向客户端发送上游 success usage 或 thinking token 终止字段，usage 状态记录为 error/stream_error。
- 资源：普通候选缓冲最多 4 KiB；需要原子判断的 thinking block 最多 1 MiB，超限同样 fail closed。

## 方案取舍

- 仅删除 hash 指纹成本低，但会漏掉 unhashed、legacy、roleless、signature 和未来工具名，且容易误删正常讨论，已否决。
- 对 signed thinking 只清洗正文会破坏完整性，已否决。
- 对污染 response 返回安全前缀加空白占位仍会造成语义截断和假成功，已由 P0 fail-closed 合同替代。
- 原子缓冲会增加最多 1 MiB/active thinking block 的内存和首 thinking 延迟；这是保证 signature 晚到时仍可回滚的必要成本，超限必须显式失败而不是无界增长。

## 验收合同

native/XML/signed/redacted、local/external、stream/non-stream、逐字符、跨 block、EOF、首输出前/后每格至少 5 轮；clean/讨论/用户/tool 数据必须 identity；任何私有 transcript 出现在 thinking、text、signature、redacted data 或 public error 即失败。真实 Claude CLI C2/C3/C4、20-tool long session 与 120k history 仍必须各至少 5 轮。

## 修复实现（2026-07-16）

- `src/anthropic/transcript_sanitizer.rs`
  - assistant request history 与完整 Anthropic response 不再跳过 thinking。
  - unsigned thinking 保持 `type=thinking`，只净化命中完整项目 transcript signature 的尾段。
  - signed thinking 同时检查 `thinking` 与 `signature`；任一命中时整块删除，绝不把修改后的正文与原 signature 重新配对。
  - redacted thinking 只做污染判定；命中时整块删除，干净数据原样保留。
  - 用户正文、tool input、tool result、代码围栏和不完整/无工具名匹配的讨论仍原样保留。
- `src/anthropic/stream.rs`
  - XML unsigned thinking 使用独立增量 sanitizer，候选不会跨入 text/tool block。
  - native reasoning 在结构边界前缓冲完整累计快照；签名晚到也不会发生“先发正文、后发现必须整块删除”的回滚问题。
  - native signed/redacted 污染整块抑制；只按实际下发的安全 thinking 计算 `thinking_tokens`。
  - 原子 thinking 缓冲硬上限为 1 MiB；超限 fail closed 并生成规范流错误，不继续积累内存。
- `src/anthropic/handlers.rs`
  - local non-stream 对 native/XML/signed/redacted 使用相同策略。
  - text 与 thinking suppression 合并写入既有 usage observability；没有伪造 thinking token 数。
- `src/external_pool.rs`
  - external non-stream 复用完整 response sanitizer。
  - external SSE 按完整 thinking block 缓冲，兼容逐字符 delta、start 内嵌 thinking、`signature_delta` 和块内 ping。
  - signed/redacted/unsigned 任一块确认污染后生成规范 SSE error，进入 fatal 状态并抑制后续 text、tool、usage 和 `message_stop`；不再用安全空白占位伪装成功。
  - clean response 原样保留 upstream usage 与未知字段；一旦确认污染，不向客户端继续发送 `output_tokens_details.thinking_tokens` 或成功 terminal。
  - 多个 SSE `data:` 行按规范以换行合并后解析；重写时只保留一个规范 data payload，CRLF、JSON 跨 data 行和逐字节 transport chunk 均不会绕过 sanitizer。
  - SSE 原子缓冲复用现有 `EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES=1 MiB`；超限后清空缓冲、发规范 SSE error、阻止后续 success terminal，并写入 stream error capture，不能伪装成空白成功。
  - raw request 预筛使用低频角色短语而不是固定 `\\n` 字节，因此可覆盖 `\\u000a`、CRLF 与混合转义；精确状态机仍负责最终判定，普通 marker 讨论保持 byte-identical。

## 当前复核证据（2026-07-16）

聚焦证据与 fail-closed 状态机结果见 [协议污染 fail-closed 证据](../evidence/protocol-contamination-fail-closed-20260716.md)。以下循环次数写在测试体内，不是把一次 cargo test 重复描述成五次：

| 场景 | 轮次 | 当前结果 |
| --- | ---: | --- |
| assistant history unsigned/signed/signature-field/redacted + user/tool 误报边界 | 每格 5 | pass |
| raw request history + complete response/external non-stream | 每格 5 | pass |
| local native signed cumulative snapshots/跨 chunk | 5 | pass |
| local native signature 字段污染 | 5 | pass |
| local redacted | 5 | pass |
| local XML unsigned 逐字符 | 5 | pass |
| local non-stream unsigned/signed/redacted | 每格 5 | pass |
| external SSE signed 逐字符 + signature delta | 5 | pass |
| external SSE unsigned 逐字符 | 5 | pass |
| external SSE start 内嵌 thinking | 5 | pass |
| external SSE redacted、clean signed content 事件原样、块内 ping 即时透传 | 每格 5 | pass |
| external SSE 多 data 行 + CRLF + JSON 跨行 + 逐字节 transport | signed/unsigned 各 5 | pass |
| raw JSON `\\u000a`/混合 CRLF 转义与 marker 讨论误报边界 | 每格 5 | pass |
| external/local 1 MiB 异常边界 | 各 1 个确定性边界测试 | pass |

已执行并通过：

- `cargo test transcript_sanitizer -- --nocapture`：`25/25`。
- `cargo test thinking -- --nocapture`：主程序 `82/82`，loadtest `3/3`。
- `cargo test signed_native -- --nocapture`：`1/1`。
- `cargo test redacted -- --nocapture`：`7/7`。
- `cargo test external_sse_ -- --nocapture`：`9/9`。
- `cargo test anthropic::stream::tests:: -- --nocapture`：`99/99`。
- `cargo test anthropic::handlers::tests:: -- --nocapture`：`85/85`。
- `cargo test external_pool::tests:: -- --nocapture`：`127/127`（需要 PostgreSQL 的 9 个集成分支按既有环境 gate 跳过）。
- 本专项最后一次 `cargo test --all-targets`：主程序 `1262 pass / 1 fail`，唯一失败为并行 admission 专题的 `token_bucket_caps_boundary_bursts_for_five_rounds`；thinking/transcript/external 聚焦集均通过。此前共享 revision 曾为 `1252/1252 + 26/26`，不能替代最终整树 gate。
- `git diff --check`：pass。`cargo fmt --check` 当前仅被并行 payload-guard 专题的 `src/anthropic/payload_guard.rs` 格式差异阻断；本专项触碰文件已 rustfmt。

## 尚未形成最终证据

- 真实 Claude Code CLI C2/C3/C4 至少 5 轮尚未执行，本项不能据此宣称最终关闭。
- fake upstream 的 local stream/non-stream 与 external HTTP/SSE 端到端 JSONL、request id、usage/stats 差值尚未归档。
- release build 曾在本专项前一版共享 revision 上通过，但 usage/multi-data/overflow 审查修订后的最终共享 revision 尚未重建；最终总 gate 仍需重跑，因为其他专题仍在并行修改共享树。

## 性能与误报边界

- marker-free raw body 走字节预筛并保持 byte-identical，不构造 JSON DOM；含 `\\u` 的 body 为防 Unicode 转义绕过会进入 JSON 精确检查。
- 1 MiB clean body 只证明正确性和有限执行，不是 p95 benchmark；B05/L5 仍需 release build 的 1 KiB-5 MiB 多档性能对照。
- matcher 必须同时确认角色行、结构和请求已知工具名/其确定性映射。任意 `artifactHashdeadbeef` 不因外形相似被信任。
- fenced、quoted、indented、inline discussion，以及 user/tool-owned 完整 fixture 均有 identity 反例测试。
- 为控制误报，未知且无法从当前/历史 tool_use 恢复的工具名不会仅凭 `Hashxxxxxxxx` 被删除；这意味着真正污染若同时丢失工具上下文，策略会保守漏检并依赖后续通用协议错误/观测。

## 残余风险

- local non-stream 污染当前直接 502，没有 response-level 换 credential；它保证不假成功，但可用性策略仍可讨论。
- external SSE 一旦 HTTP response 建立后不跨 pool 重试，即使尚未向下游提交内容；当前保证明确 stream error，不保证透明恢复。
- 当前证据主要是状态机/组件测试。真实 CLI、HTTP fake-upstream fault injection、长 session/resume、MCP/agent 混合与 release 性能尚未完成。
- signature/redacted 的官方未来字段只能按 unknown-field preservation 和原子策略兼容；出现新 block/delta 类型时应 fail conservatively 并新增 fixture。
