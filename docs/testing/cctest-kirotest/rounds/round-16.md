# Round 16：清理失效账号后的完整检测

时间：2026-11-06（UTC 14:47–14:51；北京时间 22:47–22:51）

## 目标与边界

- 明确目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端为开发实例 59139。
- 脚本使用显式 `--target-base https://kirotest.okmcode.com`；仓库外 `.env.local` 的默认 `CCTEST_TARGET_BASE` 仍指向 `kiro2.okmcode.com`，已在脚本加入主机保护，拒绝非 `kirotest.okmcode.com` 目标。
- 59138 只做 `/v1/models` 只读探针和生产库只读核对；没有重启、配置修改、Admin API 写入、PostgreSQL/Redis 写入。
- 开发实例继续使用开发模式，独立 PostgreSQL 25432、Redis 26379。

## 账号池处理

- Round 14/15 的首项 502 均由开发池失效账号耗尽重试造成：Round 14 抓包 `logs/llm-capture/capture-20261106T143958Z-ca82c634/events.jsonl` 有 60 次上游响应，402=56、403=4，6 个请求全部最终 502；Round 15 抓包 `logs/llm-capture/capture-20261106T144504Z-4c1f093e/events.jsonl` 有 60 次上游响应，402=60，6 个请求全部最终 502。
- 只在 59139 独立数据库中软删除这些抓包明确返回 402/403 且已被应用标记 `disabled=true` 的账号：Round 14 清理 5 个，Round 15 清理 35/36 个，Round 16 再清理 38 个。生产 59138 的账号状态未改。
- Round 16 开始时开发库统计：161 条未删除，151 条手工/自动禁用，10 条可选择；这些状态仍可能随共享账号池刷新变化。

## cctest 结果

结果文件：`results/20261106T144740-round-16-account-pruned.json`

总分 **73/110**，与 Round 13 相同：

| 检测项 | 得分 |
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

保护项保持通过；账号清理解决了早期 502 中断，但没有改变四个协议项的分数。Token audit 完成，`requests=27`、`local=27`、`credits=11.0571`；原始响应总成本与 cctest 基线比值为 0.73，未出现 audit anomaly。

## 抓包证据

- 脱敏采集：`logs/llm-capture/capture-20261106T144740Z-ffe86528/events.jsonl`，14,985,357 字节、9,692 行、丢弃 0；归档 ZIP 约 3.84 MiB。
- 事件统计：`client_request=26`、`request_summary=27`、`upstream_request=82`、`upstream_response=82`、`upstream_frame=4808`、`client_sse=4507`、`mcp_response_body=1`。
- 上游状态：200=25、402=57；没有 403。4 个请求最终 `api_error`/502，8 个客户端响应记录为 502（同一请求的响应/egress 双记录）；其余 23 个请求成功。
- 失败请求的每次尝试均为 402 `MONTHLY_REQUEST_COUNT`，每个请求最多 10 次；成功请求在换到可用账号后有完整 SSE。
- WebSearch 请求级结构仍包含 `server_tool_use`、`web_search_tool_result`、`caller={"type":"direct"}`、10 个结果、10 个 citation、`message_delta(end_turn)`、`message_stop`；因此 0 分没有被账号池完全解释。

## 结论

1. `kirotest` Caddy/Cloudflare 链路正常；本轮已通过 tag、stream、non-stream 等全部步骤。
2. 账号池问题是独立的环境阻断因素，必须在每轮前检查并在开发库剔除明确失效账号；不能修改 59138。
3. 协议问题仍集中在 `stream_structure=5`、`signature_proto=5`、`websearch=0`、`output_config=0`。下一轮继续按请求级字段做单变量实验，不把账号失败误判为协议根因。

## 资源

- 运行期间 `MemAvailable` 约 9.28–9.52 GiB，`SwapFree` 约 13.20–13.26 GiB，1 分钟 load 最高约 1.19；没有触发暂停阈值。
- 59138 `/v1/models` 只读探针持续返回 401，未触发生产保护暂停。
