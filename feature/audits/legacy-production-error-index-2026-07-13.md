# LLM 调用异常根因索引

基于 `tmp/analysis-usage-llm-errors` 生产证据（app `0.0.101` / revision `737f9f1`，最近 12 小时窗口，50405 条 usage / 281 条非成功）逐类分析、复现、归档。本目录每份文档对应一个独立根因，含现象、根因、复现 case、修复方案与回归清单。

## 分析窗口口径

- UTC 窗口：`2026-07-11 21:46` → `2026-07-12 09:46`
- 非成功记录：281 条；status 分布 `error 216 / upstream_timeout 41 / client_dropped 17 / stream_error 7`

## 两大类

- **程序缺陷（可根除）**：输入决定、必现，代理侧透传/校验缺失导致。工具与 schema 边界值三类，合计约占非成功请求的 74%。
- **上游 / 客户端行为（只能优化，不可根除）**：依赖上游瞬态状态或客户端行为，无法在本地稳定复现，程序侧只能提升容错或体验。

## 当前状态总览（2026-07-13，本地工作区口径）

说明：

- `tmp/analysis-usage-llm-errors` 是旧生产窗口证据，运行版本为 app `0.0.101` / revision `737f9f1`，不能直接代表当前工作区。
- “已修复”表示当前源码已实现；“已定向验证”表示本地单测/协议测试已覆盖；“已真实回归”表示已用临时 release 服务和真实接口补过证据；最终仍需发布后看生产 recurrence。
- 2026-07-13 新增跟踪文档：[runtime-usage-error-followup-2026-07-13.md](./runtime-usage-error-followup-2026-07-13.md)。
- 2026-07-14 本地待裁定 Todo：[local-todo-for-confirmation-2026-07-14.md](./local-todo-for-confirmation-2026-07-14.md)。
- 2026-07-15 跨主题总索引：[../analysis-status-index-20260715.md](analysis-status-index-20260715.md)，汇总历史问题、证据链、复现/验证矩阵和剩余动作。

### A. 程序缺陷（优先修复）

| 文档 | 根因 | 生产条数 | 当前状态 | 已验证 | 后续 |
|---|---|---:|---|---|---|
| [empty-tool-description-400-invalid-tool-use-format.md](../issues/empty-tool-description-400-invalid-tool-use-format.md) | 工具 `description` 为空串 | 201 | ✅ 源码已修复 | 单测覆盖空/空白 description；文档有真实调用证据 | 发布后查 `REQUEST_BODY_INVALID / Invalid tool use format` 是否回落 |
| ↑ 同文档 问题 B | `input_schema` 显式为 `null` | 7 | ✅ 源码已修复 | 单测覆盖 null/missing/object；已真实回归 | 发布后查入口 JSON 类型错误是否回落 |
| [tool-property-key-invalid-400-tool-schema-invalid.md](../issues/tool-property-key-invalid-400-tool-schema-invalid.md) | 工具属性键不匹配 `^[a-zA-Z0-9_.-]{1,64}$` | 另有生产样本 | ✅ 源码已修复；2026-07-13 修正 diagnostics 误报 | 默认可逆 sanitize；stream/non-stream/leaked invoke 反向映射；新增 `$defs/patternProperties/dependentSchemas` 不误报单测；已真实回归 | 发布后观察 tool schema invalid 是否复发 |
| Tool name 映射（合并到 schema 兼容项） | 上游 tool name 合法化后响应需还原 | 兼容性问题 | ✅ 源码已有 request-local 映射 | 单测已有短名/冲突映射覆盖；Claude CLI 工具调用已真实回归 | 发布后观察 CLI 工具消息是否异常 |

三类同源：对客户端工具/schema 字段的边界值（空串 / null / 非法键）缺乏入口清洗与兜底，直接透传上游被拒。2026-07-12 已修复空 `description` 与 `input_schema:null`；非法 property key 采用默认可逆 `sanitize`：只清理不匹配正则的 key，发给上游前映射为唯一 `key<hash>`，并在 stream / non-stream 工具调用响应中还原为客户端原始 key。需要硬拒绝时可切换 `toolSchemaKeyMapping=reject`；需要旧透传行为时可切换 `disabled`。

2026-07-13 修正：`ToolUseFormatDiagnostics.invalidToolSchemaPropertyKeys` 只统计真正的 `properties` key；不再把 `$defs` 定义名、`patternProperties` 正则 key、`dependentSchemas` 依赖 key 误报为非法 property key。

### B. 上游 / 客户端行为（优化，非根除）

| 文档 | 根因 | 生产条数 | 当前状态 | 已验证 | 后续 |
|---|---|---:|---|---|---|
| [02-stream-upstream-idle-timeout.md](../issues/02-stream-upstream-idle-timeout.md) | 流式上游空闲满 180s 被掐断 | 41 | ✅ 已实现首输出前安全重试 | runtime/API/direct SSE/Claude CLI 正常流已回归；usage 记录 retry attempts/reasons | 发布后观察生产复发；如需更强证据，可用隔离 DB 补首输出前故障注入 |
| [03-client-dropped-downstream.md](../issues/03-client-dropped-downstream.md) | 下游客户端提前断开 | 17 | ✅ 无需修复 | usage 已标 `client_dropped/downstream_client` | 统计服务端错误率时剔除 |
| [04-external-pool-prompt-too-long.md](../issues/04-external-pool-prompt-too-long.md) | 本地 3 次 500 高负载耗尽 → 外部池撞 1M 上限 | 7 | ✅ 已修复体验链路：token 估算、分类、public message、max-input 预检 | `prompt is too long` 分类；外部池 public message 脱敏；`externalPoolMaxInputTokens` 预检单测；本地无外部池，未污染共享 DB 做真实外部池注入 | 发布后观察；后续可选扩展 per-pool/per-model 上限 |
| [06-stream-upstream-status-error.md](../issues/06-stream-upstream-status-error.md) | 流中途上游返回错误事件 | 5 | ✅ 维持保守策略 | 现有逻辑发 SSE error，不伪装成功 | 若做 02，可把首输出前子集纳入统一安全重试 |
| [07-stream-internal-read-error.md](../issues/07-stream-internal-read-error.md) | 流 body 解码失败 | 2 | ✅ 维持保守策略 | 现有逻辑发 SSE error，不伪装成功 | 同 06 |
| [08-image-format-unsupported-400.md](../issues/08-image-format-unsupported-400.md) | 坏图/伪图无法被上游解码 | 1 | ✅ 已实现轻量结构校验 | 合法 base64/data URL/工具结果图片继续通过；伪图/截断 PNG 本地拒绝；坏图已真实回归为本地 400 | 发布后观察坏图类错误是否更清晰 |
| [09-intent-preamble-end-turn-no-tool-use.md](../issues/09-intent-preamble-end-turn-no-tool-use.md) | 模型输出短开场白后 `end_turn`，未发 tool_use | 后续现网样本 | ✅ 2026-07-13 已实现 usage-only 诊断 | 定向单测：有工具、短可见文本、无 tool_use 命中；tool_use 轮不命中 | 真实长会话观察 `suspectedIntentPreambleEndTurn` 分布 |
| [10-stream-end-turn-vs-silent-truncation.md](../issues/10-stream-end-turn-vs-silent-truncation.md) | usage 无法区分上游显式完成和本地 EOF 兜底 | 观测盲区 | ✅ 2026-07-13 已实现 usage-only 观测字段 | 解析 `messageStatus`；写入 `upstreamMessageStatus/sawUpstreamCompleted/stopReasonSource`；两套 UI 已展示；真实 stream 落库已看到 `stopReasonSource` | 发布后用长会话分布判断 H1/H2/H3 |

### C. 错误提示策略

| 项 | 当前状态 | 规则 |
|---|---|---|
| Kiro 官方上游结构化错误 | ✅ 2026-07-13 已改为可公开 message 透出 | 从上游 JSON 提取 `message/reason/code`，过滤 credential/token/外部池/调度等敏感词后返回给下游 |
| 外部池错误 | ✅ 继续脱敏 | 外部池可能返回广告、推广、非协议 HTML 或内部池信息；下游只返回 public message + error id，原文仅留 usage/内部日志 |
| 本地调度/账号/队列/内部错误 | ✅ 继续归一化 | 避免泄露 credential、fallback、scheduler、pool、lease 等内部词 |
| `/cc` / `/ha` reported usage input | ✅ 2026-07-13 已修复 input sampling 漏应用与无 read 证据 delta 丢失 | `sample-max` 始终压低展示 input；有 cache-read 证据时差额转入 cache read，无 read 证据时转入 cache writer，不伪造首轮读取，也不丢差额 |
| `/cc` / `/ha` reported usage output | ✅ 2026-07-13 已新增可配置后处理，默认启用保守补偿 | 既有 `output` 四种策略先执行；默认 `output > 1000` 后放大 50%；最后用 `200000 - 5000..12000 jitter` 限制有效上限，避免撞 200k/1m |

## 重要更正记录

- **08 图片**：早期误判「1×1 图片被拒」。实测更正：合法图片（64×64 / 512×512）正常返回 200；1×1 是被上游静默忽略（非拒绝）；真正触发 `IMAGE_FORMAT_UNSUPPORTED` 的是坏图/伪图。详见该文档。
- **04 标题误导**：分类名为「外部池 prompt 超长」，实际根因链是本地账号先 3 次 500 高负载、fallback 后才撞外部池 1M 上限。

## 优先级建议

1. **高**：工具/schema 三类（A）——必现、纯程序、约 74%，合并修复。
2. **中**：02 首字前安全重试 —— 需先做响应提交重构，严格以“未发送任何 SSE bytes”为安全边界。
3. **低**：04 外部池 per-pool/per-model 上限、08 更完整图片解码 —— 当前已做低风险版本，后续按生产复发再增强。
4. **忽略**：03（客户端行为）、06/07（上游瞬态、量极小）。

## 公共前置（复现用）

- 本地服务：优先使用临时 release 端口，避免触碰 live `9022`；API key 从本地 `config.json` 读取，文档不记录原文。
- 本地真实回归默认使用 sonnet 小流量请求；涉及 opus/长上下文只做低量诊断，避免真实账号压力。
- 生产证据目录：`tmp/analysis-usage-llm-errors/`。
