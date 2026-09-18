# sub2api-kiro 协议互转优化执行计划

Role: Claude Code/Anthropic ↔ Kiro 协议互转的实施计划、复现台账和验证入口

Status: `In Progress`

Last reviewed: 2026-09-17 Asia/Shanghai

Owner: 当前 Rust 项目 `kiro.rs` 的协议兼容与验证范围

Related:

- [协议互操作对比](../../../../../docs/analysis/sub2api-kiro-protocol-interop-comparison-20260915.md)
- [协议互转优化路线](../../../../../docs/analysis/sub2api-kiro-protocol-interop-optimization-20260916.md)
- [Thinking signature 协议安全](thinking-signature-protocol-safety.md)
- [验证与发版门禁](validation-and-release-gates.md)
- [当前协议和 API baseline](../../../baseline/protocol-and-api-contracts.md)
- [当前运行时流程](../../../baseline/runtime-flows.md)

本文是执行中的计划。当前首批实现限定为协议互转的 fixture、contract test、低并发运行时验证、复现台账和脱敏证据；除非本文件明确记录，任何候选兼容策略都不表示已经被真实 Kiro 上游接受。

## 1. 背景

### 1.1 为什么现在做

2026-09-15 的最新对比文档已经完成当前 Rust 与 `../sub2api-kiro` 的源码、测试和协议边界对照。对比结果不是“Rust 缺少所有兼容能力”，而是两边的优势不同：

- Rust 的 EventStream 帧解析、CRC、partial EOF、typed event、SSE state machine、tool mapping、schema normalization 和 usage/cache 分层更严格。
- `sub2api-kiro` 在跨版本事件 shape fallback、端到端工具名合法性扫描、trim 后 tool pair 重新收敛、WebSearch SSE index 处理、payload guard 阶段测试和具体 400 message 分类方面提供了可借鉴样本。
- thinking/signature、多个 reasoning block、Kiro endpoint 对 `origin/profileArn` 的真实要求、WebSearch index 和 payload threshold 仍有真实上游不确定性。

如果直接把 `sub2api-kiro` 的单体 translator 或宽松 fallback 移植到 Rust，可能破坏当前已经成立的严格边界。因此第一阶段必须先把“当前行为”变成可回放证据，再根据证据选择最小改动。

### 1.2 当前基线

计划开始时的仓库事实：

| 项目 | 当前值 |
|---|---|
| 分支 | `main` |
| 当前 HEAD | `5313bc1` |
| 当前标签 | `v0.0.163` |
| 最新对比文档 | `docs/analysis/sub2api-kiro-protocol-interop-comparison-20260915.md` |
| 优化路线文档 | `docs/analysis/sub2api-kiro-protocol-interop-optimization-20260916.md` |
| 指定本地验证实例 | `127.0.0.1:19023` |
| 指定运行配置 | `tmp/thinking-budget-local/config.json` |
| 指定测试存储 | PostgreSQL `kiro_thinking_budget_20260901`，Redis `127.0.0.1:26379/0` |
| 当前阶段 | P0 窄范围实现已落地；focused tests、真实本地凭据 normal/alias 闭环和 fake 外部池协议链路已验证 |

工作区中已存在的未跟踪分析/证据文件不属于本计划的业务实现改动，后续不得误删或回滚。

## 2. 目标与非目标

### 2.1 目标

1. 建立可回放的请求、EventStream、Anthropic SSE、工具、thinking、usage/cache 和错误 fixture。
2. 证明 Claude Code 看到的 SSE lifecycle、工具调用、thinking 和 usage 语义没有被互转过程破坏。
3. 对 trim、repair、schema fallback、event shape fallback 和 retry/failover 做局部、可观测、可回滚的兼容处理。
4. 将协议帧错误、语义转换错误、客户端兼容错误和调度 retry 错误分层。
5. 记录每一次复现、修复、测试命令、二进制 SHA、服务 PID、请求 ID 和残留清理结果。

### 2.2 非目标

- 不在没有真实证据前改变 Kiro `origin`、`profileArn`、User-Agent 或 endpoint 默认值。
- 不用 `map[string]any` 替换 Rust 的 typed EventStream parser。
- 不全局采用 sub2api 的窄 schema whitelist。
- 不把宽泛 JSON repair 作为默认兼容策略。
- 不忽略 upstream cache usage。
- 不生成或伪造 thinking/signature；不把 synthetic signature 默认加入生产路径。
- 不因所有 400 都属于“请求错误”而统一跨账号、跨池 failover。
- 不在本计划中改变收费、余额、成本或 usage 计费口径。
- 不对生产服务压测，不导出真实凭据、refresh token、API key 或完整账号 JSON。

## 3. 参考依据

### 3.1 直接依据

| 参考 | 用途 |
|---|---|
| [2026-09-15 协议互操作对比](../../../../../docs/analysis/sub2api-kiro-protocol-interop-comparison-20260915.md) | 当前 Rust 与 sub2api 的事实对照、证据标记和候选项 |
| [2026-09-16 协议互转优化路线](../../../../../docs/analysis/sub2api-kiro-protocol-interop-optimization-20260916.md) | 优先级、分批范围、验收条件和不建议照搬项 |
| [Thinking signature 协议安全](thinking-signature-protocol-safety.md) | signed/redacted thinking、payload guard 保护边界和真实抓包阻塞项 |
| [验证与发版门禁](validation-and-release-gates.md) | Rust、fake upstream、Claude Code CLI、PgSQL/Redis 和资源清理门禁 |
| `docs/testing/project-test-instance.md` | 指定本地服务、端口、存储、账号和临时资源的安全边界 |

### 3.2 当前 Rust 代码依据

请求方向：

- `src/anthropic/converter.rs`
- `src/anthropic/converter/content.rs`
- `src/anthropic/converter/history.rs`
- `src/anthropic/converter/schema.rs`
- `src/anthropic/converter/tools.rs`
- `src/anthropic/converter/tool_pairing.rs`
- `src/anthropic/payload_guard.rs`
- `src/kiro/endpoint/ide.rs`
- `src/kiro/endpoint/cli.rs`

响应方向：

- `src/kiro/parser/frame.rs`
- `src/kiro/parser/header.rs`
- `src/kiro/parser/decoder.rs`
- `src/kiro/model/events/base.rs`
- `src/kiro/model/events/additional.rs`
- `src/anthropic/stream.rs`
- `src/anthropic/websearch.rs`

错误与重试：

- `src/kiro/provider.rs`
- `src/kiro/retry_pipeline.rs`
- `src/external_pool/retry_pipeline.rs`
- `src/external_pool.rs`

### 3.3 sub2api 参考依据

以下只作为“实现经验和测试样本”参考，不作为官方协议规范：

- `../sub2api-kiro/backend/internal/pkg/kiro/translator.go`
  - semantic event fallback；
  - tool fragment/JSON repair；
  - usage/cache projection；
  - thinking tag buffer；
  - tool name/schema/pairing。
- `../sub2api-kiro/backend/internal/pkg/kiro/websearch_stream.go`
  - buffered SSE；
  - content block index 计算和过滤。
- `../sub2api-kiro/backend/internal/pkg/kiro/payload_guard.go`
  - 阶段化 payload guard 和 `DeferredToUpstream`/`StillOversized` 语义。
- `../sub2api-kiro/backend/internal/pkg/kiro/tool_name_charset_test.go`
  - 完整 payload 工具名合法性检查。
- `../sub2api-kiro/backend/internal/pkg/kiro/payload_guard_orphan_toolresult_test.go`
  - trim 后 tool pair 收敛。
- `../sub2api-kiro/backend/internal/pkg/kiro/schema_whitelist_test.go`
  - accepted subset 测试样本。
- `../sub2api-kiro/backend/internal/service/kiro_error_classifier.go`
  - 400 message 子分类经验。

## 4. 当前协议边界与必须保持的事实

### 4.1 请求转换边界

```text
Anthropic/Claude Code request
  -> raw request facts / route policy
  -> typed converter
  -> tools/schema/history/thinking/content normalization
  -> Kiro IDE/CLI envelope
  -> payload guard and bounded retry
  -> Kiro upstream
```

必须保持：

- raw external passthrough 不因新增测试或 semantic adapter 自动进入 parsed/local body pipeline；
- tool name mapping 在 tools、tool_choice、history tool_use、embedded tool recovery 中保持一致；
- tool_use/tool_result 修复不能制造孤儿 pair；
- schema normalization 的语义字段不能被全局窄 whitelist 静默删除；
- 当前 request/body 的兼容 profile 不能因一次 fallback 污染后续请求。

### 4.2 响应转换边界

```text
Kiro EventStream bytes
  -> frame/header/CRC/EOF validation
  -> typed Kiro Event
  -> bounded semantic adapter
  -> StreamContext
  -> Anthropic SSE lifecycle
```

必须保持：

- 半帧、坏 CRC、非法 header、非 clean EOF 不得被当作成功；
- typed event 解析成功时优先使用 typed 字段；
- unknown/malformed event 不得静默变成普通文本、空文本或 tool call；
- `message_start`、content block lifecycle、terminal `message_delta` 和 `message_stop` 顺序稳定；
- tool input fragments 必须按源顺序拼接；
- signed thinking/redacted thinking 必须按不透明协议块原样处理；
- upstream usage/cache 与本地 projection/reporting 分层。

### 4.3 调度与错误边界

```text
protocol parse error
  != semantic conversion error
  != public error normalization
  != retryability
  != credential cooldown/failover
```

一个具体的 400 message 只有在确认调用点消费逻辑之后，才能决定是否：

- 本地修复；
- fail closed；
- 同池重试；
- 跨池/跨账号；
- cooldown；
- 直接返回客户端错误。

## 5. 拟定原则与待确认决策

本节保留“拟定”与“已验证”的区分：用户已确认开始实现和低并发真实账号验证，但真实上游观察仍不能自动升级为协议规范。

### 5.1 拟定原则

1. **先证据、后策略**：第一批只建立 fixture、contract test 和复现记录，不改变默认协议行为。
2. **typed-first**：严格 typed parser 是主路径，semantic adapter 只能处理已知 shape fallback。
3. **strict-by-default**：无法证明边界的 repair、unknown event 和不透明块改写均 fail closed。
4. **局部 profile**：真实上游确认的差异按 endpoint/model/profile 定向开启，不全局放宽。
5. **可解释降级**：任何 normalize、repair、trim、fallback、retry 都必须能从报告或日志定位。
6. **协议与调度分离**：协议 400 不自动等于可换池；上游 transient 也不能绕过请求总 deadline。
7. **真实客户端优先**：直接 HTTP 只能证明代理协议，Claude Code CLI 才能证明客户端实际消费。
8. **证据不含秘密**：只保存脱敏摘要、哈希、计数、状态、请求 ID 和错误 ID。

### 5.2 实施前需要用户确认的事项

| 决策 | 推荐选项 | 原因 |
|---|---|---|
| 首批范围 | P0 测试/证据层，不改默认策略 | 当前已有大量兼容逻辑，直接改行为风险高 |
| tool pair 无法恢复名称 | 删除孤儿 `tool_result` 或 fail closed | 不能猜名称补造 `tool_use` |
| semantic adapter 默认行为 | 只对已知 shape 开启，unknown 保持诊断/失败 | 防止 map fallback 吞掉协议错误 |
| schema 降级 | endpoint/model profile，默认关闭 | 防止全局丢失有效 schema 语义 |
| thinking 多块处理 | 先抓包决定 object/array/位置化/拒绝 | sub2api 代码不能证明 Kiro schema |
| WebSearch index | 先复现，再增加局部 index helper | 当前 Rust 已有独立 MCP/SSE 路径 |
| synthetic signature | 暂不实现 | 本地 HMAC 不等于真实上游 signature |
| 真实账号/上游 | 先 fake/local isolated；用户明确确认后再做低并发真实调用 | 本轮已按该边界执行，真实账号结果只证明当前路径，不证明协议规范 |

## 6. 分阶段实施方案

### 阶段 0：计划确认与实现前盘点

状态：`已完成`

工作内容：

- 确认首批范围和允许改动的模块；
- 记录当前 HEAD、工作区状态、指定验证实例 PID/命令/端口；
- 盘点已有测试，避免与现有 stream/tool/WebSearch 合同重复；
- 生成本轮 `run_id` 和证据目录；
- 确认所有 Cargo 命令使用 `feature/tests/run-cargo-scoped.sh`。

交付物：

- 用户确认记录；
- 计划状态更新；
- 初始工作区和服务基线摘要。

不允许：

- 修改业务代码；
- 启动第二个常规 `kiro.rs` 实例；
- 触碰生产服务或真实账号状态。

### 阶段 1：P0 fixture 与 contract test

状态：`focused-validated`

建议先实现：

1. 完整 payload 工具名合法性扫描。
2. EventStream 原始 frame 的 malformed/truncated/duplicate fixture。
3. tool fragment、EOF closure、SSE lifecycle fixture。
4. tool_use/tool_result trim 场景回放。
5. 错误分类到 retry/failover/cooldown 的调用点测试。
6. fixture metadata：endpoint、model、credential class、region、source shape、expected outcome。

影响文件候选：

- `src/anthropic/converter/tools.rs` 测试区；
- `src/anthropic/converter/tool_pairing.rs` 测试区；
- `src/anthropic/payload_guard.rs` 测试区；
- `src/kiro/parser/frame.rs`、`decoder.rs` 测试区；
- `src/anthropic/stream.rs` 测试区；
- `src/kiro/provider.rs` 测试区；
- 必要时新增 `feature/tests/*-protocol-interop*.mjs`，但不把真实凭据写入 fixture。

阶段验收：

- fixture 能独立重放；
- 失败能定位到 request/event/SSE/retry 层；
- malformed/unknown 不会被断言为成功文本；
- 所有 scoped target 在每个批次结束时清理。

本轮结果：

- EventStream frame/decoder 的合法、分片、截断、坏 CRC、非法 header、重复 frame
  fixture 已完成；
- Kiro typed event 已增加已知 event key 的 nested/top-level 兼容解析；
- `toolUseEvent.input` 已覆盖字符串 fragment 与完整 JSON value；
- Anthropic SSE 已覆盖 thinking → text → tool → usage 的稳定 lifecycle；
- Claude 工具名映射已覆盖完整序列化 Kiro payload，tool-choice steering 与结构化工具定义
  使用同一个 Kiro-safe 名称；
- 未改变 strict CRC/EOF 主边界，未知 event 和 malformed known nested payload 仍 fail closed。

本轮尚未完成的 P0 候选项：trim 后 tool pair 的最小回放，以及带 fixture metadata 的独立
runner；错误分类到 retry/failover/cooldown 的调用点合同已补齐最小外部池边界，但本地
Kiro provider 全部 400/429/5xx 的真实 HTTP 消费矩阵仍保持为后续 focused case，不将本阶段
状态误解为整个互转矩阵已经完成。2026-09-16 的真实运行时
验证补充证明了 normal/alias/usage 的 Claude Code 闭环和 fake 外部池 SSE 生命周期，
但 thinking/tool-use 真实账号调用因 Kiro `402 quota exhausted` 只能记为 partial。

### 阶段 2：P1 低风险互转修复

状态：`窄范围 focused-validated；normal/alias isolated-real validated；其余待真实证据`

候选实现：

- trim 后 tool pair 重新收敛的共享 helper；
- semantic event adapter 的已知 nested/top-level fallback；
- payload guard stage/deferred/still-oversized 诊断字段统一；
- thinking/embedded tool 跨 chunk 语义矩阵；
- endpoint/model-specific schema accepted subset fixture 和最小 profile。

本轮已落地的窄范围内容仅包括“已知 event key 的 nested/top-level fallback”、完整 JSON
tool input 的兼容投影，以及 Claude tool-choice 到 Kiro-safe 工具名的一致化。trim 后
tool pair、schema profile、thinking 多块、WebSearch index 和 payload guard stage
仍按下方真实证据门槛处理，未扩大默认策略。真实运行时已验证 `sonnet` alias 到
`claude-sonnet-4.5` 的 normal/usage 闭环；thinking/tool-use 没有可用额度，不能标记
为真实通过。

进入条件：

- 阶段 1 发现明确可复现缺口；
- 缺口不是已有实现误读或测试误差；
- 可以定义默认行为、profile 开关、回滚方式和验收矩阵。

阶段验收：

- 非流式/流式语义一致；
- 不产生孤儿 tool result；
- fallback/repair/trim 可观测；
- 不改动 strict EOF、CRC 和 typed parser 主边界；
- 现有 Claude Code fake-upstream 和本地服务回归不退化。

### 阶段 3：P2 真实上游定向适配

状态：`待真实证据`

候选实现：

- WebSearch injection context 的局部 SSE index map；
- 400 message 子分类和实际 retry/cooldown 消费；
- usage/credits alias 与极端整数回放；
- 真实上游证明必要时的独立 compatibility profile。

进入条件：

- 有脱敏原始 request、response headers/status、EventStream frame 和转换后 SSE；
- 同一场景在至少两个回合或两个独立 case 中可复现；
- 已确认变更不会扩大到普通 tool/thinking/text 路径。

## 7. 复现与问题台账

本台账用于记录“问题是否真实存在、如何重现、修复是否已验证”，不把推断写成现状。

### 7.1 已有源码级复现/高可信风险

| ID | 场景 | 当前状态 | 复现入口 | 处理门槛 |
|---|---|---|---|---|
| RI-001 | payload trim 后最新 assistant tool_use 被裁掉，当前 user 保留对应 tool_result | 已在分析中定位，需补最小回放测试 | `payload_guard` trim + `tool_pairing` | 必须证明恢复、删除或 fail closed，不能发孤儿 pair |
| RI-002 | 多 reasoning block / thinking 与 tool_use 交错的上游 schema 不明确 | 源码边界已确认，真实 shape 未知 | `thinking-signature-protocol-safety.md` 抓包矩阵 | 不得凭字段数组猜测保序语义 |
| RI-003 | redacted thinking 是否被过度校验/改写 | 相关风险已有分析，需与当前 HEAD 再核对 | `src/anthropic/types.rs`、stream fixture | 必须逐字 round-trip，资源保护另行处理 |
| RI-004 | transcript sanitizer 是否扫描不透明 signature/redacted data | 相关风险已有分析，需测试锁定 | `src/anthropic/transcript_sanitizer.rs` | signed/redacted 必须作为协议原子 |
| RI-005 | EventStream 截断、坏 CRC、重复 event、tool fragment EOF | parser 主路径已有严格语义，跨层矩阵仍需集中 | `src/kiro/parser/*`、`src/anthropic/stream.rs` | 不得把 partial EOF 当成功 |
| RI-006 | 外部池 model mapping miss / model unavailable 的重试消费 | 已补边界合同：跳过当前池、允许其它合格池；不在同池重放，不混入普通请求 400 | `src/external_pool/retry_pipeline.rs`、`src/external_pool/tests.rs` | 真实第三方池仍需低并发复核，不能只按状态码判断模型路由 |

### 7.2 尚未由真实上游确认的场景

| ID | 场景 | 当前结论 | 需要的证据 |
|---|---|---|---|
| RI-101 | Kiro 是否接受一个 assistant 的多个 reasoning block | 未知 | object/array/交错请求的真实 status 和响应 shape |
| RI-102 | 工具回合完全省略 thinking/signature 是否可接受 | 未知 | 同模型、同上下文的对照请求 |
| RI-103 | thinking block 是否按模型绑定 | 设计上应保守处理，Kiro 行为待确认 | model X block → model Y request |
| RI-104 | WebSearch 插入 block 后 content-block index 是否需要偏移 | 当前 Rust 独立生成路径，未证明有缺口 | 原始/转换后 SSE 与 Claude CLI 消费结果 |
| RI-105 | payload threshold 的真实计算口径 | 当前 Rust 使用 JSON bytes，sub2api 有 weighted metric 经验 | endpoint/model/credential 低并发阈值实验 |
| RI-106 | endpoint/origin/profileArn 组合的接受范围 | 不能从两个仓库直接推断 | credential × endpoint × region 抓包矩阵 |

### 7.3 每个复现记录的固定字段

```text
run_id:
case_id:
git_commit:
working_tree_summary:
service_port:
service_pid:
service_command_redacted:
binary_sha256:
claude_cli_version:
route:
endpoint:
credential_class:
region:
requested_model:
resolved_model:
request_id:
error_id:
http_status:
first_byte_ms:
raw_frame_sha256:
converted_sse_sha256:
observed_outcome:
expected_outcome:
retry_decision:
cooldown_decision:
leak_scan:
cleanup:
```

严禁把 API key、refresh token、cookie、proxy password、完整 credential JSON 或完整用户 prompt 写入持久化证据。

## 8. 测试与验收矩阵

### 8.1 C0：静态和 Rust focused tests

每个 Cargo 命令必须通过 scoped wrapper：

```bash
feature/tests/run-cargo-scoped.sh <scope> -- cargo fmt --check
feature/tests/run-cargo-scoped.sh <scope> -- cargo test --locked <focused-filter>
feature/tests/run-cargo-scoped.sh <scope> -- cargo check --all-targets --locked
```

首批 focused filter 候选：

- tool name mapping/serialization；
- tool pairing/trim/orphan；
- payload guard；
- EventStream frame/decoder；
- stream block lifecycle/tool fragment/thinking；
- provider error classification/retryability。

验收要求：

- 记录通过/失败/ignored 数量；
- 失败保留最小复现命令和输出摘要；
- 每个 scoped target 在批次后被 wrapper 清理。

### 8.2 C1：直接 `/cc/v1` 协议检查

最低覆盖：

1. normal stream；
2. normal non-stream；
3. tool payload；
4. thinking stream/non-stream（只有真实 thinking 输出才算通过）；
5. alias model；
6. invalid request/model；
7. EventStream malformed/truncated/duplicate；
8. route-specific usage/cache。

必须检查：

- HTTP status；
- `message_start`；
- content block lifecycle；
- final `message_delta.usage`；
- request/error ID；
- public error 不泄露内部 pool/credential/scheduler。

### 8.3 C2：真实 Claude Code CLI 非交互

使用指定本地实例和隔离 CLI HOME/config：

```bash
HOME=/tmp/kiro-claude-home-19023 \
CLAUDE_CONFIG_DIR=/tmp/kiro-claude-config-19023 \
ANTHROPIC_BASE_URL=http://127.0.0.1:19023/cc \
ANTHROPIC_API_KEY=<redacted> \
claude --bare --print --verbose \
  --output-format=stream-json \
  --include-partial-messages \
  --no-session-persistence \
  --model sonnet \
  'Reply with exactly: pong'
```

覆盖：

- normal；
- model alias；
- thinking；
- Bash/工具调用；
- invalid model/request；
- moderate long output；
- tool_use/tool_result follow-up。

通过条件：

- CLI 真实执行并经由 `kiro.rs`；
- usage 非零且合理；
- thinking 只有在捕获真实 thinking block/delta 时才计为通过；
- tool use/result 数量和 ID 配对；
- 无内部术语泄漏；
- 运行结束无临时进程、端口或 target 残留。

### 8.4 C3/C4：交互、长会话和兼容回归

在 P1 代码改动后执行：

- 多轮 tool-use；
- thinking → text → tool；
- 历史 tool_result 大输出；
- MCP nested schema；
- payload 接近 guard 上限；
- signed/redacted thinking（有真实 fixture 时）；
- `/cc`、`/v1`、`/na`、`/ha` 和外部池 raw/normalized 路径。

不能用一个 happy-path `curl` 代替 C2/C3/C4。

## 9. 真实账号、fake upstream 与外部池测试边界

### 9.1 首批默认只使用 fake/local isolated

首批 P0/P1 测试优先使用：

- Rust 单元/集成测试；
- `kiro_loadtest` fake upstream；
- 隔离 external pool fake server；
- 指定的本地 `19023` 验证实例；
- caller-owned 的临时 PostgreSQL/Redis（仅专项 runner 需要时）。

fake upstream 进程可以独立启动，但必须记录端口、PID、生命周期并在 case 结束后停止。
本轮在用户明确要求后增加了低并发真实本地凭据验证，仍使用独立临时存储和临时服务；
这属于受控例外，不改变“fake fixture 不能被真实一次成功替代”的原则。

### 9.2 真实本地账号/外部池

只有在用户确认需要真实低并发验证时，才使用指定实例；若指定实例缺失，才允许
按技能例外使用一个临时项目实例，并记录原因和完整清理结果。本轮的指定实例
`127.0.0.1:19023` 与 `tmp/thinking-budget-local/config.json` 均不存在，因此使用了
`19023` 临时实例、独立 PostgreSQL 数据库和 Redis DB 15：

- 先核验 `127.0.0.1:19023` listener PID 和命令属于本仓库；
- 不为每个 case 启动新的 `kiro.rs`；
- 账号导入只通过该实例的启动 bootstrap 或 Admin API，不直接写共享库；
- 记录 `total/success/skipped/failed`，不记录秘密；
- case 之间使用 run/case ID 和受控时间窗隔离；
- 真实上游调用保持低并发和小样本；
- 完成后只清理本轮生成的记录、fake 进程和临时目录。

本轮真实验证结果：

- 账号文件聚合为 `235` 个，原始文件未修改；
- direct `haiku`/`sonnet-4.5` 的多个 case 因前置账号 `402 quota exhausted` 返回
  归一化 `502`，malformed request 返回 `400 invalid_request_error`；
- 真实 Claude Code CLI `2.1.273`、`--model sonnet` 退出码 `0`，服务端记录
  `local_credential/local_success` 且 input/output usage 非零；
- fake 外部池 non-stream/stream 均 `200`，SSE lifecycle 正常；
- 临时池 `supportedModels` 白名单格式不匹配导致 `503` 的复现已在后续
  canonical normalization 合同中处理，外部池保存/过滤使用 Claude Code 模型命令，
  本地凭据仍使用 Kiro upstream 模型名；
- runner 的宽泛 `publicLeak=true` 告警已补静态复核合同和后续报告字段：完整 stdout
  不保存，只保留 hash、行/字节数和脱敏可疑行摘要；完整长会话动态门尚未重跑。

真实账号测试不能替代 fake fixture，也不能把真实上游一次成功写成协议规范。

## 10. 回滚与停止条件

### 10.1 回滚原则

- P0 测试批次原则上不改变运行时行为，回滚只删除新增测试/fixture。
- P1 兼容实现必须有 profile 或配置开关；默认行为有明确基线。
- semantic fallback 发现未知 shape 时应 fail closed 或回到 typed error，不应继续猜测。
- WebSearch index、schema subset、thinking 多块 support 均应可单独关闭。
- 不回滚用户已有的无关工作区修改。

### 10.2 立即停止条件

出现以下任一项，停止扩大实现范围，先记录复现：

- Claude CLI 出现内部 credential/pool/scheduler 术语；
- stream 出现空白成功、静默截断、重复 terminal 或非法 block index；
- tool_use/tool_result 变成孤儿或重复；
- thinking/signature/redacted data 被改写、重排或伪造；
- malformed/partial EventStream 被当作成功；
- 一个协议 400 导致不合理跨池/跨账号放大；
- usage/cache 字段因 fallback 被双算或静默丢失；
- Cargo target、临时 fake 服务、Redis prefix 或测试数据库未清理。

## 11. 实现进度记录

只在实际执行后更新本节。每个阶段至少记录日期、提交/工作树、完成项、未完成项和下一步。

| 日期 | 阶段 | 状态 | 完成内容 | 测试/证据 | 下一步 |
|---|---|---|---|---|---|
| 2026-09-16 | 阶段 0 | `complete` | 用户已确认开始实现；完成基线、服务边界、已有测试和 P0 影响面盘点 | 尚未运行本计划新增测试；未修改业务代码 | 补齐 P0 fixture 与 contract test |
| 2026-09-16 | 阶段 1 / P0 | `focused-validated` | 完成请求方向工具名 payload 扫描、EventStream frame/decoder fixture、typed event shape fallback、tool input JSON value 兼容和 Claude SSE lifecycle 合同 | [P0 evidence](../../../../../feature/evidence/sub2api-kiro-protocol-interop-p0-20260916.md)；scoped Cargo focused tests 与全 target check/fmt 通过 | 保持 unknown shape/strict EOF/CRC 边界；把运行时结果与 focused evidence 分层 |
| 2026-09-16 | 阶段 2 / 窄范围 P1 | `focused-validated / isolated-real-partial` | 已知 Kiro event key 的 nested/top-level semantic fallback、完整 JSON tool input 投影、tool-choice Kiro-safe 名称一致化；normal/alias/usage 真实闭环通过，thinking/tool-use 受额度阻塞 | 同一份 [P0 evidence](../../../../../feature/evidence/sub2api-kiro-protocol-interop-p0-20260916.md)；真实 CLI `2.1.273`、fake external SSE 和 cleanup 均有脱敏记录 | 复核 leak 告警，处理 `supportedModels` normalization，并补可用额度下的 thinking/tool-use |
| 2026-09-16/17 | C1/C2 隔离运行时验证 | `isolated-real-validated / partial` | 真实本地凭据 normal/alias/usage 闭环通过；direct malformed 400、额度耗尽 502 分类和 fake 外部池 SSE 通过；thinking/tool-use 真实账号因额度耗尽未通过 | [P0 evidence](../../../../../feature/evidence/sub2api-kiro-protocol-interop-p0-20260916.md)；冻结二进制 SHA、CLI `2.1.273`、临时服务/DB/Redis 清理均有记录 | 复核 CLI runner leak 告警；修复或增加 `supportedModels` canonical normalization；取得可用额度后再补 thinking/tool-use |
| 2026-09-17 | 模型命名/runner 证据补强 | `contract-validated / dynamic-not-rerun` | 外部池模型保存、发现和测试下拉统一 Claude Code 命名；本地凭据测试下拉保持 Kiro 命名；长会话 runner 引入最小脱敏 stdout 证据和 secret/internal-term hard fail | [P0 evidence 后续补充](../../../../../feature/evidence/sub2api-kiro-protocol-interop-p0-20260916.md)；`claude-code-model-name-contract` 与 `claude-cli-leak-scanner.contract` 合计 `5/5` 通过 | 重跑完整长会话动态门需要冻结二进制、caller-owned PG/Redis 和可控 Claude CLI 运行窗口；可用额度后再补真实 thinking/tool-use |
| 2026-09-17 | 外部池错误消费边界合同 | `focused-validated / fake-chain-compiled / real-third-party-not-run` | `model_mapping_miss` 与 `model_unavailable` 作为池/模型路由错误跳过当前池并允许跨池；不触发同池重放；普通请求 400 仍 fail-closed，协议错误仍遵循显式开关；新增 HTTP 400 模型不可用 failover loop 合同 | scoped Cargo `external_pool_retry_boundary`：`2 passed / 0 failed / 0 ignored`；scoped fake-chain test `1 passed / 0 failed / 0 ignored`；`cargo fmt --check`、`git diff --check` 通过；target wrapper 清理完成。当前未设置 PG/Redis，因此集成测试按既有 helper 返回，不代表真实存储运行 | 在提供 caller-owned PG/Redis 时重跑 404/400 模型不可用集成矩阵；取得可用额度后再补真实第三方池与本地 provider 400/429/5xx 消费矩阵 |
| 2026-09-17 | 本地 provider 400/404 消费合同 | `focused-validated / fake-upstream-source-contract` | `MODEL_UNAVAILABLE` 的 HTTP 400 与 404 均只在存在其它可用凭据时切换账号；同账号不重复；普通 malformed/schema/tool/image/body-invalid 400 仍单次 fail-closed；不扩大到外部池或 `402 quota exhausted` | scoped Cargo `proto-interop-provider-retry-boundary-404`：`1 passed / 0 failed / 0 ignored`；`git diff --check` 通过；wrapper 清理完成。fake upstream body shape 不能替代真实 Kiro 404 证据 | 在 caller-owned PG/Redis 和可用额度条件下重跑真实本地账号 400/404/429/5xx；保持 thinking signature 同凭据受控 retry，暂不扩展 unknown error shape |

### 11.1 阶段更新模板

```markdown
### YYYY-MM-DD：<阶段/批次>

- 状态：`in progress` / `focused-validated` / `blocked` / `complete`
- 基线：`<commit/tag>`，工作区摘要：`<redacted summary>`
- 改动文件：`<paths>`
- 行为变化：`<default/profile/none>`
- 已完成：
  - ...
- 未完成：
  - ...
- 复现问题：
  - case_id:
  - 原始现象:
  - 最小重现:
  - 当前判断:
- 测试结果：
  - command:
  - pass/fail/ignored:
  - evidence:
- 资源清理：
  - target:
  - temporary processes:
  - ports:
  - Redis/PG:
- 下一步：
  - ...
```

## 12. 测试结果与证据索引规则

### 12.1 结果分级

| 级别 | 含义 |
|---|---|
| `source-verified` | 当前源码/测试直接证明 |
| `focused-pass` | 指定 focused filter 通过 |
| `fake-upstream-pass` | fake upstream/隔离 pool 通过 |
| `direct-protocol-pass` | C1 直接 HTTP/SSE 通过 |
| `claude-cli-pass` | C2/C3 真实 Claude Code CLI 通过 |
| `real-upstream-observed` | 真实 Kiro 上游观察到，仍不是官方规范 |
| `blocked` | 重复复现但缺少必要外部状态/证据 |
| `not-run` | 尚未执行 |

### 12.2 证据文件

实现后按批次新增：

```text
feature/evidence/
  sub2api-kiro-protocol-interop-p0-YYYYMMDD.md
  sub2api-kiro-protocol-interop-p1-YYYYMMDD.md
  sub2api-kiro-protocol-interop-cli-YYYYMMDD.md
  sub2api-kiro-protocol-interop-upstream-YYYYMMDD.md
```

证据摘要必须包含：

- baseline commit/tag；
- command and scope；
- binary SHA（需要运行时二进制时）；
- service port/PID；
- case count and pass/fail；
- request/error ID；
- frame/SSE hash；
- leak scan；
- cleanup result；
- 未验证项和残余风险。

原始 body/frame/日志只保存在本轮临时目录；完成摘要和哈希后删除原始文件，不把 Cargo target、增量目录或完整秘密带入 evidence。

## 13. 计划完成标准

在进入代码实现前，至少满足：

- 用户确认首批范围；
- 目标测试文件/模块和不改动边界明确；
- 每个 P0 case 有输入、期望输出和失败分类；
- 真实 CLI/fake upstream/指定服务的资源边界明确；
- rollback 和停止条件明确；
- 当前基线已记录；
- 本计划已在 README/roadmap 可发现。

在实现阶段完成后，才可把状态从 `Planning / awaiting confirmation` 更新为：

- `In Progress`：已开始编码并有未完成 TODO；
- `Focused Validated`：指定批次测试和证据通过；
- `Blocked`：同一外部阻塞条件连续三次复现且无法由当前仓库继续推进；
- `Complete`：计划范围、测试、证据和清理全部完成。

## 14. 当前结论

阶段 1 已完成 focused validation，且已知 event shape/tool input 与工具名互转的窄范围
实现已落地。2026-09-16/17 又完成了隔离真实本地凭据与真实 Claude Code CLI 的
normal/alias/usage 验证，以及 fake 外部池协议验证；当前证据属于
`focused-pass`/`isolated-real-validated`/`fake-external-pass`，仍不能替代真实
Claude Code thinking/tool-use 全矩阵或把一次真实上游观察写成协议规范。

下一步按风险从低到高：

1. 在冻结二进制与 caller-owned PG/Redis 条件下重跑完整长会话动态门，复核新增
   stdout hash/脱敏摘要报告；
2. 取得可用额度后补 C1/C2 thinking、tool-use 和长会话；仅当有脱敏真实 frame/SSE/request
   证据时，再处理 trim pair、schema profile、thinking
   多块、WebSearch index 和错误分类到 retry/failover/cooldown 的调用点行为；
3. 任何 unknown event、malformed nested payload、strict EOF/CRC 回归都保持停止扩大范围。
