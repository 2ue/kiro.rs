# Round 41：thinking start 与完整 SSE 抓包核验

## 运行信息

- 时间：2026-11-07（北京时间 11:17–11:20）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 开发二进制：`/tmp/kirotest-dev/kiro-rs-cctest-round40-thinking-start`
- SHA-256：`db4dfe587b72cb84cd6f223044a0b3651cb42c2ce6db938f52eec14f8a30b061`
- 59138：只读，未重启、未改配置、未写 PostgreSQL/Redis

## 结果

结果文件：`results/20261107T031710-round-41-capture-thinking.json`

| 项目 | 分数 |
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
| 总分 | 73/110 |

## 抓包证据

完整抓包：`logs/llm-capture/capture-20261107T031657Z-b2493f4f/events.jsonl`；同时保留 `manifest.json` 与 `capture.zip`。

- 9,593 个事件，8,917,855 字节；JSON 解析错误 0、丢弃事件 0。
- 24 个 HTTP ingress、23 个客户端请求、26 个上游请求；上游状态全部为 HTTP 200。
- 23 个客户端流各有一次 `message_start`、`message_delta`、`message_stop`；53 个 block start 和 53 个 block stop，index 连续。
- thinking、tool_use、WebSearch 的对应 delta 均存在；WebSearch 有 `caller={"type":"direct"}`、结果及 citations。
- 所有请求完成规范终止；公开事件生命周期完整没有改变四个失败分项。

抓包摘要由 `scripts/analyze_capture.py` 生成。逐请求检查排除了 message 生命周期缺失、block index 不连续、thinking 缺少 signature delta、tool 输入 delta 缺失和上游 HTTP 错误作为本轮评分差异的直接解释。

## 签名对照

逐请求对比上游 `reasoningContentEvent.signature` 与客户端 `signature_delta`：8/8 SHA-256 完全一致，前缀均为 `CAQS`，未发现 synthetic signature 或代理侧重编码。由于模型默认隐藏思考，客户端 thinking 文本为空；这是 omitted 显示路径的预期形状。尽管如此 `signature_proto` 仍为 5。

## 结论

Round 40 的 thinking start 修正保留为公开流兼容改进，但不能解释 `stream_structure=5`。不继续随机修改公开 thinking 字段；后续转查抽样语义、历史回放及私有签名验证边界。
