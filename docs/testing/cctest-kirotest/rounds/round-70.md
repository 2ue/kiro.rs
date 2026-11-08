# Round 70：部分无 caller 对照（未覆盖流式默认补字段）

## 时间与边界

- 时间：2026-11-07 18:09–18:12 UTC（北京时间 2026-11-08 02:09–02:12）
- 59139 使用 debug 热更新实例；59138 只读健康探针，状态正常。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`。
- 实验改动只删除模型驱动 WebSearch 非流 block 的 `caller`；`block_events()` 对流式 block 仍用默认 `caller={"type":"direct"}`。

## 结果

结果文件：[`20261107T180930-round-70-no-caller.json`](../results/20261107T180930-round-70-no-caller.json)

总分 **82/110**：`stream_structure=5`、`websearch=0`、`signature_proto=5`，其余项目通过，`output_config=10`。

## 抓包统计

- [`events.jsonl`](../../../logs/llm-capture/capture-20261107T180923Z-23ab31e2/events.jsonl)：8,631,771 字节，9,618 行，JSON 错误 0。
- `22` 个客户端请求，`25` 个上游请求，`25/25` 上游 HTTP 200。
- `22/22` 客户端流完整；`56/56` 个 content block 成对。
- 该轮不能作为“真正无 caller”的结论：流式 server tool 仍由 `block_events()` 默认补回 `caller=direct`。

## 结论

非流 caller 的单变量删除没有改变评分；由于流式路径未覆盖，实验不具备完整因果隔离。原始抓包继续保留，随后 Round 71 才做真正的流式无 caller 对照。
