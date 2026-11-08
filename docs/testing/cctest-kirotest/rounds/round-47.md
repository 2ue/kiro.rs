# Round 47：structured-output 调用方身份提示实验

## 运行信息

- 时间：2026-11-07（北京时间 15:08–15:12；UTC 07:08–07:12）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 工作区：当前未提交工作树；本轮仅新增 `src/anthropic/structured_output.rs` 的工具描述提示
- 59139 二进制：`.codex-artifacts/kiro-rs-round47-identity-hint`
- 二进制 SHA-256：`5ba500315d4d8ba06d823c16071a443da28736413a1aab5fe5eef77675c8c23f`
- 59139：本地开发实例，运行于 `0.0.0.0:59139`
- 59138：只读健康探针；未停止、未重启、未改配置、未写 PostgreSQL/Redis

## 代码实验

将内部 `structured_output` 工具描述从单纯的“返回最终 JSON”扩展为：

> 当 schema 要求平台或客户端身份时，使用调用方系统指令明确声明的平台，而不是执行工具的上游运行时。

该修改没有硬编码 `identity_platform`，也没有改写上游工具输入，只测试模型能否利用 Claude Code 系统上下文生成 `claude_code`。

## 结果

结果文件：[`20261107T070835-round-47-identity-hint.json`](../results/20261107T070835-round-47-identity-hint.json)

总分仍为 `73/110`：

| 检测项 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 0 |
| server_tool | 10 |
| knowledge | 10 |
| model_attribution | 10 |
| doc_recognition | 10 |
| image_recognition | 10 |

本轮无分数改善，也没有回归。

## 抓包证据

抓包目录：`logs/llm-capture/capture-20261107T070824Z-7d7cf6d2/`

- 8,530 个事件，`events.jsonl` 约 7.8 MiB，ZIP 约 1.7 MiB。
- 22 个客户端请求、25 个上游请求；上游 HTTP `200=25`。
- 客户端 SSE：`message_start=22`、`message_delta=22`、`message_stop=22`；`content_block_start/stop=51/51`；无丢弃事件。
- WebSearch 仍有完整生命周期，但该分项仍为 0。
- 结构化输出请求仍经内部 `structured_output` 工具重写，返回单一 JSON text block；该分项仍为 0。
- WebSearch `encrypted_content` 仍是 MCP 明文 snippet，`encrypted_index` 仍是可逆 Base64 的 `序号:URL` 形式；本轮不伪造 opaque 值。

## 根因判断

本轮最小提示实验不能改变 Opus 5.5 在开发上下文中的身份选择，说明仅靠内部工具描述不足以稳定复现生产样本中的 `identity_platform=claude_code`。生产只读样本还包含 `<assistant_identity>` 历史块及不同的 Claude Code 工具/上下文，不能直接复制到 59139，更不能写回 59138。

当前没有证据支持修改公开 SSE 生命周期、随机生成 Kiro signature、或伪造 WebSearch 加密字段。随后 Round 48/49 的生产对照复核确认：Round 42 已在 59139 向上游发送与生产完全相同的 `<assistant_identity>` 块，但输出仍为 `kiro`、评分为 0；Round 07 在没有该块时却返回 `claude_code` 并得 10。详见 [问题清单 I-29](../analysis/issues.md#i-29-生产-identity-块不是-output_config-的单独根因)。因此 marker 有无不能作为单独根因，后续不强制注入该块。

## 开发循环记录

本轮确认可使用 debug 开发循环：源码变化后通过 `feature/tests/run-cargo-scoped.sh cctest-kirotest-dev -- cargo build --bin kiro-rs -j2` 增量编译，复制完成二进制并记录 SHA-256，再只重启 59139。Rust 进程不能进程内热替换；“热更新”应理解为自动重编译 + 本地进程重启。59138 不参与该循环。

## 资源与隔离

- 编译前后 `/tmp` 使用率恢复到约 20%；内存可用约 9.7 GiB，Swap 可用约 16 GiB，负载约 0.7–1.5。
- 59138 健康探针在编译、重启、检测期间均为 HTTP 200。
- 59139 和 Caddy 域名健康检查均为 HTTP 200。

## 开发模式自动热更新

用户确认开发迭代应使用自动热更新。本轮新增 `scripts/dev_loop.py` 和 `scripts/dev-loop.sh`，操作说明为 [`../dev-loop.md`](../dev-loop.md)。它监听 `Cargo.toml`、`Cargo.lock`、`build.rs`、`.cargo/` 和 `src/`；变更后运行 `cargo build --bin kiro-rs -j 2`，使用独立持久 Cargo target、`CARGO_INCREMENTAL=1` 和无调试符号的 debug profile。成功后原子替换 `/root/.cache/kirotest-dev/kiro-rs-dev`，只重启 59139 并等待 `/healthz` 就绪；编译失败保留旧进程，启动失败回滚到运行中旧二进制。

资源阈值沿用 Round 11：可用内存低于 4 GiB、swap 低于 8 GiB、1 分钟 load 达到 8，或 59138 `/healthz` 不为 200 时暂停编译；恢复条件为内存至少 6 GiB、swap 至少 10 GiB、load 低于 6 且生产探针恢复。首次构建触发资源暂停，移动 target 到系统盘并关闭 debug 符号后构建成功；`/tmp` 使用率恢复约 20%。59138 全程健康。

自动热更新验证：仅执行 `touch src/main.rs`，不改变文件内容；脚本完成增量构建并将 59139 PID 从 `2362811` 更新为 `2363953`。开发端和生产端 `/healthz` 均为 HTTP 200，新进程与开发二进制 SHA-256 均为 `b722634e90285a118a2cf27893d32efcae88227cbf9a816fa2c211459dadb6db`。当前开发热更新循环由 PID `2362755` 运行；其管理的 59139 进程 PID `2363953`。本轮没有新增 CCTest 评分。
