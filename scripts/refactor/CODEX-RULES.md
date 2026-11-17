# 执行规则（每个任务都适用）

你在 Rust 仓库 `kiro-rs` 的分叉分支 `claude-main` 上工作，工作目录就是仓库根。设计文档在 `docs/claude-pool-refactor/`，其中 `11-实施说明与关键逻辑契约.md` 的第 4 节是红线，违反任一条即任务失败。

硬性约束：
1. 只改任务说明里允许改的文件。不要"顺手"重构、改名、删注释、格式化无关代码。
2. 不改任何 usage 整形算法的函数体（`src/anthropic/cache.rs`、`prompt_cache.rs`、`prompt_cache_creation_control.rs`、`bounded_shaping.rs`、`kiro_plus_shaping.rs`、`src/external_pool/usage_projection.rs`、`src/external_pool.rs` 中 `project_usage_value` 及其调用的函数）。
3. 编译目录是共享的 `/tmp/kiro-rs-rust-analyzer-target`（`.cargo/config.toml` 已配置），不要设置 `CARGO_TARGET_DIR`，不要 `cargo clean`。
4. 这个包只有 bin 没有 lib：单测用 `cargo test --bin kiro-rs -- <filter>`。不要跑 `--all-targets`，不要跑需要 PG/Redis 的集成测试（没有设置 `KIRO_RS_TEST_*` 环境变量时它们会自动跳过，这是正常的）。
5. 完成前必须：`cargo fmt --all`，然后跑任务说明里"自检"一节列出的命令，全部通过才算完成。
6. 不要 `git commit`、不要 `git push`、不要切分支、不要碰 `main`。只改工作区文件。
7. 任务说明里的「允许改动的文件」是范围边界。如果范围外的文件因你的删除而编译不过，且改动是机械性的（删引用、删匹配分支、补字段），直接改并在汇报里列出；只有当需要在范围外做实质设计决定时才停下汇报。
8. 遇到与任务说明冲突的事实（例如文件不存在、函数签名不同），以仓库实际代码为准，在最后的汇报里说明差异。不要停下来提问，不要留 TODO。
9. 最后一条消息必须是汇报：改了哪些文件、自检命令的结果、与任务说明不一致的地方。用中文。
