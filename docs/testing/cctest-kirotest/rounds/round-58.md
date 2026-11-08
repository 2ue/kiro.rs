# Round 58：同代码重复检测再次得到 82

## 运行信息

- 时间：2026-11-07（北京时间 21:38–21:41；UTC 13:38–13:41）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 代码：与 Round 57 相同，无源码修改
- 结果：[`20261107T133807-round-58-repeat.json`](../results/20261107T133807-round-58-repeat.json)
- 抓包：`logs/llm-capture/capture-20261107T133807Z-baca65b9/`

## 结果

总分 `82/110`，分项与 Round 56 相同：`stream_structure=5`、`websearch=0`、`signature_proto=5`、`output_config=10`，保护项全部通过。

## 抓包统计

- 客户端请求 22；上游请求 25；上游 HTTP 200 为 25/25
- 客户端完整流 22/22
- `message_start/message_delta/message_stop` 为 22/22/22
- `content_block_start/content_block_stop` 为 55/55
- 抓包 10,520 个事件、9,010,892 字节，丢弃 0
- 59139 当时主要由凭据 2475 承担请求；同一 CCTest 会话的 conversation sticky 使其承担 18/23 个最终请求摘要

## 结论

本轮确认协议基础在账号可用时可稳定完成检测，但单号 sticky 会集中消耗额度。后续抓包改用高余额账号优先层，并记录每轮 credentialId 分布。
