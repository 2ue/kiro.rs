# 项目分析：`kiroxy`

路径：`/Users/yuanfeijie/Desktop/procode/kiro-research/nopperabbo__kiroxy`  
最新本地提交：`2ac2ea9`，2026-05-23  
相关度：高

`kiroxy` 是这批样本里最值得从“上游调用细节、健康权重、压测、SSE 防卡死”角度学习的项目。它不适合整体替代当前项目，但有不少局部能力写得很直接。

## 关键文件

| 文件 | 作用 |
| --- | --- |
| `internal/pool/pool.go` | 账号池、stickiness、LRU/weighted pick、cooldown |
| `internal/pool/health.go` | rolling success、recent request buckets、EWMA latency、usage factor |
| `internal/pool/stickiness.go` | session pin |
| `internal/pool/usage.go` | usage limits polling |
| `internal/kiroclient/headers.go` | native Kiro headers、User-Agent profile |
| `internal/kiroclient/target.go` | profileArn 决定 AWS target |
| `internal/kiroclient/client.go` | upstream HTTP client |
| `internal/kiroclient/idle_reader.go` | body idle timeout |
| `internal/messages/service.go` | 消息执行、pool retry、latency recorder |
| `internal/messages/gate_writer.go` | 首个可见输出前缓存 SSE，便于透明重试 |
| `internal/reqconv/cache_points.go` | Anthropic `cache_control` 到 Kiro `cachePoint` |
| `internal/respconv/thinking_tags.go` | thinking tag 边界解析 |
| `scripts/loadtest/main.go` | 压测工具 |

## 账号池与健康权重

`pool.go` 的选择流程：

1. 如果有 session stickiness，优先使用 pinned account。
2. pinned 不可用则释放 pin 并重选。
3. 非 sticky 情况走 weighted selection。
4. 所有候选都低权重时退化为 LRU，避免随机抖动。
5. 选择时检查 enabled、cooldown、MinRestPeriod。

`health.go` 的 `AccountHealth` 维护：

- 最近 N 次成功率 ring。
- 5 分钟 recent request buckets。
- 最近 rate-limit 时间。
- latency EWMA。
- usage limits snapshot。

权重计算：

- success rate 越低权重越低。
- 30 分钟内 rate limit 降权到 0.1。
- 最近请求越多权重越低。
- usage 剩余越少权重越低。
- 有 overage 能力时不直接打到底。

这个思路比当前项目的 `health_balanced` 更容易向用户解释。当前项目也有 latency/error/selection pressure，但建议学习它的展示方式：把每个因子拆成管理端可见字段。

## MinRestPeriod 抗突发

`Policy.MinRestPeriod` 的注释非常关键：同一个账号短时间连续请求会触发 Kiro 风控，尤其大量账号共享同一出口 IP 时。

当前项目已有 RPM，但 RPM 是“每分钟速率”。MinRestPeriod 是更直接的“同账号最小间隔”，可和 RPM 互补。

建议当前项目后续评估：

- 账号增加 `min_rest_ms` 或复用 RPM 计算出的最小间隔。
- 管理端展示“上次调度距今”。
- 大并发下优先选满足 rest period 的账号，避免一个账号连续被打。

注意：当前项目已有 `rate_limit_interval_for_rpm`，不能重复引入两个互相冲突的限速口径。更合理做法是把 RPM 解释成最小间隔，或让账号级 min rest 作为高级 override。

## Native Kiro headers

`internal/kiroclient/headers.go` 明确区分 native Kiro IDE shape 与 legacy shape：

- native endpoint：`https://q.{region}.amazonaws.com/generateAssistantResponse`
- native content-type：`application/json`
- 关键 header：`x-amzn-kiro-agent-mode: vibe`
- User-Agent 带 machine id 和 Kiro IDE 信息。
- `x-amzn-codewhisperer-optout` 不应被随意改。

当前项目 `src/kiro/endpoint/ide.rs` 已经使用 native endpoint，且有：

- `x-amzn-kiro-agent-mode`
- machine id User-Agent
- `tokentype: API_KEY`
- `TokenType: EXTERNAL_IDP`
- body 注入 `profileArn`

可学习点不是“当前没有 native”，而是：

- 把 header shape 写成更明确的测试。
- 对不同 auth method 的 header 做 snapshot test。
- 对 User-Agent 中的 machine id / Kiro version 建立稳定生成策略。

`kiroxy` 的 `profileFromMachineID` 用 machineID hash 生成稳定 UA 变体，这能让多账号看起来像不同 IDE 安装。当前项目是否需要学习要谨慎：这可能影响 upstream fingerprint，不应默认改。更适合做可选策略。

## Endpoint failover

`kiroxy` 定义三类 upstream：

1. Kiro IDE primary：`q.{region}.amazonaws.com/generateAssistantResponse`，无 `X-Amz-Target`。
2. CodeWhisperer fallback：`codewhisperer.{region}.amazonaws.com/generateAssistantResponse`。
3. AmazonQ fallback：`q.{region}.amazonaws.com/generateAssistantResponse` + `AmazonQDeveloperStreamingService.SendMessage` target。

它的观点是不同 gateway 有时对 429/配额判断不同，可以作为 edge-level failover。

当前项目已有 endpoint 抽象，但默认主要是 IDE endpoint。建议：

- 不要默认启用多 endpoint failover。
- 可以增加管理开关和账号级 endpoint policy。
- failover 必须记录到 call trace：endpoint name、status、duration、exception。
- failover 仅对明确可重试错误启用，例如 429、5xx、network、200 JSON throttle。
- 对 400 malformed request 不应换 endpoint，否则会掩盖请求体 bug。

## GateWriter

`internal/messages/gate_writer.go` 在首个可见输出前缓存 HTTP/SSE 输出，直到确认有可见 text/tool_use 后才 Promote。

这个机制用于解决一种场景：

- upstream 输出了 thinking 或内部事件。
- 最终没有可见文本，也没有 tool_use。
- 如果已经把 SSE header/chunk 发给下游，就很难透明重试。

当前项目已经实现 thinking 输出，且用户明确要求要有真实 thinking 输出。因此不能简单“缓存直到可见输出才发”，否则会延迟 thinking 流出。

可学习点：

- 可以针对“非 thinking 模式”或“空可见 end_turn 重试”做 gate。
- thinking 模式下不应隐藏思考过程。
- 更适合学习它的状态机概念：区分 `first upstream chunk`、`first thinking delta`、`first visible output`。

当前项目已经有 `UsageLatencyTrace`：

- `first_upstream_chunk_ms`
- `first_output_delta_ms`
- `stream_gap_to_first_output_ms`
- `chunks_before_first_output`
- `events_before_first_output`

建议继续补一个 `first_thinking_delta_ms`，便于区分“首字慢”还是“先输出 thinking，用户可见文本慢”。

## cachePoint

`internal/reqconv/cache_points.go` 和本地 `kiro2api` 类似：当 Anthropic tool 带 `cache_control` 时，在 Kiro tools 数组里插入：

```json
{"cachePoint":{"type":"default"}}
```

这是当前项目高缓存后续最值得尝试的真实 upstream 方向。

建议当前项目：

- feature flag：`kiro_cache_point_enabled`。
- 初期只对 tools cache_control 开启，不对 system/messages 大范围改 body。
- 记录 upstream 是否接受，不接受时自动降级并标记 protocol error。
- 保持本地 usage projection 不变，避免账单/dashboard 口径突然跳变。

## Loadtest

`scripts/loadtest/main.go` 是当前项目最该学习的工程资产之一。当前项目已经有很多单测，但缺一个真实流式压测入口。

建议当前项目做自己的 `tools/loadtest`：

- 支持设置并发数、总请求数、stream/non-stream、模型、路径。
- 支持长会话、thinking、tool_use、cache_control 场景。
- 统计 TTFB、first thinking、first text、总耗时、错误类型、request id。
- 输出 p50/p95/p99。
- 可选抓取进程 RSS，观测内存泄漏。
- 可验证 RPM 和 max concurrent 生效。

## 比当前项目强的地方

- rolling health 因子清晰。
- MinRestPeriod 对抗突发风控的语义明确。
- native headers 和 endpoint failover 研究很深入。
- GateWriter 对“空可见输出后重试”的处理值得参考。
- loadtest 工具比当前项目体系化。
- idle reader 和 200 JSON exception 测试完整。

## 当前项目比它强的地方

- 当前项目有 PgSQL/Redis、多实例状态、usage dashboard、外部账号池。
- 当前项目 RPM/并发 lease/dispatch wait 更完整。
- 当前项目错误归一化和对外 request id 更完整。
- 当前项目 `/dfcache/*` 和 route policy 更安全。

## 建议吸收方式

P0：

- 做当前项目自己的 loadtest。
- 增加健康因子 breakdown 展示。
- 补 200 JSON exception、idle stream 测试。

P1：

- 引入账号 min rest 或把 RPM UI 文案改成“最小请求间隔”解释。
- feature flag 实验真实 `cachePoint`。
- endpoint failover 做管理开关。

不建议：

- 不要默认开启 endpoint failover。
- 不要默认改 User-Agent fingerprint。
- 不要用 GateWriter 隐藏 thinking 输出。

