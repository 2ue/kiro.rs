# Round 54：结构化 guidance 第四轮复测与请求级矩阵

## 运行信息

- 时间：2026-11-07（北京时间 18:53–18:55；UTC 10:53–10:55）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 代码：与 Round 51–53 相同；本轮没有新增源码修改，由 debug 开发循环管理 59139
- 结果：[`20261107T105305-round-54-guidance-recheck.json`](../results/20261107T105305-round-54-guidance-recheck.json)
- 抓包：`logs/llm-capture/capture-20261107T105246Z-2da55d1a/`，`includeSecrets=false`
- 脱敏矩阵：[`analysis/round-54-request-matrix.json`](../analysis/round-54-request-matrix.json)
- 矩阵生成器：[`scripts/analyze_round_matrix.py`](../scripts/analyze_round_matrix.py)

59138 仅执行只读 `/healthz` 探针，没有停止、重启、配置写入、PostgreSQL/Redis 写入或账号状态修改。59139 继续由 debug 热更新循环管理；本轮没有 release 编译。

## 结果

总分 `82/110`，与 Round 51、Round 52、Round 53 完全一致：

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

结果元数据为 `streamChannel=aws-bedrock`、`verdictKey=unofficial`、`expectedModel=responseModel=claude-opus-5-5`。这与历史开发轮次的 `aws-bedrock + signature_proto=5` 组合一致；公开结果没有给出该通道归因是否导致 5 分上限的判分代码，因此只记录为高相关候选，不把它当作已证实根因。

## 抓包统计

抓包文件：`logs/llm-capture/capture-20261107T105246Z-2da55d1a/events.jsonl`

- 8,309,543 字节，8,938 行，JSON 错误 0，丢弃事件 0
- 22 个客户端请求、25 个上游请求；上游 HTTP `200=25`
- 22 个客户端流均有 `message_start/message_delta/message_stop`
- 客户端 SSE：`message_start=22`、`content_block_start=47`、`content_block_delta=4083`、`content_block_stop=47`、`message_delta=22`、`message_stop=22`、`ping=2`
- 上游 `upstream_stream_end=21`；剩余 4 个上游请求属于非流响应或 WebSearch 内部 MCP/模型回合，不应把数量差直接解释为客户端缺失终止帧
- WebSearch 对外序列包含 `server_tool_use`、`input_json_delta`、`web_search_tool_result`、`citations_delta`、`text_delta`、`message_delta(end_turn)` 和 `message_stop`；server tool 与结果的 ID 匹配，两个 block 均携带 `caller={"type":"direct"}`

事件计数来自 `analyze_capture.py`；请求分类和字段矩阵来自 `analyze_round_matrix.py`。矩阵结果为 22 个请求：7 个 `plain_stream`、6 个 `tool_continuation`、5 个 `thinking_tool`、2 个 `thinking_text`、1 个 `server_tool`、1 个 `non_stream`。这样可以把“客户端流完整”和“上游没有显式 COMPLETED”分开记录，避免把不同协议层的信号混为一谈。

## 请求级矩阵

矩阵只保留请求 ID、事件类型、布尔状态和计数，不复制正文、请求头、凭证或对话文本。22 个请求分类如下：

| 请求分类 | 数量 | 关键特征 |
|---|---:|---|
| `plain_stream` | 7 | 流式纯文本，无 thinking、无工具 |
| `tool_continuation` | 6 | 有历史 thinking/tool，当前返回 client `tool_use` |
| `thinking_tool` | 5 | 当前同时有 thinking、signature 和 client tool_use |
| `thinking_text` | 2 | 当前有 thinking、signature 和 text |
| `server_tool` | 1 | `bash_code_execution` server tool 及结果 |
| `non_stream` | 1 | 非流 `1+1=?` 请求，单一 text block |

含 thinking 的 7 个流请求全部收到一个下游 `signature_delta`，并且对应上游为原生 reasoning 事件。其 `estimated_tokens` 形状在本轮仍有三类：数值、`null`、字段缺失；字段缺失仅出现在 Claude Code noop keepalive 形态。历史 Round 25–28 已分别改变这些字段形状而没有改变 `stream_structure`，所以本轮不据此修改代码。

矩阵还显示：

- 22 个客户端请求全部 `status=success`、`routeSubtype=local_success`；
- 流式请求全部 `client_lifecycle_complete=true`；
- 大多数上游流为 `upstreamEofWithoutCompleted=true`、`sawUpstreamCompleted=false`，随后由本地依据内容推断 `local_inferred_end_turn` 或 `local_inferred_tool_use`，最终 `terminalReason=completed`；
- 含 thinking 的请求分别覆盖纯文本和工具续接，不能把 EOF 差异归因到单一请求类型；
- WebSearch 请求的私有矩阵维度仍只有单次搜索，抓包中没有第二轮把 `encrypted_content` 或 `encrypted_index` 原样回传给上游的证据。

## 结论

1. 条件式 identity/platform guidance 在同一代码上第四轮得到 `output_config=10`，该改动继续保留。
2. `stream_structure=5` 仍没有公开事件缺失证据：生命周期、block 配对、thinking signature 和工具终止事件均完整。EOF 无 `COMPLETED` 是上游 Kiro 的实际行为，本地已有推断状态；在没有 CCTest 抽样请求 ID 或私有判分错误前，不改完成状态机。
3. `signature_proto=5` 与 `streamChannel=aws-bedrock` 在历史轮次持续共现，但当前真实 Kiro signature 已逐字节透传，不能据此删除、重编码或伪造 signature。
4. `websearch=0` 仍无法由公开 SSE 形状解释。当前字段 `encrypted_content` / `encrypted_index` 的生成没有官方公开算法，也没有真实官方 fixture；本轮不改 opaque 字段。
5. 本轮没有出现 59139 服务异常或账号池耗尽；59138 健康探针保持 200。

## 版本与隔离核对

- 59138 生产容器仍为 `0.0.239`，OCI revision `30edc00c12431371e22498298715adc7bea539fd`；它不是当前仓库 `HEAD=v0.0.240`，本轮没有对其升级或重启。
- 59139 使用当前工作树的 debug 开发二进制 `/root/.cache/kirotest-dev/kiro-rs-dev`，监听 `0.0.0.0:59139`。
- 59139 与 59138 的 `/healthz` 均为 HTTP 200。
- 抓包和结果全部保留；代码未提交、未推送。

## 资源记录

- 沿用开发循环门禁：可用内存不少于 4 GiB、可用 swap 不少于 8 GiB、1 分钟 load 小于 8；恢复阈值分别为 6 GiB、10 GiB、load 小于 6。
- 本轮未触发暂停；没有执行 release 编译、`clippy`、`test` 或最终门禁。
