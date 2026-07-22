# Protocol Transcript And Tool History Leak

Status: `current-candidate real-cli long-session pass / source-contract pass / native-real-upstream open`

Severity: P0

## 2026-07-20 真实 CLI 长会话复核

在 frozen kiro.rs binary + fake Kiro EventStream 下，真实 Claude Code CLI
2.1.197 完成 5x20 与 5x100 `--continue` session。测试同时覆盖本项目的
`bashHash...`/`readHash...` wire 映射：fake upstream 使用 wire 名称回调，CLI
侧必须恢复为公开 `Bash`/`Read`。逐 turn 对账 history、tool_result、session ID
和 usage，并扫描 `user Continue`、`Tool results provided`、
`<function_results>`、`<function_calls>`、`<invoke>`、已知/未知 Hash 指纹；
两档均为 0 泄漏。结果与限制见
[长会话证据](../evidence/claude-cli-long-session-continue-20260720.md)。

这证明修复后的长历史/继续会话路径没有在该 fake-upstream 合同下重放内部
transcript；它不替代真实 Kiro upstream、thinking signed/redacted、MCP/search/
image/agent 和错误恢复矩阵。

## 2026-07-22 当前候选复跑

当前仓库外 frozen `kiro-rs` 候选 SHA-256
`31b8c4749201b0f7666b63a9c268c0b75e21f6c1600b18c77bf39a7c6c249c2e`
再次通过真实 Claude Code CLI 2.1.197 长会话验证。本轮重点不是只扫
`bashHash...`/`readHash...`，而是同时覆盖旧占位、非 hash marker、function
scaffold、普通 `Bash`/`Read` 可见名和未知上游请求。

执行结果：

- `feature/tests/claude-cli-long-session-continue.mjs`：5 sessions × 20 tool cycles；
- CLI turns `110`，其中 `--continue` turns `105`；
- tool turns `100`，Bash / Read 各 `50`；
- inference hits `210`；
- tool_use / tool_result pairs `100 / 100`；
- `leakMatches=0`；
- `fakeUnknownRequests=0`；
- report SHA-256 `2342ef2f3c66ed84ecbeb45fb9cad471a0307e05f0de7a0fbeb85cdc289df7f7`；
- runner cleanup 全部为 true，且跳过受保护的 `127.0.0.1:9022`。

同一轮还通过协议污染与 marker inventory 源码合同：

- `protocol-marker-inventory-source-contract.test.mjs`
- `protocol-contamination-source-contract.test.mjs`
- `thinking-effort-kiro-wire-contract.test.mjs`
- `runtime-validation-paths.test.mjs`
- `thinking-effort-claude-cli-capture-signal.test.mjs`

合计 `30 tests / 30 pass / 0 fail`。这些合同锁定：

- sanitizer 不把任意 `NameHash[0-9a-f]{8}` 当内部工具名；
- `user Continue`、`user Tool results provided`、`Tool results:`、
  `<function_results>`、`<function_calls>`、`<invoke>`、`[previous output]`、
  `[trimmed output]`、`[duplicate output]` 这类相邻形态没有新的生产生成点；
- marker-free raw body 在清理路径上保持 byte-identical；
- signed/redacted thinking 污染按整块 fail closed，不重组签名块。

结论：当前候选在“真实 Claude CLI + fake Kiro EventStream + 长 `--continue`
工具历史”合同下，未复现用户报告的内部 transcript/tool leakage。该结论仍限定在本轮
fake-upstream 可观测模型内；如果未来官方 Kiro upstream 改变事件形态或新增 scaffold，
应由 fail-closed、attempt 上限和 marker inventory 合同捕获，而不是承诺绝不会出现任何未知
新形态。

## 现象与影响

Claude Code 长对话会看到 `user Continue`、`Tool results provided`、`<function_results>`、`[previous output]`、`[trimmed output]`、`[duplicate output]`、工具名和工具输出。`bashHash.../readHash.../editHash...` 是本项目映射名的强指纹，但无 hash、MCP 名和普通工具名也可泄漏，因此不能把问题缩成 hash 替换。

Git 历史还说明“hash 映射”和当前 `Hash` 指纹不是同一时点引入：`551b91f`（2026-03-31）先使用 `_` 加 8 位 SHA-256 缩短超长工具名；`df60bef`（2026-05-29）才改为当前 `Hash` 加 8 位 SHA-256，并对更多非法名称执行映射。两者都早于 `v0.0.94`/`v0.0.95`。这与用户观察到“以前也泄漏，只是指纹不同”一致，但只证明指纹演化，不单独证明泄漏根因或频率变化。

## 根因与已确认链路

1. converter/payload repair 将结构化 tool history 展平、裁剪拆散或把 malformed result 文本化。
2. 原始工具输出随后成为普通 user/assistant text。
3. 后续模型把这些内容当可见对话正文复述。
4. response sanitizer 只能删除已知形状，不能恢复结构，也无法覆盖所有 block/profile。

修复前 `payload_guard.rs` 的 Anthropic 和 Kiro repair 包含原始 tool result 到普通 text 的转换；历史 trim 每次删除后再 align/repair，可自行制造 orphan。修复前 sanitizer 又接受任意合法形状的 `*Hash<8hex>`，因此同时存在 false positive 风险。

### 占位文本是跨版本稳定注入源

Git 历史把“旧指纹”和“新指纹”连接为同一根因类，而不是两个偶发模型行为：

- `v0.0.107` 及更早版本在只有结构化 tool result、正文为空时注入 `Tool results provided.`。
- `ef70aef`（随后发布为 `v0.0.108`）仅把该占位改为 `Continue`。
- 因此旧会话可泄漏为 `user Tool results provided.`，新会话则更常表现为 `user Continue`；工具名是否带 `Hash<8hex>` 只是后续工具输出的映射指纹。
- 同一 `ef70aef` 还把 `readHash/editHash/bashHash` 等字面量写进默认 task-quality prompt，进一步向模型提示了项目私有标记；当前迁移会只对字节级匹配旧内置默认值的配置换成无指纹版本，自定义 prompt 不会被覆盖。

选定修复不再继续更换命令型占位词，而是使用最小无语义非空占位 `.`。结构化 `tool_results` 仍是事实载体；占位只满足 Kiro 的非空 content 约束，不能再向模型发出“继续”指令。旧两代占位仍保留在 sanitizer 的 legacy 检测中，以清理已经存在的会话历史。

## 稳定复现

- orphan current tool_result、mismatched historical result、duplicate result：各 5/5 原文 textify。
- 超限历史包含 `User -> Assistant tool_use -> current User tool_result`：5/5 配对被拆，result 进入普通 text。
- false-positive：完整 scaffold 后的 `artifactHashdeadbeef: legitimate output` 可被误删。
- 真实 CLI 20 tool cycles 正常基线可通过，说明泄漏依赖历史形状/故障，不是每次正常工具调用都会触发。

## 选定方向

- 为消息建立逻辑 turn：普通 user turn，或 `assistant tool_use + following user tool_result` 原子单元。
- trim 只能删除完整单元；active current pair 要么整体保留，要么返回明确 payload-too-large，不能拆。
- malformed result 默认 `reject` 或中性占位，绝不拼接原文；debug 诊断只记录长度/hash/ID 状态。
- sanitizer 只使用 request-computed mapped names 和明确 legacy fixture；不得把任意 hash pattern 当充分条件。

## 当前实现（尚未完成动态验收）

- converter 与 payload guard 已禁止 orphan、mismatch、duplicate、空 ID tool result 原文 textify；duplicate 只保留第一条合法结构化结果。
- converter 在 payload guard 关闭时也会清理历史非法结果，避免安全性依赖可选 guard。
- Kiro 与 Anthropic trim 已改为按完整逻辑 turn 原子删除，并保护 current tool-result 对应的历史 tool-use turn。
- sanitizer 只信任当前请求真实工具原名和使用 converter 同一算法得到的确定性映射名；`artifactHashdeadbeef` 之类普通正文不再仅因形状被删除。
- raw marker-free 请求走单遍预筛后保持原始 bytes；不会为清理逻辑无条件 parse/serialize body。
- raw prefilter 对 JSON escape 做固定状态扫描；不再因任意正常 `\\uXXXX` 出现就 parse/clone 整棵 DOM，同时保留 marker 任意字符被 escape 时的检测能力。
- 空 user/tool-result 正文占位已从命令型文本收敛为 `.`。

当前聚焦代码测试：converter `113/113`、payload guard `55/55`、旧 sanitizer 合同 `20/20`，并新增 2026-07-21 源码合同 `10/10` 与生产 marker inventory 源码合同 `4/4`。这些结果不替代修复后的真实 Claude CLI、长会话和 fault-injection 证据。

2026-07-21/22 C0d 当前候选继续复跑了完整 Node 源码/runner 合同批次：`node --test feature/tests/*.test.mjs` 为 `280 tests / 258 pass / 22 explicit skips / 0 fail`，其中 protocol contamination source contract 和 marker inventory contract 仍通过；feature docs 47/47 与 108 links 也通过。C0d 还通过 all-target Rust tests 与 release build，但没有完成真实 Kiro upstream、native MCP/search/image/agent 或 fault-injection service runner，因此本专题仍是 `partial / release-blocking`。

### 2026-07-21 源码合同补证：不局限 Hash 指纹

新增纯 Node 源码合同
[`protocol-contamination-source-contract.test.mjs`](../tests/protocol-contamination-source-contract.test.mjs)，
用于锁定“全类协议污染”而不是只锁定当前 `Hash<8hex>` 可见指纹。合同直接读取源码，不启动
Docker、不启动 `kiro.rs`、不调用 Cargo。

已通过：

- 单独运行：`10 tests / 10 pass / 0 fail`；
- 与业务/观测 Redis fault-domain 合批：`56 tests / 47 pass / 9 explicit live-signal skips / 0 fail`。

该合同确认：

- sanitizer 只信任当前请求已知工具名、确定性映射名和 legacy overlong 映射名，不使用任意
  `Hashxxxxxxxx` 正则作为删除条件；
- `artifactHashdeadbeef`、fenced/quoted/indented 示例、孤立 marker 和普通讨论不被误删；
- raw marker-free 请求在 JSON DOM parse 前返回 `Ok(None)`，避免清理逻辑无条件 parse/serialize
  body；
- assistant history 只清理 assistant text/thinking，不改 user/tool result/tool input/unmodeled fields；
- signed/redacted thinking 污染按整块 fail closed，不把改写后的 thinking 与旧 signature/data 重组；
- request strict profile 在上游前失败，sanitized history 不允许再走 raw external bypass；
- stream/non-stream/external 污染后进入 error/failover 语义，不允许空白或部分 success terminal。

证据见 [protocol contamination source contract 2026-07-21](../evidence/protocol-contamination-source-contract-20260721.md)。

### 2026-07-21 生产 marker inventory 补证

新增纯 Node 源码合同
[`protocol-marker-inventory-source-contract.test.mjs`](../tests/protocol-marker-inventory-source-contract.test.mjs)，
用于回答“是否还有其他特征不是 `Hash` 的类似泄漏入口”。合同排除 Rust test modules 与
`*/tests.rs` fixtures 后扫描生产源码，不启动 Docker、不启动 `kiro.rs`、不调用 Cargo。

已通过：`4 tests / 4 pass / 0 fail`。

该合同确认：

- `user Continue` 与 `user Tool results provided` 只作为 sanitizer 识别签名存在；
- `Tool results:` 只在 sanitizer 与 stream bounded observability 中出现；
- `<function_results>`、`</function_results>`、`<function_calls>`、`</function_calls>` 只在 stream
  protocol adapter 中出现；
- 生产代码不存在 bare `<invoke` / `</invoke>` 字面量，也不存在 `[previous output]`、
  `[trimmed output]`、`[duplicate output]` 生成点；
- current/history 只有结构化 tool_result、正文为空时使用 inert dot `.`，不再注入旧的命令型
  `Tool results provided`/`Continue` 文本；
- invalid duplicate/orphan tool_result 修复是 drop/dedupe，不把 rejected result content textify
  成普通 user text；旧 `textified*` 字段保留为诊断字段而非生产动作。

证据见 [protocol marker inventory source contract 2026-07-21](../evidence/protocol-marker-inventory-source-contract-20260721.md)。

## 新确认的残余逻辑缺陷：抑制后空成功或静默截断

2026-07-16 对当前工作树的状态机复核确认：`ToolTranscriptSanitizer` 在识别出完整内部 transcript 后进入 `DropUntilBoundary`，会抑制当前 text/thinking 段直到可信结构边界或 EOF。现有单测明确证明下面的输入只保留 `Safe prefix`，`</function_results>` 后的 `Let me continue` 也被删除：

```text
Safe prefix
user Tool results provided.

Tool results:

[readHash9b9a8d05] file contents
</function_results>
Let me continue
```

这项 fail-closed 行为可以阻止未知长度的工具输出继续泄漏，但当前调用方只记录 suppression 观测字段：

- 非流式本地响应在全部内容被抑制时补一个空格 text block，仍可按 success 返回。
- 流式本地响应可能在安全前缀已经提交后继续生成正常 terminal，形成用户不可解释的截断。
- external normalized/non-stream/SSE 路径同样需要验证是否把 suppression 当成功完成。
- 请求历史清理会删除污染点之后的同段合法历史；这是安全取舍，但必须记录，不能宣称语义完全无损。

因此本专题仍是 P0，不能以“marker 已不可见”关闭。选定修复合同是：首个下游可见字节前发现 suppression 时，把它归类为上游协议污染并在共享 attempt budget 内重试；预算耗尽后返回规范、脱敏且带 error ID 的错误。首个可见字节后发现时不得服务端重试，也不得伪造 `message_stop`/success，应发送规范 stream error 并把 usage 记为 error。历史输入清理继续 fail-closed，但必须用字段级 diff 证明 user/tool input、未知字段和污染前内容不变。

## 抑制终态修复与聚焦结果

上述合同现已在当前工作树实现：local stream 首提交前进入共享预算重试、提交后仅发 SSE error；local non-stream 返回 502/usage Error；external normalized non-stream 进入有限 pool failover；external SSE 的 text/unsigned/signed/redacted thinking 均 fail closed，污染后不再发送 success terminal。signed/redacted thinking 按整块处理。

聚焦测试包括 transcript 37/37、external SSE 12/12、external non-stream 2/2、reasoning leak 2/2，以及 signature、XML thinking 和 terminal 专项；关键形态在测试体内各 5 轮。完整命令、输入空间和残余 retry gap 见 [协议污染 fail-closed 证据](../evidence/protocol-contamination-fail-closed-20260716.md)。这仍不是 D01-D02/C06 的真实 CLI 与 HTTP 故障注入证据。

## 验收、回滚与残余风险

见 `../tests/reverification-matrix.md` A01-A04、A08、C06、D01-D02。每个动态形态至少 5 轮，20/100 cycle 长历史必须保持一一配对；JSONL、SSE、usage 和 fake upstream capture 均无内部 scaffold 或原始 malformed output。任何 suppression 都必须对应重试或显式 error，不能只验证关键词消失后仍接受空白/部分 success。

### 2026-07-19 长历史 fake-upstream 复核

本轮重新执行 r8 frozen candidate 的 fake upstream capture，修正了一个过宽的测试 oracle：`old_history_entry_with_large_tool_result` / `summarized_history_entry_with_hash_and_excerpt` 是 loadtest 构造的普通 tool_result 文本，不是内部 transcript 指纹。它们允许在合法 `toolResults` 中按 head/tail shaping 保留；不能据此要求代理改写 user/tool_result 数据。

实际禁止项仍是内部 transcript scaffold。复核结果见 [长历史 tool_result 边界证据](../evidence/long-history-tool-result-boundary-20260719.md)：

- `preemptive_large_tool_results`、`preemptive_mixed_pathological`、`preemptive_schema_key_mapping`、`on_too_long_large_tool_results` 共 20/20 请求成功；
- captured Kiro text fields 中 `user Continue`、`user Tool results provided`、`Tool results provided`、`<function_results>`、`</function_results>`、`[previous output]`、`[trimmed output]`、`[duplicate output]` 均为 0；
- structured tool_use/tool_result 无 orphan；
- `on_too_long` 另有故障注入：首个 554 KiB inference 被 fake Kiro 返回 `Input is too long`，代理 exactly 1 次 retry，retry body 约 37 KiB，仍无内部 transcript 指纹。

当前残余项是实际 HTTP 首输出前 retry、真实 CLI 20/100 tool cycle、120k history/resume、MCP/agent 混合和最终性能。回滚不得恢复命令型 placeholder、原文 textify、任意 `Hashxxxxxxxx` matcher 或 suppression 后 success；未知未来 scaffold 应 fail closed，并以结构化观测支持后续扩展。
