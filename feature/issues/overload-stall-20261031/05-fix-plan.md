# 修复内容

分支 `fix/overload-stall-20261031`，基于 main `a028c727`（0.0.227）。根因见 [02-root-cause.md](02-root-cause.md)，验证见 [04-reproduction.md](04-reproduction.md) 和 [06-test-plan.md](06-test-plan.md)。

原则：

- 过载时，每个请求的开销与配置大小、排队长度和账号数无关。
- 推进调度的工作（释放租约、续期、心跳、usage 写入）不能被拒绝流量饿死。
- 进程自己变慢，不能被当成 Redis 故障。

## 改动

| # | 改动 | 位置 | 解决什么 |
| --- | --- | --- | --- |
| F1 | 运行配置改为共享快照：`RwLock<Arc<Config>>`，读取时只克隆 `Arc`；热路径改用 `runtime_config_snapshot()`。`RequestRuntimeConfig` 按配置代数缓存，配置不变时多个请求共用 | `token_manager/manager.rs`、`kiro/provider.rs`、`anthropic/handlers.rs` | 每个请求在锁内深拷贝 92KB 配置 5 次以上（根因 1） |
| F2 | `AppState` 里的大配置字段改为 `Arc`：缓存策略、usage 投影、模型映射、注入、路由功能、外部池等 | `anthropic/middleware.rs` | axum 每个请求、每条连接都克隆一次 `AppState`，连带深拷贝和释放这些 BTreeMap（停摆时的最大热点） |
| F3 | 全局选号锁 `selection_reservation_gate` 从同步锁改为 tokio 异步锁 | `token_manager/manager.rs` | 几百个排队请求等这把锁时，tokio worker 线程被同步阻塞，Redis 连接驱动和租约释放任务拿不到 CPU（根因 2、4） |
| F4 | Redis 说"满"而本地认为有空槽时，记 500ms 的判满标记。期间本地选号跳过这个账号，账号释放一个槽或应用 Redis 状态时立即清除 | `account_state.rs`、`capacity.rs`、`concurrency.rs`、`redis_runtime.rs` | 释放积压时，每个请求都依次试遍所有账号，每次一个 Redis 往返，形成自我维持的风暴 |
| F5 | 租约释放仅因超时失败时，最长 1 秒后重试，不再按系统故障退避到 30 秒；失败日志带上最后一个错误 | `token_manager/concurrency.rs` | 释放写不进 Redis 时，账号在 Redis 里一直显示占满 |
| F6 | Redis 调度操作超时如果发生在本进程调度延迟期间（最近 2 秒延迟 ≥ 50ms），不计入 breaker 打开。新增运行时延迟探针：每 20ms 测一次唤醒延迟，`/readyz` 输出 `runtimeLagMs` | `common/runtime_lag.rs`、`token_manager/manager.rs`、`main.rs` | 进程自己慢，breaker 却当成 Redis 不可用，停止全部调度（根因 5） |
| F7 | 拒绝路径减负：队列满早拒绝的"没有空槽"结果按容量代数缓存，最长 200ms，容量变化立即失效；调度拒绝日志每秒最多一条，带被省略的次数；内存 usage 记录加 id 索引，在锁外克隆和释放；本地过期租约扫描最多每 250ms 一次；流式 `touch` 本地每秒最多一次 | `manager.rs`、`handlers.rs`、`request_entry.rs`、`usage.rs`、`concurrency.rs`、`common/log_throttle.rs`、`common/capacity_signal.rs` | 被拒请求每次都要扫描全部账号、写 2 行 WARN 日志、线性查找 5000 条记录（根因 3） |
| F8 | PG usage 队列超过一半时，本地调度拒绝记录（没有凭据、没有计费）按秒采样，每秒保留 1 条，并在 `error_detail` 里注明被省略的条数。成功和计费记录不采样 | `anthropic/usage.rs` | 拒绝记录挤满写入队列，成功记录被丢弃，积压几分钟写不完（根因 4） |
| F9 | 退出：收到 SIGTERM 后先 `begin_shutdown`，排队中和新进来的调度立即返回，不再等槽。HTTP 排空最多 5 秒，后台阶段最多 4 秒，统计、刷新、usage 排空并行进行，总时长在 docker 默认的 10 秒以内 | `main.rs`、`manager.rs` | 优雅退出 25–34 秒，被强杀后 usage 丢失（根因 6） |
| – | `/readyz` 的 `dispatch` 增加 `pendingRedisReleases`（尚未写进 Redis 的释放数） | `manager.rs` | 现网能直接看出"释放积压导致账号显示占满" |

## 不改的

- 调度语义保持不变：优先级、分散、会话绑定、RPM、冷却、自愈、熔断开关都不动。判满标记只影响 500ms 内是否再试同一个账号。
- 运行配置、数据库结构和迁移都不改。
- 不按 API key 限并发，不打开熔断。
- 不改 sub2api。

## 需要用户在现网做的（建议，非必需）

- compose 加 `stop_grace_period: 15s`，留出余量。
- 健康检查改用 `/readyz`。
- json-file 日志加 `max-size` / `max-file`，升级前先导出旧容器日志。
- sub2api 中指向 kiro-rs 的账号，可以减少池模式下的同账号重试（`pool_mode_retry_count`）。
