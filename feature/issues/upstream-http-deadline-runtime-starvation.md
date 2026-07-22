# Upstream HTTP Deadline Under Runtime Starvation

Status: `focused-unit-provider-pass / frozen-load-pending`

Severity: P1 latency and failure-classification correctness

Last updated: 2026-07-18

## 问题、现象与影响

配置的 Kiro 上游 response-header/body timeout 在 Tokio executor 高负载或长时间未调度时可能失去严格 wall-clock 语义。已配置 1 秒 header timeout 的请求，在完整单测树压力下于 1.651 秒收到 fake upstream 的 HTTP 500，并被记录为 `server_error/status=500`；同一请求的前三次 attempt 则在约 1.001 至 1.003 秒正确记录为 `upstream_timeout/status=None`。

用户可见结果可能是首字延迟超过配置上限、错误类型和 cooldown 原因随调度时序变化、迟到响应继续触发重试或账号健康逻辑。该问题没有 `Hashxxxxxxxx`、tool transcript、thinking 或图片指纹，也不要求长会话；其无指纹特征是 attempt duration 超过 timeout，但仍带有真实上游 status/body 分类。

影响面包括所有通过 `send_with_response_header_timeout`、`execute_with_response_header_timeout`、`response_bytes_with_limit_and_body_timeout` 和测试用 text body helper 的 Kiro API、MCP、profile/model discovery、OAuth/外部辅助调用。是否实际经过某个 helper 仍由各调用链决定，不能从本问题外推为所有网络操作都已覆盖。

## 源码链与根因

旧实现直接使用 `tokio::time::timeout(duration, future)`。Tokio 1.48 的 `Timeout::poll` 会先 poll 被包装 future，再 poll delay；当 executor 在 deadline 之后才恢复且 HTTP future 与 timer 都已 ready 时，迟到 HTTP 结果可以先返回。完整树中的 fake provider 在 1.5 秒返回 500，客户端 timeout 为 1 秒，最终第四个 attempt 在 1.651 秒接受了该 500，给出了确定性运行证据。

这不是 request 总 timeout 配置缺失，也不是 provider attempt budget 超卖。失败 outcome 仍精确消耗 4 个共享 inference send，前三个 timeout 和第四个 500 均在预算内。根因是同 poll 周期内的 ready 选择顺序，不是发送次数或账号数量。

## 复现方法

最小确定性复现使用一个已过期的 Tokio deadline 和一个立即 ready 的 future。旧 future-first 语义可返回 ready 值；修复后连续五轮必须全部返回 elapsed。正向对照使用未来一秒 deadline 和立即 ready future，连续五轮必须全部成功。

真实 HTTP 复现让 loopback server 接受连接但延迟 response headers，或先发 headers 后永久停住 body。header/body helper 必须分别返回 typed header/body timeout。provider 集成矩阵使用 6 类 transport/body fault、stream/non-stream、pool 1/20/60、每格 5 轮；header timeout 的 pool 20/60 outcome 每次真实消耗 4 个受共享预算约束的 send。

异常并发复现是完整 1714-test 默认 bin 树。它会让两个 provider fault matrix、client-cache、OAuth refresh、payload guard 百轮用例并发运行，能够制造 timer 与 1.5 秒 fake response 同时 ready 的条件。长会话不是这个时间竞争的必要维度；冻结验证仍需把相同 deadline case 与长 Claude CLI 会话、stream/non-stream 和 L3-L5 burst 同跑，确认其他工作负载不会引入新的超时路径。

## 候选与选定方案

- 仅把 fake server 的 1.5 秒延迟改成 5 秒：可降低测试竞态，但不修复生产 deadline 接受迟到响应，拒绝。
- 继续降低 provider matrix 的测试并发：会隐藏 executor 压力且不能改变生产语义，拒绝。
- 只依赖 reqwest client total timeout：不能为每个 header/body 阶段提供当前 typed boundary，也仍受异步 poll 时序影响，拒绝。
- 选定方案：公共 HTTP helper 使用同一 monotonic deadline 和 `tokio::select! { biased; timer first; ... }`。正常结果在 deadline 前 ready 时 timer 仍 pending；两者同时 ready 或 deadline 已过时必须优先返回 timeout。body read 也使用同一 helper，保持 header/body typed error 区分。

该方案不新增 HTTP 请求、账号扫描、retry 或后台任务。兼容性变化是明确的：过去可能接受的迟到响应现在按已配置 deadline 失败，这是配置语义修复。正常快速路径只增加一次 timer/select poll；聚焦完整 provider matrix 从修复前 243.67 秒变为 245.74 秒，约 0.85% 差异，当前没有可见性能回退证据，但这不是 release 负载结论。

## 验收矩阵与修复结果

- deadline 已过 + ready future：`5/5` timeout-first。
- deadline 未过 + ready future：`5/5` success。
- 真实 loopback header stall：`1/1` typed header timeout，1.00 秒。
- 真实 loopback body stall：`1/1` typed body timeout，1.01 秒。
- provider transport/body matrix：6 类 x 2 stream mode x 3 pool size x 5 轮，`1/1` 外层通过，245.74 秒；send/attempt、status/class、cooldown、隐私 marker 全部保持原断言。
- deadline 修复后的 `r11`：`1714 tests / 1708 passed / 0 failed / 6 explicit ignored`；后续当前 `r12`：`1721 tests / 1715 passed / 0 failed / 6 explicit ignored`，测试 351.96 秒、wall 581.7 秒。
- Rust 1.92.0 `cargo check --all-targets`：零 warning。

完整红绿、命令、scope 大小和自动清理见 [HTTP deadline evidence](../evidence/http-deadline-runtime-starvation-20260718.md) 与 [完整单测树证据](../evidence/full-unit-tree-red-green-20260718.md)。

## 残余风险与回滚

当前证据来自 dirty-tree debug unit/provider loopback，不是冻结 release service。仍需对最终 SHA 运行真实临时 HTTP upstream、stream/non-stream、长 Claude CLI、CPU/IO 压力、500 并发低 RPM、mixed timeout/500/recovery，并记录 p95/p99、attempt amplification、RSS、FD 和恢复率。还需静态列出未使用公共 helper 的网络路径，不能声称所有 timeout 都自动变为严格 deadline。

回滚不能恢复 future-first 迟到响应。若 biased select 在冻结负载中出现可证实的正常请求误超时，应修正 deadline 建立位置或阶段预算，而不是延长测试 server 或放宽 status 断言。
