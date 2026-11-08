# Round 72：WebSearch 两个生成块补齐 caller

## 时间与边界

- 时间：2026-11-07 18:40–18:44 UTC（北京时间 2026-11-08 02:40–02:44）
- 59139 使用 debug 热更新实例；59138 只读健康探针，未停止、未重启、未写生产配置或数据。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`。
- 代码改动：模型驱动 WebSearch 生成的 `server_tool_use` 与 `web_search_tool_result` 非流 block 均显式带 `caller={"type":"direct"}`；流式路径继续保留默认 caller。

## 结果

结果文件：[`20261107T184002-round-72-web-caller.json`](../results/20261107T184002-round-72-web-caller.json)

总分 **82/110**：`stream_structure=5`、`websearch=0`、`signature_proto=5`，`output_config=10`，其余项目通过。

## 抓包统计

- [`events.jsonl`](../../../logs/llm-capture/capture-20261107T184002Z-f9b8fa5f/events.jsonl)：9,364,979 字节，11,340 行，JSON 错误 0。
- `22` 个客户端请求，`26` 个上游请求；上游 `200=25`、`402=1`，402 在首输出前切换。
- `22/22` 客户端流完整；`58/58` 个 content block 成对。
- WebSearch 的 `server_tool_use` 与 `web_search_tool_result` 生成块均含 `caller=direct`；同轮代码执行块不属于本实验。

## 结论

补齐两个 WebSearch caller 字段没有改变 WebSearch 评分，排除 caller 缺失为 `websearch=0` 的充分根因。该改动符合官方 Anthropic SDK 类型定义，保留在代码中；后续继续定位 `encrypted_content`、`encrypted_index` 与私有评分抽样差异。原始 JSONL、ZIP、manifest 全部保留。
