# Protocol Capability Regression Matrix

Role: 协议修复、sanitizer、payload shaping 和重试改动的正常能力防回归专题

Status: `partial / fake-upstream CLI long-session evidence landed; release-blocking`

Severity: P0 release gate

Last updated: 2026-07-16

## 2026-07-20 长会话补证

`feature/tests/claude-cli-long-session-continue.mjs` 在 frozen product
`131696bd...` 上使用真实 Claude Code CLI 2.1.197 完成 5 个独立 session 的
20 和 100 tool-cycle 矩阵。首轮使用 `--session-id`，后续 105/505 次调用只用
`--continue`；Bash/Read 各 50/250 次，tool_use/tool_result 全部配对，wire
history 与 body 持续增长，0 hash/scaffold marker，0 unknown fake-upstream
request。详细 report SHA 和限制见
[长会话证据](../evidence/claude-cli-long-session-continue-20260720.md)。

这项结果只关闭 fake Kiro upstream 下的真实 CLI resume/history/tool-pairing
子项，不把它外推为 native WebSearch、MCP、image/document、agents、真实 Kiro
upstream 或故障恢复通过。

## 现象与影响

本专题不是把所有协议缺陷合并成一个根因，而是防止修复某个泄漏或大 body 问题时破坏另一项正常能力。需要防住的用户可见异常包括：

- 正常回答被误删、只剩空格、提前结束或出现假的 `message_stop`；
- thinking 被当普通 text、丢失 signature/redacted data、usage 声称有 thinking 但没有真实 delta；
- `tool_use`/`tool_result` 数量或 ID 不配对，工具名没有可逆映射，历史 trim 后 active pair 被拆；
- 图片、document、URL、WebSearch、MCP 或 agent 输入在 converter/payload guard 中被无关策略改写；
- stream 与 non-stream、local 与 external、raw 与 normalized 对同一请求产生不可解释差异；
- CLI 长会话、resume、cache read/write 或 count_tokens 出现全零、失真、截断或上下文丢失；
- 错误正文暴露 credential、scheduler、external pool、内部 transcript 或映射工具名。

`bashHashxxxxxxxx`、`user Continue`、`Tool results provided.` 是已知强指纹，但不是本专题唯一失败条件。没有这些字符串的空成功、usage 为零、事件错序、工具漏配、字段丢失和语义改变同样是失败。

## 影响面

适用于五类 Messages 路由、Claude Code CLI、local Kiro、external raw/normalized、stream/non-stream、count_tokens、payload guard、prompt policy、usage/cache projection，以及真实 CLI 发起的 Bash、Read、MCP、search、image 和 agents/subagents 工作流。

## 根因与放大条件

当前已确认的风险链不是单一 matcher：

1. converter、history repair、payload trim、prompt policy 和 response sanitizer 分别拥有部分协议字段，所有权重叠时可能重复改写或遗漏字段。
2. raw/normalized、local/external、stream/non-stream 曾在不同阶段决定 policy，导致同一配置在路径间语义不一致。
3. thinking、signature、redacted data、SSE 多 `data:` 行和 terminal state 不能用普通 text 字符串替换安全处理。
4. body shaping 若只验证最终字节数，会漏掉 tool pairing、未知字段、图片真实字节和 cache/token 事实已经失效的问题。
5. 单轮 happy path 容易掩盖长历史 trim、session resume、错误恢复、client drop 和多层 retry 才触发的缺陷。
6. mock、direct HTTP 和真实 Claude Code CLI 观测层不同，任一层成功都不能自动证明其他层。

各具体根因分别由 transcript/thinking/payload/external/stream/retry 专题负责；本文件负责证明这些修复组合后没有破坏正常能力。

## 修复前与历史正向基线

2026-07-15 deep-audit binary 上曾记录：

- Claude CLI normal、Bash、Read 各 `3/3`；
- 20 次 Bash tool cycle `3/3`，每轮 20 个 `tool_use`、20 个 `tool_result`；
- 120k prompt `3/3`；
- session/resume `3/3`，resume 有 cache read；
- MCP `search_fixture` `3/3`；
- direct valid/normalized/bad image 各 5 轮；
- bounded normal/thinking/tool/mixed fake-upstream 流量完成且 RSS/FD 在观察窗内回落。

这些结果只定义修复前可用能力和性能对照。当前工作树已大幅修改 converter、payload、stream、external、scheduler 和 admission，必须在统一冻结候选上全部重跑。

## 复现方案

### 最小单点

每项先单独运行，避免组合成功掩盖单项失败：

- text：短文本、长 Unicode、代码围栏、引用和包含 marker 讨论的普通正文；
- thinking：native/XML、unsigned/signed/redacted，stream/non-stream；
- tool：auto/none/any/named、长名、Unicode 名、非法 schema key、空 description、null schema；
- media：PNG/JPEG/WebP/GIF、data URL、URL、空图、坏图、伪 MIME、decoded-byte 边界；
- document/web fetch：小/大内容、失败、超时和取消；
- usage/cache：message_start 与 final usage、thinking tokens、cache read/write、count_tokens；
- errors：invalid request/model/tool/image、429、500、HTTP 200 exception、malformed stream。

每个 fixture 至少 5 轮。clean 输入必须 value-identical；raw clean 路径必须 byte-identical；不允许通过删除整个 block 来“通过”泄漏扫描。

### 多轮与组合

- text -> thinking -> text -> tool_use -> tool_result -> final text；
- image/document + tools + thinking；
- local 首次失败后 external fallback，以及 external error 后规范失败；
- prompt master ON/OFF 与 tool_choice、thinking、chunk policy 的正交组合；
- 20 和 100 个 tool cycle，分别在不触发和触发 history trim 时运行 5 个 session。

组合断言包括事件顺序、block 数量、tool ID 配对、未知字段、usage、upstream hit 和最终 terminal，不只扫描关键词。

### 真实 Claude Code CLI 与长会话

使用隔离 `HOME`、`CLAUDE_CONFIG_DIR`、临时端口和当前 release binary：

- normal、alias、thinking、Bash、Read 各 5 次；
- 20-tool、100-tool、120k history、session/resume 各 5 个 session；
- MCP search 5 次；
- native WebSearch、agents/subagents、CLI image 在当前 CLI 真正暴露能力时各 5 次；不可用时只记录环境限制，不用 MCP 或 direct curl 冒充；
- 每轮保存脱敏 stream-json、request/error ID、服务端 route/model、tool/usage 计数和 fake-upstream stats 差值。

### 故障、负载与恢复

- 首输出前/后 idle、read error、client drop、429、500、JSON exception、坏 CRC、截断和缺 terminal；
- sudden burst、invalid traffic burst、proxy restart、Redis latency/disconnect/restart；
- 三次 15 分钟 fake-upstream soak，记录 p50/p95/p99、RSS、FD、状态分布和恢复请求。

故障后必须回到低并发 normal 流量并 `5/5` 成功；资源必须在 idle 窗口回到矩阵阈值内。

## 方案比较与选定设计

- 只扫描已知泄漏关键词：无法覆盖无 hash、thinking、空成功和结构错配，且会误删正常讨论，否决。
- 只跑全量单测或一个真实 CLI happy path：无法定位能力与路径，否决。
- 仅保存最终文本截图：看不到 SSE、usage、tool pairing 和重试放大，否决。
- 选定设计：结构单点、隔离 HTTP、真实 CLI、负载混沌四层分别取证；先按能力单点，再跑组合和长会话；所有结果绑定同一冻结 binary SHA。

## 性能与兼容性风险

- signed/redacted thinking 原子缓冲会增加首 thinking 延迟与最多 1 MiB/active block 内存；必须测 p95/p99 和超限 fail-closed。
- raw marker prefilter 若过宽会增加 DOM parse；若过窄会漏掉转义变体。必须同时测 marker-free 1 KiB-5 MiB 和 Unicode 转义污染。
- payload trim、schema sizing 和未知字段保留可能增加序列化；要求 clean raw 0 次 guard serialize，并验证 CPU/RSS 近线性。
- 真实 CLI 版本可能不暴露 native WebSearch/agents/image；此类环境限制不能降低其他适用门禁，也不能写成能力通过。
- 上游未来新增 block/event 字段只能通过 unknown-field preservation 和明确 fail-closed 策略兼容，不能静默丢弃。

## 实施与验收计划

权威 case 位于 [重新验证矩阵](../tests/reverification-matrix.md) 的 B、C、D、F01-F02：

1. 聚焦组件测试固定每项结构合同和反例。
2. fake Kiro/external 服务跑 local/external、stream/non-stream、raw/normalized 全路径。
3. 冻结 release binary，完成 C1 direct 与 C2-C4 真实 Claude CLI。
4. 完成 L1-L5 burst、chaos、recovery 和 soak。
5. 对修复前/后 capture 做字段级 diff，只允许专题文档明确批准的变化。

任何真实能力失败、usage 全零、tool pairing 丢失、未知字段无合同丢失、错误泄漏私有词、attempt 超预算或资源不回落均阻止发布。

## 当前修复与验证结果

- transcript、thinking、tool history、图片 decoded-byte、payload history 性能、400 classifier 和 request-key admission 已有各自聚焦结果，见对应专题及 [证据索引](../evidence/README.md)。
- API-key token-bucket 突发边界在生产一分钟窗口下重新执行，测试体内 5 轮均为前 32 个准入、第 33 个规范拒绝。
- 修复前 CLI normal/tool/MCP/long-history 结果仍只是回归基线，不计作当前候选通过。
- 2026-07-31 当前冻结候选的真实 Claude Code CLI thinking/effort wire 子门禁已通过：`cli`/`ide` × 6 effort × 5 轮，共 `60/60`；显式 `max` 未降级，未发现 violations/unknown request，详见 [2026-07-31 thinking wire evidence](../evidence/thinking-effort-kiro-wire-20260731.md)。该结果只关闭 thinking/effort 子门禁，不关闭本专题的 native WebSearch/image/agents、真实上游、签名响应、混合长历史、浏览器和 L1-L5 门禁。

## 未执行项与残余风险

- 当前统一冻结候选的 C1-C4 尚未完成。
- native WebSearch、agents/subagents 和真实 CLI image 尚无当前环境证据。
- 100 tool cycles、120k history/resume 与混合 MCP/agent 长会话尚未重跑。
- L1-L5、三次 soak、Docker 完整构建和两 UI 浏览器 gate 尚未完成。
- 在这些门禁完成前，本专题保持 P0 `protection-incomplete`，不能承诺“所有类似显示或逻辑异常以后都不会出现”。最终可声明的范围只限于已列输入、CLI 版本、路由、故障模型、负载档位和观察窗口。

## 发布与回滚

本专题全部适用 case 有当前 binary identity 和可复算证据后才允许进入发布。发布记录必须指向上一远端 tag、工作提交、版本提交和新 tag；协议回归时回滚二进制/配置到记录点，不能通过重新打开一个会改变 tool_choice/thinking 的旧 prompt 总开关掩盖问题。
