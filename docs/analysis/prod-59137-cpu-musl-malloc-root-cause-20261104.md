# kiro-rs-2ue-59137 CPU 打满问题分析（2026-11-04）

## 1. 现象

- 2026-11-04 下午，深圳机器（8 核 16GB）在约 400 rpm 时整机 CPU 到 90% 以上。
- 主要消耗方是 `kiro-rs-2ue-59137-app` 容器，`docker stats` 常见 120%~270%，按秒采样峰值 654%。
- CPU 抖动大：同样请求量下，按秒 CPU 在 5% 到 650% 之间跳。
- 内核态占比很高：10 秒按线程统计 user 88% / sys 32%，尖峰时 sys 超过 user（例：user 169% / sys 214%）。
- RSS 平时 750~980MB，VmHWM 到过 2175MB。
- 同期日志：3 分钟内上游发送 926 次，其中 317 次是 `protocol_error` 换号重试（上游 eventstream 一帧都没有）。请求体从几十 KB 到 19MB 不等。

## 2. 环境与配置

| 项 | 值 |
|---|---|
| 镜像 | `ghcr.io/2ue/kiro-rs:latest`，版本 0.0.237，源码提交 `73bae2e7` |
| 构建 | `rust:1.92.0-alpine3.23` 编译，运行在 `alpine:3.23`，静态链接 musl；`lto = true`，`strip = true` |
| 全局分配器 | 未设置，用 musl 自带 malloc。`src/http_client.rs` 的 `CountingAllocator` 在 `#[cfg(test)] mod allocation_probe` 里，release 不包含 |
| tokio | 多线程运行时，进程 20 个线程 |
| 相关配置 | `compression.whitespaceCompression` 开；`payloadGuardMaxBytes=460800`；`kiroUpstreamStreamRetryMaxAttempts` 未写，默认 6（`DEFAULT_KIRO_UPSTREAM_STREAM_RETRY_MAX_ATTEMPTS`，`src/model/config.rs`） |
| 容器资源限制 | 无 cpu / 内存限制，和 sub2api、caddy、postgres、redis 共用 8 核 |

## 3. 对前一会话（a6911875）结论的核对

| 前一会话的说法 | 核对结果 |
|---|---|
| kiro-rs 是主要 CPU 消耗方 | 属实 |
| 零帧失败触发整包换号重试，重试放大 | 属实。本次 120 秒采样：发送 590 次，279 个请求，167 次重试，全部 `protocol_error` |
| 每次发送都重新处理整份 body，没有缓存 | 属实。重试先调 `refresh_kiro_request_identity`（`provider.rs:1192`，整包解析成 `serde_json::Value` 再复制两次），每次发送再调 `transform_ide_api_body`（`ide.rs:209`，有 profileArn 时再整包解析成 Value 并重新序列化）、`maybe_compress_json_whitespace`（扫描加整包校验）、`kiro_payload_weight`（开压缩时算两遍） |
| CPU 尖峰对应大 body 和重试集中的秒 | **基本不成立**。120 秒逐秒采样，CPU 与发送字节相关系数 0.04，与重试次数 -0.04；按 10 秒聚合也只有 0.26 和 -0.08。与缺页次数相关系数 0.56 |
| musl malloc 慢、不爱还内存（当时是推断） | **属实，而且是主因**。见第 4 节 |
| `CountingAllocator` 可能进了 release | 不成立，有 `#[cfg(test)]` |
| 大 body 卡住 tokio worker 导致抖动 | 未证实。perf 里应用自身代码只占 24% |
| 零帧失败率和 body 大小无关 | 本次未重新验证 |

结论：前一会话把**现象和放大因素**（重试多、body 大、每次重复处理）当成了原因。它们决定了分配次数有多少，但 CPU 真正花在了哪里，是下面的分配器锁竞争。

## 4. 根本原因

**kiro-rs 用的是 musl 自带的 malloc。它对所有线程用一把全局锁。kiro-rs 每次上游发送都会对多 MB 的 JSON 做多次 DOM 解析和整串复制，产生海量小块分配。8 个 tokio worker 同时这样做，就在这把锁上排队，自旋失败后进 futex 睡眠和唤醒。大块释放时 malloc 还会频繁 `madvise`/`munmap` 还页，引发跨核 TLB 刷新中断。CPU 被耗在锁、系统调用和缺页上，不在业务逻辑上。**

证据（`perf` 采样运行中的进程，2026-11-04 16:2x）：

1. CPU 分布（`perf record -F 199 -g`，30 秒，3187 个样本）：
   - musl libc 地址段（`0x2280000-0x22b0000`）占 52.5%
   - 内核占 23.3%
   - 应用和 Rust 库代码只占 24.3%
2. 最热的单个函数（10.7%）在 `0x22964a0`。反汇编是 musl `__lock` 的特征写法：`lock cmpxchg` 设 `0x80000001`，自旋 10 次，再走 `syscall 0xca`（futex）等待；`0x2296552` 是对应的 `__unlock`（`futex wake`）。第 2、3 热点（8.6%、7.7%）也是这段里的 `lock cmpxchg`。二进制被 strip，函数名是根据指令模式认的，不是符号表。
3. 系统调用（`perf stat`，10 秒）：

   | 事件 | 10 秒计数 | 每秒 |
   |---|---|---|
   | futex | 2,658,648 | 约 26.6 万 |
   | madvise | 25,401 | 约 2,500 |
   | mmap / munmap | 2,999 / 2,948 | 约 300 |
   | page-faults | 187,766 | 约 1.9 万 |
   | context-switches | 155,872 | 约 1.6 万 |
   | tlb_flush | 67,510 | 约 6,700 |

   对比：同期 sendto 只有 767 次，write 1,789 次。futex 比网络 IO 多三个数量级。
4. futex 来源（`perf record -e syscalls:sys_enter_futex -g`）：74% 从上面那个 `__lock`/`__unlock` 发出，其余 26% 来自 libc 的通用 syscall 封装。
5. 内核热点是 `__pv_queued_spin_lock_slowpath`、`futex_wake`、`asm_sysvec_call_function_single`、`smp_call_function_many_cond`（TLB 刷新 IPI）、`unmap_page_range`、`do_anonymous_page`。这对应锁竞争加上内存反复归还和重新缺页。

为什么请求量相近时 CPU 会忽高忽低：锁竞争不是线性的。同时在做大量分配的 worker 越多，失败自旋和 futex 越多，CPU 会突然翻几倍。所以 CPU 和"这一秒发了多少字节"相关性很弱，和缺页、并发分配强度更相关。

为什么重试和大 body 只是放大因素：它们增加了总分配量，但如果分配器没有全局锁，同样的分配量不会把 8 核打满。见第 5 节的对照实验。
### 4.1 59138 机器复核，2026-11-05 00:38

深圳机器流量迁走后，在 59138 机器高并发时复测了一次，用的是同一份二进制：镜像 0.0.237，镜像 ID `sha256:8e80bf81…`，可执行文件 sha256 前缀 `2520521ffbd9760d`，和深圳机器一致。

- 负载：60 秒内上游发送 648 次（首发 477，重试 137，全部零帧）。
- CPU：30 秒平均 user 113% / sys 118%，峰值 510%。sys 已超过 user。
- 系统调用（10 秒）：futex 906 万（约 90 万/s，是深圳的 3.4 倍）；madvise 3.1 万；上下文切换 67 万；tlb_flush 11.8 万。同期 sendto 只有 1,761。
- CPU 分布（15 秒，6416 个样本）：内核 43.3%，musl libc 区 40.7%，应用代码只有 15.8%。
- 热点地址和深圳完全一致：`0x22964ec`/`0x22964ba`/`0x2296561`（`__lock`/`__unlock`）、`0x228dc5b`/`0x228d55c`。内核第一名是 `__pv_queued_spin_lock_slowpath`（10.4%），其次是 TLB 刷新 IPI 和 `futex_wake`。

结论一致，而且并发越高越严重：锁竞争从用户态自旋扩散到内核 futex 哈希桶的自旋锁，应用真正干活的比例从 24% 降到 16%。

## 5. 如何复现

### 5.1 线上观测（只读）

```bash
PID=$(docker inspect -f '{{.State.Pid}}' kiro-rs-2ue-59137-app)
# 系统调用和 TLB 计数，看 futex / madvise 是否异常高
perf stat -p $PID -e 'syscalls:sys_enter_futex,syscalls:sys_enter_madvise,syscalls:sys_enter_munmap,page-faults,context-switches,tlb:tlb_flush' -- sleep 10
# CPU 热点
perf record -F 199 -g -p $PID -o /tmp/kiro.perf -- sleep 30
perf report -i /tmp/kiro.perf --no-children --sort dso,sym --stdio -g none | head -30
```

宿主机原来没有 perf，本次装了 `linux-perf`（Debian 包 6.12.115-2）。二进制是 strip 的，热点只显示地址。要拿到函数名，需要构建不 strip 的镜像，或者用 `cp /proc/$PID/exe` 拷出来后 `objdump -d` 对照反汇编。

### 5.2 离线对照实验

实验代码 `bench/` 留在排查用的机器上，没有入库。`bench/src/main.rs` 模拟一次上游发送的 body 处理：两次 `serde_json::Value` 解析、一次序列化、两次整串复制。body 是 4MB、由大量中小字符串对象组成的 JSON，和线上 history 结构相近。同一份代码分别用 musl 默认 malloc 和 mimalloc 编译，编译环境和线上镜像相同（`rust:1.92.0-alpine3.23`）：

```bash
cd bench
docker run --rm -v "$PWD":/b -w /b -e CARGO_TARGET_DIR=/b/target rust:1.92.0-alpine3.23 sh -c \
  'apk add -q musl-dev && cargo build --release && cp target/release/allocbench bench-musl \
   && cargo build --release --features mimalloc && cp target/release/allocbench bench-mimalloc'
time ./bench-musl 4 4 10       # 参数：线程数 body_MB 每线程发送次数
time ./bench-mimalloc 4 4 10
```

在本机测得的结果：

| 分配器 | 线程 | 吞吐（发送/秒） | wall | user | sys |
|---|---|---|---|---|---|
| musl | 1 | 13.7 | 0.73s | | |
| musl | 4 | 10.3 | 3.87s | 6.05s | 5.17s |
| mimalloc | 1 | 31.7 | 0.32s | | |
| mimalloc | 4 | 87.9 | 0.46s | 0.98s | 0.75s |

- musl 下 4 线程吞吐反而比单线程低，符合全局锁竞争的特征。
- 4 线程时 musl 的 user+sys CPU 是 mimalloc 的 6.4 倍，吞吐只有它的 1/8.5。
- musl 这一轮跑了 573 万次 futex、3.6 万次 munmap、31 万次 TLB flush，sys 时间和 user 时间接近，和线上表现一致。

注意：这个实验是在生产机上用 `nice 19` 跑的，规模只有几秒。它说明的是机制和量级，不代表线上精确的收益比例。

2026-11-04 在开发机上复测了一次：colima 虚拟机，aarch64，6 核。用的是同样的模型（4MB body，每线程 8 次），同样用 `rust:1.92.0-alpine3.23` 编译：

| 分配器 | 1 线程（发送/秒） | 4 线程（发送/秒） |
|---|---|---|
| musl | 7.7 | 2.9 |
| mimalloc 0.1.52 | 12.9 | 36.9 |

结论相同：musl 下并发越高，吞吐越低；4 线程时 mimalloc 的吞吐是 musl 的 12.7 倍。

### 5.3 大 body 为什么越过 payload guard

结论：默认配置下 `payloadGuardMode` 是 `on_too_long`，首发请求根本不做按大小裁剪，只有上游真的返回"输入过长"之后才裁剪重试。这是有意设计，不是 guard 失效。

代码依据（提交 `73bae2e7`）：

- `RequestRuntimeConfig::initial_payload_guard_config`（`src/anthropic/handlers.rs`）：`OnTooLong` 下把 `max_bytes` 和 `max_kiro_weight` 都置 0、`trim_history` 置 false，只保留协议修复和轻量整形。`payload_guard_config()` 里仍保留 460800 / 130 万，供"上游报过长之后的重试"使用。
- `apply_runtime_config_migrations`（`src/model/config.rs`）在 `runtime_config_migration_version < 1` 时会把旧的 `preemptive` 一次性改成 `on_too_long`。所以线上 dump 配置里即使写着 `payloadGuardMode = preemptive`，实际运行的是 `on_too_long`。
- `too_long_retry_enabled_for`：默认 `payload_too_long_handling = client_compaction`，只有 Claude Code 自己的压缩请求才由服务端裁剪重试，其余请求按官方协议返回 `prompt is too long` 让客户端压缩。
- 上游对这些请求确实返回了 200（证据见分析包的 `evidence/87-59138/10-app.log`，19MB 那类请求 `outcome="response_headers_received"`），所以不存在"越过了本该拦住的限制"。真正的风险是超大请求把上游拒绝、白跑一次重试，属于重试放大（第 2 条），不是 guard 漏洞。

要恢复按大小预裁剪，把 `payloadGuardMode` 设成 `preemptive` 即可；代价是每个请求都多一次裁剪判断和可能的历史裁剪，与"减少放大因素"的方向相反，建议先用数据决定。

## 6. 端到端对照（同一台机器、同一份负载、新旧二进制）

2026-11-04 晚在开发机的 colima 虚拟机（aarch64、6 核）上用 `rust:1.92.0-alpine3.23` 编译了两份 musl 静态二进制：

- `old`：提交 `73bae2e7`，即线上 0.0.237，musl malloc。二进制里没有 mimalloc 字符串。
- `new`：`perf/mimalloc-hot-path` 分支，mimalloc 全局分配器 + 去掉热路径 DOM 解析。

环境：`alpine:3.23` 容器，PostgreSQL 17.5 + Redis 7.4 各一个容器，上游用仓库里的 `scripts/loadtest/kiro-mock-upstream.mjs`（同时模拟 8 个账号，16 并发/账号）。压测客户端按 24 或 48 路并发持续发带多轮 history 的 `/v1/messages` 请求，每个 worker 一个独立会话。CPU 取自容器内 `/proc/1/stat` 的 utime+stime，`voluntary_ctx_switches` 取自所有线程之和。

| 场景 | 版本 | 成功/失败 | 吞吐（req/s） | CPU 合计 | user / sys | 自愿上下文切换/s | CPU 毫秒/成功请求 | p50 / p95 (ms) |
|---|---|---|---|---|---|---|---|---|
| 1.27MB body，24 并发 | old | 109 / 0 | 2.7 | 594.8% | 215 / 380 | 270,327 | 2182.8 | 9054 / 12852 |
| 1.27MB body，24 并发 | new | 1234 / 0 | 30.9 | 263.7% | 252 / 12 | 4,457 | 85.5 | 785 / 921 |
| 61KB body，48 并发 | old | 1489 / 0 | 49.6 | 341.3% | 140 / 201 | 112,659 | 68.8 | 962 / 1380 |
| 61KB body，48 并发 | new | 2201 / 0 | 73.4 | 51.7% | 42 / 10 | 4,579 | 7.0 | 658 / 801 |
| 1.27MB body，40% 上游 5xx | old | 101 / 5 | 2.5 | 600.4% | 216 / 385 | 272,760 | 2377.8 | 9160 / 14900 |
| 1.27MB body，40% 上游 5xx | new | 584 / 669 † | 14.6 | 274.5% | 264 / 11 | 4,460 | 188.0 | 730 / 1760 |

† 这一组 669 个失败里 511 个是本地返回的 429「所有可用账号均处于上游临时冷却」。8 个账号在上游持续 40% 失败时全部进入瞬态冷却，新版吞吐高 5.8 倍、把同样的失败放大成更多冷却窗口，旧版只是被自身的 CPU 瓶颈挡在冷却之前。同一种调度策略在两种负载强度下的结果，不是新版引入的行为差异。

要点：

- sys 时间从 380% 降到 12%，自愿上下文切换下降 60 倍，与"futex 和锁竞争消失"一致。
- 相同的成功请求，CPU 成本从 2.2 秒降到 85 毫秒（大 body）、从 69 毫秒降到 7 毫秒（小 body）。
- 大 body 场景吞吐提升约 11 倍，小 body 约 1.5 倍。小 body 的提升来自分配量少、DOM 解析本来就占比不高，但也仍未触及 mock 上游的上限。

**RSS 是这次唯一的倒退。** 同样 1.27MB 场景，old 结束时 110MB、峰值 587MB；new 结束时 688MB、峰值 925MB。mimalloc 按线程缓存内存、归还懒。加 `MIMALLOC_PURGE_DELAY=0` 后峰值从 976MB 降到 630MB、结束时从 786MB 降到 211MB，代价是 sys 从 7% 升到 31%、CPU 合计从 148% 升到 173%（吞吐不变）。建议上线时先用默认值并给容器设 `mem_limit`（例如 3g，历史峰值约 2.1GB），观察一两天再决定要不要开这个变量。

## 7. 解决方案

按收益排序。第 1 条针对根因，2~4 条减少放大因素。

1. **把全局分配器换成 mimalloc 或 jemalloc（根因修复）。**
   - 备选：改用 glibc 镜像（`debian:bookworm-slim` + `x86_64-unknown-linux-gnu`），效果不如 mimalloc，改动也更大。

   **为什么必须改代码、重新打镜像：** 二进制静态链接 musl，分配器在编译时就定死了。musl malloc 没有可调的环境变量，静态二进制也不能用 `LD_PRELOAD` 换分配器，所以只改配置或 compose 无效。

   **具体改动（已在 `perf/mimalloc-hot-path` 分支完成）：** `Cargo.toml` 加 `mimalloc = "=0.1.52"`（默认用 mimalloc v3），`src/main.rs` 用 `#[cfg(not(test))] #[global_allocator]` 注册。测试构建仍然用 `http_client::allocation_probe` 的计数分配器，两者不冲突。

   **镜像：** Dockerfile 不用改。mimalloc 要编译一点 C 代码，`rust:1.92.0-alpine3.23` 加上现有的 `musl-dev` 就够了。新增依赖会让依赖缓存层重编一次。按现有流程发版即可（`.github/workflows/docker-build.yaml`）。

   **上线步骤：**
   1. 只把 59138 的 `.env` 中 `KIRO_RS_VERSION` 改成新 tag，执行 `docker compose pull && docker compose up -d`。深圳 59137 保持 0.0.237。回滚就是把 tag 改回去。
   2. 高峰期复测：`perf stat -p $PID -e syscalls:sys_enter_futex,tlb:tlb_flush -- sleep 10`。改前基线：futex 约 90 万/s，kiro-rs CPU 平均约 231%（sys 118%）。预期 futex 下降一到两个数量级，sys 明显降低。
   3. 观察 RSS。mimalloc 归还内存不积极，RSS 可能偏高，配合第 5 条加 `mem_limit`。
   4. 59138 稳定后再升级深圳。

2. **降低零帧重试放大。** `kiroUpstreamStreamRetryMaxAttempts` 默认 6。前一会话统计到大部分重试请求把 6 次全用满了，说明多数重试白白消耗资源。建议先降到 2~3，再按模型和账号统计零帧失败分布（尚未做），决定是否加熔断。

3. **去掉发送热路径上的整包 DOM 解析。** 前三项已在 `perf/mimalloc-hot-path` 分支完成：
   - 新增 `src/kiro/request_head.rs`，用 serde 访问器只读出 `conversationId`、`agentContinuationId`、`modelId`、根上的 `profileArn` 和 `additionalModelRequestFields` 的推理字段，其余部分边读边跳过。没有转义的字符串直接借用请求体。判定结果和 `Value` 一致：同名键取最后一个，类型不对视为缺失，嵌套深度同样限制在 128 层。
   - `refresh_kiro_request_identity`、调度用的模型和 conversationId 提取都改用它。原来每个请求要整包解析两次，每次换身份重试再解析一次、复制两次；现在只扫描一遍，复制一次。
   - `transform_ide_api_body` 只缺 profileArn 时，直接追加到根对象末尾，不再走 DOM 往返。已有不同的 profileArn、或者推理字段需要改写时，仍走原来的 DOM 路径。
   - 每次发送只算一次 `kiro_payload_weight`。原来开压缩时 `should_log_upstream_body_size_at_info` 必然返回 true，日志字段还会再算一遍。
   - 未做：和账号无关的结果（压缩后的 body）缓存成 `Bytes`，换号时只克隆引用计数。CLI 端点的 origin 改写涉及 history 里每一条消息，仍然走 DOM。

   这一条直接减少分配次数，换了分配器之后同样有收益。

4. **大 body 为什么越过了 payload guard：已查清，是当前设计，不是故障。** 见 [§5.3](#53-大-body-为什么越过-payload-guard)。日志里有 19MB 的上游 body，Kiro 本地路径比较的不是 `payloadGuardMaxBytes`（460800），而是 `payloadGuardKiroMaxWeight`（默认 1,300,000，ASCII 计 1、非 ASCII 计 8）。

5. **部署侧兜底。** 给 kiro-rs 容器加 `cpus` 和 `mem_limit`，避免它冲高时拖垮同机的 sub2api 和 postgres。这只能限制影响范围，不解决问题。

## 8. 未验证或遗留的点

- 线上换分配器后的实际收益还没测，需要在 59138 灰度时补数据。开发机上的对照见第 6 节，量级可以参考，比例不代表线上。
- 端到端对照是单机、mock 上游、aarch64 虚拟机，CPU 时长取的是容器进程；线上是 x86_64 物理机、真实上游、与其它容器争抢 8 核，绝对值会不同。
- 零帧 `protocol_error` 本身的上游原因没有分析。
- 二进制是 strip 的，musl 函数是根据反汇编模式认的（`__lock` 的自旋 + futex 结构），没有符号表佐证。
- 更完整的证据包（逐秒 CPU、perf 原始数据、按线程统计、日志、采集脚本、离线 bench 源码）在分支 `docs/cpu-musl-malloc-root-cause-20261104` 的 `docs/analysis/kiro-rs-cpu-musl-malloc-root-cause-20261104/` 下。该分支包含生产主机地址和实例路径，没有合入公开仓库。
- 本次排查在深圳和 59138 两台宿主机都装了 `linux-perf`，用完可以 `apt-get remove linux-perf` 卸掉。
