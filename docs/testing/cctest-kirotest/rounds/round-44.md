# Round 44：identity 提醒回滚后的原始配置基线

## 运行信息

- 时间：2026-11-07（北京时间 12:22–12:25）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 配置：原始路由配置；identityPrompt 与 identityTurnReminder 均关闭
- 59138：只读，未重启、未改配置、未写 PostgreSQL/Redis

## 结果

结果文件：`results/20261107T042204-round-44-baseline-restored.json`

总分恢复为 `73/110`，11 项分数与 Round 42 完全一致：`tag_check=10`、`stream_structure=5`、`non_stream=10`、`websearch=0`、`signature_proto=5`、`output_config=0`、`server_tool=10`、knowledge/model attribution/doc/image 均为 10。

## 抓包

完整抓包：`logs/llm-capture/capture-20261107T042204Z-db17a58b/events.jsonl`；保留 manifest 与 ZIP。

- 9,975 个事件，JSON 解析错误 0、丢弃事件 0。
- 22 个客户端请求、25 个上游请求，上游响应全部 HTTP 200。
- 22 个客户端流的 message start/delta/stop 数量一致；51 个 block start/stop 配对。
- 非流分项回到 10；身份提醒引起的 64 分退化没有持续。
- 本轮 8 个 `signature_delta` 都与对应 Kiro `reasoningContentEvent.signature` 的 SHA-256 完全一致；值均为 `CAQS` 前缀，没有 synthetic signature。

资源监控 17 次：最低可用内存约 8.97 GiB、最低 swap 约 10.99 GiB、最高 1 分钟负载 1.10；59138 健康探针全部 HTTP 200。未触发暂停阈值。

## 签名对照

逐请求对比上游 `reasoningContentEvent.signature` 与客户端 `signature_delta`：8/8 哈希完全一致。Round 41 同样为 8/8。两轮 `signature_proto=5`，说明当前证据不支持代理改写或伪造签名作为失败原因。Opus 5.5 请求未指定 `thinking.display`，按模型默认 omitted，空 thinking 文本和签名保留符合现有显示路径。

## 结论

identityTurnReminder 的临时配置确实与非流回归及 thinking block 增加相伴；配置恢复后 `non_stream` 从 0 回到 10。该配置不保留，继续在原始路由配置上定位四个目标失败项。
