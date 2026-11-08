# Round 46：当前源码开发二进制复测

## 运行信息

- 时间：2026-11-07（北京时间 14:29–14:32；UTC 06:29–06:32）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 工作区 HEAD：`2e625e52d2b2225c487d6ff027ce29110f997511`
- 59139 二进制：`/tmp/kirotest-dev/kiro-rs-current`
- 二进制 SHA-256：`bdbbbd4385e56d73cb036aaafc2205aff079f27bf0b4098f48425ff6f2d24b97`
- 59139 PID：`2152465`；绑定 `0.0.0.0:59139`
- 59138：仅读取健康探针，未停止、未重启、未改配置、未写 PostgreSQL/Redis

## 结果

结果文件：[`20261107T062942-round-46-current-source.json`](../results/20261107T062942-round-46-current-source.json)

总分仍为 `73/110`：

| 检测项 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 0 |
| server_tool | 10 |
| knowledge | 10 |
| model_attribution | 10 |
| doc_recognition | 10 |
| image_recognition | 10 |

## 抓包与公开协议复核

当前源码抓包保留于 `logs/llm-capture/capture-20261107T062826Z-f9a8e460/`，包含 `events.jsonl`、`manifest.json` 和 ZIP。

- 8,241 个事件，8,040,033 字节，JSON 错误 0，丢弃 0。
- 22 个客户端请求、25 个上游请求；上游响应 `200=25`。
- 22 个客户端流均有 `message_start/message_delta/message_stop`；46 个 block start/stop 配对且 index 连续。
- 下游存在 8 个 `signature_delta`，均由真实 Kiro `reasoningContentEvent.signature` 透传；没有代理重编码。
- WebSearch 子请求仍有完整 `server_tool_use → web_search_tool_result → text/citations → message_delta(end_turn) → message_stop` 生命周期。
- 结构化输出仍是单一 text block、合法 JSON、严格满足 schema，但 `identity_platform=kiro`，该项为 0。

本轮与 Round 45 的差异主要是事件数量和模型采样内容；失败项与公开生命周期、上游 HTTP 错误无关。

## 新的结构化输出证据

Round 46 的结构化请求 `req_01JieY5HbF2JcYqHQCiakkZG` 的客户端系统消息包含 `You are Claude Code, Anthropic's official CLI for Claude.`，但 Kiro 上游请求的 `conversationState.history` 只有：

1. 首轮用户消息，内容是合并后的 Claude Code system prompt；
2. 代理追加的 `I will follow these instructions.` assistant 历史。

该请求最终仍返回 `identity_platform=kiro`。因此仅保留 Claude Code system prompt、开启默认 identityPrompt 或修正 JSON/schema 外形，都不足以稳定改变身份字段。生产 59138 的历史样本另有 `<assistant_identity>` 块，不能直接复制到开发实例。

## 本地验证

- 当前源码完整单测：`2930 passed / 0 failed / 18 ignored`。
- 普通非流、普通流、enabled thinking 流、WebSearch、structured output 本机冒烟均 HTTP 200。
- `https://kirotest.okmcode.com/healthz` 返回 HTTP 200。
- `http://127.0.0.1:59138/healthz` 返回 HTTP 200。

## 结论

Round 46 证明当前工作树替换开发二进制没有引入回归，但没有改善四个目标分项。下一轮先做最小化的 structured-output 语义提示实验；不硬编码响应 JSON，不改 59138，不伪造 Kiro signature 或 WebSearch opaque 字段。
