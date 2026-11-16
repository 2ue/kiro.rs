# 任务 s2b：恢复被误删的通用测试（阶段 2 补救）

## 背景

s2 任务在删除 Kiro 时，把 `src/external_pool/tests.rs` 与 `src/anthropic/handlers/tests.rs` 中一批**与 Kiro 无关**的测试一并删掉了。这些测试覆盖的是账号池调度、流式换号、early_commit、路由策略矩阵、thinking 功能、注入开关等保留功能，是后续阶段的回归依据，必须恢复。

## 目标

把下面两个清单里的测试从 `git show dac1e409:<file>`（阶段 1 提交，Kiro 仍在、测试完整；当前 HEAD 是 15ede804 阶段 2 提交）恢复到当前文件，按新代码做最小适配后全部通过。**不放宽断言**；只允许因字段/类型删除做机械适配。

- `scripts/refactor/tasks/s2b-restore-tests.external.txt`：`src/external_pool/tests.rs` 中要恢复的函数名（含辅助函数）。
- `scripts/refactor/tasks/s2b-restore-tests.handlers.txt`：`src/anthropic/handlers/tests.rs` 中要恢复的函数名（含辅助函数）。

## 适配规则

- `route.direct_policy_reason = Some(...)`、`route.local_attempted`、`route.local_preflight`、`route.local_attempts`、`route.fallback_reason`：这些字段已删，相关赋值行直接删除。
- `UsageRouteSubtype::ExternalDirectPolicy` / `ExternalFallback*` / `Local*`：统一改为 `UsageRouteSubtype::External`。
- 断言 `record.local_attempted == Some(false)`、`record.fallback_reason.is_none()` 之类针对已删字段的断言行删除，其余断言保留。
- 测试里构造 `AppState` / router 的辅助函数若引用了 `with_kiro_provider`、`extract_thinking`、`file_store`、`tool_format_debug_recorder`，改为当前 `AppState` 的构造方式（看 `src/anthropic/middleware.rs` 与现存的 `handlers/tests.rs` 里已经适配好的辅助函数）。
- 入口测试里原来通过 fake Kiro 上游验证的行为，现在用 fake Anthropic 上游（`external_pool/tests.rs` 已有 `spawn_*` 一类的 mock server 辅助函数）。如果某个测试本质上只能靠 Kiro 事件流验证（例如断言 EventStream 帧），把它从清单里剔除并在汇报中列出原因；这种情况预计很少，因为清单已按函数体依赖筛过。
- 两个 `_for_five_rounds` 测试与高并发测试运行时间长，保持原样，不要缩减轮数。
- `early_commit_*` 测试依赖的 `external_stream_config_for_test`、`external_sse_*`、`forward_early_commit`、`parse_downstream_sse`、`wait_for_usage_record` 等辅助函数也在清单里，一并恢复。

## 另外修复

- `src/anthropic/handlers/tests.rs::parse_messages_payload_rejects_empty_model_before_routing`：s2 把它改成了 `to_response(&envelope::request_id())` 但断言 `request_id` 以 `req_01` 开头失败，说明 `error_response_with_id` 用的是传入的 id 还是别的。查 `envelope.rs`，让测试按原意通过（原来通过 `parse_messages_payload(&body)` 这个测试辅助函数，该辅助函数若被删就恢复它）。
- `src/anthropic/transcript_sanitizer.rs`：上一轮已修复，32 个测试全部通过，无需再动。
- 上一轮结束时全量 1055 个测试已全部通过，上述“另外修复”中除恢复 `parse_messages_payload` 辅助函数外的问题都已不存在；重点只在恢复清单里的测试。

## 允许改动的文件

`src/external_pool/tests.rs`、`src/anthropic/handlers/tests.rs`、`src/anthropic/transcript_sanitizer.rs`（只允许改测试与搬运的函数，不改清洗算法）、`src/anthropic/envelope.rs`（只允许改测试）、`src/model/config.rs`（只改上述两个断言）、`src/storage/postgres.rs`（只删/修上述两个测试）。如需在非测试代码里加 `#[cfg(test)]` 辅助函数可以，但不得改生产逻辑。

## 自检

```
cargo fmt --all
cargo test --bin kiro-rs -q -- golden_usage
cargo test --bin kiro-rs -q -- external_pool::tests
cargo test --bin kiro-rs -q -- anthropic::handlers::tests
cargo test --bin kiro-rs -q -- anthropic::transcript_sanitizer
cargo test --bin kiro-rs -q 2>&1 | tail -30
grep -c "#\[test\]\|#\[tokio::test" src/external_pool/tests.rs     # 应 ≥ 380
grep -c "#\[test\]\|#\[tokio::test" src/anthropic/handlers/tests.rs  # 应 ≥ 85
```
