# Runtime Stack Overflow And Handler Future Size

Status: `debug-only-stack-threshold-confirmed / current-default-unit-tree-pass / release-http-load-pending`

Severity: P2 test-infrastructure; upgrade to a production release blocker only if the frozen release HTTP gate reproduces an abort

Last updated: 2026-07-18

## 问题、现象与影响

真实 Router + reqwest fake-upstream 的 handler fault fixture 在未优化 debug test binary 中使用约 2 MiB 工作线程栈时发生进程级 stack overflow。该失败发生在测试进程内，不能被 Rust panic 捕获。相同源码的旧 release test binary 在显式 2 MiB Tokio worker 上完成了最小 case 和当时的完整 35-case precommit matrix，因此现有证据不支持把它继续标成已证实的生产 P0/P1。

它仍然是有效的测试基础设施问题：默认 debug 测试若直接承载完整 Router/handler/reqwest 调用链会 abort，导致协议故障矩阵无法给出业务断言。当前重型 debug fixture 因此在独立 4 MiB OS thread + current-thread Tokio runtime 中运行；这只是隔离测试构造的工程措施，不是生产 runtime 配置变更，也不是最终生产安全证明。

## 已确认的根因边界

固定旧 debug test binary 的离散阈值结果为：

| `RUST_MIN_STACK` | 结果 |
| --- | --- |
| 1 MiB | signal 6 / stack overflow |
| 2 MiB | signal 6 / stack overflow |
| 4 MiB | pass |
| 8 MiB | pass |
| 16 MiB | pass |

最小触发已缩到单个 HTTP 200 JSON exception、一轮，2 MiB debug 仍 abort，因此不是 16 MiB response body 在栈上分配导致。静态 future 大小为：case future 576 B、handler call future 472 B、upstream start future 144 B；这排除了“future 对象自身接近数 MiB”，但不能测量未优化 poll/调用链的瞬时栈深。

旧 release test binary 的反证：

- 路径：`target/release/deps/kiro_rs-8e21067b2ccc5c02`
- SHA-256：`4cf63c759a39d1f1987dbdf7ecc0b1da3bca3c7c622d7902cc2a7d9d51e96d15`
- 基线 revision：`401473ca1649997bdeccf4468e3add1bdb187248` 加当时 dirty remediation tree
- 最小 release case：2 MiB Tokio worker，`1/1` 通过
- 当时完整 release precommit matrix：2 MiB Tokio worker，外层 `1/1`、内部 `35/35` 通过

以上 SHA 是该路径在 16:14 的历史内容，之后被新构建覆盖。另有一次外层 `RUST_MIN_STACK=1MiB` 的运行只证明 1 MiB libtest thread 可以驱动测试内部显式创建的 2 MiB worker，不能声称 1 MiB worker 通过。

当前 checkpoint 在同一路径重新构建后的身份与结果：

- 构建完成：2026-07-16 17:23:59 +0800；release 编译耗时 9 分 01 秒。
- 路径：`target/release/deps/kiro_rs-8e21067b2ccc5c02`。
- 大小：28,293,904 bytes。
- SHA-256：`3b7825c33ff1c4fde3d3856a239852af7f36882f14bcf22a7d4ff7b168243a2e`。
- 显式 2 MiB Tokio worker、bad CRC 真实 handler precommit retry：`1/1` 通过。

该结果关闭“provider/handler 所有权调整后 release-only 单元尚未重跑”的缺口；它仍是 dirty checkpoint test binary，不是最终 tag binary，也不替代真实 release HTTP/load。

## 当前测试实现

- debug 重型 precommit fixture：独立 4 MiB constructor/runner thread，current-thread Tokio runtime。
- unknown-only 在默认 debug libtest 栈的最终复核中再次稳定 abort；因此同一真实 Router 调用族的 postcommit、unknown/missing terminal、non-stream fault/privacy/limit 与 legacy 正控统一使用测试专用 4 MiB thread。修改后默认命令 unknown-only `5/5` 通过，未要求 CI 设置全局 `RUST_MIN_STACK`。
- release-only 回归：`#[cfg(not(debug_assertions))]`，显式 2 MiB Tokio worker，使用 bad CRC 走真实 handler precommit retry，而非 provider JSON retry。
- 单账号 dispatch-failure fixture：同样使用 4 MiB debug fixture thread；业务断言仍要求单次真实 send、0 重发、规范 SSE error 和 `streamRetryDispatchFailures=1`。
- `handler_precommit_retry_future_sizes_remain_below_four_mib` 保留静态尺寸哨兵，但不把它当成 poll stack 证明。

2026-07-18 的完整默认 bin 单测树又暴露了两个尚未使用 4 MiB fixture thread 的真实 Router 用例：remote multimodal 21-source admission 与 local non-stream shared-attempt commit。两者改用既有 helper 后，WebSearch 10 个过滤命中通过；后续 warning/deadline 修复后的 `r11` 为 `1708/1708` 非 ignored，加入 queue/storage/provider fixture 后当前 `r12` 为 `1715 passed / 0 failed / 6 ignored`，均未再出现 stack abort。完整红绿链、普通断言修复与 scoped build 清理见 [当前单测树证据](../evidence/full-unit-tree-red-green-20260718.md)。

同一完整树还暴露了 provider fault matrix 的测试资源峰值：两个矩阵各自 `join_all` 15 个独立 provider，并与全树其他 client-cache 压力并行，导致本应立即返回的 malformed-UTF8/HTTP-200 JSON error fixture 超过 30 秒。矩阵现在保留全部 cell 和断言，但测试内部最多四个 provider 在途；聚焦运行分别为 `141.73s` 和初始 `243.67s`。后续完整树又证明 future-first HTTP timeout 会在 executor 饥饿时接受迟到 500；deadline-first 修复后的同一 transport/body 矩阵为 `245.74s`，完整树通过。测试并发上限只属于测试组织；deadline-first 是独立的生产 HTTP 正确性修复。

## 复现方法

诊断 debug 阈值时应固定同一个 test binary 和最小 case，分别在 1/2/4/8/16 MiB 各启动独立进程，避免 Cargo 增量编译或并行 build lock 干扰结果。业务矩阵必须分别测小 JSON exception、坏 CRC、截断 frame、idle/read error 与 16 MiB non-stream body，不把 body 大小、future 大小和调用栈混为一个指标。

最终生产形态验证必须重新构建冻结 release binary，再启动隔离端口和独立 fake upstream，通过真实 HTTP 请求执行相同 fault matrix。记录 binary SHA、端口/PID、请求数、进程退出状态、RSS、FD、线程数、p95/p99 和恢复请求；不得连接受保护的 `127.0.0.1:9022`。

## 方案与取舍

- 不在生产 runtime 全局增大 worker stack：当前没有 production abort 证据，按线程增加栈预算会扩大虚拟内存和潜在 RSS 风险。
- 不为消除 debug 现象盲目 boxing 生产 handler：静态 future 很小，且旧 release 反证通过；只有 flamegraph/stack probe 证明具体同步 poll 链后才保留生产重构。
- 当前选用测试专用 4 MiB thread，让 debug 故障矩阵稳定产出协议断言，同时用 release-only 2 MiB worker 回归防止风险被隐藏。
- 若冻结 release HTTP 能复现，立即升级严重级别，并以拆分同步 poll 链、明确 heap 状态机或有证据的 boxing 修复；修复后必须在默认生产配置通过，而不是仅提高栈上限。

## 验收与残余风险

已完成的是 debug 阈值、最小触发、静态 future size、历史 release 反证、当前 checkpoint 的 release-only 2 MiB case，以及当前 dirty tree 的完整默认 bin 单测树。尚未完成的发布门禁是：最终 tag binary 再绑定同一 gate；隔离 release HTTP 至少 1,000 次 fault/normal 请求和三轮 burst；进程全程存活；RSS/FD/线程数在 idle 窗口回落；结果与 debug 业务矩阵一致。

在这些门禁完成前，只能结论为“目前证据指向 debug-only fixture stack depth，未发现 release 单元复现”；不能承诺生产绝无 stack overflow，也不能因 debug abort 阻止所有其他聚焦测试继续运行。
