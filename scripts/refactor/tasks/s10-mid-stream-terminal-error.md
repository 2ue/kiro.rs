# 任务 s10：首输出后上游断流/超时必须以脱敏 error 事件收尾，不能静默断流

## 背景

quick 验收矩阵（`scripts/loadtest/run-acceptance-matrix.sh quick`）19 个用例 18 个 PASS，唯一 FAIL 是 `mid_stream_resets`：mock 上游在流输出到一半时 `socket.destroy()`（`scripts/loadtest/anthropic-mock-upstream.mjs` 的 `midStreamResetRate`），53 次断流全部变成客户端侧 `stream_cut`：HTTP 200 已发出，下游收到部分 `content_block_delta` 后连接直接结束，**既没有 `message_stop` 也没有 `event: error`**。服务端日志对应 53 条 `error_class="external_stream_read_error"` + 53 条 `external_stream_response_error`。

设计契约 `docs/claude-pool-refactor/11-实施说明与关键逻辑契约.md` 3.7："首输出之后出现 error 事件：转发脱敏后的 error 事件并结束，不换号"。上游断流、idle 超时、lease 丢失在首输出之后发生时属于同一类"首输出后不可恢复"，应当同样以**脱敏 error 事件**收尾，让 Claude Code 这类客户端拿到可识别的终止错误（`overloaded_error` 可重试），而不是看到一条被掐断的流。main 上该场景的注释也是这么写的（`scheduler-scenarios.mjs`："the client sees overloaded_error, never a silently cut stream"）。

## 根因位置

`src/account/mod.rs` 普通流路径的 `futures::stream::unfold`（约 7590-7860 行）里有 4 个分支在首输出之后向下游 `return Some((Err(std::io::Error::other(...)), ...))`：

1. ~7713 `account stream event buffer exceeded limit`
2. ~7739 `external_stream_read_error`（本次失败的分支）
3. ~7799 `account lease coordination was lost`
4. ~7819 `model endpoint stream idle timeout after N seconds`

外层 `ExternalStreamUsageGuard` 的 unfold（约 10064-10110）把内层 `Err` 原样映射成 `Err` 并 `record_stream_error`。axum 遇到 body 流 `Err` 直接关连接，于是客户端看到断流。

`early_commit.rs` 已有正确模式：`ExternalEarlyCommitDownstream::send_terminal_error` 发一个 `event: error` 再结束。

## 修复（最小改动）

1. 在 `src/account/mod.rs` 新增一个函数（放在 `external_safe_processing_error_event` 旁）：

```rust
/// 首输出之后上游不可恢复（断流/idle 超时/lease 丢失/缓冲超限）时写给下游的终止事件。
/// error.type 用 overloaded_error：Claude Code 对它会重试整个请求；buffer 超限用 api_error（重试也无用）。
fn external_post_output_terminal_error_event(
    mask: &ExternalStreamErrorMask,
    error_type: &'static str,   // "overloaded_error" | "api_error"
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    detail: &str,               // 写进 capture.stream_error_message，不出现在下游文案里
) -> Vec<u8>
```
   - 文案用 `envelope::public_message_with_error_id(envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE, &mask.error_id)`，带 `request_id`，格式与 `maybe_mask_external_stream_error_event` 末尾一致。
   - 若 `capture` 的 `stream_error_message` 为 None 则设为 `detail`（这样外层 guard 的 `record_success` 会走 `StreamError` 分支并记录 `upstream_status_error`，不会被记成 Success，也不会计费）。
2. 上述 4 个分支改为：`drop(lease)`；构造事件字节；`return Some((Ok(Bytes::from(event)), (body_stream, None, Vec::new(), None, last_chunk_at, last_keepalive_at, true)))`。`finished=true` 保证下一次 poll 返回 `None`，流正常结束。分支 1 用 `api_error`，分支 2/3/4 用 `overloaded_error`。
3. 现有 `tracing::warn!` 保留；日志里加 `terminal_event = error_type`。
4. **首输出之前**的处理（`pre_read_external_stream_before_commit`、early_commit 路径）不动：那里按设计是换号重放。
5. `maybe_mask_external_stream_error_event` 对上游**显式** error 事件的处理不动（它已经发 `api_error`）。

## 回归单测（`src/account/tests.rs`）

参考 `account_stream_error_after_content_start_does_not_replay_request`（约 19539 行）与 `account_stream_pre_output_eof_read_error_and_idle_fail_over_to_next_pool`（约 19719 行）写：

- `account_stream_read_error_after_content_start_ends_with_overloaded_error_event`：fake 上游 `[message_start, text_start, text_delta("partial"), ExternalStreamFakeStep::error("socket reset")]`，第二个池是正常成功池。断言：`stream_error == None`（流干净结束）；body 含 `partial`；`parse_downstream_sse` 最后一个事件是 `error` 且 `error.type == "overloaded_error"`、message 含 error_id、不含 `socket reset`；没有 `message_stop`；备用池 `snapshot()==0`（不重放）；usage 记录 `status == StreamError`，`account_billing.is_none()`。
- `account_stream_idle_timeout_after_content_start_ends_with_overloaded_error_event`：fake `[message_start, text_start, text_delta("partial"), delay(1500ms), text_delta("late")]`，`external_stream_config_for_test()` 的 `stream_idle_timeout_secs: 1`。断言同上，另断言 body 不含 `late`。
- `account_stream_eof_after_content_start_without_message_stop_is_not_an_error_event`：fake `[message_start, text_start, text_delta("partial")]` 然后正常 EOF（无 error step）。这是上游"正常结束但没发 message_stop"的边界，当前行为是流正常结束无 error 事件——**保持现状**并用测试钉住（断言无 `error` 事件、`stream_error == None`），避免修复扩大化。
- 检查现有测试里是否有断言 `stream_error.is_some()` 或 `"external stream read error"`/`"idle timeout"` 文案落在首输出之后的用例；若有，按新行为改断言（断言 error 事件而不是 Err），并在汇报里列出。

## 验收脚本补一个单场景模式

`scripts/loadtest/run-acceptance-matrix.sh` 的 `case "$MODE"` 增加：

```
  scenarios)  # 只跑指定闭环场景：run-acceptance-matrix.sh scenarios mid_stream_resets long_stream_burst
    shift; SCHEDULER_CASES="$*"; OVERLOAD_CASES='' ;;
  overload)   # 只跑指定开环用例
    shift; SCHEDULER_CASES=''; OVERLOAD_CASES="$*" ;;
```
（`MODE=${1:-quick}` 之后 `shift` 要放在 case 内部；`OUT_DIR` 名字用 `$MODE`。空的 `OVERLOAD_CASES` 时跳过 `overload-verdict.py`。）

## 允许改动的文件

`src/account/mod.rs`（只改上述 4 个分支与新增函数，不改调度选择、不改 usage 投影链）、`src/account/tests.rs`、`scripts/loadtest/run-acceptance-matrix.sh`。**不得改** `src/anthropic/**`、`src/account/usage_projection.rs`、`src/account/golden_tests.rs`、`tests/golden/**`。

## 自检

```
cargo fmt --all
cargo build -q --release --bins
cargo test --bin claude-pool -q -- after_content_start
cargo test --bin claude-pool -q -- golden_usage
cargo test --bin claude-pool -q 2>&1 | tail -3
bash -n scripts/loadtest/run-acceptance-matrix.sh
KEEP=0 scripts/loadtest/run-acceptance-matrix.sh scenarios mid_stream_resets long_stream_burst connection_resets
```
最后一条必须 `总结果：PASS`，把 SUMMARY.md 路径写进汇报。注意用的是 release 二进制 `/tmp/kiro-rs-rust-analyzer-target/release/claude-pool`，所以 `cargo build --release` 必须在跑矩阵之前完成。
