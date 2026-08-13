# Claude Code 内部协议泄漏与稳定性深度审计

日期：2026-07-15  
范围：当前工作树 release binary、Anthropic `/cc` 与 external pool 路径、Claude Code CLI 2.1.197、payload guard、prompt steering、retry/RPM、资源稳定性。  
结论：**当前候选不能宣称已经彻底修复，也不满足发布门禁。**

## 1. 安全边界与测试环境

- 未访问、停止或重启现有 `127.0.0.1:9022` 服务。
- 未读取或修改五个 `kiro_idc_users*.txt` 文件。
- 未使用真实凭据做负载；上游全部为本地 fake Kiro 或 fake Anthropic server。
- 隔离代理：`127.0.0.1:49022`。
- fake Kiro：`127.0.0.1:49080`。
- fake external Anthropic：`127.0.0.1:49180`。
- 隔离 PostgreSQL：`127.0.0.1:45432`。
- 隔离 Redis：`127.0.0.1:46379`。
- 当前 release SHA-256：`4623cdf4e3f7bc0e2fe3defa4e0862237e1f64bccbb91671c134e0d6b51556d8`。
- 旧 A/B 快照 SHA-256：`4a63ca5b4390e4e1b3064e0e7b2e3b181552731bbf551506d69879ed54cda12a`。
- Claude Code CLI：`2.1.197`。
- `cargo test --all-targets` 既有结果：主测试 `1199/1199`、loadtest `26/26` 通过。
- `cargo fmt --check` 仍失败，集中在 `src/external_pool.rs`。

原始报告与 capture 位于：

- `target/validation/deep-audit-20260715/reports/`
- `target/validation/deep-audit-20260715/captures/`
- `target/validation/deep-audit-20260715/direct_payload_matrix.rb`
- `target/validation/deep-audit-20260715/external_matrix.rb`
- `target/validation/deep-audit-20260715/claude_cli_matrix.rb`
- `target/validation/deep-audit-20260715/claude_resume_matrix.rb`
- `target/validation/deep-audit-20260715/count_tokens_matrix.rb`

## 2. 总体判断

`bashHash/readHash/editHash` 是项目工具映射的可见指纹，但不是泄漏的唯一根因。当前证据支持以下链路：

1. 合法结构化工具历史曾被展平或在 repair/trim 后被文本化。
2. 文本化内容以普通 user/assistant 正文进入后续模型上下文。
3. 模型可能复述 role scaffold、工具名和工具输出。
4. `Hashxxxxxxxx` 只是其中一种容易识别的工具名；无 hash 的 `[previous output]`、`[trimmed output]`、`[duplicate output]` 同样会泄漏内部工具内容。
5. v0.0.108 的默认 task prompt 持续注入 `readHash/editHash/bashHash` 字面 token，增加了回显这些指纹的概率；已有持久化配置没有 migration，会在升级后继续注入旧 prompt。

工具短名 hash 不是 v0.0.95 后才引入，但需要区分两次变化：`551b91f`（2026-03-31）首次引入“截断前缀 + `_` + 8 位 SHA-256”的可逆短名；`df60bef`（2026-05-29）才把格式改成当前项目特有的“截断前缀 + `Hash` + 8 位 SHA-256”，并扩大到包含分隔符等不符合 Kiro 约束的名称。Git ancestry 与 tag 内容复核表明 `v0.0.94`、`v0.0.95` 均已包含当前 `Hash` marker。因此 95 之后频率上升不能解释为 hash 映射首次出现，更接近占位文本、prompt steering、历史处理和重试链路后续变化的叠加结果。

## 3. 阻断级问题

### 3.1 Thinking 是稳定的泄漏绕过面

动态证据：

- local `thinking_delta` 中完整 transcript：3/3 泄漏。
- text -> thinking -> text 跨 block：3/3 泄漏。
- external `thinking_delta`：5/5 泄漏。
- external text -> thinking -> text：5/5 泄漏。
- 真实 Claude Code CLI：thinking leak 3/3，存在真实 `content_block_start(type=thinking)` 和 `thinking_delta`。
- CLI 最终包含 `output_tokens_details.thinking_tokens=10`，所以不是仅因提示词含 `think` 的假阳性。

当前 sanitizer 主要处理普通 text delta，没有覆盖 native/XML/external thinking。带 signature 的 thinking 不能简单修改正文，否则签名与内容失配；必须定义清晰策略，例如整块抑制、降级为公开错误，或仅在明确兼容 profile 下允许。

### 3.2 Payload trim 会重新制造工具结果文本化

即使 completed-cycle flatten 已删除，历史裁剪仍可能拆开：

```text
User prompt
Assistant tool_use
User tool_result
```

当前顺序可能先删 User，再删成为首项的 Assistant，随后 repair 把 tool_result 当 orphan，转成普通正文：

```text
[trimmed output]
<原始工具输出>
```

动态证据：显式 current shaping 下，paired current tool_result 5/5 从结构化 `toolResults=1` 变为 `toolResults=0`，原文进入普通 current user content。

修复要求：按完整逻辑 turn 原子裁剪；active `Assistant tool_use + current User tool_result` 必须整体保留，不能依赖 textify 修复由 guard 自己制造的 orphan。

### 3.3 Malformed tool history 仍会把原始输出转成正文

各 5 轮稳定复现：

| 输入 | 最终行为 |
| --- | --- |
| orphan current tool_result | 完整原文拼入 `[previous output]` |
| mismatched historical tool_result | 完整原文拼入 `[trimmed output]` |
| duplicate current tool_result | 第一份保留结构化，第二份完整拼入 `[duplicate output]` |

这些路径不依赖 hash 工具名，能解释“以前也泄漏，但指纹不同”。生产策略应决定 malformed output 是拒绝、只保留中性占位，还是仅在 debug profile 文本化；默认不应把未受信工具原文伪装成用户正文。

### 3.4 External sanitizer 不完整且破坏 strict passthrough

raw external 各 5 轮：

- 普通 text、逐字符、CRLF：能够拦截。
- thinking delta：泄漏。
- 跨 thinking/tool block：泄漏。
- SSE 多 `data:` 行：泄漏。
- `content_block_start.content_block.text`：泄漏。
- EOF：pending leak 被隐藏，但下游没有 `message_stop`，协议不完整。

strict profile 各 5 轮：

- clean raw request byte-identical。
- polluted raw request 被修改。
- non-stream 和 stream text response 被修改。
- thinking response 又保持泄漏。

因此 strict 当前既不是严格透传，也不是完整安全清洗。`ExternalRouteRequest` 需要携带 request-scoped sanitizer policy，raw/normalized/direct/fallback/retry 全程一致；不能仅根据 endpoint 推断。

### 3.5 多层重试造成真实 RPM 放大

直接 HTTP 既有证据：

- 默认 20 凭据：1 个 500 -> 20 upstream。
- 默认 20 凭据：5 个并发 500 -> 100 upstream。
- 默认 20 凭据：1 个 429 -> 20 upstream。
- 限制 `credentialRetryMaxAttempts=3` 后：5 个 500/429 -> 15 upstream。
- 首个可见输出前断流：5 downstream -> 10 upstream。

真实 Claude Code CLI、有界 15 秒测试：

| 场景 | CLI 调用 | fake upstream | 平均放大 | 结果 |
| --- | ---: | ---: | ---: | --- |
| partial stream disconnect | 3 | 27 | 9x | 3/3 超时，无 message_stop/usage |
| malformed eventstream | 3 | 9 | 3x | 3/3 被误报 success，output_tokens=0 |
| HTTP 500 | 3 | 90 | 30x | 3/3 超时 |
| HTTP 429 | 3 | 90 | 30x | 3/3 超时 |

错误波后 normal CLI 3/3 恢复，账号池未永久失效；但错误期间的瞬时放大已经不可接受。

需要统一跨以下层级的 attempt budget：provider credential retry、stream rectification、payload/cachePoint retry、external failover、local rescue 和客户端可重试响应。服务端还应提供按下游 key/channel 的 RPM 与并发准入，避免客户端 SDK 重试无限转化为上游流量。

### 3.6 HTTP 200 异常和 malformed stream 被误报成功

- HTTP 200 + `ThrottlingException`：非流式 6/6 返回空成功，usage 记录 success。
- malformed AWS EventStream：流式 6/6、非流式 5/5 返回 HTTP 200。
- 真实 CLI malformed：3/3 success，`output_tokens=0`，每次两轮。

这会同时造成逻辑错误、usage 污染和额外重试。必须把 AWS JSON exception、CRC/frame parse failure、缺失终止事件归一化成公开错误，不能合成 `end_turn` 空成功。

## 4. 高优先级问题

### 4.1 Sanitizer 有 false positive 和 matcher 绕过

稳定误删：

- `artifactHashdeadbeef: legitimate output`：3/3 被删。
- 已声明普通工具 `Bash: legitimate documentation`：3/3 被删。

稳定绕过：

- 前导空格的 `  user Continue`：3/3 穿透。
- 大写 `User Continue`：3/3 穿透。
- thinking block 正文：3/3 穿透。

修复应只信任本请求实际计算出的 mapped tool name，不接受任意 `[A-Za-z]...Hash[0-9a-f]{8}`；sanitizer 只能作为历史兼容兜底，不能替代结构化历史修复。

### 4.2 Raw sanitizer 的固定性能成本和 DoS 面

当前 raw 入口在 profile/route 决定前进行完整 JSON DOM parse、clone 和 typed parse。clean 大图片或长历史也承担成本。应先做廉价 byte marker prefilter，只对疑似污染 body 进入 DOM sanitizer；raw external 清洗应延迟到实际选中 raw pool 后执行。

旧/当前二进制 mixed pathological A/B：

| 二进制 | 请求 | 成功 | 本地 429 |
| --- | ---: | ---: | ---: |
| 旧快照 | 150 | 150 | 0 |
| 当前 release | 150 | 134 | 16 |

16 个错误全部位于 `stage=dispatch_queue`，Redis hot-op 固定 75 ms 超时后触发至少 2 秒 fail-closed。RSS 峰值没有翻倍，因此更可能是新增入口处理改变并发/调度时序，而不是单纯内存翻倍。

### 4.3 Payload max bytes 不是硬上限

`preemptive + max=100000`：

- 300k history：24 history -> 4，最终约 58 KB。
- 4 x 100k historical tool_result：每个缩至约 7.7k，结构仍保留。
- 60 个深层 tool schema：5/5 最终约 607 KB，远超 100 KB。
- 8 个大定义工具：5/5 最终约 224 KB，远超 100 KB。

工具压缩只裁 description/schema annotation，不保证 schema 总体进入预算。`still_oversized` 必须转化为明确可观察结果，不能让 UI 把 max bytes 描述成硬约束。

### 4.4 Current shaping 会重复图片 placeholder

有效、结构完整且超过 current image budget 的 PNG：5/5 图片被删除，但同一个 omission placeholder 被追加两次。

另一个边界：当前 5 MiB 检查比较 base64 字符串长度，而非 decode 后原始字节，约 3.75 MiB 原图编码后可能被按 5 MiB 提前丢弃。

### 4.5 External normalized 的 prompt 开关无效

`applyToExternalPool=false + normalized`：5/5 上游仍含 `<prompt_steering>`；body 从 266 B 变为约 2874 B，未知顶层和 message 字段被丢弃。

`raw_passthrough + applyToExternalPool=true`：clean request 仍 byte-identical，且没有 prompt。

这说明 normalized 在决定 external policy 前已经变异 parsed payload，而 raw direct 又绕过注入；同一开关在两条路径含义相反。

### 4.6 External 清洗缺少 usage 可观测性

明确命中 sanitizer 的 external 请求在 `usage_records` 中仍没有：

- `suppressedToolContextLeakBlocks`
- `suppressedToolContextLeakChars`
- `suppressedToolContextLeakKinds`

无法从生产 usage 判断是否发生清洗、删除多少、是哪一类泄漏。

### 4.7 启动模型目录同步可产生账号数级 RPM 尖峰

fake server 未实现合法 `/ListAvailableModels` 响应时，每次 temp proxy 启动会在约 0.1 秒内连续请求 20 个 enabled credentials。它不是每条生成请求的 payload retry，但证明启动/手工同步错误路径会产生 N-account 突发。

需要候选上限、健康采样、错误分类短路、退避抖动、singleflight 和同步冷却；模型目录 RPM 应与 inference RPM 分开统计。

## 5. Prompt Steering 与 Count Tokens

### 5.1 Master switch 错误控制协议语义

动态 3 轮：

- master ON + `tool_choice=none`：上游工具数 0。
- master OFF + 相同请求：3/3 收到 `Bash tool_use`。
- master ON：none/named/any 对应工具数 0/1/N。
- master OFF：none/named/any 全部退化为 N。

总开关不应控制结构化 `tool_choice` 过滤。建议拆为：

- Operator prompt：language/task/custom 及 scope/external/count_tokens policy。
- Protocol conversion：tool_choice、thinking conversion、chunk policy、schema/name mapping。

### 5.2 Scope 只约束一部分注入

`scope=cc_only` 各 3 轮：

- `/cc`：language/task prompt、tool_choice、synthetic thinking、chunk policy 均存在。
- `/v1`、`/na`、`/ha`：language/task prompt 消失，但 tool_choice、thinking、chunk 仍存在。

如果这是预期，UI 必须明确 scope 只管 operator prompt；如果 UI 宣称是总 scope，则 converter 必须收到 endpoint/scope 并统一执行。

### 5.3 Count tokens 与 Messages 转换不一致

各 10 轮：

- master ON：auto/none/named/any 全部固定为 889。
- master OFF：auto/none/named/any 全部固定为 80。

它只反映 prompt block 是否注入，不反映 tool_choice、synthetic thinking 或 chunk policy 的 Messages 实际变换。不能继续声称 count_tokens 与 Messages 使用完整同一口径。

### 5.4 UI 存在双配置权威

`bodyConversion.*` 与 `promptSteering.*` 是两套独立后端字段，但两个 UI 保存时会用 prompt 子开关覆盖前三个 bodyConversion 字段。经 API 设置的独立值可能被下一次 UI 保存静默改写。

## 6. 通过项与正向证据

以下通过不能抵消上述阻断项，但说明部分能力工作正常：

- valid PNG：5/5。
- JPEG 声明 + PNG 内容：5/5，规范化为 PNG。
- data URL PNG：5/5。
- 无效图片、不可达 URL：各 5/5 在本地 400，0 upstream。
- schema 非法 key、长工具名、MCP 风格名、Unicode 工具名：流式/非流式多轮 round trip，未向下游泄漏映射名。
- 真实 Claude CLI normal：3/3 success，usage 非零。
- Bash：3/3，1 tool_use 对 1 tool_result。
- Read：3/3，1 tool_use 对 1 tool_result。
- MCP `search_fixture`：3/3，server connected，tool result 含 `MCP_SEARCH_OK`。
- Claude CLI 20 次 Bash loop：3/3；每轮 20 tool_use、20 tool_result、21 message_stop，无 hash/scaffold。
- 120k CLI prompt：3/3 可消费，无 marker。
- session + resume：3/3 同 session id，resume 有 cache read，无 marker。
- 串行负载：normal 150/150、thinking 90/90、tool-use 90/90、mixed pathological 90/90。
- mixed 峰值 RSS 约 293 MB，结束瞬时约 203 MB，空闲 30 秒回落到约 20.7 MB。
- FD 从约 30 稳定到 37，本轮未观察到继续增长。
- 500/429 错误波后 normal CLI 各 3/3 恢复。

Claude CLI 2.1.197 在隔离 `--bare` 配置中未暴露内建 WebSearch；显式 `--tools WebSearch` 仍得到空工具集。因此没有把内建 WebSearch 伪报为通过。MCP search 已覆盖本地搜索工具协议，但不是内建 WebSearch 的等价证明。

## 7. 修复优先级

### P0：结构与安全

1. 历史 trim 按完整 turn 原子删除，永不拆 active tool pair。
2. 默认禁止 orphan/mismatched/duplicate tool output 原文 textify。
3. 删除或迁移持久化旧 task prompt 中的具体 hash 指纹。
4. 定义 thinking 清洗策略，并覆盖 local/external/native/XML/signature。
5. external sanitizer 加 compat/profile gate，strict 必须有确定语义。
6. SSE 按规范支持多 data line、CRLF、start text、EOF/error 和完整 stop 配对。

### P0：重试与错误

1. 引入统一 request attempt budget，跨 provider/stream/payload/external/rescue。
2. provider 默认重试改为小常数，不再默认覆盖整个账号池。
3. 识别 HTTP 200 AWS exception 和 malformed eventstream，返回规范公开错误。
4. 为下游 key/channel 增加服务端 RPM、并发与队列准入。
5. 错误响应携带合理 `Retry-After`，避免 Claude SDK 高频重试。
6. 分开记录 downstream requests、inference attempts、OAuth/profile auxiliary calls、scheduler selections 和 weighted capacity units。

### P1：Payload 与性能

1. raw sanitizer 先 byte prefilter，再按需 DOM parse。
2. 避免 clean request 的重复 clone、serialize 和无条件 SHA-256。
3. history trim 使用批量/二分预算，避免每删一条完整 repair + serialize 的近二次复杂度。
4. current fit 不应最多 64 次全量 serialize；增加 serialize count/iterations/stage latency 指标。
5. 工具 schema 无法进入预算时返回明确结果或采用可靠压缩策略。
6. 修复 current image placeholder 重复和 decoded-byte size 边界。
7. Redis hot-op 超时不要让整个进程进入全局 fail-closed 2 秒退避。

### P1：配置模型

1. master switch 只控制 operator prompt。
2. tool_choice/thinking/chunk 由 bodyConversion 或新的 compatibility policy 独立控制。
3. 两个 UI 不再镜像覆盖两套配置。
4. count_tokens 要么扩展到同一转换口径，要么删除“完全一致”的 UI 承诺。

## 8. 修复后发布门禁

必须全部满足：

1. `cargo fmt --check`、`git diff --check`、全量测试、release build 通过。
2. 提交脱敏真实 transcript fixture，不能依赖环境变量存在时才运行。
3. local/external stream/non-stream、text/thinking/tool boundary、逐字符、CRLF、多 data line、EOF/error 各至少 5 轮。
4. false-positive fixture（`artifactHashdeadbeef`、合法 `Bash:` 文档）保持原文。
5. strict raw/normalized request/response byte/semantic contract 明确且通过。
6. realistic Claude Code long history：严格 role alternation，至少 20 tool cycles，active current tool result，trim 前后配对完整。
7. Claude CLI normal/thinking/Bash/Read/MCP/resume/long context 各至少 3 轮，JSONL 扫描无内部 marker、事件错序或 active usage=0。
8. 429/500/partial disconnect/malformed 各至少 5 轮，统一预算下 upstream amplification 有明确硬上限。
9. 突发错误后 normal recovery 全部成功；RSS/FD 空闲后回落并稳定。
10. 当前/旧 binary 对称 A/B 不再出现当前版独有 dispatch_queue 429。
11. usage 中能关联 downstream request id、每层 attempts、suppression counters 和最终错误分类。

在这些门禁完成前，不能对外承诺“后续不会再出现内部解析暴露”。
