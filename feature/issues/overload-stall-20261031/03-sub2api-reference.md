# sub2api 的过载处理与借鉴

来源：只读阅读 `../sub2api/backend/internal/`（提交 `287624f42`），没有运行。下文路径都相对这个目录。

## sub2api 为什么过载时不死

主要有三条：

1. 排队有上限，满了立刻拒绝。并发数和排队数都用 Redis Lua 脚本原子判断，满了直接回 429。拒绝路径上只有一次 Lua 调用，不写数据库。
2. 排队有自己的硬超时，客户端断开就立即退出。
   - 等待时轮询 Redis：间隔从 100ms 开始，每次乘 1.5，最大 2s，带 ±20% 抖动。总时长由 `context.WithTimeout` 控制。
   - 默认超时：粘性请求 120s，非粘性 30s。
   - 每个阶段都能被 `ctx.Done` 打断。
3. 槽位先释放，再写库。
   - 账号槽在 Forward 返回后立即释放（`handler/gateway_handler.go:907`），早于提交 usage（`:940`）。
   - usage 和错误日志都走有界异步队列。usage worker 有 128–512 个，队列容量 16384。错误日志队列满了直接丢。

槽位释放用 `sync.Once` 加 `context.AfterFunc`，正常结束和 ctx 取消都会触发（`handler/gateway_helper.go:188`）。释放失败只打 Warning，靠三层兜底回收：

- 槽位自带 30 分钟 TTL。
- 后台每 30s 清理一次。
- 启动时按进程前缀，清掉旧进程留下的槽位和等待计数。

sub2api 没有全局并发上限、负载卸除或网关熔断。

## sub2api 会放大 kiro-rs 负载的行为

| 行为 | 位置 | 对 kiro-rs 的影响 |
| --- | --- | --- |
| 池模式（`pool_mode=true`）下，401/403/429 在同一账号上隔 500ms 重试 3 次，再换号，最多换 10 次 | `handler/failover_loop.go:36-38`、`:220-233`；`account.go:1133` | 一次 429 最多变成 11 次请求。20 个账号都指向 59137，换号还是打回 59137 |
| 池模式下收到 429 不冷却账号（`pool_mode_error_skipped`） | `service/ratelimit_service.go:336-341` | 不会因为 kiro-rs 过载而暂停对它的调度 |
| 不看 Retry-After，只解析 `anthropic-ratelimit-unified-*-reset` | `ratelimit_service.go:1201` | kiro-rs 返回的 Retry-After 对它无效 |
| 503 会走 2s 退避循环，直到达到 `max_account_switches=10` | `failover_loop.go:283-316` | 返回 503 同样会被重复打 |
| 首字节前断流：同账号重试 3 次，然后封禁 1 分钟 | `gateway_upstream_response.go:1062` | 首字节前断开会被重打 |
| 下游客户端断开后，发往上游的请求用 `context.WithoutCancel`，继续读完 | `gateway_usage_billing.go:525`、`gateway_forward.go:375`、`gateway_upstream_response.go:1090` | kiro-rs 收不到断开信号，在途请求照常占槽；sub2api 却已释放自己的槽，继续放新请求进来 |

现网 59137 的 20 个 sub2api 账号确实在走池模式：日志里有 `pool_mode_error_skipped` 和 `failover_same_account_retry`。故障期间 sub2api 对 59137 每分钟发起约 2,500 次尝试，正常时是 150–200 次。

## kiro-rs 可以借鉴的（按优先级）

1. 准入判断 O(1)。满了立即拒绝，拒绝路径不加全局锁、不做 I/O，日志限速。
2. 排队有自己的硬超时，并且不被慢请求拖住。kiro-rs 的等待上限已经是 60 秒；排队推进本身也不能依赖可能被拖慢的组件（见 02）。
3. 槽位守卫跟着请求走，并且先于写库释放。kiro-rs 已经是 RAII 加实例心跳回收，要确认释放不受 usage 写入和 PG 影响。
4. 写库有界、异步、批量，满了就丢。kiro-rs 的 usage 写入已经是有界队列，但写入器被拖慢后积压会持续增长，退出时丢失的量随之变大。
5. 重试要有双预算，次数和总时长都限制。

不照搬的：

- 用 Redis 轮询做等待。进程内的 Notify 更省。
- 30 分钟的槽位 TTL。kiro-rs 已经用实例心跳在 30 秒内回收。

## kiro-rs 侧的应对（不改 sub2api 也要成立）

- 过载回应要便宜，而且尽量让 sub2api 少重试：
  - 排队满时尽量在解析请求体之前就拒绝。
  - 不回 503，不在首字节前断流。
- 同账号重试的次数由 sub2api 配置决定（`pool_mode_retry_count`、`pool_mode_retry_status_codes`），只能由用户在 sub2api 后台调整。kiro-rs 这边要保证：就算每个被拒请求再打 10 次，拒绝的开销也不会拖慢在途请求。
- kiro-rs 自身的调度和完成速度不能被拒绝流量拖垮。这是本次修复的核心。

要改 sub2api 账号配置（减少同账号重试、对 429 冷却），需要用户在 sub2api 后台手动操作，这里只作建议。
