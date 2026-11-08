# Round 10：去掉 server tool 重复 input delta（未完成评分）

时间：2026-11-06（北京时间）

## 变更

`src/anthropic/handlers/model_driven_websearch.rs` 保留 `server_tool_use` 的完整 `input` 在首个 `content_block_start`，移除该 block 后续重复的 `input_json_delta`；普通客户端 `tool_use` 仍发送 `input_json_delta`。

这轮使用开发模式 `cargo run`，没有 release 编译；59138 未重启、未改配置或写生产数据库/Redis。

## 结果状态

结果文件：`results/20261106T122556-round-10-server-tool-lifecycle.json`。

检测在 WebSearch 步骤收到 HTTP 502，状态为 `error/resumable`，只有前三项已评分：`tag_check=10`、`stream_structure=5`、`non_stream=10`。本轮不计入满分序列，也不能据此判断事件改动有效或无效。

## 抓包证据

抓包：`logs/llm-capture/capture-20261106T122544Z-dc32fb4f/events.jsonl`，约 4.7 MiB，2344 行，全部保留。

失败的 WebSearch 内部请求 `req_01YV63ZJtuqRxE36sicFh6fT` 与 `req_01W9cGCTLPsCTTREdrNNJg56` 均显示 Kiro HTTP 200 header，随后代理返回 `api_error: websearch_model_call_failed`、HTTP 502，未产生完整对外 WebSearch block。另一个同轮请求有完整 200 流。请求 summary 将失败标为 `local_error_no_fallback`，因此该轮首先暴露的是上游 eventstream 完整读取/解析或账号池波动，尚未到 cctest WebSearch 字段判分。

同轮抓包记录一个账号 402 月度额度耗尽并禁用、两个账号 403 暂停，属于共享 Kiro 账号池的上游状态变化。开发使用独立 PostgreSQL/Redis 容器，不影响生产存储；生产 59138 进程未操作。

## 结论

Round 10 是环境/上游失败轮，不改变 WebSearch 根因判断。重复 `input_json_delta` 的代码修复保留，待上游账号池稳定后重新跑完整检测；`caller` 字段仍是下一轮最小协议实验候选。
