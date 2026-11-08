# Round 95：完整抓包复测仍为 82/110

## 时间与边界

- 时间：2026-11-08 12:23–12:27（北京时间；结果和抓包事件为 UTC 04:23–04:27）。
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139；模型 `claude-opus-5-5`；并发 1。
- 代码：仓库 `HEAD=2e625e52`、`v0.0.240`；本轮没有源码修改，59139 继续由 debug 增量开发循环管理。
- 59138 只读：Docker 容器 `kiro-rs-2ue-59138-app` 仍为 `ghcr.io/2ue/kiro-rs:latest`，OCI revision `30edc00c12431371e22498298715adc7bea539fd`，版本 `0.0.239`；没有重启、配置写入、数据库/Redis 写入或账号状态修改。
- 结果：[`20261108T042355-round-95-capture.json`](../results/20261108T042355-round-95-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T042226Z-cb0abf9a/`。
- 请求矩阵：[`analysis/round-95-request-matrix.json`](../analysis/round-95-request-matrix.json)。

## 结果

| 检测项 | Round 94 | Round 95 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 10 | 10 |
| server_tool | 10 | 10 |
| 其它保护项 | 40 | 40 |
| **总分** | **82/110** | **82/110** |

本轮没有新增评分回归，也没有任何失败请求导致 CCTest 中断。当前最高分仍为 `82/110`，连续稳定区间仍为 `73–82/110`。

## 抓包完整性与资源门禁

- `events.jsonl`：9,981 行，8,750,287 字节；JSON 错误 0；`droppedEvents=0`。
- 22 个客户端入口请求、22 个客户端流、25 个上游请求；25 个上游响应均为 HTTP 200。
- 22/22 客户端流完整；`client_stream_end` 22 次；MCP 响应 1 次。
- 上游流结束事件 21 次，另有 1 个非流响应；公开下游生命周期均闭合。
- 开始抓包前可用内存约 7.1 GiB，Swap 可用约 11 GiB，系统负载约 0.65；开发循环未触发暂停阈值。

文件哈希：

```text
events.jsonl  e31b53ca276664b4b0667094a7e117f0773b0943ba867c09404a5dbb905b03ff
capture.zip   2db8ca1ecad82f730095425312bacb9f6c61c86e7a93e6660dabb778215ba70a
manifest.json a43a629dce8ab54ba58732f6bd7a3e9033191333c2cc1c8c0232bb0553238863
```

唯一 `events.jsonl`、`capture.zip`、`manifest.json` 均保留，没有清理抓包源文件。

## 请求级证据

本轮 22 个请求矩阵中：

- 含 reasoning 的 thinking 流均保留真实 `signature_delta`；上游 `reasoningContentEvent` 和下游 `thinking` block 成对出现。
- 多个 thinking/tool 请求在上游出现 `reasoningContentEvent + toolUseEvent`，下游对应 `thinking + tool_use` block；tool continuation 的输入块也闭合。
- 普通流有两类：上游只有 `assistantResponseEvent`，或上游包含 reasoning 但下游按 `thinking` 显示策略只下发 text；这两类都完成 `message_stop`。
- 上游多数在 EOF 前没有显式 `COMPLETED`，代理以 `local_inferred_end_turn` 或 `local_inferred_tool_use` 终止；本轮未出现公开 SSE 缺帧、重复 block、最终 `stream_error` 或 502。
- 本轮非流请求 `req_01awNUXuexqdpedRrfouid1q` 上游只有 3 个 `assistantResponseEvent`，没有 `reasoningContentEvent`，请求矩阵没有下游 block 条目，CCTest `non_stream=10`。没有新的“reasoning + 空 thinking block + non_stream=0”样本。

WebSearch 的 API/MCP/回灌仍然返回成功，公开 tool 生命周期没有新增差异；`websearch=0` 与前轮一致。真实 Kiro signature 仍原样透传，`signature_proto=5` 与 `stream_structure=5` 未变化。

## 结论

Round 95 是当前配置和代码的完整抓包稳定复测。它再次排除账号 HTTP 错误、客户端流断开、MCP 失败和明显 SSE 生命周期缺陷作为本轮评分差异的充分根因。三个长期失败项仍是私有评分/opaque 语义/EOF 与采样组合候选：

1. `stream_structure=5`：公开流闭合，但多数上游 EOF 无 `COMPLETED`，尚无评分失败请求 ID，暂不伪造完成事件。
2. `websearch=0`：公开 API/MCP/回灌闭环完整，仍缺官方 `encrypted_content` / `encrypted_index` fixture 和第二轮原样回传证据，暂不随机生成 opaque 值。
3. `signature_proto=5`：真实 Kiro opaque signature 非空、未改写；没有 verifier fixture 或失败请求 ID，暂不删除或重签名。

继续保留 `thinkingTrigger=off`、开发模式热更新和原始抓包；尚未满足连续三轮 `110/110`，不执行 release 冻结编译。
