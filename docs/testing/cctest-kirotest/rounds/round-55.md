# Round 55：账号池耗尽与恢复后 WebSearch 502

## 运行信息

- 时间：2026-11-07（北京时间 21:05–21:06；UTC 13:05–13:06）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 代码：与 Round 54 相同；59139 使用 debug 热更新进程，没有新增源码修改
- 结果：[`20261107T130507-round-55-dev-recheck.json`](../results/20261107T130507-round-55-dev-recheck.json)（账号耗尽，中断）
- 恢复后结果：[`20261107T132246-round-55-account-restored.json`](../results/20261107T132246-round-55-account-restored.json)
- 抓包：`logs/llm-capture/capture-20261107T130345Z-c891551e/`、`logs/llm-capture/capture-20261107T132246Z-683813a7/`

## 结果与根因

第一次执行在 tag 阶段因 59139 可调度账号耗尽返回 503，不能作为协议评分轮次。账号恢复后检测进入 WebSearch，但三个内部请求均使用开发凭据 `2530`，上游 HTTP 200、响应体 0 字节，最终本地返回 502，抓包中归类为 `local_error_no_fallback`。

账号 2530 只在 59139 独立数据库中禁用，59138 对应账号保持原状态。之后 Round 56 不再出现该 WebSearch 502。

## 隔离核对

59138 仅执行只读健康探针和只读状态查询，没有停止、重启、配置写入、生产 PostgreSQL/Redis 写入或生产账号状态修改。59139 账号剔除属于开发实例本地状态。
