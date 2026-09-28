# 当前协议问题修复计划与进度

Status: release-pending
Owner: Codex
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
  - Bash、WebSearch、MCP、Read、Edit；
  - 两轮 resume 会话，第二轮保留 cache read usage；
  - 所有通过 case 均返回预期文本、退出码 0，并在 usage 记录为真实 local account / `local_success`。
- 2026-09-29：agent 能力未宣称通过。CLI `--print` 的若干尝试中，一次是模型文本模拟 `<function_calls>`，另两次仅调用 Bash；`subagent_stats.spawned=0`。`--bg` 需要 CLI 自身的权限免责声明和交互式会话初始化，本轮隔离探测已清理 idle 后台进程，未形成服务端错误。
- 2026-09-29：直接协议和 CLI 证据已归档到 `tmp/thinking-budget-local/cli-protocol-evidence-20260929/summary.md`；进入 release gate，之后按用户要求打 tag 并推送，再由子 agent 监控发布。
