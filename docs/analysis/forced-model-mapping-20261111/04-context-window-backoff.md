# 04 上下文窗口：退避方案

## 1. 问题

客户端以为可用窗口是 1M，实际只有 200K。这个差距**在客户端那一侧**，服务端无法让客户端改变认知。

Claude Code 官方文档（`code.claude.com/docs/en/model-config`，"Context window behind an LLM gateway"）：

> Claude Code gives each model it recognizes the same context window the model has on the Anthropic API. Fable models, Sonnet 5 and later, Haiku 5.5, and Opus 4.7 and later get the 1M window … Claude Code can't detect a lower limit that the gateway or the server behind it enforces.

这会带来两个后果：

1. 客户端要到它认为的约 1M 附近才会自动压缩，历史会一路增长到远超 200K。
2. 现有兜底（`payloadTooLongHandling=client_compaction`）返回 `prompt is too long: N tokens > 200000 maximum`。这个做法既是报错，又把真实窗口暴露了出去，**强制模式下必须禁用**。

客户端可调的开关（只对自己可控的客户端有效）：

- `CLAUDE_CODE_AUTO_COMPACT_WINDOW=200000`：在 200K 处自动压缩。
- `CLAUDE_CODE_DISABLE_1M_CONTEXT=1`：把默认 1M 的模型按 200K 处理。

服务端不能依赖客户端做这些配置，所以要自己完成退避。

### 现状对退避的影响

| 现状 | 影响 | 处理 |
|---|---|---|
| 本地没有 token 预检，超长全靠上游报错来识别（`handlers.rs:7176`） | 每个超长请求都要先被上游拒绝一次，延迟翻倍，还白白消耗账号并发 | 强制模式下**新增本地预检**。用现有的输入 token 估算（与 count_tokens 是同一套），超出预算就直接从 L1 开始退避，不等上游拒绝 |
| payload guard 按字节加权计量（1.3M 加权上限、450KiB），和 token 窗口是两套口径（`payload_guard.rs:36`） | 两套限制都可能先触发 | 两者都保留。token 预算负责模型窗口，payload guard 负责 Kiro 的内容长度阈值。任何一个超限都走同一套退避分级 |
| `contextUsageEvent` 达到 100% 时置 `stop_reason: model_context_window_exceeded`（`stream.rs:2849`） | 客户端会看到外显模型在远未到 1M 时就"窗口已满"，既暴露真实窗口，也会让客户端提前停止 | 强制模式下把这个结束原因改成 `end_turn`；如果是因为输出超限结束的，改成 `max_tokens`。`contextUsageEvent` 换算出的 input_tokens 按第 4 节的口径回报 |
| Claude Code 压缩请求的重试预算用 `get_context_window_size`（不查 catalog，不带 `[1m]` 就按 200K）乘以 90%（`handlers.rs:1489`） | 在目标模型是 200K 的情况下，这个值碰巧正确 | 改成按目标模型的 catalog 取值，不依赖这种巧合 |

## 2. 预算

```
输入预算 = 目标窗口(200K) − 预留输出 − 安全余量
预留输出 = min(客户端 max_tokens, 64K)，同时不低于 8K
安全余量 = 输入预算的 5%（覆盖本地估算与 4.5 tokenizer 之间的偏差）
```

预留输出不够时，先下调 max_tokens（L0），再动输入。

## 3. 退避分级

超出预算时，按顺序逐级执行，直到放得下为止。每一级都必须是**确定性**的：同样的输入得到同样的输出，这样前缀才稳定，缓存才能命中。

| 级别 | 动作 | 会损失什么 |
|---|---|---|
| L0 | 把 max_tokens 和思考预算降到剩余空间以内 | 无 |
| L1 | 去掉最近一轮之前所有 assistant 的 thinking 文本（签名照常保留）；历史图片按目标长边压缩并去重；去掉重复的 system-reminder | 几乎没有 |
| L2 | 清理旧的工具结果：保留最近 K 个 tool_result 原文（默认 K=8），更早的改成中性占位，例如 `[output omitted]`。tool_use / tool_result 的配对不变 | 旧工具输出的细节 |
| L3 | 锚定摘要：用目标模型旁路请求一次，摘要最早的一段历史，替换成一条 user 摘要块 | 早期细节 |
| L4 | 硬裁剪：从最早的轮次开始丢弃。复用现有 `trim_retry` 的配对保护，以及"保留首条用户意图" | 早期对话 |
| L5 | 单个块过大（一个文件或一个工具结果本身就超出预算）：对最大的块做中间截断，保留头尾 | 该块中段内容 |

### 缓存稳定性（L2 / L3 的关键）

- 切分边界按**固定步长**推进。例如每当历史再增长 50K 才挪一次边界，而不是每轮都重新切。两次挪动之间，连续的请求共享同一个处理后的前缀，prompt cache 照常命中。
- L3 的摘要按 `会话标识 + 被摘要段的内容哈希` 缓存，复用现有 Redis/Postgres 存储，命中后不再发起旁路请求。
- 旁路摘要请求本身不计入用户 usage，计入内部成本。它会占用账号并发，需要沿用现有调度准入。

### 上游仍然拒绝

本地估算和上游 tokenizer 之间有偏差，上游仍可能返回输入过长。处理方式：

1. 复用现有 `too_long_retry` 路径（`local_body_pipeline.rs:393`），内部升一级退避后重试，最多两次。
2. 两次之后直接按 L4/L5 强制裁剪，保证请求能发出去。
3. **对外永远不返回 `prompt is too long`。**

现有的 `payloadTooLongHandling=client_compaction` 在强制模式下不生效。不管配置是什么，都按本节的服务端退避处理。

### Claude Code 自己的压缩请求

Claude Code 发起 `/compact` 或自动压缩时，请求里带的是完整历史，最长可能接近 1M。

- 现有识别逻辑：根据 "create a detailed summary of…" 这段文案判断（`handlers.rs:1456`），识别后按 payload guard 裁剪再重试。
- 强制模式下这类请求同样走 L1–L4，保证压缩本身能完成。
- 摘要质量取决于保留下来的内容。L2 要优先保留用户消息和 assistant 的文字结论，丢弃旧的工具输出。

## 4. usage 口径与客户端压缩的联动

Claude Code 是否自动压缩，依据的是响应 usage 里的 input + cache_read + cache_creation 总量。这里有两种口径：

| 口径 | 结果 |
|---|---|
| 报**上游实际**输入（≤200K） | 客户端永远看不到接近 1M，**永远不会自动压缩**。请求体无限增长，每一轮都要做 L3/L4，越到后面信息损失越大，单个请求的体积也越来越大 |
| 报**客户端发送**的输入总量（退避前的本地估算） | 客户端在它认为的约 1M 处正常压缩，行为和真模型一致。退避只在 200K 到 1M 之间生效 |

**推荐第二种口径。** 具体做法：

- usage 的输入类字段按"退避前的完整请求"估算后回报；输出类字段按实际回报。
- cache_read 与 cache_creation 的拆分沿用路由挂载的现有 usage 整形策略（参见现有整形策略按路由描述 usage 的约定）。整形的上下限仍是最终硬约束。
- 这个口径会影响下游计费：下游看到的 input token 比上游实际处理的多。是否这样做，以及怎么计价，列入第 06 篇的待决策项。

## 5. 配置

```jsonc
"contextBackoff": {
  "targetWindowTokens": null,      // null = 取目标模型目录里的 maxInputTokens
  "safetyMarginRatio": 0.05,
  "minReservedOutputTokens": 8000,
  "keepRecentToolResults": 8,
  "summaryEnabled": false,  // L3（P6 实现）；关闭时 L2 之后直接进入 L4
  "summaryStepTokens": 50000,
  "reportClientInputUsage": true,   // 第 4 节的口径
  "localPrecheck": true   // 本地 token 预检，超出预算时直接开始退避
}
```

## 6. 风险

- L3/L4 会丢失早期上下文。长任务后期可能"忘记"早期约定。可以缓解，但无法根除。L3 摘要要明确保留用户的原始目标和已做出的决策。
- 单轮请求就超过 200K（例如一次性粘贴大文件）时，只能走 L5 截断，模型看到的是不完整的内容。
- 估算偏差：本地估算偏小会触发上游拒绝，然后走重试；偏大则会过早退避。上线前需要用真实流量校准安全余量。
