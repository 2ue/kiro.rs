# 59137 本地账号并发排队根因分析

分析对象是服务器 `152.53.243.159` 上 host port `59137` 对应的 kiro.rs 部署。本文只讨论 `routeKind=local_credential` 的本地账号请求，外部池请求不纳入结论。所有生产证据均通过只读 SSH、只读 PostgreSQL 事务、Redis `INFO`/`SCAN`/`ZRANGE`/`ZCARD` 查询取得，没有写 Redis、写 PostgreSQL、修改配置、重启容器或清理 lease。

## 修订说明（2026-09-24）

此前文档把“曾经观察到高并发”和“当前异常时刻的并发槽持续满”混为一谈，也把 TLS 连接和单条请求混为一谈。下面只把 Redis/usage 直接观测到的事实、0.0.169 代码保证的生命周期、以及仍未证实的协议假设分开写。新增的 10 分钟完整抓包仍是 TLS 密文，不能证明 `COMPLETED` 是否缺失或发生了协议变化。

## 结论

当前可证实的容量机制是：本地账号的 in-flight 槽位在 HTTP response body 完成前持续占用；上游每产生一个 body chunk，0.0.169 都会 touch lease 并重置 180 秒 idle 计时。因此只要上游仍在发数据（包括可能不产生下游可见文本的 reasoning），槽位就不会因 idle 清理释放，新请求会排队。

这解释了“并发槽持续满”的运行机制，但还不能仅凭已有密文抓包断言具体异常请求是正常长生成、上游停顿、未知事件、或某个解析状态滞留。最需要补齐的证据是同一请求的 lease acquire/release、明文逐帧事件、body EOF 和客户端断开时间。

```text
本地账号获取 lease
  -> 上游响应头返回后 lease 仍然保留
  -> 上游 response body 长时间产生 reasoning_content，或首个 body chunk/首个可见输出很晚
  -> 每个上游 chunk 都 touch lease，活跃请求不会被 15 分钟过期清理
  -> reasoning 内容通常先在本地原子缓冲，不立即产生下游可见 SSE
  -> 请求继续占用本地账号并发槽
  -> 10 个本地账号的可用容量被持续消耗
  -> 新请求进入 dispatch queue
```

`duration_ms` 是请求从入口到终止的总耗时，`usage_records.created_at` 是写入完成记录的时间。`credentialAttempts.durationMs` 只覆盖该次上游 attempt 到响应头；`upstreamHeaderMs` 从处理器入口开始，可能包含排队。因此这些字段可以定位阶段，但不能单独给出精确排队时长。

补充的两个 15 分钟真实流量窗口共观察到 80 条 `routeKind=local_credential AND stream=true` 请求：77 条成功、3 条客户端断开。第一窗口请求重叠峰值为 11，第二窗口为 5；第一窗口 p90 总耗时 258.3 秒，第二窗口出现约 455 秒后仍正常成功的请求。该证据说明正常深度 reasoning 和上游慢响应确实会长期占用 lease，不能用固定墙钟把正常请求改成错误。

目前没有发现普通路径中“客户端断开后 lease 永久不释放”的明确代码漏洞。lease 的完成、流错误、idle timeout 和 Drop 路径都有释放逻辑。问题更像是**真实长流占用 + 上游事件协议/输出形态导致首个可见输出延迟**，造成容量被长时间占满，外观上类似并发泄漏。

## 线上部署和实时容量

59137 对应：

- 容器：`kiro-rs-2ue-59137-app`
- 镜像：`ghcr.io/2ue/kiro-rs:latest`（OCI version `0.0.169`，revision `e7be13034c3e59c7410e93a14e08fb1cafb85813`）
- 容器端口：`8990`
- 宿主端口：`59137`
- 容器启动后状态：`running`、`healthy`
- `/healthz`：`{"service":"kiro-rs","status":"ok"}`
- `/readyz`：PostgreSQL、Redis、Redis runtime events 均为 true
- `OOMKilled=false`

关键配置：

| 配置 | 当前值 | 影响 |
|---|---:|---|
| `credentialMaxConcurrentRequests` | 10 | 单个本地账号的请求并发上限 |
| `dispatchMaxQueuedRequests` | 200 | 调度队列上限 |
| `dispatchGlobalMaxConcurrentRequests` | 0 | 不设置额外全局并发上限 |
| `credentialDispatchMaxWaitSecs` | 300 | 调度等待上限 |
| `credentialInFlightLeaseMaxSecs` | 900 | lease 无 touch 时的最大存活时间 |
| `kiroUpstreamResponseTimeoutSecs` | 600 | 等待上游 HTTP response headers 的超时 |
| `kiroUpstreamStreamIdleTimeoutSecs` | 180 | 上游 response body 连续无数据的超时 |

Redis 实时状态采样中：

- `scheduler:global:inflight:acquired` 为 54；
- 各本地账号 observed acquired 数为 6、6、6、6、6、6、6、6、3、3，总计 54；
- `scheduler:global:queue_leases:v1` 在约 20 秒内从 136 到 144、148、138 变化；
- 队列并非完全静止，说明仍然存在释放和重新获取；但队列长期处于高位，消费速度低于进入速度；
- Redis 没有观察到驱逐、慢命令、内存耗尽或连接拒绝：`evicted_keys=0`，`slowlog_commands_count=0`，使用内存约 6 MiB。

这说明排队是有效容量压力，不是 Redis 整体故障。

## “usage 原始数据诊断”当前能提供什么

59137 的配置中确实启用了 usage 相关诊断和原始 usage 字段：

- `reportedUsage.enabled=true`；
- 默认 input/output mode 为 `raw`；
- `cacheRead` 为 `preserve`；
- `rawUsage`、`kiroMeteringUsage`、`latencyTrace`、`credentialAttempts`、`routeKind`、`routeSubtype` 等字段写入 usage record；
- `toolFormatDebug.enabled=true`，但它是工具格式/请求体诊断，不是通用的上游原始 SSE 抓包；
- tool-format debug 会截断大小、字符串和记录数量，也不会保存完整上游 wire response。

因此，usage 原始诊断**可以用于当前根因分析**，因为它已经记录了：

- 上游 headers 到达时间；
- 第一个 body chunk 到达时间；
- 第一个输出事件时间；
- chunk/event 数量；
- 首个输出前收到的上游 bytes 和 frames；
- 上游事件类型统计；
- `sawUpstreamCompleted`、`upstreamEofWithoutCompleted`、`lastUpstreamEventType`；
- 当前请求实际使用的 credential；
- 每次 credential attempt 的 HTTP status 和 attempt duration。

但它**不能完全替代原始协议抓包**。usage 保存的是解析后的摘要，不保存每个原始上游 JSON/SSE frame 的完整内容。因此它能证明“长在哪里、是否收到 completed、是否大量 reasoning”，但不能单独证明上游某个未知字段的完整 wire shape 是否发生变化。

## 本地账号 usage 统计

查询范围：最近两小时、`data->>'routeKind'='local_credential'`，只取非删除记录。

结果：

| 指标 | 数值 |
|---|---:|
| 本地账号请求数 | 2716 |
| 总耗时平均值 | 80.9 秒 |
| 总耗时 p50 | 78.8 秒 |
| 总耗时 p90 | 120.5 秒 |
| `upstreamHeaderMs` 平均值 | 39.2 秒 |
| `upstreamHeaderMs` p50 | 26.4 秒 |
| `upstreamHeaderMs` p90 | 86.4 秒 |
| `firstUpstreamChunkMs` 平均值 | 90.2 秒 |
| `firstUpstreamChunkMs` p50 | 57.9 秒 |
| `firstUpstreamChunkMs` p90 | 218.0 秒 |
| `firstOutputDeltaMs` 平均值 | 93.8 秒 |
| `firstOutputDeltaMs` p50 | 61.4 秒 |
| `firstOutputDeltaMs` p90 | 236.2 秒 |
| `streamGapToFirstOutputMs` 平均值 | 4.5 秒 |
| `streamGapToFirstOutputMs` p90 | 2.9 秒 |
| 首个可见输出超过 5 分钟 | 21 条 |
| 上游 response header 超过 5 分钟 | 21 条 |
| `client_dropped` | 37 条 |
| `upstream_eof_without_completed=true` | 324 条 |
| `sawUpstreamCompleted=true` | 0 条 |

这里的 `sawUpstreamCompleted=true` 为零并不表示所有请求都异常；当前本地路径大量记录最终通过 clean EOF、metering/context usage 或本地推断结束完成。它表示上游显式 `messageStatus=COMPLETED` 没有被记录到这些样本中，不能把本地 `success` 直接等价为上游发出了显式 completed。

按 terminal reason 分组：

| terminal reason | 数量 | 平均总耗时 | 平均 header | 平均首 chunk | 平均首输出 | 平均 gap |
|---|---:|---:|---:|---:|---:|---:|
| 缺失/空值（多数普通记录） | 2346 | 71.3 秒 | 31.1 秒 | — | — | — |
| `completed` | 331 | 132.3 秒 | 83.9 秒 | 84.3 秒 | 84.9 秒 | 1.3 秒 |
| `client_dropped` | 37 | 222.6 秒 | 141.5 秒 | 143.0 秒 | 182.2 秒 | 35.9 秒 |
| `internal_error` | 1 | 127.8 秒 | 118.7 秒 | 118.7 秒 | 118.7 秒 | 0 秒 |
| `upstream_status_error` | 1 | 174.8 秒 | 11.9 秒 | 11.9 秒 | 11.9 秒 | 0 秒 |

这些数据把两类延迟区分开了：

1. **header 主导型**：`upstreamHeaderMs` 已经很高，header 后到 chunk/输出的差距较小。它只能说明处理器入口到响应头/首 chunk 的总时间很长；由于该计时包含本地账号调度，必须结合 `credentialAttempts.durationMs` 和调度快照，不能单独归因于上游网关、Kiro 服务或模型准备阶段。
2. **流阶段主导型**：header 较早返回，但 first chunk 或 first output 很晚，或者 `streamGapToFirstOutputMs` 很大。它更接近上游 body 事件形态、reasoning 缓冲或事件到下游输出之间的问题。

## 代表性本地请求

### 请求 A：长请求但不是账号重试

该类请求的 `credentialAttempts` 形态为：

```json
[
  {
    "action": "response_headers_received",
    "status": 200,
    "attempt": 1,
    "durationMs": 2306,
    "credentialId": 1411
  }
]
```

同一条 usage 的 `latencyTrace` 可以出现：

```json
{
  "upstreamHeaderMs": 80604,
  "firstUpstreamChunkMs": 80604,
  "firstOutputDeltaMs": 80604,
  "streamGapToFirstOutputMs": 0,
  "localAttempts": 1,
  "externalAttempts": 0,
  "sawUpstreamCompleted": false,
  "upstreamEofWithoutCompleted": true
}
```

解释：单次 HTTP attempt 记录约 2.3 秒，而从处理器入口到响应头/首 chunk 的总计时约 80 秒。两者差值包含调度等待和 attempt 之前的处理，不能被直接称为“上游 response headers 等待”。它不是多个账号重试造成的；该记录也没有显示首 chunk 后的 SSE 解析耗时。

### 请求 B：reasoning frame 先到，但下游首个可见输出很晚

一个代表性记录包含：

```json
{
  "upstreamHeaderMs": 434544,
  "firstUpstreamChunkMs": 435885,
  "firstOutputDeltaMs": 518153,
  "streamGapToFirstOutputMs": 82268,
  "upstreamBytesBeforeFirstOutput": 132619,
  "upstreamFramesBeforeFirstOutput": 931,
  "upstreamFramesWithoutDownstreamEventsBeforeFirstOutput": 931,
  "upstreamEventTypesBeforeFirstOutput": {
    "reasoning_content": 930,
    "assistant_response": 1
  },
  "terminalReason": "client_dropped"
}
```

这里上游已经发出了大量 frame，但前面几乎全是 `reasoning_content`。从首 chunk 到首个下游可见输出又经过约 82 秒，最终客户端断开。该请求在整个期间都占用本地账号 lease。

另一条同类记录在首个可见输出前累计了 2366 个 reasoning frame，`streamGapToFirstOutputMs` 约 176 秒。该现象直接解释了为什么客户端看不到输出，但本地账号的并发槽仍然被占用。

### 请求 C：上游 EOF 无显式 completed

大量成功记录的尾部形态是：

```json
{
  "lastUpstreamEventType": "meteringEvent",
  "sawUpstreamCompleted": false,
  "upstreamEofWithoutCompleted": true,
  "terminalReason": "completed",
  "stopReasonSource": "local_inferred_end_turn"
}
```

这说明上游流结束时没有显式 `messageStatus=COMPLETED`，服务端根据已收到的 assistant/reasoning/metering/context 信息推断结束。它会让请求一直占用到 EOF，但 EOF 到达后 lease 会释放；单凭这个字段不能认定为永久泄漏。

## 代码级 lease 生命周期

### 获取和 Redis 确认

`src/kiro/token_manager/manager.rs:4393-4457` 先加入本地 provisional slot，再在 `4460-4554` 调用 Redis `acquire_dispatch_lease`。Redis 返回 None 或错误时，代码会 `confirm_redis_not_acquired()` 并 drop guard，回滚本地占用。这个路径不支持“Redis 获取失败但本地永久占用”的普通泄漏解释。

### 上游响应头之后仍然占用

`src/kiro/provider.rs:10346-10350` 的 response-header timeout 只负责等待上游 HTTP response headers。收到 200 headers 后，`10432-10478` 把 lease 移交给 `KiroStreamCompletion`。代码注释和实现都表明：收到 200 headers 不等于请求完成，lease 要一直保留到 response body 流结束、出错、idle timeout 或 stream 被 drop。

所以 usage 中：

```text
credentialAttempts.durationMs 很短
但 durationMs 很长
```

是符合代码生命周期的。前者只描述 attempt 到 response headers 的时间，后者覆盖整个流。

### 每个上游 chunk 都会续租

`src/anthropic/handlers.rs:8557-8568` 每收到一个上游 body chunk 就调用 `state.completion.touch()`。`src/kiro/token_manager/concurrency.rs:720-753` 会更新时间戳并异步 touch Redis，touch 周期按配置动态限制在约 250ms 到 30s 范围。

因此，只要上游持续发送 reasoning frame，即使下游没有可见文本，lease 仍然被视为活跃。`credentialInFlightLeaseMaxSecs=900` 只会清理超过 15 分钟且没有 touch 的 lease，不能清理持续收到 chunk 的长流。

### stream idle timeout 的边界

`src/anthropic/handlers.rs:9084-9117` 在连续无上游数据达到 idle timeout 时调用 `report_upstream_stream_failure()` 并释放 lease；当前配置为 180 秒。

这意味着：

- 上游完全停住超过 180 秒：应触发 idle timeout，释放 lease；
- 上游每隔一段时间发送 reasoning chunk：不会触发 idle timeout，会继续占用 lease；
- 上游大量发送 reasoning，但不产生下游事件：仍然会持续占用 lease。

### 正常完成、错误和 Drop

`src/kiro/provider.rs:899-952` 的 `report_success`、`report_soft_failure`、`report_upstream_stream_failure` 都会 take 掉 `in_flight_lease`。`provider.rs:977-984` 的 `Drop` 继续提供兜底释放。

`src/kiro/token_manager/concurrency.rs:680-719` 的 `release()` 具备原子防重、本地计数扣减、容量唤醒和异步 Redis release；`776-779` 的 `Drop` 会调用它。

客户端断开时，`StreamUsageGuard::Drop` 会记录 `client_dropped`；实际 lease 由 `KiroStreamCompletion` drop 释放。当前代码没有发现一个普通客户端断开一定会永久保留 lease 的路径。

## 结束标识是否可能变化

当前代码识别的 Kiro 事件类型包括：

- `assistantResponseEvent`
- `reasoningContentEvent`
- `contextUsageEvent`
- `meteringEvent`

`src/kiro/model/events/assistant.rs` 中的 `messageStatus` 支持显式 `COMPLETED`。`src/anthropic/stream.rs:1856-1905` 将无 completed、无 meaningful response、incomplete tool buffer 等情况分类为不完整或协议问题；`handlers.rs:8870-9079` 在 clean EOF 后决定成功或 stream failure。

先更正线上版本：本次采集时 `59137` 实际运行镜像是 `ghcr.io/2ue/kiro.rs:latest`，OCI 版本为 `0.0.169`，revision 为 `e7be13034c3e59c7410e93a14e08fb1cafb85813`。下文关于代码行为按该 revision 核对；早先把线上版本写作 `0.0.161` 是错误的。

抓包窗口内有 46 条本地账号 usage 记录**完成**，不是开始：`usage_records.created_at` 在结束、写 usage 时赋值。46 条全成功，其中流式 14 条、非流式 32 条，使用 10 个凭据。对这 14 条流式记录，`upstreamEofWithoutCompleted=true`，没有显式 `COMPLETED`，但也没有首输出前的 frame/event 解析错误；它们均 clean EOF 成功。这个窗口没有证据显示结束标识变化导致卡住。

此前更宽窗口 usage 汇总显示：

- `upstreamEofWithoutCompleted=true`：324 条；
- `lastUpstreamEventType` 大量停在 `meteringEvent` 或 assistant response；
- `sawUpstreamCompleted` 没有为 true 的本地样本；
- 但多数仍然通过 clean EOF 和本地推断完成。

这些 EOF 统计本身不是并发长期占用的证明。尤其不能把 `upstreamHeaderMs` 直接说成“上游慢”：它从处理器入口开始计时，包含本地账号调度等待，到 provider 响应头返回才结束。`headerMs - credentialAttempts.durationMs` 只能近似请求到账号 attempt 之间的时间，仍不是精确排队时间。抓包窗口这 46 条记录没有解析错误计数，但 usage `created_at` 是结束时间，且 TLS 流无法与单条账号请求可靠对应，因此不能用它们声称已排除某个请求在途期间的协议异常。

现有证据没有证明结束标识变化。更重要的是，0.0.169 的实现即使解析到 `messageStatus=COMPLETED`，也不会据此提前结束上游 HTTP body；lease 仍持有至 body EOF、读错误、idle timeout 或 drop。因此，缺失 `COMPLETED` 不能解释代码为什么继续等待；它只影响 EOF 完整性判断和 usage 观测。

## 是否需要部署抓包服务

### 直接 tcpdump 不够

59137 到上游通常经过 TLS。直接在远端抓 TCP/TLS 只能看到：

- 连接建立和关闭；
- TLS 握手；
- packet/segment 到达时间；
- body 是否持续有网络数据；
- 是否在 idle timeout 前持续收包。

它不能直接看到 JSON/SSE 中的 `messageStatus`、event type 或新增字段。

### 当前优先使用 usage 原始诊断

usage 已经包含事件尾部、事件计数、首输出前 frame 类型和 EOF/completed 状态，足以先回答“是否大量 reasoning、是否首 chunk 慢、是否 EOF 无 completed、是否解析错误”。因此在不影响生产的前提下，当前阶段可以直接使用 usage 做第一轮协议分析，不需要立刻部署 MITM。

### 需要确认 wire 协议时的安全方案

若要确认未知上游事件的完整 JSON，应采用短期、按采样、脱敏的应用层捕获，而不是无限制全量抓包：

1. 在上游事件解析入口增加 opt-in debug recorder；
2. 只记录本地账号、指定模型或指定请求 ID；
3. 只保存 event type、字段名集合、长度、hash、状态字段和前后少量脱敏样本；
4. 不保存 access token、cookie、完整 prompt、完整 reasoning 或完整 response；
5. 设置最大记录数、最大字节数和自动过期时间；
6. 部署前先在本地 fake upstream 测试 unknown event、missing completed、truncated frame、reasoning-only 和 client drop；
7. 采集完成后立即关闭采样并删除远端临时文件。

这类应用层 debug recorder 才能直接回答“是否出现新增 event type/字段、是否是结束状态名称变化、是否是 decoder 没有识别的 frame”。

## 工作区中的应用层采集代码（尚未部署）

工作区增加了一个默认关闭的 `toolFormatDebug.upstreamProtocol` 采集配置，但它只存在于本地未提交代码，且当前 `rg` 检查显示尚未接入线上 0.0.169 的 handler 调用点。因此不能把它描述为现网已经具备的采集能力。本次远端 pcap 没有解密，不能产出协议帧内容。

默认记录内容：

- request ID、endpoint、requested/upstream model、credential ID；
- EventStream `messageType`、`eventType`、header 名称；
- payload 字节数和 SHA-256；
- JSON 顶层字段集合；
- `messageStatus`、`status`、`stopReason`、`type`、`eventType` 等结束/状态候选字段；
- 不保存 payload 内容。

只有显式打开 `capturePayload` 时，才保存每个采样 frame 的有限长度 payload 样本。采集仍受时间窗口、最大记录数、最大 payload 字节数、JSONL 单条记录上限和请求 ID 白名单约束。未知事件类型、非标准 message type 和带状态字段的事件会优先采集；需要查看完整事件序列时再打开 `captureAllEvents` 并填写具体 request ID。

配置示例：

```json
{
  "toolFormatDebug": {
    "enabled": true,
    "upstreamProtocol": {
      "enabled": true,
      "capturePayload": true,
      "captureAllEvents": true,
      "requestIds": ["req_具体请求ID"],
      "maxRecordsPerWindow": 200,
      "windowSecs": 600,
      "maxPayloadBytes": 4096
    }
  }
}
```

输出仍写入 `toolFormatDebug.dir`，默认是 `logs/tool-format-debug`。采集完成后应关闭 `upstreamProtocol.enabled`；payload 样本可能包含 reasoning、工具参数或其他请求内容，不能长期保留或打包分享。

实现位置：`src/anthropic/tool_format_debug.rs`、`src/anthropic/handlers.rs`、`src/model/config.rs`、`src/kiro/parser/header.rs`。已验证 `cargo check --locked`、既有 tool-format debug 测试、配置归一化测试，以及未知 EventStream 事件摘要采集测试。

## 根因分层

### 已由线上证据确认

1. 早前某个采样时点 59137 有真实高位队列和 54 个本地 in-flight；这不能直接代表本次 08:12–08:15 pcap 窗口。
2. 本次 08:12:16–08:15:16 UTC 有 46 条本地账号 usage 完成记录，全部成功，其中 14 条流式、32 条非流式；14 条流式记录都无显式 completed、无首输出前解析错误，并在 EOF 后成功。
3. `upstreamHeaderMs` 含处理器入口到账号 attempt 前的排队/调度时间，不能单独证明上游慢。
4. pcap 只能证明若干 TLS TCP 连接在窗口内持续双向传输数据；无法解密 EventStream，也无法把连接归属到某条 usage 或 credential。
5. 0.0.169 本地 lease 释放条件为 response body EOF、读取错误、idle timeout 或 stream drop；每个 body chunk 都会重置 idle timeout 并 touch lease。

### 高可信代码级根因

1. 0.0.169 的 lease 生命周期覆盖整个流式 body，而不是只覆盖到 headers。
2. 任意成功读取的 body chunk 都会续 lease 并重置 idle deadline；若上游持续有数据，即使没有下游可见文本，idle timeout 也不会触发。
3. reasoning frame 可能被缓冲而不立即产生下游可见 SSE；这是长占用的可行机制，但本次 TLS pcap 没有解密，不能据此认定该窗口发生了多少此类请求。

### 尚未由当前 usage 单独证明

1. 上游是否新增了完全未知的 event type。
2. 上游是否把原来的 `COMPLETED` 改成了其他具体字符串。
3. 是否存在某个特定模型/账号/区域的上游协议差异。
4. 是否有个别任务由于 stream object 未及时 Drop，超过正常生命周期后仍短暂保留 lease。

## 本次 08:23–08:28 UTC 完整 TLS pcap

文件：`tmp/prod-evidence/20260924-59137/raw/upstream-netns-window2-20260924T0823Z.pcap`，实际抓到 **33,267 packets**，约 13 MiB；远端和本地 SHA-256 一致：`8aa45b3587c4952e1b4ee2f4626d66ebc4d79a8edc614a5c24cc567f3632410e`。59137 抓包开始和结束时均为 `running/healthy`。

修正为双向 TCP 四元组后是 4 条连接，不是旧分析器报告的 8 条单向 flow。最长连接贯穿约 299 秒并双向传输了大量 TCP payload；其它连接持续约 32、155、247 秒。TCP payload 最大间隔分别显示连接有持续/间歇传输，但 TLS 加密且连接可能复用多个 HTTP/2 请求；持续一个 TCP 连接不等于一个生成请求持续这么久，也不能从 FIN/RST 推出 EventStream 结束标识。

容器配置的 TLS 后端为 Rustls，进程没有 TLS key log，因此此 pcap 无法用 Wireshark/tshark 解密。当前宿主也没有 `tshark`；但增加 `tshark` 仍不会解决缺少会话密钥的问题。真实协议帧必须从解密后的应用层解析点记录，或在后续运行时预先配置 TLS key logging；不能从现有密文倒推出协议事件。

## 根因结论边界

已由事实确认的是：曾经有高位本地调度队列/并发占用；本地流式请求会在 body 整段期间持有账号 lease；每个 body chunk 续租，因此持续有数据的长流不会被 180 秒 idle timeout 清理。本次 14 条完成的流式 usage 都是成功 clean EOF，无首输出前解析错误，也没有显式 `COMPLETED`。完整 pcap 看到多个长 TLS 连接持续传输，但连接不能与 usage 对应。

因此，目前没有事实支持“上游变更结束标识造成并发一直占用”。可确认的直接容量机制是：**请求尚未到 HTTP body 结束时，账号槽位按设计一直被占用；持续有上游 body 数据则继续续租。** 但本次采样无法判定异常高并发时，具体请求是上游生成很慢、reasoning 事件连续输出、单连接复用，还是 EOF 后存在应用状态滞留；也不能仅凭本次平稳窗口还原先前异常峰值的根因。

要对下一次异常窗口得出协议级结论，必须在运行版本的应用层同时记录 request ID、credential、每帧事件类型和状态候选字段、帧时间、上游 body EOF 时间，以及 lease acquire/release 时间。当前工作区新增的 `upstreamProtocol` 采集器是可行的应用层采集入口，但它尚未存在于现网 0.0.169；单靠当前 sidecar TLS pcap 不具备解密能力。

## 最小验证路径

下一步若要把“上游协议变化”从假设变成证据，建议按以下顺序：

1. 从 usage 中筛选 `upstreamEofWithoutCompleted=true`、`upstreamEventTypesBeforeFirstOutput` 非空、`lastUpstreamEventType` 为 `meteringEvent` 的本地请求，按模型和 credential 分组。
2. 对这些请求增加短时应用层事件摘要采样，只保留事件名、字段集合、状态值、frame 长度和时间间隔。
3. 对照 `stream.rs` 和 event deserializer 的已知枚举，统计 unknown event 和 unknown messageStatus。
4. 如果没有 unknown event/status，而是已知 reasoning frame 持续时间很长，则根因归为上游首输出/流式 reasoning 长占用，不是协议解析失败。
5. 如果存在 unknown event/status，再针对该形状增加兼容解析和测试；不要先修改生产结束判定。

## 文件和代码参考

- 生产部署明细：`tmp/prod-evidence/20260924-59137/summary/inventory.md`
- 生产原始 usage 样本：`tmp/prod-evidence/20260924-59137/raw/local-long-requests.txt`
- Redis 容量快照：`tmp/prod-evidence/20260924-59137/raw/redis-capacity.txt`
- PostgreSQL usage 汇总：`tmp/prod-evidence/20260924-59137/raw/postgres-usage.txt`
- lease 获取/确认/清理：`src/kiro/token_manager/manager.rs:4393-4554`、`4659-4720`
- lease touch/release/Drop：`src/kiro/token_manager/concurrency.rs:680-779`
- 流式 completion lease 生命周期：`src/kiro/provider.rs:899-984`、`10346-10478`
- 上游 chunk touch、EOF、idle timeout：`src/anthropic/handlers.rs:8557-8568`、`8870-9079`、`9084-9117`
- reasoning 事件缓存和下游输出：`src/anthropic/stream.rs:2395-2417`、`2640-2754`、`3344-3385`

## 新增 10 分钟实时抓包（2026-09-24 08:57:26–09:07:25 UTC）

抓包程序在 `kiro-rs-2ue-59137-app` 的 network namespace 内独立运行，未重启或修改 59137。文件为 `tmp/prod-evidence/20260924-59137/raw/upstream-netns-live-20260924T085725Z.pcap`，远端和本地 SHA-256 均为 `ce3bd41b557d4eb377b51a0a4023859512a5736294612c9f018410bd0646d8a7`，共 78,666 个完整包、约 13.9 MiB。

双向 TCP 分析得到 3 个四元组：

- `172.18.0.4:58132 ↔ 3.219.15.140:443` 持续约 599.881 秒，双向均有大量 TLS payload；容器到上游方向最大 payload 间隔 28.87 秒，上游到容器方向最大间隔 0.665 秒；采集开始前已建立，采集结束时仍未关闭。
- `172.18.0.4:56352 ↔ 159.195.19.11:443` 持续约 145.749 秒，期间上游到容器方向最大 payload 间隔 142.809 秒，最终观察到 FIN/RST。这个连接说明存在长时间网络空档，但它仍不能等价为某条 HTTP 请求的 lease 时长。
- `172.18.0.4:48100 ↔ 159.195.19.11:443` 为短连接，约 18.337 秒后关闭，payload 很少。

这轮抓包证明了真实上游 TLS 连接中同时存在持续传输和长空档，且长连接可能跨越整个采集窗口；它**不能**证明某一条请求持续占用并发、不能判断 EventStream 的结束标识、也不能判断同一 HTTP/2 连接上的请求数量。因为线上使用 rustls 且未预先保存 TLS 会话密钥，无法事后解密这些包。

抓包同时段 Redis 快照（只读）显示 global acquired 为 60、队列从 81 降至 54，随后仍有容量压力；这表明槽位确实在被占用且有释放/重新获取，不能把该快照解释为“所有 lease 都永久泄漏”。
