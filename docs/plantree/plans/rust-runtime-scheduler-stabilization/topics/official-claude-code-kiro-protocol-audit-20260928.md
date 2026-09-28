# 官方 Claude Code / Anthropic 与 Kiro 协议互操作审计计划

- 日期：2026-09-28（Asia/Shanghai）
- 工作模式：先 `audit` 只读分析与验证；随后按用户要求实施本地 reasoning
  fallback 修复并准备发版。不修改生产服务器。
- 关联计划：[Rust Runtime Scheduler Stabilization](../README.md)
- 关联发布：`v0.0.175`，提交 `9c27f881abc0b5100c7d0b39166c166406f1db26`
- 当前远端现场版本：用户已切回 `v0.0.174`；现场复现与 `v0.0.175`
  的发布结果必须分开记录。
- 指定本地验证实例：`127.0.0.1:19023`
- 生产证据边界：`152.53.243.159:59137` 只读，不执行远端写入、重启、迁移或压测

## 用户目标

在已完成发布后，系统性判断当前项目的 Claude Code `/cc` 协议、Anthropic
Messages 兼容层、Kiro 上游转换和错误/流式/usage/工具/thinking 行为是否符合
官方协议与真实 CLI 预期。结论必须区分：

1. 官方规范明确要求；
2. 当前代码和测试直接证明的事实；
3. 真实 Claude Code CLI 或真实 Kiro 上游观察到的事实；
4. 仅由源码推断、尚未被真实上游证实的风险；
5. 必须修复、可优化、暂不应修改以及证据不足的事项。

## 范围

### 必须覆盖

- Anthropic Messages 请求字段、thinking/reasoning、签名和 redacted thinking。
- 工具定义、`tool_use`/`tool_result` 配对及多轮历史。
- 流式 SSE 事件顺序、终态、usage 和错误 envelope。
- Claude Code CLI 的非交互和可行的交互式行为。
- Kiro 请求/事件转换，包括 native reasoning、prompt steering、模型 alias、
  EventStream/JSON content-type、EOF 和上游错误。
- 入口拒绝记录的完整性：`model=requested/upper` 为什么出现
  `unknown`、`content`/诊断为何可能为空，以及这些字段是否在错误发生前
  已被解析、是否只是错误记录序列化/采样丢失。
- 真实测试：正常请求、thinking、工具、别名、异常响应、HTTP 200 但 body 报错、
  HTTP 4xx/5xx、签名错误和重试后的行为。
- 用户已提出的 reasoning 400、第一次签名失败/第二次成功、输入过长未裁剪、
 以及“上游错误时 Claude Code CLI 是否中断”的因果分析。

### 明确不做

- 审计阶段不修改 Rust、UI、配置默认值、数据库、Redis 或生产部署；修复阶段
  仅修改当前仓库代码和文档，不修改远端生产服务器。
- 不以 fake upstream 结果冒充真实 Kiro 官方协议证明。
- 不把社区项目、逆向结构或当前实现约定冒充 Kiro 官方公开规范。
- 修复建议与实际落地分开记录；本次已将 reasoning fallback 作为单独修复落地。

## 证据与方法

1. 读取官方 Anthropic 文档，记录 URL、页面日期和适用字段。
2. 读取当前转换、provider、stream、handler、model-capability 和错误归一化代码。
3. 使用指定 `19023` 实例和真实 `claude` CLI，记录请求 ID、HTTP 状态、
   stream 事件、最终文本、usage、thinking/tool-use 和退出码。
4. 对同一请求建立“输入字段 → Kiro 请求 → Kiro 事件 → Anthropic 输出 → CLI
   结果”的证据链；不打印密钥、refresh token、完整签名或完整正文。
5. 对每个异常分别判断：入口拒绝、上游拒绝、流中断、客户端重试、客户端是否继续。
6. 只有可复现且因果链闭合的结果才进入“确定根因”；否则标记为待证实。

## 阶段与进度

| 阶段 | 内容 | 状态 | 产物 |
| --- | --- | --- | --- |
| P0 | 发布状态确认、指定实例和 CLI 隔离条件确认 | 已完成 | `v0.0.175` 发布成功；现场已切回 `v0.0.174`；本地实例和远端只读边界已确认 |
| P1 | 官方 Anthropic 协议资料逐字段整理 | 已完成 | 最终报告“官方 Anthropic 协议逐项对照” |
| P2 | 当前仓库转换/流/错误实现逐条映射 | 已完成 | 最终报告“当前实现的因果链”和源码索引 |
| P3 | 真实 Claude Code CLI C1/C2/C3/C4 可行矩阵 | 已完成（C3 明确未全量通过） | 远端 C1、本地 C1/C2、定向 C4；交互式/MCP/agent 长会话保留为后续验证 |
| P4 | 真实 Kiro 上游异常、签名、reasoning 400 和终态验证 | 已完成当前证据范围 | 远端 reasoning 400、普通请求对照、历史 signature/too-long、HTTP 200 流错误根因链 |
| P5 | 必须修复/可优化/暂不改/证据不足分类 | 已完成 | 最终报告“必须修复、可以优化、暂不修改、证据不足” |
| P6 | 中文最终报告、证据索引、发布状态汇总 | 已完成并更新修复验证 | [`docs/analysis/official-claude-code-kiro-protocol-audit-20260928.md`](../../../../../analysis/official-claude-code-kiro-protocol-audit-20260928.md) |
| P7 | reasoning fallback 修复、真实 CLI 验证、发版 | 进行中 | 候选二进制 SHA-256 `0bcbe543417999db779ccc73f0f9b566115817d4807785e0ffa9ebcf056803a2`；待提交、打 tag、发布 |

## 验收标准

- 官方 Anthropic 字段和事件至少逐项引用官方文档，不只引用仓库源码。
- 结论覆盖每一项已列范围；没有用“整体兼容”掩盖未验证项。
- 每个关键结论都有 request id、测试命令/配置、状态码、事件或源码定位。
- 明确区分 HTTP 层错误、Anthropic envelope 错误、Kiro 上游错误和 CLI 行为。
- 对“第一次签名失败、第二次成功”“400 后是否继续”“HTTP 200 实际报错”
  给出可复现证据和因果解释。
- 最终报告中文可读；敏感信息只保留脱敏标识。
- 工作树除本计划和最终文档外不产生代码或运行配置修改。
- 修复阶段的工作树代码变更需有 scoped Cargo、真实 direct API 和真实 Claude
  Code CLI 证据，且不泄露 request/admin key。

## 进度日志

- 2026-09-28：发布 `v0.0.175` 已推送；真实 Claude Code CLI `2.1.280`
  已在 `19023` 完成正常、thinking、工具请求的初步 C2 验证。当前开始补齐官方
  协议资料和异常矩阵。
- 2026-09-28：确认 `v0.0.175` 的发布流水线成功，但用户已将远端现场切回
  `v0.0.174`；后续所有远端请求必须先记录现场版本。
- 2026-09-28：新增待办：审计远端 `request_rejection` 中
  `localBodyPrepareKind=conversion_error`、`unsupported_content`、
  `reasoning ... unavailable` 与 `model=unknown/content=unknown` 的完整因果链；
  必须区分请求尚未解析、转换失败后的错误记录丢字段、以及 UI/查询层脱敏或
  采样导致的显示缺失。
- 2026-09-28：远端真实 `/cc/v1/messages` 验证闭合：`claude-opus-5` 的
  reasoning 请求 `req_01qRzjRznciijGKDL6GwP8aY` 在 upstream dispatch 前返回
  HTTP 400；同模型普通请求 `req_01TXEwmmoLw8p3n69T5M2XC7` 返回 HTTP 200。
  远端 capability discovery 为 `4/5 cohorts observed`，且
  `promptSteering.enabled=false`，确定两条 reasoning wire path 同时不可用。
- 2026-09-28：本地真实 Claude Code CLI `2.1.280` 在 `127.0.0.1:19023`
  通过普通、thinking、Bash tool 用例；direct stream/non-stream、
  adaptive/effort 变体也已完成。完整 interactive、MCP、agent 和长历史
  C3/C4 仍标记为后续证据。
- 2026-09-28：`thinking_signature_retry` 定向回归
  `11 passed / 0 failed`；当前源码已将 signature retry 后的 too-long 纳入
  统一 payload guard，但没有把该源码状态写成远端 `v0.0.174` 已部署修复。
- 2026-09-28：创建中文最终报告
  `docs/analysis/official-claude-code-kiro-protocol-audit-20260928.md`。
  审计阶段只更新计划和分析文档，没有修改 Rust、配置、数据库、Redis、容器或远端。
- 2026-09-28：按用户要求实施 reasoning fallback 修复：显式客户端 reasoning
  不再被 `promptSteering.enabled=false` 阻断专用 thinking 兼容传输；新增
  `explicit_reasoning_keeps_compatibility_transport_when_operator_prompt_master_is_off`
  回归。验证通过：`cargo fmt --check`、新增回归、原 operator prompt master
  回归、`converter` 154 项、`local_body_pipeline` 2 项、release build、build
  artifact inventory gate。
- 2026-09-28：候选二进制在 `127.0.0.1:19023` 真实验证通过：direct 普通
  non-stream `protocol-pong`；direct 显式 thinking non-stream 有 `thinking`
  block 且 `thinking_tokens=93`；Claude Code CLI `2.1.280` 普通、`--effort high`、
  `sonnet-thinking --effort high` 和 Bash tool case 均成功，其中可见 thinking
  case 有 1 个 thinking block、`thinking_tokens=129`，tool case 有 1 个
  `tool_use` 和 1 个 `tool_result`。
