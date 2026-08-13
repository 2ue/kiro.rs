# 生产慢首字根因分析

分析日期：2026-07-06  
时间口径：用户侧展示按 Asia/Shanghai；生产主机本地时区为 UTC+02:00；数据库查询按 UTC 对齐后再换算。  
范围：只读查看生产容器、Postgres usage 记录、运行时配置和日志片段；未重启、未改配置、未清理日志、未对生产压测。

## 结论摘要

本次生产慢首字的主因不是本机内存爆炸，也不是本地 SSE 解析慢，而是调度容量模型和真实上游负载不匹配。

当前系统主要按 RPM、请求并发条数、账号错误率和延迟 EWMA 调度；但生产流量里大量请求是长上下文、高 cache read/cache creation、高工具 schema 体积的重请求。一个 5k token 的轻请求和一个 200k+ cache read、几十个 tools 的重请求，在当前调度里基本都只占一个并发槽。这会让“70 多 RPM”低估真实上游压力。

2026-07-06 17:52 Asia/Shanghai 这一分钟：

- 成功流请求约 `73` 个。
- 上报 token 口径合计约 `31,685,638 tokens/min`，包括输入、cache read、cache creation、输出。
- 成功流请求在途并发处在 40-50 量级。

因此上游看到的不是“低 RPM 轻请求”，而是几十条长流式、长上下文、工具重、cache 重的请求同时占用模型容量。

## 目标请求样本

请求 ID：`req_017wUH2qJV5eoRer2d1yhjmg`  
上海时间：`2026-07-06 17:52:38`  
模型：`claude-sonnet-5`  
路由：本地凭据成功  
账号：`#462`

耗时拆解：

| 阶段 | 数值 |
| --- | ---: |
| 总耗时 | `67.38s` |
| 请求检查 | `1ms` |
| 上游响应头 | `7.59s` |
| 首个流分片 | `52.75s` |
| 首次输出 | `52.81s` |
| 分片到输出 | `59ms` |
| 输出前分片 | `1` |
| 输出前事件 | `1` |

这条记录的直接含义：

- 本地请求检查和 payload guard 不是卡 52 秒的地方。
- 本地拿到首个可输出内容后，`59ms` 内就完成了下游输出。
- 最大等待发生在“上游响应头已到”到“上游首个 body chunk 到达”之间，约 `45s`。

所以这条请求不能解释成“本地解析分片慢”。它是上游已经接受并返回 HTTP header 后，迟迟没有开始有效 body/SSE 数据。

## 生产配置证据

运行时配置版本：`145`，更新时间 `2026-07-06 05:12:21 UTC`。

关键运行时配置：

| 配置 | 值 | 影响 |
| --- | ---: | --- |
| `credentialRpm` | `70` | 全局默认本地凭据 RPM |
| `credentialMaxConcurrentRequests` | `10` | 全局默认本地凭据并发 |
| `dispatchGlobalMaxConcurrentRequests` | `0` | 本地凭据全局派发并发无限制 |
| `dispatchMaxQueuedRequests` | `300` | 本地派发队列上限 |
| `credentialRetryMaxAttempts` | `100` | 重试上限过高，容易放大上游压力 |
| `credentialRateLimitCooldownSecs` | `30` | 429 后短冷却 |
| `kiroUpstreamResponseTimeoutSecs` | `600` | 上游整体等待很长 |
| `kiroUpstreamStreamIdleTimeoutSecs` | `180` | 流式 idle 等待很长 |
| `payloadGuardEnabled` | `true` | payload guard 开启 |
| `payloadGuardMode` | `on_too_long` | 输入过长后裁剪重试 |
| `payloadGuardMaxBytes` | `460800` | 裁剪预算 |

主力账号 `#461/#462/#463` 自身配置：

| 账号 | `rpm` override | `maxConcurrentRequests` override |
| --- | ---: | ---: |
| `#461` | `50` | `50` |
| `#462` | `50` | `50` |
| `#463` | `50` | `50` |

代码里账号级 `maxConcurrentRequests` 会覆盖全局值，而不是取全局和账号配置的较小值：

- `src/kiro/token_manager/capacity.rs`
  - `effective_max_concurrent_requests(entry, global)` 返回 `entry.credentials.max_concurrent_requests.unwrap_or(global)`。
  - `entry_has_concurrency_capacity` 只判断当前账号 in-flight 是否小于该有效并发。

因此这几个账号的有效并发不是全局的 `10`，而是账号配置的 `50`；再加上 `dispatchGlobalMaxConcurrentRequests = 0`，本地凭据全局没有总并发刹车。

## 在途并发和 RPM 换算

2026-07-06 17:00-18:00 Asia/Shanghai，按 `created_at + duration_ms` 估算成功流请求在途：

| 口径 | 平均 | p50 | p95 | 峰值 |
| --- | ---: | ---: | ---: | ---: |
| 全部成功流请求 | `38.5` | `39` | `50` | `55` |
| `claude-opus-4-8` | `30.0` | `29.5` | `40.05` | `45` |
| `claude-sonnet-5` | `3.5` | `3` | `6` | `6` |
| 账号 `#463` | `18.0` | `19` | `28.25` | `32` |
| 账号 `#462` | `7.2` | `7` | `14.05` | `17` |
| 账号 `#461` | `6.8` | `7` | `12` | `17` |

70 RPM 对短请求不算夸张，但对长流式请求不能这样看。粗略换算：

```text
有效在途并发 ~= RPM * 平均耗时 / 60
```

如果 70 RPM 的平均耗时是 40s，则有效并发约为 47。生产在 17:00-18:00 的观测和这个量级一致。

## 模型维度的慢点不同

2026-07-06 17:00-18:00 Asia/Shanghai，成功流请求整体：

| 指标 | 数值 |
| --- | ---: |
| 成功流请求数 | `3438+` |
| p50 首字 | `11.45s` |
| p95 首字 | `63.2s` |
| 首字超过 10s | `1870` |
| 首字超过 30s | `578` |

按模型拆解后，慢点不一样：

- `claude-sonnet-5`
  - p95 首字约 `81.4s`。
  - p95 header-to-first-chunk 约 `73.4s`。
  - p95 chunk-to-output 约 `540ms`。
  - 主要慢在 HTTP header 已到之后，上游迟迟不发 body/SSE 数据。

- `claude-opus-4-8`
  - p95 首字约 `72.4s`。
  - p95 upstream header 约 `55.6s`。
  - p95 chunk-to-output 约 `15.95s`。
  - 主要慢在上游返回 HTTP header 前，也有一部分是已有 chunk 但没有 visible output。

同一个账号、同一个会话、同一个模型也会忽快忽慢。目标会话里 `#462` 的 Sonnet 5 请求，有的首字 `3.5s`，有的 `60s`。这说明不是一个固定本地函数稳定慢，而是上游模型容量、排队、cache/prefill 状态在不同时间段波动。

## 快慢桶对比

2026-07-06 17:00-18:00 Asia/Shanghai，按 `firstOutputDeltaMs` 分桶：

| 模型 | 桶 | 数量 | 平均输入 | 平均 cache read | 平均 cache write | 平均 body bytes | 平均 tools bytes | 平均 tools | p50 header | p50 header-to-chunk | p50 chunk-to-output |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `claude-opus-4-8` | `<5s` | `224` | `110,689` | `177,842` | `7,039` | `379,943` | `74,897` | `40.2` | `2.58s` | `12ms` | `0ms` |
| `claude-opus-4-8` | `5-30s` | `1544` | `162,505` | `192,886` | `10,533` | `657,053` | `67,769` | `34.7` | `10.29s` | `1.06s` | `0ms` |
| `claude-opus-4-8` | `>=30s` | `475` | `327,627` | `225,021` | `12,027` | `1,301,540` | `61,058` | `32.6` | `36.50s` | `249ms` | `89.5ms` |
| `claude-sonnet-5` | `<5s` | `28` | `53,592` | `80,032` | `11,060` | `225,252` | `84,381` | `38.8` | `2.77s` | `9ms` | `1ms` |
| `claude-sonnet-5` | `5-30s` | `178` | `117,394` | `187,290` | `11,021` | `335,497` | `120,914` | `76.2` | `9.11s` | `1.14s` | `37ms` |
| `claude-sonnet-5` | `>=30s` | `77` | `189,237` | `254,679` | `13,583` | `468,333` | `120,327` | `76.9` | `16.54s` | `30.97s` | `63ms` |

关键判断：

- Opus 4.8 慢桶主要是 header 阶段变慢，说明上游在返回 HTTP header 前就已经长时间排队或预处理。
- Sonnet 5 慢桶主要是 header 到首个 body chunk 阶段变慢，说明上游已经建立响应并返回 header，但 body/SSE 首包迟迟不来。
- 两者都不是本地 chunk 解析慢；chunk-to-output 的中位数都是毫秒级。

## 429 和重试放大

17:00-18:00 Asia/Shanghai，有约 `205` 个请求发生多次 credential attempts。

中间尝试里有：

- `429 Too Many Requests`
- `400 Input is too long`
- `500`
- timeout / stream error

其中 429 能说明账号/上游已经明确进入限流状态。`credentialRetryMaxAttempts = 100` 会在压力已经存在时继续尝试，放大上游负载和客户端尾延迟。输入过长类错误也不应按普通 transient 大量重试；只有在 payload guard 确认缩减后才有必要重试一次。

## Payload guard 与本机资源

当前资源快照：

- app 容器内存约 `327-416MiB`。
- Redis 约 `521MiB`。
- Postgres 约 `700-970MiB`。
- 机器 available memory 约 `4.8GiB`。
- memory PSI 基本为 `0`。
- 未看到 OOM 证据。

17:00-18:00 Asia/Shanghai 的 payload guard 耗时：

| 原始 body 大小 | 数量 | 平均 guard | p50 | p95 | 最大 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `>=1MB` | `626` | `44.6ms` | `32ms` | `108ms` | `319ms` |
| `460KB-1MB` | `494` | `13.7ms` | `13ms` | `19ms` | `55ms` |
| `100-460KB` | `1384` | `4.5ms` | `4ms` | `9ms` | `17ms` |
| `<100KB` | `89` | `1.2ms` | `1ms` | `2ms` | `5ms` |

Payload guard 是高并发大 body 下的 CPU 风险点，尤其生产里存在最大超过 `20MB` 的 body。但从目标请求和这一小时统计看，它不是 20s、50s 首字的主因。首字慢主要发生在上游 header 或 header-to-body 阶段。

## 本地凭据与外部池的共性

本地凭据调度：

- 账号容量按 in-flight 条数判断。
- 分数主要按 `in_flight / max_concurrent`、错误率、延迟 EWMA、选择压力计算。
- 当前没有按本次请求的 token、cache read/write、tool schema bytes、图片等做容量权重。

外部池调度：

- 外部池容量也主要看每池并发和外部池全局并发。
- `ExternalPoolManager::skip_reason` 里判断 `pool.max_concurrent_requests` 和 `external_pool_global_max_concurrent_requests`。
- 同样没有按本次 body 的重度给请求加权。

因此本地凭据和外部池都会在长上下文、大并发、慢首字场景下出现同类问题。根因不是某一种账号类型，而是容量模型没有表达真实请求成本。

## 指标语义：上游响应头、首个流分片、首次输出

### 字段定义

本地 usage latency trace 字段定义在 `src/anthropic/usage.rs`：

- `payloadGuardMs`
- `upstreamHeaderMs`
- `firstUpstreamChunkMs`
- `firstOutputDeltaMs`
- `firstThinkingDeltaMs`
- `firstVisibleTextDeltaMs`
- `streamGapToFirstOutputMs`
- `chunksBeforeFirstOutput`
- `eventsBeforeFirstOutput`

本地凭据路径的记录点主要在 `src/anthropic/handlers.rs`：

- `mark_upstream_header()`：记录上游 HTTP 响应头到达。
- `mark_first_upstream_chunk()`：记录第一次从上游 response body stream 读到 chunk。
- `mark_stream_events()` / `mark_first_token_if_output()`：解析 SSE event 后，记录第一个可算作输出的事件。

外部池路径有同构逻辑，见 `src/external_pool.rs`：

- `ExternalLatencyTraceState::mark_upstream_header`
- `ExternalLatencyTraceState::mark_first_upstream_chunk`
- `ExternalStreamUsageGuard::mark_first_token_if_output`

### 上游响应头是什么

`upstreamHeaderMs` 是本地代理从请求开始计时，到成功拿到上游 HTTP response status line + headers 的时间。

它不是下游响应头，也不是首个 token。它大致包含：

- 本地凭据/外部池调度等待。
- 本地构造上游请求。
- 代理到上游的连接、TLS、HTTP/2/HTTP/1.1 请求发送等待。
- 上游网关、Kiro 服务或模型服务在返回 HTTP response header 前的排队和预处理。
- 如果本次请求前面发生过 retry，这个值可能包含前面失败 attempt 的累计等待，具体要结合 `credentialAttempts` 看。

它通常不包含：

- 上游 response body 的完整读取。
- 本地 SSE event 转换后的下游输出。
- 下游客户端读取速度。

代码上，本地凭据流式路径在 provider 返回成功 `Response` 后调用 `usage_context.mark_upstream_header()`，然后才开始处理 response body stream。

### 首个流分片是什么

`firstUpstreamChunkMs` 是本地代理第一次从上游 response body stream 里读到 bytes chunk 的时间。

它是本地观测到的时间戳，但 chunk 的内容来自上游。也就是说：

- “首个流分片”不是本地凭空生成的业务事件。
- 它也不严格等价于 SSE event；一个 chunk 可能包含半个 SSE event、一个完整 SSE event、多个 SSE event，或者只有 transport 层聚合出来的一段 bytes。
- 在 HTTP/2 下，它也不一定一一对应某个 DATA frame，因为 hyper/reqwest 会做缓冲和聚合。

本地凭据流式路径里，收到 `body_stream.next()` 的第一个 `Ok(chunk)` 时调用 `mark_first_upstream_chunk()`。外部池流式路径也在 `body_stream.next()` 读到 chunk 后记录。

### 首次输出是什么

`firstOutputDeltaMs` 是代理解析上游 SSE/event 后，第一次识别到“可以算作输出”的语义事件的时间。

当前代码把以下内容视为首个输出：

- 非空 `text_delta`
- 非空 `thinking_delta`
- 非空 `input_json_delta.partial_json`
- `content_block_start` 中的 `tool_use`
- `content_block_start` 中的 `server_tool_use`
- `content_block_start` 中的 `redacted_thinking`

因此，首个 chunk 到了不代表一定有首次输出。可能先到的是：

- `message_start`
- `content_block_start` 但不是可输出类型
- `message_delta`
- usage/context metadata
- ping/heartbeat
- 空 delta
- 被转换器等待闭合边界的半截 SSE event

### `streamGapToFirstOutputMs`

`streamGapToFirstOutputMs = firstOutputDeltaMs - firstUpstreamChunkMs`。

它表示上游 body 已经开始返回后，到代理识别出第一个有效输出之间的间隔。

如果这个值很大，通常不是 CPU 处理一个 chunk 很慢，而是：

- 上游已经发了 body，但前面是非输出 event。
- 上游发了 chunk 后长时间没有继续发能输出的 event。
- SSE event 被切在多个 chunk 中，本地必须等待完整 event。
- thinking/tool 边界需要跨 chunk 判断。

### `chunksBeforeFirstOutput` 和 `eventsBeforeFirstOutput`

`chunksBeforeFirstOutput` 表示第一个有效输出前，已经读到多少个不含有效输出的 body chunk。

`eventsBeforeFirstOutput` 表示第一个有效输出前，已经解析到多少个不算有效输出的 SSE event。

如果两个值都是 `0`，并且 `firstUpstreamChunkMs` 与 `firstOutputDeltaMs` 基本相等，说明第一个 body chunk 里就包含了可输出内容，本地没有明显分片处理等待。

## 新样本解释

用户提供的新耗时：

| 阶段 | 数值 |
| --- | ---: |
| 总耗时 | `21.75s` |
| 请求检查 | `39ms` |
| 上游响应头 | `21.04s` |
| 首个流分片 | `21.07s` |
| 首次输出 | `21.07s` |
| 分片到输出 | `0ms` |
| 输出前分片 | `0` |
| 输出前事件 | `0` |

这条记录的阶段判断：

1. `请求检查 39ms`：本地前置检查、payload guard 或请求准备很轻，不是问题主因。
2. `上游响应头 21.04s`：主要等待发生在上游返回 HTTP header 之前。这里包含调度、发起上游请求、连接等待、上游网关/模型排队、上游预处理等。
3. `首个流分片 21.07s`：header 到第一个 body chunk 只差约 `30ms`，说明上游一旦返回 header，很快就开始发 body。
4. `首次输出 21.07s`：第一个 body chunk 里就有有效输出。
5. `分片到输出 0ms`、`输出前分片 0`、`输出前事件 0`：本地没有明显 chunk 等待或语义解析等待。

因此这条新样本是典型的 `dominant_upstream_header_wait`：慢在上游响应头之前，而不是首个流分片之后。

这个 `21.04s` 里的“上游响应头”不能简单理解为模型第一个 token；它更像“代理终于拿到了上游 HTTP response 的开始”。它可能包括：

- 本地调度排队。
- 凭据或外部池容量等待。
- 上游连接建立和请求发送。
- 上游 Kiro 网关排队。
- 模型服务预填充、cache 处理、工具 schema/context 处理。
- 如果有 retry，则还可能包含之前失败 attempt 的时间。

要判断是哪一段，需要同时看：

- `credentialAttempts` 中每次 attempt 的 `durationMs`、`status`、`credentialId`。
- 账号当时 in-flight 数。
- 同分钟同模型的 p95 header、p95 header-to-chunk。
- 是否有 429/500/Input too long。
- payloadBreakdown 里的 total bytes、tools bytes、history bytes。

## 为什么“首个流分片”不是单纯本地概念

它是本地指标，但不是本地生成内容。

准确说：

- 本地定义了一个观测点：第一次从上游 response body stream 读到 bytes。
- 上游决定何时发送这些 bytes。
- 网络栈、HTTP 库可能影响 chunk 聚合边界。
- 本地只在读到 chunk 时记录时间，不会人为制造上游 chunk。

所以当 `upstreamHeaderMs` 和 `firstUpstreamChunkMs` 差距很大，说明上游返回 header 后 body 首包迟迟没来；当两者接近，但都很大，说明主要慢在上游 header 之前；当 `firstUpstreamChunkMs` 和 `firstOutputDeltaMs` 差距很大，说明 body 已开始但还没有可输出语义，或者本地转换器在等待完整事件/边界。

## 解决方向

### 生产配置止血

建议优先降低尾延迟风险，而不是继续只看 RPM：

1. 降低主力账号 `maxConcurrentRequests`，不要继续 `50`。可从 `5-8` 量级试起，观察 p95 首字和 429。
2. 设置 `dispatchGlobalMaxConcurrentRequests`，不要继续 `0` 无限。可按账号数和模型容量设置一个全局上限。
3. 将 `credentialRetryMaxAttempts=100` 降到 `2-3`。429、500、timeout 后需要明确退避，而不是长 retry chain。
4. 对 `claude-sonnet-5`、`claude-opus-4-8` 做模型级并发限制，不和轻模型共用同一容量口径。
5. `Input is too long` 不按普通 transient 重试；只有在 payload guard 确认缩减后允许一次重试。

### 代码层根治

需要把调度从“请求条数调度”升级为“请求成本加权调度”：

1. 请求入调度前估算成本：
   - body bytes
   - input tokens
   - cache read tokens
   - cache creation tokens
   - tools bytes / tool count
   - image count / image bytes / image token estimate
   - max_tokens
2. 将重请求占用多个并发权重，而不是永远占 1 个并发槽。
3. 本地凭据和外部池共用同一套成本估算与容量权重接口。
4. 增加模型级容量池：
   - Sonnet 5 独立容量。
   - Opus 4.8 独立容量。
   - 轻模型独立容量。
5. 增加 adaptive circuit breaker：
   - 按账号 + 模型记录 p95 header、p95 header-to-chunk、429、timeout。
   - 当某账号某模型进入慢区，主动降权或冷却。
6. 增加 heavy request queue：
   - 轻请求不被长上下文请求堵住。
   - 长上下文/高 cache read/工具重请求低并发排队。
7. Dashboard 增加 RPM 之外的压力指标：
   - active streams
   - weighted in-flight
   - tokens/min
   - p95 upstream header
   - p95 header-to-first-chunk
   - retry count
   - 429 count
   - payload guard ms
   - tool schema bytes

## 当前判断

这次问题不是“某个请求偶发慢”或“本地机器内存爆炸”。核心是容量模型没有表达真实上游成本，导致长上下文、cache-heavy、tools-heavy 请求在请求数看起来不高时，仍然把上游模型容量压到长首字状态。

新样本 `21.75s` 的拆解进一步说明：当 `upstreamHeaderMs ~= firstUpstreamChunkMs ~= firstOutputDeltaMs`，且 `streamGapToFirstOutputMs = 0`、输出前 chunk/event 都是 0 时，本地分片处理不是瓶颈；主要等待在上游响应头之前。
