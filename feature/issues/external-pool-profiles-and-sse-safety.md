# External Pool Profiles And SSE Safety

Status: `focused-response-state-machine-pass / handler-cli-load-pending`

Severity: P0

Related cases: B01, C01-C06, D01-D05, F01-F02

## 问题与影响面

external pool 有 `raw`、`normalized` 与 strict profile 三组相互独立的请求/响应路径。修复前，普通 text delta 可能被局部清理，但 thinking、多 `data:` 行、`content_block_start.content_block.text`、跨 block、逐字符 transport chunk 与 EOF 均可绕过；另一类失败是只删除污染片段后继续发送 `message_stop`，让 Claude Code 把被截断或空白回答误判为成功。

这不是 `Hashxxxxxxxx` 独有问题。可见指纹可能是 `user Continue`、`Tool results provided.`、`<function_results>`、原始或缩短工具名，也可能只是跨 block 后的半句、空回答、thinking/signature 异常或 usage 显示成功而正文缺失。影响 external direct、local fallback、跨池 failover、stream/non-stream、thinking、tool、MCP/agent 历史和长会话恢复。

strict 的旧行为也不自洽：clean raw 可以 byte-identical，但 polluted request 或普通 text response 会被修改，thinking 仍可能泄漏。normalized 路径可能在 external policy 决定前已经注入 prompt 并丢失未知字段；raw passthrough 即使配置允许 external prompt 也不会注入。usage 过去也没有 suppression blocks/chars/kinds，无法区分正常空回答和保护命中。

## 根因与源码链

1. sanitizer policy 不是 request-scoped route value，无法保证 direct/fallback/retry 使用同一合同。
2. request body 在 profile 和 external body mode 决定前被统一处理，混淆了 raw byte identity 与 normalized semantic identity。
3. 旧 SSE 投影以 transport chunk/简单事件切分为单位，只检查部分 text delta；没有把 `event:`、多行 `data:`、CRLF、start/delta/stop 和 EOF 组合成完整 Anthropic content-block 状态机。
4. thinking/signature/redacted thinking 需要原子保留或原子失败；逐 delta 删除会破坏签名或留下可见前缀。
5. response suppression 没有和 terminal/usage 状态绑定，污染被删除后仍可能合成 success terminal。

当前主要实现链是 `src/external_pool.rs` 的 request preparation、non-stream projection、SSE event drain 与 `ExternalAnthropicTranscriptState`，共同调用 `src/anthropic/transcript_sanitizer.rs`。最终 handler/fallback 语义还需由 `src/anthropic/handlers.rs` 的 request-scoped policy 和 downstream-commit 状态共同证明。

## 稳定复现

### 最小结构复现

向 external SSE 投影依次提供 `content_block_start`、thinking/text delta、`content_block_stop` 和 `message_stop`，将同一个污染串分别放入：单个 text delta；thinking 与随后 text 的跨 block 拼接；`content_block_start.content_block.text`；CRLF 和多个 `data:` 行；逐字节 transport chunk；tool boundary 前；没有 stop/terminal 的 EOF；signed thinking、独立 signature delta 和 redacted thinking。

污染串每轮分别使用带 hash、原始工具名和没有工具名的 scaffold。验收不是关键词消失，而是整个受污染 response fail closed、无伪造 `message_stop`，clean signed/redacted block value-identical。

当前聚焦入口：

```bash
cargo test external_sse_ -- --nocapture
cargo test external_non_stream_response_contamination_is_retryable_not_partial_success -- --nocapture
```

### 端到端与多轮复现

隔离启动 fake external 与临时 kiro.rs，在 raw/normalized/strict、stream/non-stream、direct/fallback/failover 的笛卡尔积上每格至少 5 轮。fake external 记录实际 body SHA、未知字段、上游 hit 与事件顺序；客户端记录首字节、terminal、usage、request/error ID。再以 20/100 tool cycle、120k history/resume、MCP/agent 混合历史重复同一矩阵。

### 异常、性能与恢复复现

分别注入首字节前污染、可见 text 后污染、thinking 后污染、tool_use 前后污染、半帧、CRC 错误、EOF、500/429 与 client drop。首输出前最多按共享 attempt budget 重试；首输出后 upstream hit 必须保持不变并返回规范 stream error。对 clean 1 KiB/100 KiB/1 MiB/5 MiB body 和 1 MiB 原子 thinking 上限记录 p50/p95/p99、RSS 与 serialize 次数。

## 方案比较

| 方案 | 优点 | 不接受或限制 |
| --- | --- | --- |
| 继续增加字符串/hash 正则 | 改动小 | 无法覆盖无 hash、跨 block、SSE 分块和签名；误删正常正文；不作为根治 |
| 统一把所有 external body parse 后重序列化 | 处理简单 | 破坏 raw byte identity、未知字段和签名；正常大 body 有不必要 CPU/内存成本 |
| request-scoped profile + 结构化 content-block 状态机 | 可以定义 raw/normalized/strict 合同，覆盖所有 route/attempt | 实现和测试面较大；是当前选定方案 |
| 污染命中后静默删除并结束 | 看似提高成功率 | 会制造空白成功/静默截断；明确禁止 |

## 选定修复与性能边界

- `strict/raw/normalized` 分别定义 request、response、prompt、sanitizer 合同；profile 值随 request 进入 direct/fallback/retry。
- clean raw 先做低成本 marker prefilter，未命中不 parse、不 serialize，保持 byte-identical；normalized 只按已声明合同改写。
- SSE parser 按完整事件和 content block 工作，覆盖 CRLF、多 `data:`、start/delta/stop、thinking/tool/signature/redacted 与 EOF。
- unsigned text/thinking 可在完整 block 上判定；signed/redacted 不局部改写。污染、原子 buffer overflow 或缺 terminal 一律明确失败。
- usage 记录 suppression blocks/chars/kinds、policy、route、attempt 和 terminal outcome；公开错误不包含原始污染内容。
- 原子 buffer 必须有固定上限；小 clean 请求不得承担大历史扫描或重复 JSON serialize。若状态机产生不可解释的正常路径 p95 回退，则回滚该批 response projection，但不能回滚到静默成功。

## 当前修复后证据

当前聚焦测试已覆盖 local/external response 的 raw/unhashed/deterministic-hash 工具名、unsigned/XML/signed/signature-field/redacted thinking、跨 block、CRLF、多 `data:`、逐字符和逐字节 transport、EOF/tool boundary、clean identity 与 1 MiB 原子上限。污染 response fail closed，clean signed/redacted 不重组签名，不能再产生成功 terminal。证据索引见 [协议污染 fail-closed](../evidence/protocol-contamination-fail-closed-20260716.md)。

这些结果来自演进中的 dirty test binary，不是最终 handler、真实 CLI 或统一 release candidate 结论。external SSE pre-byte 跨池 retry、完整 route policy、长会话和性能仍为 pending。

2026-07-18 的完整树同时暴露了 external Kiro-RS Tool usage precedence 的潜伏问题：测试在修改 system/history 后没有刷新 token 派生状态，因此通用 `input=raw` 偶然落在 `32..=4096` 并通过。刷新派生状态后，真实冲突变为通用 reported-usage 覆盖 Kiro-RS Tool 自己的 `reportedInputMin/Max`。当前修复让已解析的 Kiro-RS Tool 路由在通用 shaping 关闭时仍执行自身 cache/usage projection；只有显式启用通用策略时才覆盖。失败不提交 cache、成功后下一轮 cache read 和默认输入范围均已有精确测试；当前完整树 `1715/1715` 非 ignored 通过。该 dirty-tree 结果仍不替代 external handler/CLI/load gate，详见 [单测树证据](../evidence/full-unit-tree-red-green-20260718.md)。

## 最终验收与残余风险

B01、C01-C06 每路径至少 5 轮；D01-D05 覆盖真实 Claude Code CLI；F01-F02 覆盖 client drop、error burst/recovery 与 3 x 15 分钟 soak。必须同时满足：clean raw byte-identical；normalized 字段保留符合合同；所有污染形态无泄漏、无空白成功、无伪 terminal；总 upstream hit 不超过共享预算；首输出后 0 retry；RSS/FD idle 后回落；小 clean 请求性能在总门禁预算内。

未知未来 Anthropic/Kiro event 类型仍是残余风险。最终只能声明列出的 event、分块、profile、CLI 版本和观察窗口通过；未知类型应 fail closed 并保留脱敏观测，不能靠默认透传承诺永久无泄漏。
