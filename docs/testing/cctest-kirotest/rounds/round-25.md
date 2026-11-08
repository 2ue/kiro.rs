# Round 25：thinking_delta 显式 null 实验

时间：2026-11-06（UTC 18:51–18:56；北京时间 02:51–02:56）

## 环境

- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端仅为开发实例 59139。
- 模型：`claude-opus-5-5`。
- 代码工作区：`2e625e52d2b2225c487d6ff027ce29110f997511`。
- 开发二进制：`/tmp/kirotest-dev/kiro-rs-cctest-null-estimate`，SHA-256 `eb8040aa2b9f24810045a67bd4067fb340dd6f71bff1ce359b44acd853c2ff1b`。
- 实验变量：beta 请求的隐藏思考空 delta 在没有数值估算时增加 `estimated_tokens: null`；普通空 delta 保留。
- 抓包：`logs/llm-capture/capture-20261106T185102Z-3d6f7251/events.jsonl`。
- 抓包统计：7161 行、7578190 字节、25 个上游响应，全部 HTTP 200；`client_sse` 3322 个，含 22 个完整 message 流。

59138 全程只读，未重启、未改配置、未写 PostgreSQL/Redis。Caddy/Cloudflare 链路正常。

## 结果

结果文件：`results/20261106T185313-round-25-null-estimate.json`

总分仍为 `73/110`：

| 检测项 | 得分 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 0 |
| server_tool | 10 |
| knowledge / model_attribution / doc_recognition / image_recognition | 10 |

## 抓包证据

- 隐藏 thinking block 的公开结构仍为 `content_block_start(thinking, signature="")`、空 `thinking_delta`、`signature_delta`、`content_block_stop`。
- 带 token-count beta 的请求中，空 delta 出现了 `estimated_tokens: null`、无该字段、以及 `estimated_tokens: 50` 三种形状。
- 普通 local 请求和 1 个 websearch 请求均完成；没有上游 4xx/5xx，故本轮分数变化不能归因于账号池或 Caddy。

## 结论

显式 `null` 没有改变 `stream_structure`、`signature_proto` 或总分。`estimated_tokens` 字段形状仍是公开协议差异候选，但不是已证实的唯一根因。保留该实验开关，下一轮继续区分由 thinking 过滤器产生的 delta 与 Claude Code noop keepalive 产生的空 delta。

## 资源

检测期间可用内存约 9.9 GiB，负载约 0.60，开发进程 RSS 约 216 MiB，未触发暂停阈值。
