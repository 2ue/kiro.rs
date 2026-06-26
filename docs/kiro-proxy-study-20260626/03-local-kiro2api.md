# 项目分析：本地 `kiro2api`

路径：`/Users/yuanfeijie/Desktop/procode/kiro2api`  
相关度：高

本地 `kiro2api` 的价值主要在协议转换层：Anthropic/OpenAI 到 Kiro request 的转换、Kiro EventStream 到 Anthropic SSE 的转换、thinking/tag 处理、真实 cachePoint、工具结果格式。它不像当前项目那样生产化，但协议层实现有很多可学习点。

## 关键文件

| 文件 | 作用 |
| --- | --- |
| `internal/reqconv/build_payload.go` | 构建 Kiro payload |
| `internal/reqconv/cache_points.go` | Anthropic `cache_control` 转 Kiro `cachePoint` |
| `internal/reqconv/tool_results.go` | tool_result / tool_use 转 Kiro 结构 |
| `internal/reqconv/schema_sanitize.go` | 工具 schema 清理 |
| `internal/respconv/streaming.go` | Anthropic SSE writer |
| `internal/respconv/event_processor.go` | Kiro event 到内部 delta |
| `internal/respconv/thinking_tags.go` | `<thinking>` tag 解析 |
| `internal/respconv/tool_calls.go` | tool_use event 转换 |
| `internal/kiroclient/idle_reader.go` | stream idle timeout |
| `internal/tracing/*` | trace id、transport、middleware |
| `internal/server/e2e_*_test.go` | 协议 e2e 测试 |

## 真实 cachePoint

`internal/reqconv/cache_points.go` 的核心逻辑很短：

- 遍历 Anthropic tools。
- 每个 tool 对应一个 Kiro `ToolEntry`。
- 如果该 tool 有 `cache_control`，就在该 tool 后追加 `CachePoint{Type:"default"}`。

这说明 Kiro upstream 可能理解 tools 数组中的 `cachePoint` entry。当前项目现在的高缓存主要是本地 usage projection 和 prompt cache tracker，不等于真实请求 Kiro 建缓存。

建议当前项目后续：

- 保持现有 high-cache route 和 reported usage 不变。
- 新增可选真实 cachePoint 层。
- 初期只处理 tool-level `cache_control`，不处理 message/system 级别。
- 失败时 fallback 到原始请求，不影响下游。
- usage 中记录 `upstream_cache_point_enabled`、`upstream_cache_point_applied`、`upstream_cache_point_error`。

风险：

- Kiro 对 cachePoint 的支持可能随账号类型、模型、endpoint 不一致。
- 工具顺序被改动可能影响 tool_use 协议。
- 如果 cachePoint 插入位置不对，可能触发 400。

因此必须 feature flag + 真实长会话测试。

## Thinking tag 解析

`internal/respconv/thinking_tags.go` 解析 `<thinking>...</thinking>`：

- 支持 tag 跨 chunk。
- 只有在响应开头、可见文本前出现时才当作控制 tag。
- 已经输出可见文本后，再出现 `<thinking>` 就保留为普通文本。
- 进入 thinking 后直到 `</thinking>` 前的内容都走 thinking buffer。
- `partialTagSuffix` 保留半截 tag，避免 chunk 边界误输出。

当前项目 `src/anthropic/stream.rs` 的 thinking 解析更复杂，支持 `<think>` 等变体，已有大量测试。但本地 `kiro2api` 的可学习点是“规则非常清楚”：只有开头的 thinking tag 才是控制语义。

建议当前项目检查：

- 代码块里的 `<thinking>` 是否会被误识别。
- 已经输出 text 后的 `<thinking>` 是否保留文本。
- tool_use 紧跟 thinking 结束时是否正确关闭 thinking block。
- long chunk split tag 是否稳定。

这些当前项目已有部分测试，但可以按本地 `kiro2api` 的边界再补完整。

## SSEWriter

`internal/respconv/streaming.go` 的 `SSEWriter` 有几个值得学习的点：

- `OnVisibleOutput` hook：首个 visible text/tool_use 前触发。
- `writeRawSSE`：热点 delta 事件不走 map + json marshal，减少分配。
- `IsEmptyVisibleEndTurn`：识别 thinking-only end_turn。
- `ResetAccumulator`：保留 writer 状态，重置 accumulator，方便重试/续写。
- `WriteErr`：客户端断开后停止继续写。

当前项目 `src/anthropic/stream.rs` 功能更完整，但可以学习：

- 对高频 SSE delta 使用更低分配写法。
- 明确区分 first thinking、first visible output、first tool_use。
- 对 client dropped 的写错误路径做专项压测。

## Event processor

`internal/respconv/event_processor.go` 的处理顺序：

- `assistantResponseEvent` 先用 `ComputeDelta` 做增量。
- delta 再过 thinking tag parser。
- text 进入 stop sequence 和 max_tokens 过滤。
- `reasoningContentEvent` 进入 thinking，但如果 XML tag 已解析出 thinking，就 suppress reasoningContent，避免重复。
- `toolUseEvent` 要等 `ToolStop` 才输出完整 tool_use。
- `metadata` 优先于 `metering`。
- `contextUsageEvent` 单独记录。
- exception/invalid state 转错误。

当前项目也有类似逻辑，但这个顺序值得对照回归，特别是：

- Kiro 同时输出 XML thinking 和 reasoningContentEvent 时不能双算。
- tool input JSON 不应被 max_tokens 截断成非法 JSON。
- metadata/metering 优先级要稳定。

## Tool result 格式

`internal/reqconv/tool_results.go` 把 Anthropic `tool_result` 转成 Kiro 工具结果时，使用：

```json
{
  "exit_status": "0",
  "stdout": "...",
  "stderr": ""
}
```

错误结果则 `exit_status: "1"`。

这是从 Kiro CLI 抓包来的形态。当前项目需要确认自己的 tool result 转换是否同样兼容，尤其是 Claude Code CLI / MCP 场景：

- 普通文本 result。
- error result。
- 空 result。
- 图片/结构化 content result。
- 多 tool_result 顺序和上一轮 tool_use 顺序一致。

当前项目已处理很多 malformed 400，但这个 `exit_status/stdout/stderr` 形态可以作为兼容性测试样本。

## tracing

本地 `kiro2api/internal/tracing` 有 trace id middleware、transport wrapper、header attrs。当前项目有 request id 和 usage trace，但可以借鉴其独立 tracing module 的边界。

建议：

- 当前项目保持 request-id 对外统一。
- 内部 trace exporter 独立成模块。
- 不把 usage 记录和 tracing exporter 混成一个对象。

## 比当前项目强的地方

- cachePoint 实现直接、清晰。
- thinking tag parser 的边界规则更容易读。
- SSE writer 把 visible output hook 抽象出来。
- tool result 的 Kiro CLI 形态值得做兼容样本。
- tracing 模块边界清楚。

## 当前项目比它强的地方

- 生产调度、PgSQL/Redis、外部账号池、usage dashboard 更完整。
- 错误归一化和对外 request id 更完整。
- payload guard 覆盖更多 malformed tool-use 场景。
- high-cache route policy 更可配置。

## 建议吸收方式

P0：

- 把本地 `kiro2api` 的 thinking/tool/cachePoint 场景整理成当前项目测试样本。
- 增加 first thinking / first visible latency 字段评估。

P1：

- feature flag 支持真实 Kiro `cachePoint`。
- 优化热点 SSE delta 写法，减少高并发分配。
- 增加 Kiro CLI tool_result 形态兼容测试。

不建议：

- 不要把当前调度简化成本地 `kiro2api` 的轻量模型。
- 不要一开始就对所有 cache_control 都插 cachePoint。
- 不要隐藏 thinking 输出来换取重试便利。

