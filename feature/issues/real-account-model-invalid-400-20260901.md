# P003：真实本地账号模型请求大量 `400 invalid_request_error`

Status: `partially-fixed / real-local-validation-passed / attribution-of-historical-400-incomplete`

Severity: P1

## 摘要

2026-09-01 在本项目唯一长期测试实例上，用操作员提供的 220 条真实 Kiro
凭据做了两组修复前后对照：

- 修复前 baseline：`/cc/v1/messages`、并发 2，逐请求报告 20/20 为 HTTP 400；
  10 秒汇总报告实际完成 13/13，其中 11 条为 400、2 条为 502。
- 修复前模型矩阵：5 个请求模型各 5 次，共 25/25 为 HTTP 400。
- 修复后 thinking 边界矩阵：`budget_tokens == max_tokens`、预算大于
  `max_tokens`、adaptive/disabled 携带非法 budget，各用真实本地账号请求，
  4/4 为 HTTP 200，并产生正常 usage。

因此，本问题不能被概括成“增加重试后恢复”。已确认的修复是请求入口字段规范化；
历史 400 中是否全部由 thinking 字段导致，仍不能从旧的公开错误正文确定。

## 测试边界

| 项目 | 值 |
| --- | --- |
| 测试日期 | 2026-09-01 |
| 证据时间窗 | 2026-09-01 10:36--14:53 UTC（18:36--22:53 Asia/Shanghai） |
| 唯一服务实例 | `127.0.0.1:19023` |
| 配置 | `tmp/thinking-budget-local/config.json` |
| PostgreSQL | `kiro_thinking_budget_20260901` |
| Redis | `127.0.0.1:26379/0` |
| 候选二进制 | `/tmp/kiro-thinking-candidate.tf6Wsb/kiro-rs` |
| 候选 SHA-256 | `f02b9883f5b8ce831b4801f0b14802d9a53619bff44c6bcdb72dcc7c76a15ffd` |
| 源凭据数量 | 220（操作员提供的 JSON 数组；原始内容未复制） |
| 服务实际加载 | 220 |
| 外部池 | 0；本轮没有外部池动态验证 |

测试遵循 [项目测试实例规则](../../docs/testing/project-test-instance.md)：
所有 case 复用同一个服务进程，不为每个用例创建新 `kiro.rs`，也不操作用户现有浏览器。

## 用户可见现象

历史客户端收到统一的：

```text
400 invalid_request_error
The request body is invalid. Simplify the message, tools, tool results, files, or images and retry.
```

该公开文案没有暴露具体坏字段。它不能单独证明错误来自模型别名、thinking、
工具、图片、历史签名或账号授权中的任一项。

## 修复前事实

### 20 请求、并发 2

证据：

- [baseline-20x2.json](../evidence/real-account-tests-20260901/baseline-20x2.json)
- [baseline-20x2-detail.json](../evidence/real-account-tests-20260901/baseline-20x2-detail.json)

逐请求文件记录 20 条请求，20/20 为 HTTP 400。相同运行的 10 秒汇总文件因为
发送窗口结束时只完成了 13 条，所以记录 `sent=13, completed=13, ok=0,
failed=13`，其中 `400=11, 502=2`。这两个文件是不同统计层级，不能直接相加。

### 25 次模型矩阵

模型输入为：

```text
sonnet
claude-sonnet-4
claude-sonnet-4.5
claude-sonnet-4-6
claude-sonnet-4-5-20250929
```

每个模型 5 次，共 25 次，25/25 为 HTTP 400。所有公开错误正文都相同，
没有上游坏字段详情。

这组结果只能确认“该请求形态在当时的本地账号运行中大面积失败”，不能确认
25 次全部是同一个根因。特别是显式 `claude-sonnet-4-6` 过去可能被静默降级，
这本身会改变模型能力和请求语义，但不能反推历史每一个 400 都由该降级造成。

## 账号和路由边界

当前实例的脱敏账号快照见
[credentials-snapshot-20260901.json](../evidence/real-account-tests-20260901/credentials-snapshot-20260901.json)：

- 认证方式：`social=199`、`api_key=15`、`idc=6`
- endpoint family：`ide=205`、`cli=15`
- 套餐：`KIRO FREE=78`、`KIRO POWER=16`、`KIRO PRO MAX=126`
- `profileArn`：存在 199、缺失 21
- `effectiveApiRegion`：全部为 `us-east-1`
- `disabled=0`、`available=220`
- 当前外部池数量：0

本轮请求全部走本地账号路径。没有外部池，不能据此得出“本地 400 可以切换
外部池恢复”或“外部池也会返回同样 400”的结论。

## 模型标识的三个层次

排查时必须区分以下字段：

1. **requested model**：客户端在 `/cc/v1/messages` 中发送的 `model`，例如
   `claude-sonnet-4-6`。
2. **resolved/upstream model**：本地模型能力目录和显式配置 mapping 解析后的
   模型。usage 中对应 `model`、`upstreamModel`、`modelResolutionSource`。
3. **Kiro `modelId`**：发送到 Kiro IDE/CLI 请求结构中的最终模型字段。它由
   converter 根据已解析模型和 endpoint family 构造，不等同于客户端别名。

当前修复后，普通别名仍可按目录和显式 mapping 解析；显式 Claude minor 版本
不再在没有明确 mapping 时静默降级到另一个 minor。只有 dash/dot 等价拼写或
明确配置规则才允许改变 outbound model。

## 已确认的根因

### A. thinking 控制字段的入口组合非法

Anthropic 兼容请求要求 enabled thinking 满足：

```text
1024 <= budget_tokens < max_tokens
```

旧入口把下列可修复的客户端组合直接送入严格校验或上游：

- `budget_tokens == max_tokens`
- `budget_tokens > max_tokens`
- enabled thinking 缺少 budget
- adaptive/disabled 仍携带 `budget_tokens`

这会产生本地或上游的 400，而不是账号容量问题。

### B. 显式 minor 自动降级存在协议语义风险

将显式 `claude-sonnet-4-6` 静默变为 `claude-sonnet-4.5` 会改变：

- 模型能力；
- thinking/reasoning 支持；
- 输出上限；
- 计费和 usage 口径；
- Kiro 最终 `modelId`。

这不是一个可接受的通用“修错”方式。当前实现对未知显式 minor
`pass-through`，让真实上游返回真实能力错误；不会伪装成另一个模型成功。

### C. 历史矩阵还存在未确认因素

历史公开错误只有统一 `invalid_request_error`，没有保留具体上游字段。因此以下
因素仍可能参与，不能被文档写成已证实：

- assistant thinking/signature/redacted history；
- 工具 schema、工具结果或图片格式；
- 账号授权、订阅或风险控制；
- endpoint family（IDE/CLI）；
- 上游模型发现失败或能力目录不完整；
- 请求体中其他协议字段。

模型能力快照本身也明确记录 `2/6` reasoning cohorts，说明模型列表可用不等于
所有账号和 thinking 形态都已被证明支持。

## 选定修复

### 1. 入口 surgical normalization

修改：

- `src/anthropic/request_facts.rs`
- `src/anthropic/handlers/request_entry.rs`

入口在选路前调用
`normalize_raw_reasoning_protocol_with_probe_and_limit`：

- equality：将 budget 调为 `max_tokens - 1`；
- budget 大于 max：仅在模型上限、2 倍比例、64K 增量和 128K 总上限内扩大
  `max_tokens` 到 `budget + 1`；
- 无法安全扩大：保留原 max，收敛 budget 到 `max - 1`；
- enabled 缺失 budget：推导至少 1024 且保留输出空间的值；
- adaptive/disabled：删除不适用的 `budget_tokens`；
- 重写仅作用于顶层 `thinking` 和 `max_tokens`，不重新序列化 messages、
  tools、图片或历史签名；
- JSON 重复键、扫描错误、无法安全识别的 malformed/ambiguous body 不强行改写，
  继续走严格验证；
- 只有 body 确实被重写才绕过 raw external passthrough，避免 raw 语义与标准
  规范化语义混用。

### 2. 显式 minor 不自动降级

修改：

- `src/anthropic/model_capabilities.rs`

精确模型、dash/dot 等价拼写和显式 mapping 仍可命中；没有明确规则的显式
minor 直接 pass-through。这样错误会暴露真实上游能力边界，不会把错误掩盖成
另一个模型的成功响应。

### 3. retry/fallback 不是本问题的根修复

重试只适用于配置允许的 transient 408/429/5xx、网络错误，或首语义输出前且
确认可安全重放的流错误。确定性的请求格式 400 不应通过无限重试、盲目换号或
强制切外部池掩盖。若未来确认是账号授权类 400，应单独按账号状态分类和配置的
fallback 规则处理。

## 修复后真实账号验收

使用同一个 19023 实例和真实本地账号，短 prompt `Reply with exactly: pong`，
`claude-sonnet-4.5`，四个用例均 HTTP 200：

| 用例 | 输入 | 结果 | 关键证据 |
| --- | --- | --- | --- |
| equality | enabled，`budget=2048,max=2048` | 200；`output_tokens=131`，thinking=130 | usage record |
| bounded expansion | enabled，`budget=4096,max=2048` | 200；请求记录 `requestedMaxTokens=4097`，`output_tokens=257`，thinking=256 | usage record |
| adaptive cleanup | adaptive 携带 budget | 200；`output_tokens=1` | usage record |
| disabled cleanup | disabled 携带 budget | 200；`output_tokens=1` | usage record |

对应快照：

- [usage-normalization-snapshot-20260901.json](../evidence/real-account-tests-20260901/usage-normalization-snapshot-20260901.json)
- [model-capabilities-snapshot-20260901.json](../evidence/real-account-tests-20260901/model-capabilities-snapshot-20260901.json)

四条 usage 记录均为：

```text
endpoint=/cc/v1/messages
model=claude-sonnet-4.5
credentialId=76
status=success
errorType=null
errorMessage=null
```

## 源码和测试验收

通过的 scoped 命令：

```bash
feature/tests/run-cargo-scoped.sh model-resolution -- \
  cargo test -q model_capabilities -- --nocapture
# 35 passed

feature/tests/run-cargo-scoped.sh thinking-normalization -- \
  cargo test -q request_facts -- --nocapture
# 22 passed

feature/tests/run-cargo-scoped.sh thinking-entry-normalization -- \
  cargo test -q messages_entry_normalizes_omitted_or_conflicting_thinking_controls \
  -- --nocapture
# 1 passed
```

三组 target 均由 wrapper 自动清理。`git diff --check` 通过。

## 残余风险和下一步

- 历史 20x2/25 矩阵的公开错误缺少具体上游字段，不能把全部 400 归因成
  thinking；应在受控 debug 采样中记录脱敏的请求事实和上游分类，而不是完整
  credential 或签名。
- 本轮没有外部池，外部池 raw/normalized/SSE 路径需另行做 fake-upstream 和
  真实低并发验证。
- 当前 reasoning discovery 为 `2/6` cohorts；显式 native reasoning 仍按
  unknown/fail-closed 处理，不能由模型列表 alone 推断完整能力。
- 本地服务仍运行在 19023，PID 和命令应在下一次测试前再次核对。
- 本文档不表示已提交、打 tag 或发布；当前任务只完成源码修改、验证和证据整理。

