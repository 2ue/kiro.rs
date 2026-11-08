# Round 53：结构化 identity guidance 第三轮复测

## 运行信息

- 时间：2026-11-07（北京时间 18:15–18:18；UTC 10:15–10:18）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 代码：与 Round 51、Round 52 相同；没有新增源码修改，由 debug 开发循环管理 59139
- 结果：[`20261107T101524-round-53-guidance-recheck.json`](../results/20261107T101524-round-53-guidance-recheck.json)
- 抓包：`capture-20261107T101523Z-d8aa9f7d`，`includeSecrets=false`
- 59138：仅执行只读 `/healthz` 探针；未停止、未重启、未改配置、未写 PostgreSQL/Redis

## 结果

总分 `82/110`，与 Round 51、Round 52 完全一致：

| 检测项 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 10 |
| server_tool | 10 |
| knowledge | 10 |
| model_attribution | 10 |
| doc_recognition | 10 |
| image_recognition | 10 |

## 抓包证据

抓包文件：`logs/llm-capture/capture-20261107T101523Z-d8aa9f7d/events.jsonl`

- 10,108 行，8,813,674 字节，JSON 错误 0，丢弃事件 0
- 22 个客户端请求、25 个上游请求；上游 HTTP `200=25`
- 22 个客户端流均有完整 `message_start/message_delta/message_stop`
- `content_block_start/stop=51/51`
- WebSearch 客户端 SSE 35 个，MCP 响应 1 个
- 结构化输出样本返回合法 schema JSON，`identity_platform=claude_code`
- 上游有 21 个 `upstream_stream_end`，没有对应缺失的客户端终止帧；其余上游请求通过普通响应路径完成

本轮新增观察：含隐藏 thinking 的请求中，部分 `thinking_delta` 带有 `estimated_tokens=null` 或没有该字段，另一些带量化数值。缺失字段主要出现在 Claude Code noop keepalive 路径；`OmittedThinkingFilter` 生成的字段仍按 beta 逻辑处理。Round 25–28 已对 `estimated_tokens` 的数值、`null` 和 keepalive 补齐做过单变量实验，评分均未改变，因此本轮不据此修改状态机。

## 对比与结论

Round 53 在同一代码上第三次复现 `output_config=10`，说明条件式 identity/platform guidance 目前稳定保留。保护项继续全部通过。`stream_structure=5`、`websearch=0`、`signature_proto=5` 没有变化；本轮没有发现公开 SSE 生命周期缺失、上游 HTTP 错误或代理改写签名的证据。

当前不能把 `estimated_tokens` 的缺失直接认定为流结构评分根因，也不能在没有 verifier fixture 的情况下伪造 signature 或 WebSearch 的 opaque 字段。下一轮先继续同代码复测，若仍为 `82/110`，再按请求级矩阵拆分抽样条件、hidden thinking keepalive、upstream EOF/completed 信号以及多轮 WebSearch 历史回传。

## 资源与隔离

- 开发实例使用 debug 增量编译和自动重启，不执行 release 冻结编译
- 59139 监听 `0.0.0.0:59139`，`/healthz=200`
- 59138 监听 `0.0.0.0:59138`，`/healthz=200`
- 编译门禁：可用内存不少于 4 GiB、可用 swap 不少于 8 GiB、1 分钟 load 小于 8；本轮未触发暂停
- 抓包和结果文件全部保留；未提交、未推送
