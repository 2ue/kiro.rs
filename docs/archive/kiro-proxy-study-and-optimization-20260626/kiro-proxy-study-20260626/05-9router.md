# 项目分析：`9router`

路径：`/Users/yuanfeijie/Desktop/procode/9router`  
最新本地提交：`d1e98d9`，2026-06-26  
相关度：中高

`9router` 不是单纯 Kiro 代理，而是多 provider、多协议、多客户端路由系统。它不适合当前项目整体照搬，但它在 provider registry、thinking 统一化、Kiro executor endpoint fallback、tool history 400 防护、stream 终止防挂方面有参考价值。

## 关键文件

| 文件 | 作用 |
| --- | --- |
| `open-sse/providers/registry/kiro.js` | Kiro provider registry |
| `open-sse/executors/kiro.js` | Kiro executor、AWS EventStream 到 SSE |
| `open-sse/config/kiroConstants.js` | Kiro model suffix、thinking prompt、profileArn 默认值 |
| `open-sse/translator/request/openai-to-kiro.js` | OpenAI 请求转 Kiro |
| `open-sse/translator/request/claude-to-kiro.js` | Claude 请求直转 Kiro |
| `open-sse/translator/response/kiro-to-openai.js` | Kiro 响应转 OpenAI |
| `open-sse/translator/response/kiro-to-claude.js` | Kiro 响应转 Claude |
| `open-sse/translator/concerns/thinkingUnified.js` | 统一 thinking intent |
| `open-sse/handlers/chatCore.js`、`chatCore/*` | provider 执行、stream/non-stream、fallback |
| `open-sse/utils/streamHandler.js` | stream handling |
| `docs/ARCHITECTURE.md` | 多 provider 架构说明 |

## 多 provider registry

`9router` 用 registry 管理 provider、model、capability、pricing、endpoint、translator。这个设计对它必要，因为它同时接 OpenAI、Anthropic、Gemini、Kiro、Codex、Qwen 等。

当前项目是 Kiro 专项代理，不应该照搬一个庞大的 provider registry。但可以学习：

- model capability 不要散落在多个转换函数。
- thinking/tool/image/cache 支持矩阵应该有单一来源。
- route policy、model alias、model capability 可以拆出独立模块。

当前项目后续可做小型化版本：

- `KiroModelCapability`
- `ClientCompatibilityProfile`
- `RoutePolicy`

不要引入 `9router` 那种 100+ provider registry 复杂度。

## Kiro executor

`open-sse/executors/kiro.js`：

- 根据 auth method 调整 endpoint 顺序。
- API key auth 优先 `amazonaws.com` CodeWhisperer host。
- OAuth 默认 `kiro.dev` / Kiro path 优先。
- BaseExecutor 负责 endpoint fallback 和 retry。
- 成功后把 AWS EventStream 转 OpenAI SSE。
- `reasoningContentEvent` 转成 OpenAI `delta.reasoning_content`。
- toolUseEvent 转 OpenAI `tool_calls`。

当前项目是 Anthropic 兼容为主，不必转 OpenAI SSE。但可学习：

- Kiro upstream event 转内部统一事件，再按目标协议输出。
- endpoint fallback 必须 auth-aware。
- `reasoningContentEvent` 不能丢，要进入 thinking/reasoning 通道。
- tool call id 需要 `seenToolIds` map，避免同一个 tool id 重复开 block。

## Thinking 统一化

`thinkingUnified.js` 做了一层统一 thinking intent：

- Claude `output_config.effort`
- Claude `thinking.type/budget_tokens`
- OpenAI `reasoning_effort`
- OpenAI Responses `reasoning.effort`
- Gemini `thinkingConfig`
- Qwen `enable_thinking`
- model suffix `model(high)` / `model(8192)` / `model(auto)` / `model(none)`

然后再按 provider native format 输出。

当前项目最近已经做 think 支持，重点要确保：

- 传入 thinking 模型不能 alias 到普通模型。
- thinking intent 不能在 model alias 阶段被丢。
- 输出必须有真实 thinking/reasoning stream。

可以学习 `9router` 的“先抽象 intent，再应用 provider format”的方式，但当前项目只需要 Kiro/Anthropic 子集：

- `ThinkingIntent::Disabled`
- `ThinkingIntent::Adaptive`
- `ThinkingIntent::Budget(u32)`
- `ThinkingIntent::Effort(level)`
- `ThinkingIntent::ModelVariant`

然后再映射 Kiro：

- model variant 保留到 upstream。
- 或注入 Kiro 支持的 `<thinking_mode>` / request body 结构。
- 输出统一到 Anthropic thinking blocks。

## Kiro thinking prompt

`kiroConstants.js` 使用：

```xml
<thinking_mode>enabled</thinking_mode>
<max_thinking_length>...</max_thinking_length>
```

这和当前项目近期的 think 实现方向相关。需要注意：

- 这是 prompt injection，不是 Kiro 官方公开协议字段。
- 它可能对模型行为有影响。
- 如果 Kiro 已经通过模型名或 reasoningContentEvent 原生支持 think，优先保留原生模型，不要 alias 到普通模型。

当前项目应该把“model variant thinking”和“prompt trigger thinking”分开配置，不要混成一件事。

## Tool history 400 防护

`openai-to-kiro.js` 和 `claude-to-kiro.js` 都写了两个重要防护：

1. 客户端没有传 tools 时，把历史里的 tool_use/tool_result flatten 成普通文本。
2. 客户端传了 tools 时，把孤立 tool_result fold 回 user text，避免 dangling structured reference 触发 Kiro 400。

这和当前项目之前遇到的 `Invalid tool use format` 完全相关。

当前项目已有：

- `src/anthropic/converter.rs` 的 tool pairing。
- `src/anthropic/payload_guard.rs` 的孤立/重复/空 tool 修复。

但建议把 `9router` 的两个策略作为当前项目测试 oracle：

- 无 tools 的 follow-up 请求，历史里有 tool_use/tool_result，最终 Kiro body 不应保留结构化 tool。
- 有 tools 的请求，孤立 tool_result 应变成文本，而不是直接删掉。
- 不能 fabricate stub tool spec，避免模型发起客户端没准备处理的工具调用。

## Stream 防挂

`9router` changelog 和 stream utils 里多次强调：

- stream stall timeout。
- upstream drops mid-stream 后必须发 terminal event。
- Responses passthrough 必须 emit `[DONE]`。

当前项目 Anthropic SSE 也必须保证：

- upstream 断流不能让客户端无限等。
- client disconnect 要释放账号 lease。
- error event 后必须结束 stream。
- 不要把无 terminal event 的 stream 当 success。

这部分应纳入当前项目 loadtest/chaos test。

## 比当前项目强的地方

- thinking intent 抽象完整。
- 多协议 translator concerns 拆得细。
- Kiro executor 对 endpoint/auth/fallback 的关系描述清楚。
- tool history 400 防护策略非常直接。
- stream 防挂和 terminal event 的意识强。

## 当前项目比它强的地方

- 当前项目专注 Kiro，生产调度能力更强。
- 当前项目 PgSQL/Redis、usage、外部池、错误归一化更强。
- 当前项目不需要引入多 provider registry 的复杂度。
- 当前项目对外 Anthropic envelope/request id 更一致。

## 建议吸收方式

P0：

- 把 `9router` 的 tool history 400 场景转成当前项目测试。
- 把 thinking intent 抽象成当前项目内部结构，避免 alias 阶段丢失。
- stream stall/terminal event 加测试。

P1：

- 小型 capability registry，用于 Kiro model/context/thinking/tool/cache 能力。
- endpoint fallback 做 auth-aware policy。

不建议：

- 不要照搬多 provider registry。
- 不要默认使用 prompt injection 替代真实 thinking 模型。
- 不要把 synthetic `-agentic` 这种行为默认加到当前系统。

