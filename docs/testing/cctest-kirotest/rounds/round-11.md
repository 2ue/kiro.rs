# Round 11：补齐 WebSearch server tool caller（检测被上游空响应中断）

时间：2026-11-06（北京时间；检测 UTC 时间 12:41 左右）

## 变更

- `server_tool_use` 与 `web_search_tool_result` 增加 Anthropic 官方 SDK 定义的 `caller: {"type":"direct"}`。
- `server_tool_use` 的首个 `content_block_start` 带完整 query，不再重复发送 `input_json_delta`。
- 最终连续 text block 合并并保留去重后的 citations。
- 本轮检测未能验证以上字段是否改善 WebSearch 评分：三个内部 WebSearch 模型调用在收到 Kiro HTTP 200 后失败，响应没有产生 EventStream 帧。

## cctest 结果

结果文件：`results/20261106T124108-round-11-server-tool-caller.json`。

检测在 WebSearch 阶段收到 HTTP 502 并以 `error/resumable` 结束，只留下 `tag_check=10`、`stream_structure=5`、`non_stream=10` 等部分结果。它不是完整评分轮，不计入连续满分序列，也不用于判定 caller 字段实验成败。

## 抓包与上游失败阶段

抓包：`logs/llm-capture/capture-20261106T124048Z-2b9d0560/events.jsonl`，1016 个事件、5,987,994 字节，无丢弃事件；ZIP 为 1,906,440 字节，全部保留。

三次失败的内部模型调用 requestId：

- `req_01YaD5qAPTaZwLhYZA1As4Lh`
- `req_018RWrKuqVXpZqWsMtbPJYxs`
- `req_01K6XVF66KeEbYAXT1GsGwwx`

每次都记录到上游 `HTTP 200` 响应头，随后外层返回 `websearch_model_call_failed` / HTTP 502；对应 requestId 没有 `upstream_frame`。抓包当时没有 `upstream_body` 类型事件，因此**无法从这轮原始证据区分空 body、200 JSON 错误体、EventStream 截断或解码失败**。不能把“200 header”当作上游成功完成。

同一抓包汇总上游响应为 `200=27`、`402=16`、`403=3`。部分凭据出现月配额耗尽或临时暂停，说明共享账号池不稳定；但三个 `websearch_model_call_failed` 都在最终选中的凭据返回 200 后发生，账号池状态不能解释这三个响应体/帧缺失，需要下一轮采集 body 证据。

## 观测修复

本轮在 `call_model` 中发现：WebSearch 内部非流式读取路径没有像普通 handler 那样调用 `capture_complete_upstream_body`。因此抓包只能记录响应头和笼统的处理失败，不能记录完整帧，也不能记录无法解码的 body。

已补上 WebSearch 响应体抓包，并在完整 body 解码时识别 EOF 遗留的未解码字节。下一轮应能把空体、JSON body、帧解码错误与截断帧区分开。抓包仅在采集功能显式开启时写入；未开启时不增加完整响应体日志。

## Caddy 链路核验

公网 `https://kirotest.okmcode.com/v1/models` 返回 HTTP 401，响应带 `via: 1.1 Caddy`、`server: cloudflare`，TLS 证书验证通过。401 是 kiro.rs 对缺失 API key 的鉴权响应，说明 Cloudflare → Caddy → 59139 开发实例链路可达；cctest 使用有效 API key 后已实际经过该站点。因此本轮 502 来自 WebSearch 内部模型调用，不是 Caddy 转发故障。

59138 只读核验：Docker 镜像标签 `ghcr.io/2ue/kiro-rs:latest` 的 OCI version 为 `0.0.239`、revision 为 `30edc00c12431371e22498298715adc7bea539fd`；本地仓库 HEAD 为 `2e625e52`、crate 版本 `0.0.240`。因此 59138 部署并非当前源码 HEAD 的最新版。未对生产容器执行写操作或重启。

## 下一步

1. 先完成本轮观测改动的 scoped Cargo 测试与开发实例重启。
2. 重跑完整 cctest 并保留新抓包，检查三个失败点是否出现 `upstream_body` / `upstream_frame` 诊断。
3. 上游能返回完整 WebSearch 时，才比较加 caller、去重 input delta 的评分变化。

## 资源与隔离

- 开发实例：59139，`cargo run` 开发模式；使用独立 PostgreSQL 25432、Redis 26379。
- 59138 与生产存储未重启、未改配置、未写入。
- 运行前内存可用约 9.8 GiB，load average `2.33/2.40/3.22`，8 CPU；本轮编译时可用内存最低约 7.6 GiB、load 最高观测约 `2.94/3.40/3.43`，未触发暂停。
- 没有执行 release 编译。
- 后续轮次执行前采用资源暂停阈值：`MemAvailable < 4 GiB`、swap 剩余 `< 8 GiB`、1 分钟 load average `> 8` 持续 60 秒，任一触发即暂停新的编译/检测；59138 只读 `/v1/models` 探针连续三次不再返回预期鉴权 401 或单次耗时超过 2 秒，也立即暂停。恢复条件为内存 `>= 6 GiB`、swap 剩余 `>= 10 GiB`、load `< 6` 连续 60 秒且生产探针恢复。
