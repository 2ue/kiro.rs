# 生产请求 `THINKING_SIGNATURE_INVALID` 与流读取错误分析

日期：2026-09-28（Asia/Shanghai）

目标服务：`152.53.243.159:59137`

目标请求（用户界面显示的错误 ID）：`req_01tA8uNUqeCYgg94MNMbNaX6`

分析范围：只读生产证据、当前仓库源码和既有问题记录。本文不修改生产代码、配置、数据库、Redis、容器、进程或账号状态，也不把本次分析当作发布或修复。

## 结论先行

这条请求包含两个不同阶段的错误，不能合并成一个“签名错误”结论：

1. **第一次上游调用：已确认的请求兼容性错误。** 同账号第一次调用收到 HTTP 400，原因是 `THINKING_SIGNATURE_INVALID`，具体路径为 `messages.27.content.0`，消息为 `Invalid signature in thinking block`。
2. **第二次上游调用：最终失败的直接原因。** 系统按既有兼容逻辑在同一账号上剥离历史 reasoning 内容后重试；第二次响应头为 HTTP 200，并已经收到大量 reasoning 内容。最终在读取第二次响应体时发生 `error decoding response body`，因此记录为 `stream_error`。

本次证据**不能证明**第二次流读取错误是由第一次签名错误直接造成的，也不能进一步证明是 Kiro 服务端、代理、压缩/分块编码、HTTP 连接中断还是其他链路问题。能确认的是：最终错误发生在第二次调用的流式 body 读取阶段，而且已经越过下游提交边界，不能安全地换账号重放。

## 1. 请求身份与证据口径

### 1.1 错误 ID 不是 `usage_records.id`

直接以用户提供的 `req_01tA8uNUqeCYgg94MNMbNaX6` 查询 `usage_records.id` 为空。进一步在账号 `#1423` 最近 8 小时的 `data` JSONB 中检索后，找到 canonical usage row：

| 字段 | 值 |
| --- | --- |
| `usage_records.id` | `req_01cs7AQe21bKgNrxrgrygdTN` |
| `data.errorId` | `req_01tA8uNUqeCYgg94MNMbNaX6` |
| 时间（UTC） | 2026-09-28 06:51:49.804869 |
| 时间（Asia/Shanghai） | 2026-09-28 14:51:49.804869 |
| endpoint | `/dfcache/mineb/v1/messages` |
| model | `claude-opus-5` |
| credential | `#1423` |
| status | `stream_error` |
| error source | `local_account_stream` |
| internal/public status | `200 / 200` |

因此，页面上的错误 ID是对外关联 ID；查询、审计和 attempt 链路应同时保留 `usage_records.id` 与 `data.errorId`，否则会误以为“没有记录”。

### 1.2 证据时间与部署快照

- 证据目录：`tmp/prod-evidence/20260928-145914-152.53.243.159-thinking-signature/`
- PostgreSQL 采集时钟快照：2026-09-28 07:20:23 UTC
- 生产 app：`ghcr.io/2ue/kiro-rs:latest`
- 生产版本：`0.0.173`
- 生产 revision：`9022fc7f0a868c83dc2550b7b88ed23a2237ce90`
- app、PostgreSQL、Redis 均 healthy；`/healthz` 与 `/readyz` 均 HTTP 200
- app 未见重启、OOM、异常退出；`Running=true`、`Restarting=false`、`OOMKilled=false`

生产当前运行的是 `v0.0.173`，不是本地后续的 `v0.0.174`。本文只用本地源码核对已经在 `v0.0.173` 中存在的处理语义，不把本地未发布代码当作生产事实。

## 2. 完整 attempt 链路

### Attempt 1：历史 thinking signature 被 Kiro 拒绝

| 字段 | 值 |
| --- | --- |
| attempt | 1 |
| credential | `#1423` |
| HTTP status | `400 Bad Request` |
| action | `thinking_signature_retry_same_credential` |
| error type | `thinking_signature_invalid` |
| duration | 227 ms |
| diagnostic | `upstream_failure class=invalid_request upstream_status=400 public_status=400 ... reason=THINKING_SIGNATURE_INVALID` |
| 上游结构化片段 | `$.message: messages.27.content.0: Invalid \`signature\` in \`thinking\` block` |
| 上游原因 | `$.reason: THINKING_SIGNATURE_INVALID` |

这一步说明：发给 Kiro 的第一份请求中存在一个被 Kiro 判定无效的 thinking signature。它是第一次调用的真实拒绝原因，不是页面拼接出来的假错误。

### Attempt 2：同账号兼容性重试获得 200 响应头

| 字段 | 值 |
| --- | --- |
| attempt | 2 |
| credential | 仍为 `#1423` |
| HTTP status | `200 OK` |
| action | `response_headers_received_after_thinking_signature_retry` |
| duration to response headers | 6525 ms |
| `inferenceAttempts.consumed` | 2 |
| `localAttempts` | 2 |
| `externalAttempts` | 0 |

源码中的 `call_api_with_retry` 只在 HTTP 400 且 JSON `reason` 精确为 `THINKING_SIGNATURE_INVALID` 时进入此分支；retry body builder 会移除历史 `reasoningContent`，并且只消费一次。它不会在这一步换账号，也不会把第一次 400 当作普通账号故障。

### Attempt 2 的流式 body 阶段：最终失败

目标 usage 的 `latencyTrace` 给出了以下事实：

| 指标 | 值 |
| --- | ---: |
| `upstreamHeaderWaitMs` | 6747 ms |
| `upstreamHeaderMs` | 6756 ms |
| `firstUpstreamChunkMs` | 8731 ms |
| `firstThinkingDeltaMs` | 19671 ms |
| `firstOutputDeltaMs` | 19671 ms |
| `firstVisibleTextDeltaMs` | 19671 ms |
| `streamGapToFirstOutputMs` | 10940 ms |
| `upstreamBytesBeforeFirstOutput` | 22452 |
| `upstreamEventsBeforeFirstOutput` | 167 |
| `upstreamFramesBeforeFirstOutput` | 167 |
| 首输出前上游事件类型 | `reasoning_content: 167` |
| 首输出前 frame decode error | 0 |
| 首输出前 event parse error | 0 |
| `downstreamCommitted` | `true` |
| `terminalReason` | `internal_error` |

最终内部错误为：

```text
upstream stream read error: error decoding response body
```

这不是“第二次又返回 400 signature invalid”的证据。第二次已经有 200 响应头和大量 reasoning event，随后才在 `response.bytes_stream()` 读取阶段收到 reqwest body error。

## 3. 为什么最终记录是 HTTP 200

这条记录的 200 是**流式响应已经提交/响应头已对客户端可见后的公共状态包装**，不是“模型调用成功”。

当前代码语义如下：

1. `create_sse_stream` 通过 `response.bytes_stream()` 读取 Kiro 上游。
2. 首个下游事件提交前，如果发生 read error，允许在共享 attempt budget 和配置允许时重试。
3. 一旦 `downstream_committed=true`，`retry_stream_before_downstream_commit` 直接禁止再次重放。
4. read error 被记录为 `stream_error`，关闭已有内容块，发送 Anthropic SSE `event: error`，不发送伪造的正常 `message_stop`。
5. `record_stream_failure_from_context` 使用 `StatusCode::OK` 构造公共错误状态，因为 HTTP 头已经不能改写。

因此客户端必须看 SSE 事件内容，而不是只看 HTTP 状态：

```json
{
  "type": "error",
  "error": {
    "type": "api_error",
    "message": "The request could not be completed. Please retry shortly. ..."
  }
}
```

从本次生产记录可以确认服务端不会在已经提交部分输出后换账号重放；但本次只做生产证据审计，没有运行真实 Claude Code CLI，因此不能凭空断言某一版本 CLI 在收到该 SSE error 后一定自动继续、自动重试或立即结束交互。服务端能保证的稳定边界是：不伪造成功、不发送正常终止、不复制已经输出的内容。

## 4. 第一次签名错误的根因边界

### 已确认

- Kiro 对第一份请求中的 `messages.27.content.0` signature 返回了结构化 400。
- 系统识别到精确的 `THINKING_SIGNATURE_INVALID`，使用了同账号、一次性的兼容性重试。
- 目标请求的 `payloadGuardReport` 为：
  - `removedHistoryThinkingBlocks = 0`
  - `removedHistoryThinkingChars = 0`
- 因此没有证据表明这条请求是在本次 payload guard 中删除历史 thinking 后才产生该 400。

### 尚未确认

下列原因都不能仅靠当前 usage 记录选定：

- 入站历史中携带了跨模型或不匹配的签名；
- history/converter 在保序、重组或模型溯源方面产生了不一致；
- 签名已过期或与 Kiro session 状态不一致；
- Kiro 上游偶发误判；
- 更早的客户端/代理转换已经改变了签名关联内容。

既有 thinking/signature 分析已指出 signature 是上游生成的不透明值，不能伪造、修改、重算或把多个块随意合并。要进一步决定 payload guard、交错 thinking、多块 reasoning 和模型溯源的修复方案，必须先有脱敏的 Kiro 请求/响应抓包矩阵；本条生产记录本身不足以授权这些结构性改动。

## 5. 第二次 `error decoding response body` 的根因边界

### 已确认

- 发生在第二次调用，而不是第一次 400 本身。
- 发生在流式 body 读取阶段，而不是响应头阶段。
- 首输出前没有上报 event parse/frame decode error；最终错误文本是 reqwest 层的 `error decoding response body`。
- 目标请求已经越过下游提交边界，所以没有继续换账号。
- 生产服务当时健康，未见 app OOM、重启、数据库/Redis readiness 故障。

### 高概率但仍需证据的解释

这类错误通常与响应体传输链路有关，例如上游提前断开、chunked/压缩解码不完整、HTTP 连接关闭或代理链路中断。这里的“通常”是对错误类型的工程解释，不是本条请求的已证实根因。

### 当前未知

- 断开的具体字节位置和最后一个完整 EventStream frame；
- Kiro 返回的 `content-encoding`、`content-length`、HTTP 版本、region/上游地址和代理路径；
- 是 Kiro 主动关闭连接、反向代理截断，还是 reqwest 解码层发现格式不完整；
- 该错误与某个 region、账号、模型或上游版本是否有关。

现有 tool-format debug 文件没有命中目标请求，容器日志的目标时间窗口也没有保存更细的底层 body 错误。因此不能把这条错误进一步归因到某个网络组件。

## 6. 8 小时对照与账号状态

### 6.1 账号 `#1423` 快照

查询窗口为采集时刻向前 8 小时，计数是快照，不是永久统计：

| 状态 | error type | 数量 |
| --- | --- | ---: |
| `success` | - | 724 |
| `error` | `api_error` | 13 |
| `stream_error` | `api_error` | 13 |
| `client_dropped` | `client_dropped` | 7 |

账号 8 小时的流错误中：

- 没有 signature 片段：11
- 带 `THINKING_SIGNATURE_INVALID` 的链路：3

全实例同窗口的 `stream_error` 中：

- 没有 signature 片段：99
- 带 `THINKING_SIGNATURE_INVALID` 的链路：17

所以不能把所有 `upstream stream read error` 都归因于 thinking signature。目标请求是“signature 触发兼容重试 + 第二次流读取失败”的复合链路。

### 6.2 runtime / Redis 状态

账号 `#1423` 的 PostgreSQL `credential_runtime_state`：

- `failure_count = 0`
- `refresh_failure_count = 0`
- `disabled_reason = NULL`
- `warmup_remaining = 0`
- 没有最近 48 小时 `credential_events`

Redis 调度健康快照仍有选择计数和延迟 EWMA，但 `recent_error_rate = 0`、`transient_failure_streak = 0`。这不能证明调度器完全没感知流错误；源码存在短期 stream failure cooldown 路径。它能证明的是：本次错误没有留下持久账号禁用或 credential event。

这也暴露出一个可观测性口径差异：usage 中有 `stream_error`，而持久账号事件和 Redis recent error rate 没有相应记录。该差异应作为监控改进项，而不是直接判定账号健康模块失效。

## 7. 配置对本次行为的影响

生产 `runtime_config` 快照显示：

```text
compatProfile = claude-code
extractThinking = true
thinkingTriggerMode = real_request
kiroUpstreamStreamRetryEnabled = true
kiroUpstreamStreamRetryMaxAttempts = 5
kiroUpstreamStreamRetryOnReadError = true
kiroUpstreamStreamRetryOnIdleTimeout = true
kiroUpstreamStreamRetryOnStatusError = true
inferenceUpstreamMaxAttempts = 5
credentialStreamErrorCooldownSecs = 3
payloadGuardEnabled = true
payloadGuardTrimHistory = true
payloadShaping.discardHistoricalThinking = true
```

需要区分两件事：

- 全局配置允许流错误在**首个下游事件前**进入重试路径；
- 目标请求已经 `downstreamCommitted=true`，所以配置不会也不应该在此时继续重放。

本次 `payloadGuardReport` 没有删除历史 thinking，不能因为配置中存在 `discardHistoricalThinking=true` 就把它写成这条请求的已证实原因。该配置仍是独立的协议风险，需要真实抓包和专门回归验证，但不应由本条记录单独触发生产改动。

## 8. 可以优化的地方

### P0：把“尝试失败”和“最终失败”分开保存和展示

当前页面/usage 同时展示：

- attempt 1 的 `rawUpstreamError = THINKING_SIGNATURE_INVALID`
- 最终的 `errorMessage = upstream stream read error`

这会让读者误以为 signature 是最终错误。建议设计上分为：

```text
priorAttemptFailures[]:
  attempt=1, phase=request, type=thinking_signature_invalid, status=400

finalStreamFailure:
  attempt=2, phase=response_body_read, type=upstream_stream_read_error
  status=200, terminalReason=internal_error
```

公共错误仍可保持脱敏；Admin 详情显示阶段、attempt、是否提交下游和最终失败原因。

### P0：补齐第二次流读取的传输观测

每次 body read error 至少应保存不含凭据/正文的以下字段：

- 上游 HTTP status、content type、content encoding、content length、HTTP 版本；
- Kiro region/endpoint 的脱敏标识、代理链路标识；
- 已收到的总字节数、chunk 数、最后完整 frame 类型、decoder pending bytes；
- 是否已收到 message start、thinking/text/tool_use、metadata、terminal；
- `downstreamCommitted`、首个可见文本时间、最后一帧时间；
- reqwest error 的稳定分类，而不是只保存 `error decoding response body`。

这能把“上游断流”“压缩解码失败”“EventStream frame 损坏”“客户端取消”分开统计。

### P1：修正错误字节数命名

本条第一次错误同时出现 `body_bytes=114` 和 `rawUpstreamError.bodyBytes=125`。源码显示前者是原始 JSON body 字节数，后者是转换为 JSON path 诊断片段后的序列化文本字节数。它不是数据损坏，但命名会误导排障。

建议明确为：

```text
original_body_bytes
diagnostic_fragment_bytes
```

并在 UI 中分别显示。

### P1：建立 stream error 与账号健康的关联指标

usage 已经能识别 `stream_error`，但账号 runtime/event/Redis health 快照没有留下同粒度的持久信号。建议增加短期、可过期、按账号/模型/region 聚合的质量计数，区分：

- upstream read error；
- idle timeout；
- EventStream frame decode/protocol error；
- client dropped；
- downstream committed 后失败。

不能把这些都累计为永久账号失败，也不能继续让 `recent_error_rate=0` 隐藏真实流错误。

### P2：signature 相关结构性改动必须先抓包

以下工作不能仅凭本条 400 直接实施：

- 改变多块 thinking 的内部表示；
- 修改交错 thinking/tool_use 的保序；
- 改动 payload guard 的保护边界；
- 引入 reasoning block 的模型溯源；
- 把 retry 从“一次剥离历史 reasoning”改成更细的块级策略。

先完成同模型 object/array、多块交错、thinking 省略、模型不匹配和响应形状的脱敏 Kiro 矩阵，再决定兼容策略。

## 9. 不建议的处理

- **不建议**在 `downstreamCommitted=true` 后盲目换账号重放，会重复 message/thinking/text/tool 事件。
- **不建议**把所有 `THINKING_SIGNATURE_INVALID` 当作账号坏掉并禁用账号；本账号 8 小时仍有大量成功，runtime state 也没有禁用证据。
- **不建议**把所有 `error decoding response body` 都归因于 signature；全实例 8 小时大多数 stream error 没有 signature 片段。
- **不建议**仅因 HTTP 200 就把该请求写成 success；这里 200 只表示流式响应头已经提交。
- **不建议**因本条记录就删除或放宽 payload guard；本条 `removedHistoryThinkingBlocks=0`。
- **不建议**把 `body_bytes=114` 与 `bodyBytes=125` 当作上游返回损坏；它们是不同处理阶段的字节口径。
- **不建议**为了“避免积分”增加未经验证的 query/内容预检；本次证据与 WebSearch 预检无关，也没有证明预检能修复流传输错误。

## 10. 稳定处理建议（行为契约）

推荐保持以下分层：

| 阶段 | 可重试性 | 对下游行为 | usage |
| --- | --- | --- | --- |
| 上游响应头前/首个语义输出前，且共享预算允许 | 可按错误类型和配置有界重试 | 不发送旧尝试的语义输出 | 记录 prior attempt |
| 已收到协议前置事件但尚未提交语义输出 | 仅在明确可丢弃前置事件时重试 | 丢弃旧尝试缓冲 | 记录 retry reason |
| 已提交 thinking/text/tool/保活事件后 | 不重放 | 关闭内容块并发送 SSE `error`，不发送伪 `message_stop` | `stream_error` / `internal_error` |
| 客户端主动断开 | 不当作上游故障重试 | 释放资源 | `client_dropped` |

该契约不能保证 Claude Code CLI 一定自动恢复，但能保证服务端不会以重复输出或伪成功换取表面“不中断”。

## 11. 最终判定

### 事实

1. 用户给出的错误 ID是 usage 数据中的 `data.errorId`。
2. 第一次上游请求 400，精确原因是 `THINKING_SIGNATURE_INVALID`。
3. 系统在同一账号上剥离历史 reasoning 后重试。
4. 第二次响应头 200，随后流式 body 读取失败。
5. 目标请求已经提交下游输出，最终为 `stream_error`，公共状态 200。
6. 生产 app、PostgreSQL、Redis 当时健康，账号没有被禁用。

### 高概率推断

1. 第一次 400 是历史 thinking signature 与 Kiro 校验不一致的兼容性问题。
2. 最终错误更像第二次响应体传输/解码瞬态，而不是第二次签名拒绝。
3. 已提交后禁止重放是协议安全上正确的保守行为。
4. 当前界面把 prior attempt 与 final failure 放在一起，造成错误归因。

### 未知

1. signature 不一致的上游来源；
2. 第二次 body decode failure 的底层网络/HTTP/代理原因；
3. Claude Code CLI 对该 SSE error 的具体自动恢复策略；
4. 是否存在 region、模型或账号维度的相关性。

本报告完成的是证据归因和稳定性设计建议，不包含生产修复、代码修改、配置变更或发版。

## 12. 证据索引

- 精确 ID 映射与 `latencyTrace`：`tmp/prod-evidence/20260928-145914-152.53.243.159-thinking-signature/raw/db/phase3-target-id-json-path.txt`
- 完整 attempt chain：`tmp/prod-evidence/20260928-145914-152.53.243.159-thinking-signature/raw/db/phase3-target-attempt-chain.txt`
- 结构化错误与账号 8 小时统计：`tmp/prod-evidence/20260928-145914-152.53.243.159-thinking-signature/raw/db/phase3-structured-error-fields.txt`
- signature/stream error cohort：`tmp/prod-evidence/20260928-145914-152.53.243.159-thinking-signature/raw/db/phase3-signature-and-stream-error-cohort.txt`
- 生产部署/健康快照：`tmp/prod-evidence/20260928-145914-152.53.243.159-thinking-signature/raw/host/phase1-inventory.txt`、`raw/docker/phase1-compose.txt`
- 脱敏 runtime config：`tmp/prod-evidence/20260928-145914-152.53.243.159-thinking-signature/raw/db/phase3-runtime-config-shape.txt`
- 相关既有问题：`feature/issues/07-stream-internal-read-error.md`、`feature/issues/thinking-signature-retry-transient-response.md`
