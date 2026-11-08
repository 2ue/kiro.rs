# Round 60：旋转高余额账号池后的复测

## 运行信息

- 时间：2026-11-08（北京时间 00:01–00:05；UTC 16:01–16:05）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 代码：与 Round 59 相同，无源码修改
- 结果：[`20261107T160148-round-60-rotated-pool.json`](../results/20261107T160148-round-60-rotated-pool.json)
- 抓包：`logs/llm-capture/capture-20261107T160127Z-c98ce104/`

## 结果

总分 `82/110`，与 Round 58、Round 59 相同：

- `stream_structure=5`
- `websearch=0`
- `signature_proto=5`
- `output_config=10`
- 其余保护项全部 10

## 抓包统计与账号分布

- 客户端请求 22；上游请求 25；上游 HTTP 200 为 25/25
- 客户端完整流 22/22
- `message_start/message_delta/message_stop` 为 22/22/22
- `content_block_start/content_block_stop` 为 49/49
- 抓包 9,211 个事件、8,454,285 字节，丢弃 0
- 最终请求摘要账号分布：2451=18、2462=2、2475=2、2458=1

本轮使用新 conversation，sticky 主账号切换到 2451，说明轮次之间可以旋转；同一轮内部仍会保持 conversation 粘性。余额复核显示 2451 仍有正余额，继续保留；后续每轮仍需复核余额，不把 Fast 层当作永久名单。

## 隔离核对

59138 `/healthz` 保持 200，生产实例未停止、未重启、未写配置、未写 PostgreSQL/Redis、未修改生产账号状态。所有 Fast 调整仅作用于 59139。
