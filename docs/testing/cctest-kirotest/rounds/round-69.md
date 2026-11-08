# Round 69：Opus 5.5 稳定复测

## 时间与边界

- 时间：2026-11-07 18:01–18:04 UTC（北京时间 2026-11-08 02:01–02:04）
- 59139 使用 debug 热更新实例；59138 只做健康探针，未停止、未重启、未写生产配置或数据。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`。
- 代码：工作树 `2e625e52`，保留未提交改动。

## 结果

结果文件：[`20261107T180140-round-69-opus55-live-config.json`](../results/20261107T180140-round-69-opus55-live-config.json)

总分 **82/110**：`stream_structure=5`、`websearch=0`、`signature_proto=5`，其余项目通过，`output_config=10`。

## 抓包统计

- [`events.jsonl`](../../../logs/llm-capture/capture-20261107T180132Z-8b6861b9/events.jsonl)：8,966,361 字节，10,231 行，JSON 错误 0。
- `22` 个客户端请求，`28` 个上游请求；上游 `200=25`、`402=3`。
- `22/22` 客户端流有完整 `message_start/message_delta/message_stop`。
- `52/52` 个 content block start/stop 成对。

## 结论

账号池中 3 个 402 在首输出前被切换，不能作为协议评分根因。公开 SSE 生命周期完整，但三个失败分项不变，说明评分差异仍依赖私有抽样或未公开语义字段。该轮原始 JSONL、ZIP、manifest 全部保留。
