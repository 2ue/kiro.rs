# Round 00：生产抓包摸底与版本核对

时间：2026-11-06（北京时间记录待补；抓包 UTC 时间为 07:46:49—07:50:48）

## 范围

只读分析 `59138` 的现有抓包和运行版本，未重启生产、未写生产配置、PostgreSQL、Redis 或凭证。

## 结果基线

来源 `results/baseline-9afa06c2.json`，总计 73 项：

- 通过：tag_check、non_stream、doc_recognition、knowledge、output_config、image_recognition、model_attribution；
- 未通过：stream_structure=5、server_tool=0、websearch=0、signature_proto=5。

## 生产版本

59138 镜像 `ghcr.io/2ue/kiro-rs:latest`，版本 `0.0.239`，revision `30edc00c`；当前仓库 HEAD 是 `2e625e52` / `v0.0.240`。生产不是当前仓库最新版本。生产进程保持运行。

## 抓包摘要

- 原始文件在仓库外，大小约 4 GiB，未复制进仓库；
- 所有 JSON 行可解析；
- upstream：200=1977、400=31、429=1、500=1；
- client SSE：message_start=1216、message_stop=1137、error=9；
- route=websearch 的 client SSE=157，MCP body=4。

## 根因与行动

1. **确定问题**：31 次 `THINKING_SIGNATURE_INVALID`。上游收到代理 synthetic signature，首选修复是阻止 synthetic marker 进入 Kiro `reasoningContent`。
2. **确定需要复核**：429/500 后 retry 成功但 request_summary 仍 `stream_error`，检查 per-attempt stream assembler 和 downstream commit 状态。
3. **待定位问题**：WebSearch/server tool 需要按 requestId 解析完整 SSE 序列后再改代码。
4. **开发运行方式**：使用 `/tmp/kirotest-dev` 隔离配置和 `127.0.0.1:59139`，后续 scoped `cargo run`；开发迭代不要求每次 release 冻结。

## 下一轮入口

- 提取失败请求的 request-level SSE 顺序；
- 实现 synthetic reasoning 边界清理的最小单测和修复；
- 建立开发实例冒烟与 cctest 调用脚本；
- 每轮将结果和抓包摘要追加到 `round-XX.md`。
