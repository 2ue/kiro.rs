# 根因分析

代码位置以 main `a028c727`（0.0.227）为准，0.0.226 的相关路径与之相同。证据有两部分：

- 本地复现，见 [04-reproduction.md](04-reproduction.md)。
- 带符号构建在停摆时的 8 次线程栈采样（每次 3 秒），以及 Redis 租约状态、Redis 连接命令计数、释放分发器日志。

## 结论

停摆是一个自我维持的循环，起点是进程级的同步锁竞争。

1. 过载时，每个请求都在同步锁里做重活，包括注定被拒的请求。sub2api 的重试又把这类请求放大 10 倍以上。重活主要有三类：
   - axum 为每个请求和每条连接克隆 `AppState`，连带深拷贝、释放其中按值存放的配置 BTreeMap（缓存策略、usage 投影等）。
   - 运行配置在锁内整份克隆，现网 92KB。
   - 全局选号锁内要扫描全部账号。
2. 全局选号锁是同步锁（`parking_lot::Mutex`）。几百个请求同时等它时，tokio worker 线程本身被阻塞。Redis 连接驱动、租约释放任务、心跳拿不到 CPU，运行时调度延迟升到 0.6–3.8 秒。
3. 已完成请求的 Redis 租约释放因此超时（2 秒）。释放分发器把超时当成系统故障，整体退避，最长 30 秒。Redis 里的租约越积越多：本地复现最多 211 个、释放积压 500 多个，账号在 Redis 里全部显示占满。
4. 本地的请求早已结束，本地视图认为账号有空。于是每个请求都选中一个账号去 Redis 占槽，被告知已满，再换下一个，最多试遍全部 57 个账号。每次尝试都要再进一遍全局选号锁，再做一次 Redis 往返。这条路径最贵，所有请求都走它，锁竞争进一步加剧，回到第 2 步。
5. 进程自身变慢后，Redis 调度操作超过 75/250/500ms 期限，breaker 被打开。打开期间选号被跳过，空闲账号也不调度，并对外返回"Redis 调度协调状态不可用"的 429。
6. 负载降下来以后，几千个积压的客户端连接还在重试，循环维持不住才会退出。实际上只有重启能让它停下来。

## 证据

### 线程栈（停摆时，上游在途 0，`/readyz` 5–8 秒）

修复前，9 个 tokio worker 全忙。热点：

| 热点 | 说明 |
| --- | --- |
| `parking_lot::RawMutex::lock_slow` | 等全局选号锁（`manager.rs:6864`）、`request_runtime_config` 的 config 锁、早拒绝的 entries 锁、usage 记录锁 |
| `BTreeMap::clone_subtree`，以及 `drop_glue<AppState>`、`drop_glue<CachePolicyConfig>`、`BTreeMap::IntoIter<ReportedUsagePathPolicy>` | 每个请求和连接克隆、释放 `AppState` 和运行配置 |
| `tracing` 格式化，`Stdout::lock` | 每个 429 同步写 2 行 WARN 日志 |
| `cthread_yield`、`swtch_pri` | parking_lot 自旋让出 |

把运行配置改为共享快照之后，配置克隆从 859 次采样降到约 100 次，但仍然停摆。剩下的最大热点是 `AppState` 的克隆和释放（约 12,000 次采样，来自 `axum::serve::handle_connection`），以及全局选号锁等待（约 5,000 次）。

### Redis 侧

- 停摆期间，Redis 每秒只收到 200–600 条命令，正常过载时是 4,000–9,000 条。各连接空闲几十秒。Redis 本身一直空闲，是 kiro-rs 发不出命令。
- Redis 租约 211 个，账号计数合计 183 个；mock 上游实际在途 0。
- 释放分发器日志：`completed=0 failed=16 last_error="Redis lease release timed out after 2000ms"`，积压 252 增长到 536。

### 修复后对照

修复后同一个 spike_sub2api 用例：

- 不再停摆，回落后自动恢复。
- 结束时 Redis 租约归零。
- 内存 302MB（修复前 2–3GB）。
- 运行时延迟最高 683ms（修复前 3,856ms）。

## 各环节

| 环节 | 代码 | 问题 |
| --- | --- | --- |
| `AppState` 按值存放配置 | `middleware.rs:262` | axum 每个请求、每条连接都克隆。缓存策略等 BTreeMap 深拷贝 |
| 运行配置 | `manager.rs:969` `Mutex<Config>`，`runtime_config()` 是 `lock().clone()` | 一个请求在锁内整份克隆 5 次以上 |
| `request_runtime_config` | `handlers.rs:1584` | 每个请求 normalize 并克隆缓存策略等，现网 75KB |
| 全局选号锁 | `manager.rs:1054`，`parking_lot::Mutex<()>` | 同步锁。排队者多时阻塞 worker 线程；锁内扫描全部账号 |
| Redis 判满后换号 | `manager.rs` slot race 分支 | 本地视图与 Redis 不一致时，每个请求依次试遍所有账号 |
| 释放分发器 | `concurrency.rs` 系统级退避，最长 30 秒 | 本进程慢导致的超时也按系统故障退避，释放长时间写不进 Redis |
| Redis breaker | `manager.rs` `complete_failure` | 期限包含本进程排队时间，进程慢时必然超时并打开 |
| 拒绝路径 | `manager.rs:3994`、`handlers.rs` 日志、`usage.rs:2734` | 每个被拒请求：扫描全部账号，写 2 行日志，线性查找 5000 条内存记录 |
| usage 写入 | `usage.rs` 单个 writer，PG 队列 4096 | 拒绝记录挤满队列，成功记录被丢弃 |
| 退出 | `main.rs` HTTP 排空 30 秒，后台 45 秒 | 超过 docker 默认的 10 秒，被强杀时丢失内存中的 usage |

## 和现象的对应

| 现网现象 | 原因 |
| --- | --- |
| 并发占满、RPM 接近 0 | 释放写不进 Redis，Redis 里账号全满；本地选号试遍账号都被拒 |
| 拒绝要几十秒才返回 | 拒绝路径也在抢同一组同步锁，worker 线程被阻塞 |
| "Redis 调度协调状态不可用" 429 | breaker 把进程自身变慢当成 Redis 故障 |
| usage 写入每批 10–60 秒，记录丢失 | writer 拿不到 CPU，拒绝记录挤满队列 |
| 负载降下后不恢复 | 积压的客户端连接持续重试，循环自我维持 |
| 重建第一次没好 | 下游积压立即把新进程打回同一状态 |
| 被强杀 | 退出超过 10 秒 |
| 正常负载下偶发调度停几秒 | 统计刷新 `block_in_place` 等抖动触发 breaker，打开后调度停住 |
