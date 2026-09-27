# v0.0.174 发布回归计划与进度

记录时间：2026-09-27 21:51:09 CST  
执行范围：本地发布前回归、版本号提交、`v0.0.174` tag 发布。  
生产边界：不触碰远端生产机器、生产数据库、生产 Redis 或生产配置。

## 目标

- 在发布前重新验证本次 WebSearch 错误稳定性、Kiro 加权 payload guard、Admin/API/UI 配置传播没有引入回归。
- 使用仓库发布规则确认版本权威、远端 tag、工作提交、版本提交和发布 tag。
- 只有在回归门禁通过后才推送 `main` 与 tag。

## 发布模型

- 发布权威：`Cargo.toml` 的 `[package].version`。
- 当前版本：`0.0.173`。
- 当前基线 tag：`v0.0.173`。
- 目标版本：待远端 tag 复核后预计为 `0.0.174`。
- 目标 tag：待远端 tag 复核后预计为 `v0.0.174`。
- Docker 发布：`.github/workflows/docker-build.yaml` 在 `v*` tag 推送时触发，并通过 `scripts/ci/check-release-version.mjs` 校验 tag 与 Cargo 版本一致。

## 回归门禁

- 仓库状态/发布权威确认：通过；发布权威为 `Cargo.toml` `[package].version`。
- 构建产物守卫：通过；未发现未知 target、reservation 或 target 进程。
- 前端契约、UI、Admin UI：通过。
- Rust 格式、Clippy 基线、no-default/full all-targets 测试、release build：通过。
- WebSearch / payload guard 聚焦回归：通过。
- 本地协议/CLI 验证：通过；使用本项目指定实例 `127.0.0.1:19023`，不发送生产负载。
- 最终 diff/artifact/metadata 检查：通过；版本 bump 前 metadata 仍为 `0.0.173`，等待独立版本提交。

## 进度

- 2026-09-27 21:51 CST：读取发布、load/chaos、Claude CLI 验证、plan-tree 规则；确认 Cargo 命令必须走 scoped runner，发布必须业务提交和版本提交分离。
- 2026-09-27 21:51 CST：`origin/main` 与本地 HEAD 一致；当前工作区包含 WebSearch、P001 payload guard、UI/Admin 配置和分析文档变更。
- 2026-09-27 21:56 CST：第一轮无 Cargo 门禁中，产物守卫通过；UI/Admin UI 构建通过；前端契约发现 `CredentialDiagnosticsResponse` 只在主 UI 导出，已补齐 Admin UI 同名响应类型后准备重跑。
- 2026-09-27 22:02 CST：前端契约重跑通过，`185` 个共享类型一致；UI build、Admin UI build、`git diff --check` 通过。
- 2026-09-27 22:06 CST：Clippy 基线首次在 `rustc 1.92.0` 下发现 `src/anthropic/websearch.rs` 新增 `too_many_arguments` warning 桶，已将 WebSearch 请求/响应生成参数收敛为上下文结构体。
- 2026-09-27 22:12 CST：WebSearch 聚焦测试通过，`37 passed / 0 failed`；覆盖 recoverable tool-error、SSE/non-SSE 协议形状、空/陈旧 query 拒绝、usage 归属。
- 2026-09-27 22:14 CST：`cargo fmt --check` 与 Clippy 基线通过；Clippy `764 <= 849`，无新增 warning 桶。
- 2026-09-27 22:44 CST：`cargo test --locked --all-targets --no-default-features -- --test-threads=1` 通过：`kiro-rs 2187 passed / 0 failed / 6 ignored`，`kiro_loadtest 31 passed / 0 failed`。
- 2026-09-27 22:44 CST：`cargo test --locked --all-targets -- --test-threads=1` 通过：`kiro-rs 2187 passed / 0 failed / 6 ignored`，`kiro_loadtest 31 passed / 0 failed`。
- 2026-09-27 22:44 CST：release build 通过，冻结候选路径 `/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T//kiro-release-174.Cyj0nu/kiro-rs`；SHA-256 `24867938d793c94c0da21579369ed73b15d30c3a99c7c650c57ff6e5fa040ae7`。`kiro_loadtest` SHA-256 `800a1e8a68a8ca09198eb7599d30a1cedcf31fc8f7cce0d82e0a94dcad451e81`。
- 2026-09-27 22:45 CST：L1 fake upstream smoke 完成；`normal-stream`、`normal-non-stream`、`slow-first-byte`、`slow-thinking-then-text` 成功；`json-exception200`、`rate-limit429`、`server-error500`、`invalid-tool-format`、`malformed-sse`、`client-drop` 按预期进入错误场景；`mixed-chaos` 在内置 22s 分层慢首字下 `4/5` 成功。
- 2026-09-27 22:51 CST：已把本项目指定测试实例 `127.0.0.1:19023` 切到本轮冻结候选二进制；进程 PID `9078`，承载于 tmux session `kiro_release_174`，健康检查和 ready 检查通过。
- 2026-09-27 22:57 CST：直连 `/cc/v1/messages` 非流式真实请求通过：HTTP 200，文本 `pong`，usage 非零。
- 2026-09-27 22:58 CST：直连 `/cc/v1/messages` 流式真实请求通过：SSE 包含 `message_start`、`content_block_delta`、`message_delta`、`message_stop`，文本 `stream-pong`，最终 usage 非零。
- 2026-09-27 22:59 CST：真实 Claude Code CLI `2.1.280` 非交互烟测通过；隔离 `HOME`/`CLAUDE_CONFIG_DIR`，通过 `ANTHROPIC_BASE_URL=http://127.0.0.1:19023/cc` 调用，最终文本匹配 `cli-pong`，usage 非零，未发现内部池/凭据词泄漏。
- 2026-09-27 23:02 CST：真实 WebSearch 非流式通过：HTTP 200，`server_tool_use=web_search`，`web_search_tool_result` 返回 10 条结果，首条 host 为 `openai.com`，usage 非零。
- 2026-09-27 23:03 CST：真实 WebSearch 流式通过：HTTP 200，SSE 中包含 text、`server_tool_use`、`web_search_tool_result`、最终 text 与 `message_stop`，返回 10 条结果，usage 非零。
- 2026-09-27 23:05 CST：WebSearch 空 query 边界通过：HTTP 400 `invalid_request_error`，不生成 `server_tool_use` 或 `web_search_tool_result`，保持 MCP 前硬输入错误语义。
- 2026-09-27 23:06 CST：最终 `git diff --check`、`node scripts/check-frontend-contracts.mjs`、`node feature/tests/inventory-build-artifacts.mjs --gate`、scoped `cargo metadata --locked --no-deps` 均通过；metadata 确认版本 bump 前包版本仍为 `0.0.173`。

## 待发布结果

- 工作提交：待创建。
- 版本提交：待创建。
- 发布 tag：待创建。
- 推送结果：待执行。
