# Round 57：同代码重复检测与结构化输出波动

## 运行信息

- 时间：2026-11-07（北京时间 21:33–21:36；UTC 13:33–13:36）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 代码：与 Round 56 相同，无源码修改
- 结果：[`20261107T133342-round-57-repeat.json`](../results/20261107T133342-round-57-repeat.json)
- 抓包：`logs/llm-capture/capture-20261107T133342Z-2f8ec325/`

## 结果

总分 `73/110`。与 Round 56 唯一差异是 `output_config` 从 10 变为 0，其余分项完全相同。说明条件式 identity/platform guidance 提高了命中概率，但不能保证每轮模型都返回评分器期望的身份语义。

## 抓包统计

- 客户端请求 22；上游请求 25；上游 HTTP 200 为 25/25
- 客户端完整流 22/22
- `message_start/message_delta/message_stop` 为 22/22/22
- `content_block_start/content_block_stop` 为 49/49
- 没有账号池耗尽、WebSearch 502 或公开 SSE 丢帧

## 结论

当前同一份代码的完整检测区间为 `73–82/110`；稳定底线是 73，82 是条件命中时的最好结果，不能宣称为稳定分数。
