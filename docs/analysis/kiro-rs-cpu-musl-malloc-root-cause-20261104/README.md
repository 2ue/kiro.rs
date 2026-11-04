# kiro-rs 高并发 CPU 打满根因分析：musl malloc 全局锁竞争（2026-11-04）

本文和同目录的证据、脚本一起构成完整的分析包，不依赖任何聊天会话也能看懂和复现。源码引用都是相对 kiro.rs 仓库根目录的路径，对应提交 `73bae2e7`（v0.0.237）。时间不特别说明时都是北京时间（+08:00），日志里的时间戳是 UTC。

## 0. 结论摘要

- **现象**：深圳机器 59137 在约 400 rpm 时整机 CPU 到 90% 以上。kiro-rs 单进程短时最高约 6.5 核，而且忽高忽低，sys 态占比异常高。87 机器 59138 接流量后也出现同样问题，而且更严重。
- **根本原因**：kiro-rs 镜像基于 alpine，静态链接 musl libc，代码里没有指定 `#[global_allocator]`，所以用的是 musl 自带的 mallocng 分配器。mallocng 所有线程共用一把锁 `__malloc_lock`。kiro-rs 每次上游发送都要对 MB 级的 JSON 请求体做多次 DOM 解析和整串复制，产生海量小块分配。8 个 tokio worker 线程同时在这把锁上排队：用户态自旋失败后进 futex 睡眠和唤醒，释放内存时又频繁 madvise/munmap，引发跨核 TLB 刷新。结果 CPU 主要花在加锁、内核调度和 TLB 上，不在业务逻辑上。
- **关键证据**：
  - 两台机器、同一个二进制（sha256 `2520521f…`）。perf 显示 CPU 有 38%~55% 落在 musl 的分配器和锁函数里，内核又占 15%~43%，业务和依赖库只占 16%~30%。
  - futex 系统调用最高约 135 万次/秒，100% 来自 musl 的 `__lock`/`__unlock`。
  - CPU 和每秒请求数、请求字节数、重试次数基本不相关（|r| ≤ 0.36）。
  - 离线对照：同样的 body 处理代码，只换成 mimalloc，4 线程吞吐提高约 5.8 倍，futex 从 593 万次降到 6 次。
- **不是根因的东西**：零帧重试、大请求体、每次发送重复处理 body，都是真实存在的放大因素，它们决定了分配量有多大。但在分配器没有全局锁的情况下，同样的分配量不会把 8 核打满（见 §5.6）。
- **修复**：代码里加 `#[global_allocator]` 换成 mimalloc，然后重新打镜像。Dockerfile 不用改。先在 59138 灰度。重试次数和 body 重复处理作为第二阶段优化（见 §8）。

## 1. 背景与现象

| 时间 | 事件 |
|---|---|
| 11-04 15:44 | 把 sub2api 中 59137 的 28 个 kiro-rs 账号复制为 59138（87 机器），新池优先级更高，流量逐步迁往 87 |
| 11-04 约 16:00 | 深圳机器在约 400 rpm 时整机 CPU 90%+，用户反馈"打爆了" |
| 11-04 16:02-16:13 | 第一轮分析（会话 a6911875），结论是重试放大和 body 处理过重，见 §6 的核对 |
| 11-04 16:17-16:31 | 第二轮：深圳 perf 采样，定位到 musl 锁，做离线对照实验 |
| 11-04 16:38-16:41 | 87 首次复核，现象一致 |
| 11-04 17:05-17:07 | 两台机器同时间窗口完整采集原始证据（本包 `evidence/shenzhen-59137`、`evidence/87-59138`） |

现象细节（来源：`evidence/*/11-per-second-cpu.txt`、`20-threads-10s.txt`，以及 `evidence/shenzhen-59137-first-pass/`）：

- kiro-rs 是机器上最大的 CPU 消耗方。深圳同机的 sub2api、caddy、postgres 加起来不到它的零头。
- 逐秒 CPU 波动极大。深圳 120 秒窗口内平均 106%（100% = 1 个核），中位只有 26%，最高 617%。87 平均 285%，最高 642%。
- sys 态很高，峰值秒里 sys 超过 user。例如深圳某一秒 user 167% / sys 450%，87 某一秒 user 182% / sys 460%。
- 内存不紧张。RSS 深圳 460~980MB，87 约 600~840MB，历史峰值约 2.1GB。
## 2. 环境与配置

| 项 | 深圳（59137） | 87（59138） |
|---|---|---|
| 主机 | 152.53.243.159，8 核 / 16GB，内核 6.12.94 | 152.53.242.87，8 核 / 16GB，内核 6.12.107 |
| 镜像 | `ghcr.io/2ue/kiro-rs:latest`，即 `sha256:8e80bf81…7f349`，v0.0.237 | 同左 |
| 运行中二进制 sha256 | `2520521ffbd9760df354f79466188434f659b3adbed89ed85eac10eff0ee33cb` | 同左（逐字节相同） |
| 容器资源限制 | 无 cpu / 内存限制 | 无 |
| 进程线程数 | 20 | 22 |

构建方式（`Dockerfile`、`Cargo.toml`）：

- 编译镜像是 `rust:1.92.0-alpine3.23`，目标 `x86_64-unknown-linux-musl`，运行镜像是 `alpine:3.23`，二进制静态链接。
- Rust 编译 musl 目标时，链接的是工具链自带的 `self-contained/libc.a`，即 musl 1.2.5，分配器是 mallocng。
- `Cargo.toml` 里设置了 `[profile.release] lto = true`、`strip = true`，所以二进制没有符号表。
- 代码里唯一的 `#[global_allocator]` 在 `src/http_client.rs:34`，属于 `allocation_probe` 模块。这个模块有 `#[cfg(test)]`（`src/http_client.rs:19`），只在测试时编译。**release 构建用的是 musl 默认 malloc。**

和请求处理相关的运行配置，取自各机 `config/config.json`：

- `compression.enabled = true`，`whitespaceCompression = true`
- `payloadGuardMaxBytes = 460800`，`payloadGuardMode = preemptive`
- `kiroUpstreamStreamRetryMaxAttempts` 未配置，使用默认值 6（`src/model/config.rs:5935`）

## 3. 证据包结构

```
README.md                               本文
evidence/
  shenzhen-59137/                       深圳，2026-11-04 17:05:06-17:07:06 同窗口采集
  87-59138/                             87，同一时间窗口采集
    00-environment.txt                  主机、镜像、二进制 sha256、进程内存、全部容器 CPU
    10-app.log                          采样窗口内的 kiro-rs 日志（去颜色，邮箱替换为 <email>）
    11-per-second-cpu.txt               120 秒逐秒 utime/stime（tick，100 tick/s = 1 核）、缺页、RSS
    20-threads-10s.txt                  10 秒内每个线程的 user/sys
    30-perf-stat-10s.txt                10 秒系统调用 / 缺页 / 上下文切换 / TLB 刷新计数
    40-perf-cpu.data.gz                 30 秒 perf CPU 采样原始数据（199Hz，带调用栈）
    41-perf-cpu-flat.txt                上面数据的平铺报告（按地址）
    42-perf-cpu-callgraph.txt           调用图报告
    51-perf-futex-flat.txt              futex 系统调用的发起地址分布
    60-load-correlation.txt             analyze-load.py 输出：CPU 和请求量 / 字节 / 重试的相关性
    70-musl-symbolized.txt              symbolize-musl.py 输出：把热点地址还原为 musl 函数名
  shenzhen-59137-first-pass/
    transcript-excerpts.txt             深圳 16:17-16:31 首轮测量的命令和原始输出
    prior-session-a6911875-excerpts.txt 第一轮分析（a6911875）的命令和输出，供 §6 核对
  87-59138-first-check/
    transcript-excerpts.txt             87 在 16:38 首次复核的命令和原始输出
  offline-bench/results.txt             离线对照实验的原始输出
musl-symcheck/
  rust-1.92.0-self-contained-libc.a.gz  从编译镜像里取出的 musl libc.a（gzip），就是链接进生产二进制的那份
  *.lo.dis / *.lo.nm                    musl __lock、malloc、free、realloc 的反汇编和符号表
  musl-version.txt, rustc-version.txt, alpine-release.txt
scripts/
  collect-evidence.sh                   只读证据采集脚本，evidence/*-59137、*-59138 由它生成
  analyze-load.py                       CPU 和日志的对齐与相关性分析
  symbolize-musl.py                     strip 后二进制的 musl 函数识别
bench/                                  离线对照实验源码（Cargo 项目，带 Cargo.lock）
```

`first-pass` 和 `first-check` 两个目录是早期测量。当时的原始文件在临时目录里，已被清理，这里保存的是当时命令输出的逐字副本。17:05 同窗口采集的完整原始数据可以完全替代它们，保留下来是为了说明结论在不同时段都成立。
## 4. 根本原因

### 4.1 因果链

```
请求体为 MB 级 JSON（中位约 230-330KB，P90 约 1.1-1.6MB，最大 18MB）
  └─ 每次上游发送都对整包做多次 serde_json DOM 解析、序列化和整串复制（§7）
       └─ 产生海量小块堆分配和释放
            └─ musl mallocng：所有线程共用一把锁 __malloc_lock（malloc.lo 引用 __lock/__unlock）
                 └─ 8 个 tokio worker 同时分配 → 锁竞争
                      ├─ 用户态：__lock 中 lock cmpxchg 自旋 10 次失败
                      ├─ 内核：futex 等待和唤醒（最高约 135 万次/秒），调度器自旋锁、上下文切换
                      └─ 释放大块时 madvise/munmap 还页 → 跨核 TLB 刷新 IPI、再次缺页
                           └─ CPU 主要花在锁和内核上；竞争是非线性的，所以忽高忽低
零帧换号重试（每个失败请求最多 6 次）、大 body → 放大分配总量（放大因素，不是根因）
```

### 4.2 证据一：CPU 花在 musl 分配器和锁里

来源：`evidence/*/41-perf-cpu-flat.txt`（原始）、`70-musl-symbolized.txt`（识别结果）。17:05 同窗口，每台采样 30 秒、199Hz。

| CPU 归属 | 深圳 59137 | 87 59138 |
|---|---|---|
| musl libc（分配器、锁、memcpy） | **55.4%** | **37.6%** |
| 内核 | 15.0% | **41.6%** |
| 应用代码 + Rust 依赖库 | 29.7% | 22.0% |

musl 内部的函数明细：

| 函数 | 作用 | 深圳 | 87 |
|---|---|---|---|
| `alloc_slot` | mallocng 分配槽位 | 17.2% | 6.9% |
| `__lock` | musl 内部锁的加锁（自旋 + futex 等待） | 13.3% | 10.4% |
| `__libc_free` | 释放 | 12.2% | 5.1% |
| `get_meta` | mallocng 查元数据 | 3.4% | 4.0% |
| `__unlock` | 解锁（futex 唤醒） | 3.7% | 4.0% |
| `memcpy` | 整串复制 | 4.0% | 3.7% |

87 内核占用最高的几项（`41-perf-cpu-flat.txt`）：

- `__pv_queued_spin_lock_slowpath` 9.65%
- `asm_sysvec_call_function` 3.15%（跨核 IPI，主要是 TLB 刷新）
- `futex_wake` 2.15%
- `_raw_spin_lock` 1.68%

这几项都是 futex 唤醒、调度器加锁和 TLB 刷新的开销。并发越高，竞争越会从用户态自旋扩散到内核里的自旋锁，所以 87 的内核占比比深圳高。

**函数名是怎么来的**：生产二进制被 strip 了，perf 只能给出地址。`scripts/symbolize-musl.py` 的做法是：

1. 读取 `musl-symcheck/rust-1.92.0-self-contained-libc.a.gz`（解压后使用）。这份 libc.a 取自编译镜像，Rust 编 musl 目标时链接的就是它，musl 1.2.5。
2. 拿 1222 个函数各自的机器码，重定位字段当通配符，在生产二进制的 `.text` 段里逐字节搜索。
3. 337 个函数唯一命中，得到它们的地址范围。例如 `__lock` 在 `0x22964a0-0x2296552`，`__unlock` 在 `0x2296552-0x229658c`。

这是机器码逐字节比对，不是靠读指令猜的。`musl-symcheck/malloc.lo.nm` 显示，`__libc_malloc_impl` 所在目标文件引用了 `__lock`/`__unlock`，并定义了 `__malloc_lock`，可以证实这把锁就是 malloc 的锁。

### 4.3 证据二：系统调用以 futex 为主，全部来自 musl 锁

来源：`evidence/*/30-perf-stat-10s.txt`、`51-perf-futex-flat.txt`、`shenzhen-59137-first-pass/transcript-excerpts.txt`。

| 10 秒计数 | 深圳 16:27（首轮） | 深圳 17:05 | 87 16:38（首次） | 87 17:05 |
|---|---|---|---|---|
| futex | 2,658,648 | 118,814 | 9,066,048 | **13,535,600** |
| madvise | 25,401 | 14,961 | 31,293 | 32,139 |
| mmap / munmap | 2,999 / 2,948 | 2,751 / 2,485 | 2,193 / 2,144 | 2,274 / 2,258 |
| page-faults | 187,766 | 69,505 | 209,381 | 188,124 |
| context-switches | 155,872 | 13,591 | 669,460 | **932,942** |
| tlb_flush | 67,510 | 29,594 | 117,842 | 126,524 |
| sendto（对照，网络发送） | 767 | 553 | 1,761 | 1,838 |

- 87 在 17:05 平均每秒约 135 万次 futex、9.3 万次上下文切换，网络发送却只有每秒 180 次左右，相差近 4 个数量级。
- futex 的发起地址（`51-perf-futex-flat.txt` 经 `70-musl-symbolized.txt` 识别）：深圳 `__unlock` 73.3%、`__lock` 22.4%；87 `__unlock` 75.9%、`__lock` 24.0%。**基本全部来自 musl 锁**，不是 tokio 或 std 的锁。
- 深圳 17:05 的 futex 数比 16:27 少很多，因为那个窗口的 CPU 前低后高（中位 26%），竞争集中在其中十几秒（见 `11-per-second-cpu.txt`）。这正说明竞争是突发的、非线性的。

### 4.4 证据三：CPU 和业务量不相关

来源：`evidence/*/60-load-correlation.txt`，由 `scripts/analyze-load.py` 把逐秒 CPU 和同一秒的日志事件对齐计算。

| Pearson r：CPU 对 | 深圳 | 87 |
|---|---|---|
| 当秒 + 前一秒发送字节 | 0.15 | -0.26 |
| 当秒发送次数 | 0.14 | -0.36 |
| 当秒重试次数 | 0.05 | -0.04 |
| 当秒缺页次数 | 0.18 | 0.07 |

CPU 最高的几秒里请求并不多。例如深圳最高的一秒 617%（user 167 / sys 450），那一秒只有 1 次发送、0.3MB。深圳 16:17 首轮 120 秒的结果也一样，r 分别是 0.04、-0.05、-0.04（见 `first-pass` 摘录）。

如果 CPU 是由"处理字节"直接消耗的，应该和字节数强相关。实际 CPU 和业务量脱钩，说明 CPU 花在随并发程度突变的锁竞争上。
## 5. 如何复现

### 5.1 线上取证（只读，不改服务）

前提：宿主机 root，并已安装 `linux-perf`（Debian 13：`apt-get install linux-perf`；两台机器本次都已装上）。

```bash
bash scripts/collect-evidence.sh kiro-rs-2ue-59138-app ./evidence/new-run 120
python3 scripts/analyze-load.py ./evidence/new-run > ./evidence/new-run/60-load-correlation.txt
```

脚本依次采集：环境信息 → 120 秒逐秒 CPU，结束后一次性导出同窗口日志 → 10 秒按线程统计 → 10 秒 `perf stat` → 30 秒 `perf record` → 10 秒 futex 调用点采样。总耗时约 3.5 分钟，对线上的开销是 perf 采样本身，大约百分之几。

最快的判断方法是只看 futex 每秒次数。正常的服务应该是每秒几千次级别：

```bash
PID=$(docker inspect -f '{{.State.Pid}}' kiro-rs-2ue-59138-app)
perf stat -p $PID -e syscalls:sys_enter_futex,context-switches,tlb:tlb_flush -- sleep 10
```

### 5.2 识别 strip 二进制里的 musl 函数

```bash
cp /proc/$PID/exe /tmp/kiro-rs.bin          # 拷出运行中的二进制
gunzip -c musl-symcheck/rust-1.92.0-self-contained-libc.a.gz > /tmp/libc.a
python3 scripts/symbolize-musl.py /tmp/kiro-rs.bin /tmp/libc.a \
  evidence/87-59138/41-perf-cpu-flat.txt evidence/87-59138/51-perf-futex-flat.txt
```

依赖 binutils（`readelf`、`objdump`、`ar`）。如果镜像换了 Rust 版本，需要从对应的 `rust:<ver>-alpine` 镜像里重新取 libc.a，路径在 `/usr/local/rustup/toolchains/<ver>-x86_64-unknown-linux-musl/lib/rustlib/x86_64-unknown-linux-musl/lib/self-contained/libc.a`。

### 5.3 离线对照实验：只换分配器

`bench/src/main.rs` 模拟一次上游发送对 body 的处理：两次 `serde_json::Value` 解析、一次插入字段后的序列化、两次整串复制，和 §7 的线上代码路径对应。

body 是 4MB 的 JSON，由大量中小字符串对象组成，结构接近线上的 `conversationState.history`。同一份代码分别用 musl 默认 malloc 和 mimalloc（`--features mimalloc`）编译，编译环境和生产镜像相同。

```bash
cd bench
docker run --rm -v "$PWD":/b -w /b -e CARGO_TARGET_DIR=/b/target rust:1.92.0-alpine3.23 sh -c \
  'apk add -q musl-dev && cargo build --release && cp target/release/allocbench bench-musl \
   && cargo build --release --features mimalloc && cp target/release/allocbench bench-mimalloc'
for t in 1 2 4 8; do time ./bench-musl $t 4 10; time ./bench-mimalloc $t 4 10; done   # 参数：线程数 body_MB 每线程次数
perf stat -e syscalls:sys_enter_futex,syscalls:sys_enter_munmap,tlb:tlb_flush ./bench-musl 4 4 10
```

### 5.4 对照实验结果

原始输出见 `evidence/offline-bench/results.txt`。在深圳机器上以 `nice -n 19` 运行，当时线上还有负载，所以绝对值有噪声。

| 线程 | musl 吞吐（次/秒） | mimalloc 吞吐 | 倍数 | musl user+sys | mimalloc user+sys |
|---|---|---|---|---|---|
| 1 | 13.5 | 29.9 | 2.2× | 0.71s | 0.36s |
| 2 | 20.4 | 46.1 | 2.3× | 1.77s | 0.86s |
| 4 | 12.9 | 75.2 | 5.8× | 9.34s | 2.00s |
| 8 | 18.8 | 106.4 | 5.7× | 21.9s | 4.25s |

4 线程各跑一轮的系统调用：

| | futex | munmap | tlb_flush |
|---|---|---|---|
| musl | 5,928,829 | 34,784 | 305,958 |
| mimalloc | 6 | 10 | 2,183 |

### 5.5 实验说明了什么

- musl 从 2 线程加到 4 线程，吞吐反而下降，CPU 时间却翻了 5 倍。这是全局锁竞争的典型特征。mimalloc 则随线程数基本线性增长。
- 同样的工作量，musl 在 4 线程时会产生近 600 万次 futex，mimalloc 只有个位数。
- musl 下 sys 时间和 user 时间接近，和线上 "sys ≈ user" 的现象一致。

### 5.6 为什么放大因素不是根因

实验里两组的分配量完全相同（同样的 body、同样的解析和复制次数），只换了分配器，CPU 就降到约 1/5，futex 基本消失。

所以同样的重试次数和 body 大小，在不带全局锁的分配器上不会把 8 核打满。放大因素决定了"要分配多少"，根因决定了"每次分配有多贵、能不能并行"。
## 6. 对第一轮分析结论的核对

第一轮分析（16:02-16:13，命令和输出见 `evidence/shenzhen-59137-first-pass/prior-session-a6911875-excerpts.txt`）认为原因是"重试放大 + body 处理过重 + 大请求不均匀"。逐条核对如下：

| 第一轮的说法 | 核对结果 | 依据 |
|---|---|---|
| kiro-rs 是主要 CPU 消耗方 | 属实 | `00-environment.txt` 的 docker stats |
| 零帧失败触发整包换号重试 | 属实，是放大因素 | 17:05 窗口：深圳 366 次发送里 120 次是重试，87 是 1148 次里 298 次；原因几乎都是 `protocol_error`（上游 eventstream 一帧都没有） |
| 每次发送都重新处理整份 body，没有缓存 | 属实，是放大因素 | 见 §7 源码 |
| CPU 尖峰对应大 body 和重试集中的那几秒 | **不成立** | §4.4，相关系数 \|r\| ≤ 0.36，最高 CPU 的几秒只有 1 次发送 |
| musl malloc 在多线程下慢（当时只是推断） | **属实，而且是根因** | §4.2-4.3、§5.4 |
| `CountingAllocator` 可能进了 release | 不成立 | `src/http_client.rs:19` 有 `#[cfg(test)]` |
| 大 body 卡住 tokio worker 是抖动原因 | 未证实，不是主因 | 应用代码只占 16%~30% CPU |
| 零帧失败率和 body 大小无关 | 当时的分桶结果支持这个说法（7%~23%），本次没有重新验证 | 摘录中 `zero_frame_retry` 分桶 |

第一轮的问题在于把**放大因素当成了根因**。重试多、body 大、重复处理，确实都会增加分配量，但它们没有解释"为什么同样的分配量能把 8 核打满，而且 CPU 和业务量不相关"。

## 7. 放大因素：每次发送都重复处理整份 body（源码）

一个请求每发送一次（首发或换号重试）都会执行下面这些步骤，没有按请求缓存。

### 7.1 流式重试前换会话 ID

`src/anthropic/handlers.rs:11636` 调用 `src/kiro/provider.rs:1192` 的 `refresh_kiro_request_identity`。实际只是替换 `conversationId` 和 `agentContinuationId` 两个 36 字节的 UUID，代码却做了这些：

1. `serde_json::from_str::<Value>` 把整包解析成 DOM
2. `body.to_string()` 整包复制一次
3. `matches().count()` 整包扫描
4. `replacen` 再整包复制，每个 key 一次
5. 调用方 `Arc::<str>::from` 再复制一次

### 7.2 每次发送的处理

发送循环在 `src/kiro/provider.rs:13208`，处理代码在 `:13339` 起。

- `endpoint.transform_api_body` → `src/kiro/endpoint/ide.rs:209` 的 `transform_ide_api_body`。凭据带 profileArn 时，会整包解析成 `Value`，插入字段后再整包序列化。
- `src/http_client.rs:161` 的 `maybe_compress_json_whitespace`：扫描一遍，用 `IgnoredAny` 完整校验一遍，再压缩一遍。
- `src/kiro/provider.rs:435` 的 `should_log_upstream_body_size_at_info`：开启压缩时总是返回 true。它内部调用 `kiro_payload_weight`（`src/anthropic/payload_guard.rs:1584`，逐字符遍历），日志字段又在 `:13356` 再算一遍。

### 7.3 规模

17:05 窗口的数据（`60-load-correlation.txt`）：

| | 发送次数 | 发送体积合计 | 平均 |
|---|---|---|---|
| 深圳，2 分钟 | 366 | 257MB | 约 2.1MB/s |
| 87，2 分钟 | 1148 | 698MB | 约 5.8MB/s |

单个请求体最大 12.7MB。每 MB JSON 做 DOM 解析，都会产生数万到数十万次小分配。按上面的处理次数，每秒的分配次数在百万量级，正好落在 musl 全局锁最怕的场景里。

### 7.4 另一个未解决的现象

| | weight 超过 1,300,000 | 超过 `payloadGuardMaxBytes` 460800 |
|---|---|---|
| 深圳 | 74 / 381 | 173 / 381 |
| 87 | 126 / 1172 | 426 / 1172 |

请求在超出 payload guard 阈值的情况下仍然发了出去。原因没有查清，列入 §9。
## 8. 解决方案

### 8.1 第一阶段：更换全局分配器（根因修复）

**为什么必须改代码、重新打镜像。** 分配器在编译时就静态链接进了二进制，下面这些办法都无效：

- musl mallocng 没有可调的环境变量；
- 静态二进制不读 `LD_PRELOAD`；
- 只改 config.json 或 docker-compose 都不会改变分配器。

**代码改动，两处：**

```toml
# Cargo.toml → [dependencies]，固定版本
mimalloc = "=0.1.46"
```

```rust
// src/main.rs 顶部（mod 声明之后）
// musl 自带 malloc（mallocng）用全局锁，高并发下 CPU 大量耗在 futex 上，
// 见 docs/analysis/kiro-rs-cpu-musl-malloc-root-cause-20261104/README.md。
#[cfg(not(test))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
```

必须加 `#[cfg(not(test))]`。测试构建里 `src/http_client.rs:34` 已经有一个 `#[global_allocator]`（`CountingAllocator`），一个程序里不能有两个。`CountingAllocator` 内部调用的是 `System`，换成 mimalloc 后，测试里的分配统计逻辑不受影响。

**镜像：** Dockerfile 不用改。mimalloc 的构建脚本要编一小段 C 代码。`rust:1.92.0-alpine3.23` 里有 gcc，加上 Dockerfile 已经安装的 `musl-dev` 就够了。§5.3 的实验就是在这个镜像里编译成功的。新增依赖会让 Dockerfile 的 `dependencies` 缓存层重编一次，之后照常复用。按现有流程发版（`.github/workflows/docker-build.yaml`）。

**备选方案：** 改用 glibc 镜像（`debian:bookworm-slim`，目标 `x86_64-unknown-linux-gnu`）。glibc malloc 用的是每线程 arena，没有这把全局锁，但改动更大（运行时依赖、证书、体积都会变），效果一般也不如 mimalloc。不推荐。

### 8.2 上线步骤

1. 发出新版本镜像（比如 v0.0.238）。
2. 只升级 87 的 59138：`.env` 里 `KIRO_RS_VERSION` 改成新 tag，执行 `docker compose pull && docker compose up -d`。深圳 59137 保持 0.0.237，作为对照。
3. 高峰期用 `scripts/collect-evidence.sh` 再采一次，和本包 `evidence/87-59138/` 对比。

   | 指标 | 改前基线（87，17:05） | 预期 |
   |---|---|---|
   | futex | 1,353 万 / 10 秒 | 下降两个数量级以上 |
   | context-switches | 93 万 / 10 秒 | 显著下降 |
   | kiro-rs CPU | 平均 285%（sys 148%） | sys 显著下降 |
   | musl 的 CPU 占比（`symbolize-musl.py`） | 37.6% | 接近 0；mimalloc 的函数不在 musl 范围内，这一项会自然消失 |

4. 观察 RSS。mimalloc 按线程缓存内存，归还给系统比较懒，RSS 可能比现在高一些。建议同时给容器设 `mem_limit`（比如 3g，当前历史峰值约 2.1GB）。可选环境变量 `MIMALLOC_PURGE_DELAY=0`，让它更积极地归还内存。
5. 87 稳定一天后，再升级深圳。
6. 回滚：把 tag 改回 0.0.237，`docker compose up -d`。

### 8.3 第二阶段：减少放大因素（降低分配总量）

1. **限制零帧重试**。把 `kiroUpstreamStreamRetryMaxAttempts` 从默认 6 降到 2~3。这是运行配置，改完重启生效，不用重建镜像。第一轮统计里，被重试的请求平均要发 5.3 次，多数重试没能救回请求。
2. **body 每个请求只处理一次**（需要改代码）：
   - `refresh_kiro_request_identity` 改成定位到两个 UUID 后原地替换 36 字节，不做 DOM 解析，也不整串复制；
   - profileArn 注入改成字符串拼接；
   - 和凭据无关的压缩结果缓存成 `bytes::Bytes`，换号时只做引用计数克隆；
   - `kiro_payload_weight` 每次发送只计算一次，日志直接复用结果。
3. **部署兜底**：给 kiro-rs 容器设 `cpus` 和 `mem_limit`，免得它冲高时拖垮同机的 sub2api、postgres。这只能限制影响范围，不能解决问题。

## 9. 未验证 / 遗留

- 换成 mimalloc 后线上的实际收益还没测，要在 59138 灰度时按 §8.2 第 3 步补数据。
- 零帧 `protocol_error` 本身在上游是什么原因，没有分析。
- 请求体超出 payload guard 阈值仍被发出（§7.4），原因待查。
- `symbolize-musl.py` 唯一定位了 337 / 1222 个 musl 函数。`malloc`、`free`、`realloc` 等入口函数被 LTO 内联或者和其他函数相邻，没有唯一命中。perf 报告里出现的 `alloc_slot`、`__libc_free`、`get_meta` 属于唯一命中的那部分。没被识别的 musl 代码会算进"应用 + Rust 依赖库"，所以 musl 的真实占比只会比表里的数字更高。
- 两台宿主机都为这次排查安装了 `linux-perf`。不需要的话可以 `apt-get remove linux-perf`。
