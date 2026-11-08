# Round 52：结构化 identity guidance 同代码复测

## 运行信息

- 时间：2026-11-07（北京时间 17:38–17:41；UTC 09:38–09:41）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 代码：与 Round 51 相同；无源码修改，使用现有 debug 自动热更新进程
- 抓包：`capture-20261107T093736Z-0dd4aa47`，15 分钟上限，`includeSecrets=false`
- 59138：只读 `/healthz` 探针；未停止、未重启、未改配置、未写 PostgreSQL/Redis

## 结果

结果文件：[`20261107T093827-round-52-guidance-recheck.json`](../results/20261107T093827-round-52-guidance-recheck.json)

总分 `82/110`，与 Round 51 完全一致：

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

抓包文件：`logs/llm-capture/capture-20261107T093736Z-0dd4aa47/events.jsonl`

- 8,405,507 字节，9,112 行，JSON 错误 0，丢弃 0
- 22 个客户端请求、25 个上游请求，HTTP `200=25`
- 22 个客户端流均有 `message_start/message_delta/message_stop`
- `content_block_start/stop=52/52`；公开 message 生命周期完整
- WebSearch 路由 35 个客户端 SSE 事件；内部 MCP 响应 1 个
- 结构化请求 `req_01QAF9V59PcAg8eZxr12yXdM` 返回合法 schema JSON，`identity_platform=claude_code`，本轮 `output_config=10`

请求级关联使用脱敏脚本 `scripts/analyze_output_config_samples.py`。完整配对总数由此前 38 增至 44；`claude_code` 为 10/10 得分，`kiro` 为 34/34 失分。此关系在两个同代码完整轮次中持续成立，但仍是评分关联证据，不代表私有评分器的完整规则已经公开。

## 结论

Round 51 的条件式 identity/platform guidance 在同一代码上再次得到 `output_config=10`，因此该改动已通过两轮完整复测，保留在工作树。保护项均保持 10 分。剩余 `stream_structure=5`、`websearch=0`、`signature_proto=5` 没有变化；本轮未发现公开 SSE 生命周期缺失或上游 HTTP 错误。

## 资源与隔离

- 59138、59139 `/healthz` 均为 HTTP 200
- 使用现有 debug 开发实例，无 release 冻结编译
- 抓包和结果全部保留；未提交、未推送
