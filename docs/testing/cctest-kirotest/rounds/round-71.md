# Round 71：真正无 caller 流式 A/B

## 时间与边界

- 时间：2026-11-07 18:21–18:24 UTC（北京时间 2026-11-08 02:21–02:24）
- 59139 使用 debug 热更新实例；59138 只读健康探针，未受修改。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`。
- 实验改动：流式 `block_events()` 在输入 block 缺少 `caller` 时不再补 `{"type":"direct"}`；其它逻辑不变。

## 结果

结果文件：[`20261107T182132-round-71-no-caller.json`](../results/20261107T182132-round-71-no-caller.json)

总分 **82/110**。`stream_structure=5`、`websearch=0`、`signature_proto=5`、`output_config=10`；其余项目均通过。

## 抓包统计

- [`events.jsonl`](../../../logs/llm-capture/capture-20261107T182036Z-2b9413b3/events.jsonl)：8,549,780 字节，9,442 行，JSON 错误 0。
- `22` 个客户端请求，`25` 个上游请求，`25/25` 上游 HTTP 200。
- `22/22` 客户端流完整；`51/51` 个 content block 成对。
- WebSearch 流式 `server_tool_use` 和 `web_search_tool_result` 均未携带 `caller`；评分仍为 `websearch=0`。

## 结论

真正覆盖流式路径的 caller 删除没有改变任何评分项，排除 `caller=direct` 是当前三项失败的充分根因。代码已恢复默认 caller 逻辑，停止该猜测方向。该轮 JSONL、ZIP、manifest 全部保留。
