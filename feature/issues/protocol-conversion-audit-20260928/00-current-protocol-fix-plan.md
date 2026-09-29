# 当前协议问题修复计划与进度

Status: revised / v0.0.177 fallback released; all-request pre-conversion shaping in working tree (2026-09-29, not committed)
Owner: Codex (v0.0.177), Claude Code session (2026-09-29 revision)
Updated: 2026-09-29

## 目标

本轮只处理当前线上正在出现并影响 Claude Code CLI 使用稳定性的协议问题，先修复，再用真实本地账号调度验证。

需要优先关闭的问题：

1. 本地转换前置拒绝：`consecutive assistant messages contain multiple native reasoning blocks and cannot be merged losslessly`。
2. Kiro 上游 `400 Improperly formed request` / `malformed_request`，尤其是历史 reasoning/signature 兼容性导致的同类请求失败。
3. 修复后必须用真实 Claude Code CLI 调本地 `127.0.0.1:19023` 服务，隔离 CLI 的 `HOME` 和 `CLAUDE_CONFIG_DIR`，不得改动用户正在运行的本地 Claude Code CLI 环境。

## 当前判断

- 连续 assistant 多个原生 reasoning 的 400 是本地转换顺序问题：本地路径在转换后才丢弃旧历史 thinking，导致转换器先遇到 Kiro 单个 assistant history 只能承载一个 reasoning union 的结构限制。
- 但“所有请求都先做转换前 shaping”范围过大，会把单个历史签名 thinking 也提前移除，破坏既有的 Kiro 上游签名校验失败后同账号 stripped-retry 路径。
- 合理修复应是：先按原始请求转换；仅当转换失败属于已知的历史 reasoning 无损合并失败时，才对副本执行一次历史 thinking shaping 并重试转换。
- `malformed_request` 不能笼统按普通坏请求处理；当首个上游 400 是 Kiro 对历史 reasoning/signature 的泛化拒绝，且存在可构造的 stripped-reasoning retry body 时，应同账号重试一次，避免直接把可恢复协议兼容问题暴露给 CLI。

## 执行步骤

1. 修正本地转换 fallback：从“无条件转换前 shaping”改为“转换失败命中特定 reasoning 合并错误后 fallback shaping”。
2. 保留并补强 handler 单测：连续 assistant 多 reasoning 仍能到达上游，且旧私有 thinking/signature 不进入上游 body。
3. 保留单个历史签名 thinking 的既有回归：先让 Kiro 返回签名错误，再同账号 stripped retry 成功。
4. 增加/保留 malformed request 同账号 stripped retry 单测。
5. 通过 scoped Cargo 跑聚焦回归、格式检查和候选 release build。
6. 用候选二进制替换经核验属于本项目的 `127.0.0.1:19023` 指定实例。
7. 执行真实 Claude Code CLI 测试矩阵，记录请求 ID、模型、usage、thinking/tool/MCP/WebSearch/agent 行为和失败复现步骤。
8. 若真实测试暴露新协议问题，继续修复并重跑受影响 case。
9. 测试证据归档、执行 release gate，提交修复并按远端最新 tag 计算下一版本；推送 tag 后由独立 agent 监控发布状态。
10. tag 推送完成后继续保留本地指定实例，做补充的真实 Claude Code CLI 场景验证；不修改用户正在运行的 CLI 环境。

## 进度记录

- 2026-09-29：已阅读 `kiro-claude-cli-validation` 技能、测试矩阵和项目指定实例约束。
- 2026-09-29：已阅读 `22-consecutive-assistant-native-reasoning-merge-rejected.md`，确认其根因分析可用，但修复方案需要收窄为失败 fallback，避免破坏单历史签名 thinking 的上游重试路径。
- 2026-09-29：已将本地路径改为“原始 payload 先转换；仅在历史 reasoning 结构冲突转换失败时，对副本执行一次历史 thinking shaping fallback”。
- 2026-09-29：聚焦回归通过：
  - `local_preconversion_shaping_handles_consecutive_assistant_reasoning_for_five_rounds`
  - `handler_thinking_signature_retry_accepts_json_labeled_eventstream_success_for_five_rounds`
  - `reasoning_malformed_retry_strips_reasoning_same_credential_for_five_rounds`
- 2026-09-29：根据“不能把配置当作错误原因”的要求，已把历史 reasoning 结构冲突 fallback 改成代码级协议兼容兜底；即使 `payloadShaping.enabled=false` 且 `discardHistoricalThinking=false`，命中特定转换错误时也会对副本强制丢弃旧历史 thinking 后重转。
- 2026-09-29：新增并通过 `local_reasoning_fallback_ignores_payload_shaping_disabled_for_five_rounds`，锁定配置关闭时这类请求仍不能变成本地 400。
- 2026-09-29：边界回归已通过：
  - `multiple_or_mixed_native_reasoning_blocks_are_rejected_for_five_rounds`
  - `consecutive_assistant_native_reasoning_cannot_be_merged_lossily`
  - `one_native_reasoning_survives_consecutive_assistant_merge`
  - `anthropic_guard_discards_historical_thinking_even_when_body_fits`
  - `anthropic_breakdown_counts_whole_native_reasoning_blocks_for_five_rounds`
- 2026-09-29：候选二进制已在指定实例 `127.0.0.1:19023` 使用真实本地账号运行；健康检查 `/healthz`、`/readyz` 均成功。候选二进制 SHA-256 为 `dac29f7177182e494ace678d667aedbfcdd7dec36bf61eda6e66a6de71df14b5`。
- 2026-09-29：真实直接 `/cc/v1/messages` 验证通过：
  - normal stream/non-stream；
  - Sonnet/Haiku alias；
  - stream 与 non-stream thinking，均捕获真实 thinking block/delta 和非零 thinking usage；
  - 强制 `tool_use`；
  - 本地拒绝错误包络（400、request id、无内部账号信息）；
  - 连续 assistant native reasoning 真实上游请求返回 200；
  - 无效历史 signature 首次被真实 Kiro 以 400 拒绝，随后同一账号 stripped retry 返回 200。
- 2026-09-29：真实 Claude Code CLI `2.1.280` 隔离环境验证通过：
  - normal Sonnet；
  - Haiku alias、完整 Sonnet model id；
  - 被动 `ultrathink`、wrapper prompt、`sonnet-thinking`；
  - Bash、MCP、Read、Edit；
  - 两轮持久化 resume 会话，第二轮保留 cache read usage；
  - 所有通过 case 均返回预期文本、退出码 0，并在 usage 记录为真实 local account / `local_success`。
- 2026-09-29：对 WebSearch 做了额外的协议级复核，修正“CLI 有 `server_tool_use` 标记就等于执行 native WebSearch”的误判：
  - 直接构造官方 `web_search_20250305` native tool 的 non-stream/stream 请求，均真实返回 `server_tool_use` + 配对的 `web_search_tool_result`，服务端 usage 有真实 MCP 调用；
  - `claude --tools WebSearch` 在本轮隔离 CLI 中没有形成 native tool_use，服务端 `mcpAttempts=0`、`sawUpstreamToolUse=false`，模型输出的是 XML/function-call 文本或已合成文本；这不能算 CLI native WebSearch 通过，已单独记为 CLI 能力协商限制；
  - native 空 query 真实返回 400 `invalid_request_error`，且不发 MCP；这是已验证的真实无效输入，不删除轻量 query 预检。
- 2026-09-29：tag 后多轮 resume 补测初次因测试命令带 `--no-session-persistence` 而无法恢复，已在隔离 config 去掉该开关重新验证，两轮均成功，第二轮 `cache_read_input_tokens=8802`。
- 2026-09-29：agent 能力未宣称通过。CLI `--print` 的若干尝试中，一次是模型文本模拟 `<function_calls>`，另两次仅调用 Bash；`subagent_stats.spawned=0`。`--bg` 需要 CLI 自身的权限免责声明和交互式会话初始化，本轮隔离探测已清理 idle 后台进程，未形成服务端错误。
- 2026-09-29：直接协议和 CLI 证据已归档到 `tmp/thinking-budget-local/cli-protocol-evidence-20260929/summary.md`；进入 release gate，之后按用户要求打 tag 并推送，再由子 agent 监控发布。
- 2026-09-29：`v0.0.177` 已推送并完成发布监控；GitHub Actions run `36460755631` 的 quality/amd64/arm64/manifest 全部成功，GHCR `0.0.177` 与 `latest` 已更新到相同双架构 manifest。Docker Hub 匿名接口仍返回 404，未将其误判为失败。

## 2026-09-29 方案复核：改为所有请求在转换前处理历史 thinking

### 时间线（之前已经处理过一次）

1. 第 1 版（23:49，工作树内，未提交）：只要 `payloadShaping.enabled`，就在转换前统一执行一次历史 thinking shaping。
2. 第 2 版（`3a1306d`，已随 `v0.0.177` 发布）：Codex 把它收窄成"转换失败才触发的 fallback"。先按原始请求转换，只有命中两条 reasoning 结构冲突错误时，才强制丢弃历史 thinking 并重转。当时收窄的理由见本文"当前判断"第 2 条：担心提前移除单个历史签名 thinking，会破坏 stripped-retry 路径。
3. 第 3 版（本次，工作树内）：恢复为所有请求在转换前处理，同时保留第 2 版的冲突 fallback，作为配置关闭时的兜底。

### 复核发现：第 2 版的前提不成立

本地路径上，Kiro 层的 `discardHistoricalThinking` 只在请求超限的分支里才会执行，代码见 `src/anthropic/payload_guard.rs:581`：`if size_limit_enabled && report.final_weight > max_weight && config.shaping.enabled { apply_payload_shaping(...) }`。首发时 `on_too_long` 模式的 `max_bytes=0`（`src/anthropic/handlers.rs` 的 `initial_payload_guard_config`），而每次都会执行的 `apply_payload_safety_shaping`（`src/anthropic/payload_guard.rs:2063`）只负责丢弃超限的历史图片。

因此，在默认配置 `discardHistoricalThinking=true` 下，本地首发请求会把**所有**历史签名 thinking 原样发给 Kiro。外部池路径则会无条件丢弃（`apply_anthropic_payload_safety_shaping`，`src/anthropic/payload_guard.rs:2080`）。同一个配置项在两条路径上的语义不一致，本地路径实际上没有按配置执行。

这带来三个后果：

- **不必要的上游失败和重试。** 只要历史里有一个 Kiro 不再接受的旧签名（跨账号、跨模型或过期），首发就会收到 `THINKING_SIGNATURE_INVALID` 或 `Improperly formed request`，每次都要多一次同账号 stripped retry，既消耗 RPM，也增加延迟。生产样本 `req_01hJ2YhjMEyjrrVEQFTZpqri` 正是这条链路：签名 400，stripped retry，然后 too-long，最终 502。见 `docs/analysis/production-thinking-signature-followup-root-cause-20260928.md`，其中 payload guard 记录为 `removedHistoryThinkingBlocks=0`。
- **P22 这类转换冲突。** 多个历史 reasoning 要并入 Kiro 的单一 union，于是出现本地 400。第 2 版只能在失败之后补救。
- **历史 thinking 占用体积。** 历史 thinking 会计入 body 体积和权重，推高 too-long 的风险。

第 2 版所说的"破坏 stripped-retry 路径"，实际只是测试夹具的问题：`handler_thinking_signature_retry_*` 用一个**非受保护**的历史签名来触发重试。功能上，stripped retry 仍然覆盖受保护的工具续写 assistant 的签名，这部分签名会继续原样发送。Kiro 拒绝时，重试照常生效。

### 第 3 版方案（已在工作树实现）

改动位于 `src/anthropic/handlers/local_body_pipeline.rs` 的 `prepare_with_plan`：

1. 当 `payload_guard.config.enabled && shaping.enabled && shaping.discard_historical_thinking` 成立时（默认成立），所有本地请求都先对副本执行 `sanitize_anthropic_messages_for_external_forwarding`，与外部池路径完全一致，再用这个副本转换。
2. 受保护的当前工具续写 assistant（`active_anthropic_tool_turn_assistant_index`）保留它的 Kiro 原生签名 thinking。这满足 Claude Code 协议"工具续写必须回传 thinking"的要求。
3. 保留第 2 版的冲突 fallback，但只在没有做前置处理时启用，也就是运营方关闭了 shaping 或 discard 的情况。这样即使配置关闭，结构冲突也不会变成本地 400。
4. 签名重试和 malformed 重试不变，继续覆盖受保护 assistant 的签名。
5. 测试夹具 `handler_thinking_signature_retry_request` 改为工具续写形态（assistant 包含 thinking 和 tool_use，user 包含 tool_result），使签名属于受保护 turn，继续验证真实的重试路径。

### 取舍与风险

- **收益**：
  - 本地路径与外部池路径的语义一致，`discardHistoricalThinking` 按配置名实际生效；
  - 历史旧签名不再导致首发 400 和额外重试；
  - P22、P23 中涉及旧 assistant 的冲突在转换前就被消除；
  - body 更小。
- **代价**：
  - 非工具续写的历史 thinking 不再发给 Kiro，模型看不到自己之前几轮的推理。Claude Code 协议允许省略旧 turn 的 thinking。部分新模型在官方 API 上默认保留历史 thinking，Kiro 侧对推理质量有没有影响尚无实测，需要在真实 CLI 长会话中观察。
  - 如需保留历史 thinking，可以把 `discardHistoricalThinking` 设为 `false`，此时回到"全部发送 + 冲突 fallback"的行为。
- **prompt cache**：每一轮，上一轮受保护的 assistant 会变成非受保护，它的 reasoning 随之被移除，只影响历史末尾一项，前缀缓存基本不受影响。上线后第一轮会有一次性 miss。
- **回滚**：删除前置处理块即可回到 `v0.0.177` 的行为；也可以通过配置 `discardHistoricalThinking=false` 立即恢复。

### 验收（隔离要求见 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)）

- 聚焦单测：签名重试、malformed 重试、P22 两个 handler 用例、转换器边界、payload_guard 相关用例，全部通过（结果见下方"第 3 版进度记录"）。
- 真实验证：在 `127.0.0.1:19023` 指定实例上使用隔离的 `HOME` 和 `CLAUDE_CONFIG_DIR`：
  - 20 轮以上开启 thinking 的多工具长会话，全部返回 200，没有本地 400；
  - usage 中签名重试次数（`thinking_signature_retry_same_credential`、`reasoning_malformed_retry_same_credential`）明显低于 `v0.0.177`；
  - 工具续写中受保护的签名仍然出现在上游 `reasoningContent` 里。

### 第 3 版进度记录（2026-09-29）

- **代码改动**：只改了 `src/anthropic/handlers/local_body_pipeline.rs` 的 `prepare_with_plan`，在转换前加入 shaping，冲突 fallback 只在没有做前置处理时启用。测试改动在 `src/anthropic/handlers/tests.rs`：签名重试夹具改为工具续写形态，并新增 2 个 handler 用例。usage 的重新计算和整形逻辑没有改动。usage 的 `input_tokens` 估算仍然基于客户端原始 payload（`local_body_pipeline.rs` 的 `token::count_all_tokens(&payload...)`）。
- **新增单测**，均为 5 轮：
  - `local_first_send_discards_historical_reasoning_but_keeps_tool_turn_for_five_rounds`：默认配置下，首发请求不包含旧的 reasoning 和签名，旧 assistant 的可见文本保留，受保护的工具续写签名原样进入 `reasoningContent`。
  - `local_historical_reasoning_is_kept_when_discard_disabled_for_five_rounds`：`discardHistoricalThinking=false` 时，历史 reasoning 原样发送。
- **单测结果**：所有 Cargo 命令都通过 `feature/tests/run-cargo-scoped.sh` 执行，target 目录在执行后已清理。
  - 聚焦过滤（reasoning、signature、local_body_prepare、preconversion、payload_guard、thinking）：270 passed / 0 failed。
  - 全量 `cargo test --bin kiro-rs`：2203 passed / 0 failed / 6 ignored。
  - 签名重试、fallback 和新增用例的专项复跑：17 passed。
  - 对两个改动文件执行 `cargo fmt --check`：通过。
- **真实上游**：测试实例为 `127.0.0.1:19023`，使用 `tmp/thinking-budget-local/config.json`、同一套 PG 和 Redis，未启动第二个实例。候选二进制 SHA-256 为 `a8ca8f4dabb5db1f1f9190423cb45c3ab98e7ae1b97ca5fd4823099688fd3ad9`。测试结束后，实例已恢复为原二进制并通过 readyz 检查。
  - **直连协议 thinking 矩阵**（18 类场景，共 23 个请求）。基线（原二进制）与候选版本的结果一致，没有回归：
    - thinking 的几种形态：enabled 流式和非流式、adaptive、adaptive + effort、disabled、不带 thinking 字段、`*-thinking` 别名。
    - 历史回放：真实历史回放；thinking 后接 tool_use，再接 tool_result 续写（流式和非流式）；工具续写结束后再开一轮新 turn。
    - 异常历史形态：P22 形态（连续 assistant）；旧 assistant 里有两个签名 thinking；旧历史中签名与无签名 thinking 混用；非受保护历史带无效签名；受保护工具 turn 带无效签名；无签名 thinking 历史；`redacted_thinking` 历史。
    - 其他：interleaved beta 加 tools；非法 `thinking.type` 返回 400。
    - 上述请求全部返回 200 或预期的 400，没有本地 `local_body_prepare` 拒绝。
    - 受保护工具 turn 带无效签名的用例（T15）：候选版本上真实 Kiro 先拒绝签名，同账号 `thinking_signature_retry_same_credential` 重试后返回 200。这说明受保护签名仍会发给 Kiro，重试路径依然有效。
    - 非受保护历史带无效签名的用例（T14）：候选版本只用了 1 次 attempt，没有触发重试。
  - **真实 Claude Code CLI 2.1.283**（隔离的 `HOME=/tmp/kiro-claude-home-19023`、`CLAUDE_CONFIG_DIR=/tmp/kiro-claude-config-19023`），基线与候选版本都通过：
    - `ultrathink` 关键词、`MAX_THINKING_TOKENS`、普通请求均正常；
    - 开启 thinking 后执行 Read + Bash 工具调用，并进行 3 轮 `--resume` 多轮会话，结果正确，退出码为 0。
- **未能覆盖**：
  - 当前测试实例的 native reasoning 能力处于 `Unknown`，因为 `ListAvailableModels` 返回 400，属于 fail closed。可用的 sonnet-4.5 账号返回的是 XML 兼容 thinking，没有签名；opus 系列账号返回 503，暂无可用账号。所以"真实 Kiro 原生签名的多轮回放"这一项，只通过伪造的无效签名和单测覆盖，没有用真实原生签名验证。需要在 native reasoning 能力恢复、或者有 opus 账号可用时补测。基线同样存在这个限制，与本次改动无关。
  - `claude --model sonnet-thinking` 在基线和候选版本上都没有出现 thinking block，属于已有现象，与本次改动无关，需要单独排查。
- **证据**：`tmp/thinking-budget-local/preshaping-evidence-20260929/`，包括矩阵脚本、脱敏结果、CLI 原始输出的哈希。原始 CLI 捕获和临时密钥文件已删除。
- **状态**：改动尚未提交。
