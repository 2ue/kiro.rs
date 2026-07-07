# kiro.rs vs sub2api 首字延迟关联分析记录

日期：2026-07-07

## 背景

现网体感反馈是：同一 Milus/Kiro 兼容上游分组下，`sub2api` 整体首字更快，经过 `kiro.rs` 后整体首字明显变慢。此前分析只做了日志分布、少量真实请求和代码路径比对，尚未完成“同一条请求在 kiro.rs 与上游后台日志的一一对应比对”。

## 已做过的只读观察

- 低流量 `kiro.rs` 外部池 `milus` 配置为 `raw_passthrough + current_path_policy`。
- 高流量 `kiro.rs` 外部池 `milus` 配置为 `normalized + current_path_policy`。
- 外部池 URL 生成不是按入站 path 原样透传：`/cc/v1/messages` 会生成上游 `/v1/messages`。
- 外部池响应不是字节级纯透传：当前代码会按完整 SSE event 边界 drain，并执行 usage capture/projection。
- `sub2api` Anthropic passthrough 记录 first token 的口径是第一个非空 `data:` 行；`kiro.rs` 记录的是第一个可见输出事件。
- 本地凭证路径存在 Kiro/AWS event-stream pending：收到多个 body chunk，但尚未形成完整 frame/event，导致 `chunksBeforeFirstOutput` 高、`eventsBeforeFirstOutput=0`。

## 最近一次真实请求测试结果

低并发真实请求测试没有稳定复现“kiro.rs 外部池必然比直连上游慢很多”：

- 低流量 `kiro.rs` 机器，直连外部池上游 short 5 轮：首次可见输出 p50 约 6.29s，p90 约 15.72s。
- 低流量 `kiro.rs` 机器，经 `kiro.rs /cc/v1/messages` 外部池 short 5 轮：p50 约 4.90s，p90 约 8.50s。
- `sub2api` 机器，直连 `milus-kiro-wuhuan` short 5 轮：p50 约 2.90s，p90 约 6.31s。
- `sub2api` 机器，经 sub2api group 86 short 5 轮：p50 约 4.49s，p90 约 12.37s。
- 高流量 `kiro.rs` 机器，直连外部池上游 short 5 轮：p50 约 11.72s，p90 约 13.72s。
- 高流量 `kiro.rs` 机器，经 `kiro.rs /cc/v1/messages` short 5 轮：p50 约 1.76s，p90 约 3.46s。

这些测试只能证明“少量主动测试没有稳定复现 kiro.rs 外部池额外拖慢”，不能推翻现网整体体感。

## 需要修正的分析口径

此前“按分布推断主因”的证据不足。要解释用户看到的现网事实，必须做同一请求关联：

1. 从 `kiro.rs` usage_records 中抽取慢首字请求，记录 `request_id`、时间、模型、账号/外部池、路由类型、输入/输出、费用、`upstreamHeaderMs`、`firstOutputDeltaMs`、`streamGapToFirstOutputMs`。
2. 到 Milus 上游后台日志中按绝对时间、模型、输入 token、输出 token、费用、账号/token、状态匹配同一条上游请求。
3. 对比同一条请求的：
   - 上游后台首字；
   - `kiro.rs` 的 `upstreamHeaderMs`；
   - `kiro.rs` 的 `firstUpstreamChunkMs`；
   - `kiro.rs` 的 `firstOutputDeltaMs`；
   - `kiro.rs` 的 `streamGapToFirstOutputMs`。
4. 只有同一请求比对后，才能判断差距来自上游、网络、kiro.rs 协议转换、usage projection、还是指标口径。

## 2026-07-07 追加质疑记录

用户明确指出：现网事实不是“某些样本慢”，而是同一 Milus/Kiro 兼容上游分组下，`sub2api` 与上游后台体感整体较快，经过 `kiro.rs` 后整体首字明显更慢。此前分析虽然列出了代码路径、指标口径和慢请求分布，但仍然缺少能解释这个事实的决定性证据。

因此后续分析必须避免以下问题：

- 不能用 `kiro.rs` 与上游后台的零散样本分别观察后直接合并推断。
- 不能只说“上游也有慢请求”，因为这无法解释 `kiro.rs` 慢得更频繁。
- 不能只用少量主动压测或短请求测试反驳现网体感；主动测试只能作为补充，不能替代生产请求关联。
- 不能把 `upstreamHeaderMs`、`firstUpstreamChunkMs`、`firstOutputDeltaMs` 的本地含义直接等同于上游后台首字；两边必须在同一条请求上对齐。

决定性验证方式调整为：

1. 先选取 `kiro.rs` 真实生产慢请求，尤其是外部池 `milus`、`claude-opus-4-8`、`firstOutputDeltaMs >= 10s` 的请求。
2. 对每条请求记录 `request_id`、北京时间、输入/输出 token、cache read/write、routeSubtype、fallbackReason、`upstreamHeaderMs`、`firstUpstreamChunkMs`、`firstOutputDeltaMs`、`streamGapToFirstOutputMs`、`rawUsage`。
3. 在 Milus 上游控制台日志中按该请求的时间窗口、模型、输入 token、输出 token、费用和状态匹配同一条上游请求。
4. 对每个匹配结果给出“同一请求”的差值：`kiro.rs firstOutputDeltaMs - Milus 上游首字`。只有这个差值持续显著偏大，才能证明 `kiro.rs` 在上游之后额外增加了首字延迟。
5. 如果多数慢请求在 Milus 上游后台同一条日志里也已经慢，则说明 `kiro.rs` 观察到的是上游慢；但仍需解释为什么 `sub2api` 体感更快，可能要继续按请求体大小、上下文长度、模型写法、stream 协议、缓存策略、并发成本模型和账号分配做分层。

## 2026-07-07 再次修正：必须按同一条请求闭环解释

用户再次质疑的核心是正确的：如果没有把 `kiro.rs` 的某一条慢请求，与 Milus 上游后台的同一条请求日志对上，就不能解释“为什么用户真实体感是经过 `kiro.rs` 后整体更慢”。此前把 `kiro.rs` 慢样本、Milus 后台零散慢样本、`sub2api` 少量主动测试放在一起比较，最多只能形成方向性怀疑，不能作为最终归因。

后续所有结论必须满足以下证据分级：

- A 级证据：同一条 `kiro.rs` request_id 对应到 Milus 后台同一请求，并能同时看到两边的首字/耗时/token/费用。只有 A 级证据可以用于主结论。
- B 级证据：同一时间窗口、同模型、相近 token/费用但无法唯一匹配。只能用于候选解释，不能用于定论。
- C 级证据：代码路径、主动测试、聚合分布、体感观察。只能用于提出假设和指导下一步验证。

当前必须追踪的判断链路：

1. 如果 `kiro.rs` 慢请求在 Milus 后台同一请求中首字也同样慢，则该请求的慢主要发生在上游进入首字之前；但仍不能解释 `sub2api` 体感更快，需要继续比较请求体、缓存命中、模型写法和账号选择。
2. 如果 Milus 后台同一请求首字明显快，而 `kiro.rs firstOutputDeltaMs` 慢，则证明 `kiro.rs` 在收到上游后额外增加了延迟；再按 `streamGapToFirstOutputMs`、SSE 事件类型、协议转换、flush、usage projection、客户端输出口径拆分。
3. 如果 `kiro.rs upstreamHeaderMs/firstUpstreamChunkMs` 已经很慢，而 Milus 后台首字很快，则要重点查 `kiro.rs` 到 Milus 的网络/连接复用/代理层等待、上游后台首字口径差异、以及是否匹配错请求。
4. 如果 `kiro.rs firstUpstreamChunkMs` 快但 `firstOutputDeltaMs` 慢，才可以把重点放到 `kiro.rs` 的协议解析、事件过滤、thinking/tool_use/空 delta 处理、输出 flush。

因此，下一轮分析不再接受“上游也有慢请求”作为解释。必须先给出至少若干条 `kiro.rs` 慢请求与 Milus 后台日志的逐条对账表，再讨论主因。

## 暂定假设

当前只能保留以下假设，不能当作最终结论：

- 外部池慢请求如果上游后台同一请求首字也慢，则主因是上游处理/排队。
- 外部池慢请求如果上游后台首字很快，但 `kiro.rs firstOutputDeltaMs` 明显更慢，则需要重点查 SSE event buffering、usage projection、flush、客户端断开或响应包装。
- 本地凭证慢请求如果出现 `chunksBeforeFirstOutput` 高且 `eventsBeforeFirstOutput=0`，则主因更可能是 Kiro binary event-stream frame pending，而不是外部池 SSE。
- `sub2api` 与 `kiro.rs` 的 first token 指标口径不同，必须同时比较同一条请求的上游后台首字，不能只比较两个系统内部记录。

## 下一步验证计划

- 优先抽取最近 2-3 小时内 `kiro.rs` 外部池 `milus` 慢首字样本，按 `firstOutputDeltaMs >= 10s` 分层取样。
- 额外抽取 `local_credential` 中 `chunksBeforeFirstOutput >= 50 AND eventsBeforeFirstOutput=0` 的样本，单独归类。
- 使用上游 Milus 控制台日志页面或其接口，逐条按时间窗口匹配同一请求。
- 输出一张“kiro.rs 请求 vs Milus 上游日志”的对应表，未匹配的请求单独列出原因。

## 2026-07-07 候选样本和当前阻塞

### 低并发 `kiro.rs` 机器候选样本

只读查询范围：`model='claude-opus-4-8'`、`status='success'`、`duration_ms >= 10000`，先用时间/model/status/duration 收窄，再在小结果集读取 `data.latencyTrace`，避免大范围 JSON 扫描。

| request_id | 北京时间 | endpoint | duration | first_out | upstream_header | first_chunk | gap | raw input/output | 判断 |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- |
| `req_01xMfcSyFEhjUJtiooTjYr3P` | 2026-07-07 22:19:54 | `/cc/v1/messages` | 118.704s | 115.575s | 115.485s | 115.574s | 0.001s | 845745 / 62 | `kiro.rs` 侧几乎全部慢在上游首分片前 |
| `req_012onkirjiGVYB8UafEWvYaq` | 2026-07-07 22:20:33 | `/cc/v1/messages` | 34.381s | 30.232s | 30.147s | 30.232s | 0s | 846107 / 215 | `kiro.rs` 侧几乎全部慢在上游首分片前 |
| `req_01ANuv5Y5MGbbLAfbWMq6XW3` | 2026-07-07 22:53:10 | `/cc/v1/messages` | 68.196s | 47.438s | 47.366s | 47.437s | 0.001s | 873091 / 1040 | `kiro.rs` 侧几乎全部慢在上游首分片前 |
| `req_01ABUecn47KtyGrEuWtD9Tre` | 2026-07-07 20:35:38 | `/cc/v1/messages` | 51.454s | 47.011s | 44.838s | 44.900s | 2.111s | 556790 / 162 | 上游首分片前慢为主，另有约 2.1s 输出前等待 |
| `req_01FK3QsqPT4bYT5ELebMqvbm` | 2026-07-07 21:08:42 | `/ha/v1/messages` | 232.131s | 41.368s | 32.391s | 32.411s | 8.957s | 213434 / 1367 | 上游首分片前慢 + `kiro.rs` 输出前等待 |
| `req_011vLERsBe6qWedMMC1DVyiW` | 2026-07-07 21:54:43 | `/ha/v1/messages` | 52.392s | 27.544s | 16.024s | 16.044s | 11.500s | 110035 / 411 | `kiro.rs` 收到首事件后到可见输出有明显等待 |

低并发机的样本已经能分成两类：

- `gap≈0`：`firstOutputDeltaMs` 基本等于 `firstUpstreamChunkMs`，如果 Milus 后台同一请求首字也慢，则慢发生在上游首字前；如果 Milus 后台同一请求首字快，则要查 `kiro.rs` 到上游的连接/网络/指标口径。
- `gap` 明显：上游首个 SSE event 到达后，`kiro.rs` 等到可见输出还多花了约 2-11.5s，这类需要查 SSE 事件内容、thinking/tool_use/空 delta、协议转换和 flush。

### 高并发 `kiro.rs` 机器精确样本复查

按此前记录的 request_id 精确查询，确认这些请求存在：

| request_id | 北京时间 | endpoint | duration | first_out | upstream_header | first_chunk | gap | raw input/output | routeSubtype / reason |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- |
| `req_01a8o4igiEYYLDNRw6rdvJf4` | 2026-07-07 22:47:05 | `/cc/v1/messages` | 92.088s | 87.061s | 86.960s | 87.023s | 0.038s | 950851 / 111 | `external_fallback_preflight` / `local_all_disabled` |
| `req_0173kWJbqh5zsg5F8aqNBAqy` | 2026-07-07 22:48:46 | `/v1/messages` | 101.548s | 100.515s | 100.415s | 100.515s | 0s | 886305 / 30 | `external_fallback_preflight` / `local_all_disabled` |
| `req_01namh4xadng7gmrBgK8tchQ` | 2026-07-07 22:50:04 | `/cc/v1/messages` | 95.823s | 92.183s | 92.108s | 92.182s | 0.001s | 958429 / 143 | `external_fallback_preflight` / `local_all_disabled` |
| `req_01K49f1MYD7J3ATycr34MA8d` | 2026-07-07 22:50:25 | `/v1/messages` | 107.672s | 101.144s | 100.531s | 100.609s | 0.535s | 953035 / 159 | `external_fallback_preflight` / `local_all_disabled` |

这些高并发样本同样主要表现为 `firstOutputDeltaMs≈firstUpstreamChunkMs≈upstreamHeaderMs`。但这仍然不能直接得出“上游慢”的最终结论，因为还没有匹配到 Milus 后台同一条请求。

### 已确认的历史关联缺口

`usage_records.data` 当前没有保存 Milus 上游返回的 request id 或可用于后台精确检索的上游日志 id。外部池成功记录里只保存：

- 本地 `request_id`；
- `externalAttempts` 的 pool/status/duration/outboundModel；
- `latencyTrace`；
- `externalPoolBilling.rawUsage/reportedUsage/shapedUsage`；
- route subtype/reason。

代码层面也确认：

- 外部池发起请求时使用 `forward_headers(&route.headers, pool)`，不会主动把本地生成的 `request_id` 注入到上游请求 header。
- 如果入站请求本身带了允许转发的 `request-id` / `anthropic-request-id`，它可能被转发；但 `kiro.rs` 自己生成的 request id 没有在 `forward_once` 里注入上游。
- 外部池响应 header 会经过 `apply_forwarded_response_headers` 转发允许的响应 header，再调用 `envelope::insert_request_id_headers(out, request_id)` 补本地 request id；但这些上游响应 header 没有落入 usage record。

因此，历史请求目前无法仅靠 `kiro.rs` 数据库做到 A 级精确闭环，只能借助 Milus 后台页面按时间/token/费用匹配。

### Milus 页面匹配当前状态

尝试通过用户默认 Chrome 打开 `https://newapius.milus.one/console/log` 做页面只读匹配，但当前 Chrome 扩展控制通道不可用，桌面可视化页面显示为空白；相邻 `New API` 标签页被休眠扩展挂起，恢复后仍没有表格内容。未读取 cookie/localStorage，也未对 Milus 侧做接口抓取。

当前因此还没有完成任何 A 级“同一请求”匹配。后续如果页面恢复，优先匹配：

- `req_012onkirjiGVYB8UafEWvYaq`：2026-07-07 22:20:33，raw input/output `846107/215`，`kiro.rs first_out=30.232s`。
- `req_01xMfcSyFEhjUJtiooTjYr3P`：2026-07-07 22:19:54，raw input/output `845745/62`，`kiro.rs first_out=115.575s`。
- `req_0173kWJbqh5zsg5F8aqNBAqy`：2026-07-07 22:48:46，raw input/output `886305/30`，`kiro.rs first_out=100.515s`。

### 目前不能下的结论

不能说“Milus 上游一定慢”，因为还没有同一请求后台日志。

不能说“`kiro.rs` 一定额外拖慢 30-100s”，因为多数候选样本在 `kiro.rs` 内部记录中 `firstOutputDeltaMs` 已经贴近 `firstUpstreamChunkMs/upstreamHeaderMs`，这类样本不像是上游首分片到下游输出之间的长时间本地阻塞。

当前唯一可以确定的是：`kiro.rs` 现有历史记录不足以独立闭环解释用户体感差异，必须补上 Milus 后台同一请求对账，或在代码里新增低成本的上游 request id/首字口径记录，供后续请求闭环分析。

## 2026-07-08 外部池流式透传优化更正

### 需要更正的结论

`usage projection` 本身不应被直接描述为“会稳定造成几十秒首字延迟”。它主要做的是：

- 从 SSE event 的 `data:` 行解析 JSON；
- 在存在 `usage` 字段时读取或投影 `input/cache/output` 相关字段；
- 更新外部池内部 billing capture，供历史、费用和兼容展示使用。

这类 CPU/JSON 操作可能在极端长事件、异常 payload 或高并发下放大开销，但不能解释大量 `streamGapToFirstOutputMs` 达到几十秒的样本，除非同一请求能证明上游首分片已到达且 `kiro.rs` 在本地长期不输出。因此，后续分析不能把“本地会投影 cache read/cache creation”直接当成主因。

同时，之前“外部池不是纯透传”的判断是成立的，但需要更精确：

- 旧逻辑不是 raw chunk 级透传，而是按完整 SSE event 边界 drain。
- 旧逻辑会在流式响应中尝试改写 `usage` event，让下游看到投影后的 usage。
- 旧逻辑还会检查并屏蔽外部池流式错误事件，避免把内部池、凭证、fallback 等信息暴露给客户端。
- 因为必须保留错误屏蔽，不能简单改成完全字节级 raw chunk 透传；否则上游中途返回的错误 event 可能泄露内部信息或破坏统一错误格式。

因此，本次优化选择的是“event 级透传 + 旁路 capture”，不是无条件 raw chunk 透传。

### 本次实施的优化

新增全局配置：

- 后端字段：`externalPools.externalPoolStreamResponseMode`
- 页面入口：新旧运行配置页的“接口兼容/外部池流式响应”
- 可选值：
  - `event_passthrough_capture`：默认值。正常 SSE event 原样下发给客户端，只在旁路解析 usage 并更新内部费用/历史记录；流式错误 event 仍会被本地屏蔽。
  - `projected_rewrite`：回到旧行为。流式 `usage` event 会被改写为投影后的字段。

默认改为 `event_passthrough_capture` 后，外部池正常流式响应的下游内容不再为了 usage projection 被重写。这样能减少本地 JSON 序列化和响应体改写的主路径影响，也更接近 Kiro 兼容上游本来的输出节奏。

本次还增加了外部池 SSE event 缓冲上限：

- 单个未完成 SSE event 缓冲超过 `1 MiB` 时，终止该外部池流并记录 warning。
- 目的不是改变正常请求行为，而是防止 pathological payload 或上游长期不发 event delimiter 时，本地 buffer 无上限增长造成内存压力。
- 该限制只保护未完成 event 的累积缓冲；正常完整 event 会尽快 drain 并输出。

### 优化能解决什么

这次优化主要降低以下风险：

- 外部池流式 usage event 被本地改写导致的额外处理开销；
- 正常 SSE event 因本地重序列化而偏离上游原始输出；
- 异常或恶意上游 payload 导致未完成 SSE event buffer 持续增长；
- 后续排查时无法区分“下游看到的是上游原始 usage”还是“kiro.rs 改写后的 usage”。

在新默认值下，`externalPoolBilling.streamResponseMode` 会记录当前流式响应处理模式，便于后续从 usage record 判断这条请求是否走了 event 级透传。

### 优化不能证明或不能解决什么

这次优化不能替代同一请求对账，也不能单独解释所有现网慢首字：

- 如果 `firstOutputDeltaMs≈firstUpstreamChunkMs≈upstreamHeaderMs`，慢主要发生在 `kiro.rs` 收到上游首分片之前；本次透传优化不会明显改变这类请求。
- 如果 `firstUpstreamChunkMs` 很快，但 `streamGapToFirstOutputMs` 很大，本次优化才更可能降低本地处理和输出前等待。
- 如果上游后台同一请求首字本身也慢，主因仍要回到上游处理、账号队列、上下文规模、缓存策略或网络路径。
- 如果 Milus 后台首字很快但 `kiro.rs firstUpstreamChunkMs` 很慢，需要继续查连接复用、网络路径、代理层等待、TLS/HTTP2 行为或请求匹配是否错误。

因此，现网“`kiro.rs` 体感整体慢于 `sub2api`”仍需按 A 级证据闭环：拿 `kiro.rs` 的真实慢请求，与 Milus 后台同一条请求逐条匹配，比较上游首字和 `kiro.rs` 的 `upstreamHeaderMs / firstUpstreamChunkMs / firstOutputDeltaMs`。

### 回滚方式

如果上线后发现某些下游客户端依赖旧的流式 usage 投影字段，可以把全局配置改为：

```json
{
  "externalPools": {
    "externalPoolStreamResponseMode": "projected_rewrite"
  }
}
```

这会恢复旧的流式 usage rewrite 行为。非流式响应不受这个开关影响，仍按现有非流 usage projection / cache strategy 逻辑处理。

## 2026-07-08 运行配置归属更正

### 非流式请求无缓存

之前“非流请求不整形”这个名字容易误导，实际应归入缓存策略里的 usage/cache 展示行为，而不是外部池或兼容模式。

当前正确口径是：

- 配置字段：`reportedUsage.*.skipNonStreamUsageProjection`，在新的 `cachePolicy` 路径策略里也会作为 `reportedUsage` 子配置保存。
- 页面文案：`非流式请求无缓存`。
- 生效范围：只影响命中该路径/策略的非流式请求。
- 具体效果：非流式请求不做本系统缓存展示投影，不写入本地缓存状态，返回和历史记录尽量按无缓存 usage 口径处理。
- 不影响：流式请求继续按该路径原有缓存/usage 策略执行；外部池流式响应的透传模式也不由这个开关控制。

这解释了“打开后是不是所有非流请求都没有缓存”的边界：不是全局所有非流请求，而是命中对应默认策略或路径覆盖的非流请求。要让所有内置入口都这么做，需要在默认策略或每个内置路径策略上开启；要只影响 `/cc`、`/ha`、`/dfcache/{name}`，就在对应路径上开启。

### 兼容模式与请求体处理的边界

用户质疑“很多兼容开关实际影响请求体处理，不应该都放在兼容行为里”是正确的。现在应按实际影响面拆分：

- 请求体处理页：压缩、payload guard、图片展开/下载/base64 修复、工具 schema 规范化、工具名映射、tool_choice 引导、历史 thinking 处理、tool_result 配对修复等。这些会改变发往上游的请求体。
- 缓存策略页：本地模拟缓存、Kiro-RS Tool 缓存、reported usage 字段策略、非流式请求无缓存、路径覆盖。这些改变 usage/cache 展示和本地缓存状态。
- 模型解析页：模型名解析、别名/映射规则、自动生成规则。这些改变模型路由和上游模型名。
- 接口兼容页：客户端接口 profile、Kiro 工作模式、thinking 输出展示、代理告警、外部池流式响应模式。这些主要改变响应/协议兼容和诊断行为。

因此，本次新增的 `externalPoolStreamResponseMode` 放在“接口兼容/外部池流式响应”是合理的：它不改变请求体，也不决定缓存策略；它决定外部池流式响应是否按 SSE event 原样下发，还是沿用旧的流式 usage rewrite。

### supportedModels 的合理配置

`supportedModels` 是账号/外部池调度白名单，不应该要求运维把每一种模型写法都手工枚举一遍。当前后端匹配逻辑已经支持常见等价写法：

- 空列表表示不限制模型。
- 非空列表表示只允许候选模型命中。
- 匹配时会使用请求候选模型、上游 payload 模型、raw client model 等候选值。
- `model_support` 会做规范化和版本等价/显式 Anthropic 日期别名匹配，但不会做跨模型家族兜底，避免把 sonnet、opus、haiku 混用。

合理做法：

- 常规账号不配置 `supportedModels`，让模型解析/映射规则处理请求模型名。
- 确实只支持一部分模型的账号，再配置白名单。
- 页面已有“同步支持模型”能力，优先从订阅/上游能力同步官方模型列表，再人工二次确认、删减或补充。
- 未来更好的 UI 是把同步结果做成 tag/chip：默认生成 Kiro 官方标准写法和 Claude Code/Anthropic 常见标准写法，提交前弹窗确认，允许继续增删。当前不要求运维手写所有兼容写法。

### pathological payload 处理

当前请求体侧已经有 payload guard、工具结果/历史裁剪、图片处理和异常 JSON stream buffer 上限。本次外部池流式响应侧补上了 SSE event 缓冲上限：

- 正常完整 SSE event 会及时 drain，不长期占用 buffer。
- 上游长期不发 SSE delimiter 或发送异常巨大未完成 event 时，超过 `1 MiB` 会终止该流，避免单个异常响应造成内存压力。
- 这属于防御性保护，不改变正常 Kiro/Milus 兼容 SSE 响应。

因此，极端 payload 当前先以观察为主；如果后续日志证明有大量超过上限的真实请求，再按证据调大阈值或增加更细的事件级诊断。

## 2026-07-08 Milus 本地透传验证

### 验证环境

本次把现网外部池 `milus` 复制到本地临时服务验证，避免改动线上运行状态。

- 本地服务：`127.0.0.1:19042`
- 本地数据库：`kiro_rs_milus_test`
- Redis 前缀：`kiro_rs:milus-test`
- 外部池：`milus`，`baseUrl=http://43.110.29.132:3000`
- 外部池并发上限：本地降为 `4`
- 路由策略：`externalDirectPolicyEnabled=true`
- 流式响应模式：`externalPoolStreamResponseMode=event_passthrough_capture`
- 本地 Kiro 凭证：空列表，确保请求走外部池直连

### 直接协议验证

直接调用本地 `/cc/v1/messages` 和上游 `/v1/messages` 的低流量验证结果：

| case | 状态 | 首分片 | 总耗时 | 事件形态 | usage |
| --- | --- | ---: | ---: | --- | --- |
| 本地 haiku stream | 200 | 2125ms | 2231ms | `message_start -> content_block_delta -> message_delta -> message_stop` | `input=4103, output=2` |
| 本地 opus 4.8 stream | 200 | 1695ms | 1739ms | 正常 SSE | `input=6483, output=2` |
| 本地 haiku non-stream | 200 | 1560ms | 1560ms | JSON message | `input=5, output=2` |
| 直连 Milus haiku stream | 200 | 1539ms | 1653ms | 与本地 haiku stream 事件顺序一致 | `input=4103, output=2` |

本地 haiku stream 与直连 Milus haiku stream 的事件顺序、文本 delta、最终 `message_delta.usage` 完全一致；差异只在请求 ID、模型名等请求上下文。说明 `event_passthrough_capture` 下正常 SSE event 没有被重写后再输出。

usage record 也能证明两套口径同时存在：

- `externalPoolBilling.streamResponseMode=event_passthrough_capture`
- `externalPoolBilling.rawUsage` 保存上游原始 usage
- `externalPoolBilling.reportedUsage` 仍按本地 `current_path_policy` 做内部计费/历史兼容整形
- 下游流式响应保留上游 usage，不再被本地 projected usage 改写

### Claude Code CLI 验证

使用真实 Claude Code CLI `2.1.197`，隔离 HOME 和 `CLAUDE_CONFIG_DIR`，通过：

```bash
ANTHROPIC_BASE_URL=http://127.0.0.1:19042/cc
claude --bare --print --verbose \
  --output-format=stream-json \
  --include-partial-messages \
  --no-session-persistence
```

验证结果：

| case | CLI 结果 | CLI API 耗时 | CLI 首 token | 流事件 | CLI 最终 usage |
| --- | --- | ---: | ---: | --- | --- |
| `claude-3-5-haiku-20241022` | `cli-ok` | 1644ms | 2650ms | 7 个 `stream_event`，含最终 `message_delta.usage` | `input=5960, output=3` |
| `claude-opus-4-8` | `cli-opus-ok` | 12176ms | 11024ms | 7 个 `stream_event`，含最终 `message_delta.usage` | `input=8538, output=5` |

两条 CLI 用例均成功完成，未出现 Claude Code CLI 解析失败、usage 为 0、SSE event 缺失、内部错误词泄漏等问题。CLI 看到的是上游透传 usage；服务端 usage record 同时保留整形后的 `reportedUsage`。

需要注意：Claude Code CLI 单次 `--print` 可能发起不止一条 `/cc/v1/messages`。本次 Opus 用例产生两条外部池请求，CLI `modelUsage` 的 `input=15582, output=19` 正好等于两条请求的 raw usage 合计；最终 `result.usage` 对应最后一条主响应 `input=8538, output=5`。这属于 Claude Code CLI 的调用行为，需要在成本分析里按服务端 usage record 对账，而不能只看 CLI 最终 result。

### 错误路径验证

单次无效模型请求返回公开错误：

- HTTP 状态：`503`
- 对外错误类型：`api_error`
- 对外消息：只包含“稍后重试”和 error ID/request ID
- 未向客户端暴露：`credential`、`external pool`、`fallback`、`api_key`、`bearer`、`sk-`

服务端 usage record 和日志保留了上游 `model_not_found` 证据，便于排查，但客户端看不到内部池、凭证或密钥细节。

### 本次结论

在低流量真实上游验证范围内，Milus 外部池接入本地服务并开启 `event_passthrough_capture` 没有发现协议兼容问题：

- 直接 SSE 正常。
- Claude Code CLI `stream-json` 正常。
- 下游流式 usage 保持上游透传。
- 内部 `reportedUsage` 仍能按本地策略整形并落库。
- 错误路径会做公开错误遮蔽。
- 外部池状态保持 `dispatchable=true`、`inFlight=0`、`autoDisabled=false`。

本次没有对真实 Milus 上游做高并发压测，避免给上游和线上账号造成额外压力；高并发/长上下文仍需要在 fake upstream 或明确限流的真实环境中单独验证。
