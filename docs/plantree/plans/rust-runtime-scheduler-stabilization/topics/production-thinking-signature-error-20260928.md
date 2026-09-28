# 生产 Thinking Signature 错误审计与根因跟进

Status: `Completed / read-only evidence analyzed / remediation not applied`

Last reviewed: 2026-09-28 Asia/Shanghai

## 当前任务边界

本专题只分析生产机器 `152.53.243.159:59137` 上用户指定请求的
`THINKING_SIGNATURE_INVALID`、signature retry、stream read error 和 retry 后
too-long 错误。仅允许只读证据采集，不修改生产代码、配置、数据库、Redis、
容器、进程或账号状态。

用户已明确：不要拿 v161 做历史数量对比，因为旧数据已经清理；“错误增多”只作为
现象描述，分析应聚焦当前机制上哪些路径可能导致这些问题。

## 追加跟进：`req_01hJ2YhjMEyjrrVEQFTZpqri`

Status: `Completed / root-cause follow-up documented / no code changes`

### 待办

- [x] 固定任务边界：只分析当前签名/重试/体积问题，不切换任务，不修改生产。
- [x] 精确查询 `usage_records.id=req_01hJ2YhjMEyjrrVEQFTZpqri`。
- [x] 导出最近 5000 条 usage 中所有 `THINKING_SIGNATURE_INVALID ->
  CONTENT_LENGTH_EXCEEDS_THRESHOLD` 同类记录。
- [x] 重新采集 runtime config payload guard 快照，确认 `on_too_long` 和
  `payloadGuardMaxBytes=460800`。
- [x] 对照生产版本 `v0.0.173` 源码，确认 signature retry 与 too-long retry 的
  状态机顺序。
- [x] 输出中文根因文档和可优化方案。

### 根因摘要

`req_01hJ2YhjMEyjrrVEQFTZpqri` 不是前一条 stream read error 的同形态问题。
它的真实链路是：

```text
Attempt 1: 400 THINKING_SIGNATURE_INVALID
  -> 同账号 signature retry，剥离历史 reasoningContent
Attempt 2: 400 CONTENT_LENGTH_EXCEEDS_THRESHOLD
  -> 没有经过统一 too-long classifier / outbound admission
  -> 最终记录为 thinking_signature_retry 场景下的 invalid_request
```

同一个 usage 记录包含两次上游 attempt。第二次不再是 signature，不代表签名被修复；
只是 retry body 已经没有第一次的历史 signed thinking。目标请求最终失败的直接原因是
5.76 MB / 1996 history entries 的 retry body 仍然过大，而当前生产 `v0.0.173` 没有
用统一 outbound body admission 和 too-long classifier 处理这个派生请求的第二次
上游拒绝。

当前窗口内共导出 28 条同类记录，全部为 `/ha/v1/messages`、`claude-opus-4-8`、
credential `#1428`、约 5.76 MB、1996 history entries、`64000` max tokens 的
同一形态。

正式报告：

- [生产 `req_01hJ2YhjMEyjrrVEQFTZpqri` Thinking Signature 根因跟进](../../../../analysis/production-thinking-signature-followup-root-cause-20260928.md)

本地证据：

- `tmp/prod-evidence/20260928-162208-152.53.243.159-signature-followup/`

## 用户提供的现象

- 内部 HTTP 状态：`200`
- 错误阶段：`local_account_stream`
- 请求 ID：`req_01tA8uNUqeCYgg94MNMbNaX6`
- 上游状态：`400`
- 上游原因：`THINKING_SIGNATURE_INVALID`
- 上游消息：`messages.27.content.0: Invalid signature in thinking block`
- 账号：`#1423`
- 内部摘要：`upstream stream read error: error decoding response body`

以上内容已完成只读核对。最终结论是：页面里的 `THINKING_SIGNATURE_INVALID`
是第一次上游 attempt 的真实 400；最终失败是第二次同账号兼容性重试在 HTTP 200
响应头之后发生的流式 body read error。两者有关联但不是同一个直接根因。

## 待办

- [x] 建立本地只读证据目录和命令记录。
- [x] 盘点远端部署目录、容器、版本、健康状态、挂载和数据连接。
- [x] 通过 PostgreSQL 精确查询请求 ID 的 `usage_records.data` 和尝试链路。
- [x] 核对账号 `#1423` 的 runtime state、最近失败/刷新/冷却事件。
- [x] 核对 thinking/signature 相关 runtime 配置和请求路径。
- [x] 仅在业务证据不足时，按请求 ID 采集有限应用日志和 tool-format debug 证据。
- [x] 区分已证实事实、代码推断、仍未知项，判断 200 包装是否为错误归因问题。
- [x] 形成问题分类、根因假设、风险、优化建议和不修改生产的后续验证方案。
- [x] 更新本专题、implementation status 和 evidence index。

## 关键事实

- `req_01tA8uNUqeCYgg94MNMbNaX6` 是 `usage_records.data.errorId`，对应
  canonical usage row `req_01cs7AQe21bKgNrxrgrygdTN`。
- Attempt 1：账号 `#1423`，HTTP 400，`THINKING_SIGNATURE_INVALID`，
  `messages.27.content.0`，action 为 `thinking_signature_retry_same_credential`。
- Attempt 2：仍为账号 `#1423`，同账号剥离历史 reasoning 后重试，HTTP 200
  response headers received。
- Attempt 2 已收到 167 个 reasoning frame、22452 个首输出前上游字节，首个
  thinking/输出时间为 19671 ms，且 `downstreamCommitted=true`。
- 最终 usage 错误是 `upstream stream read error: error decoding response body`，
  状态为 `stream_error`，公共状态保留 200。
- `payloadGuardReport.removedHistoryThinkingBlocks=0`，
  `removedHistoryThinkingChars=0`，不能把本条请求的第一次 400 归因到本次
  payload guard 删除历史 thinking。
- 生产 app/PostgreSQL/Redis healthy；账号 `#1423` 没有被持久禁用，也没有
  最近 credential event。
- 账号 `#1423` 8 小时 stream error 中，11 条无 signature 片段、3 条带
  `THINKING_SIGNATURE_INVALID`；全实例 8 小时 stream error 中，99 条无
  signature 片段、17 条带 signature 片段。

## 结论

本次问题是复合链路：

```text
原始请求含无效历史 thinking signature
  -> 第一次上游 400 THINKING_SIGNATURE_INVALID
  -> 系统同账号剥离历史 reasoning 后重试
  -> 第二次上游响应头 200
  -> 已接收 reasoning 并提交下游
  -> 第二次流式 body 读取阶段 error decoding response body
  -> post-commit 不安全重放，只能发送 SSE error 并记录 stream_error
```

其中第一次 400 是已确认事实，最终失败的直接原因是第二次流读取错误。当前证据
不能证明第二次流读取错误由第一次 signature invalid 直接导致。

## 输出

正式中文报告：

- [生产请求 `THINKING_SIGNATURE_INVALID` 与流读取错误分析](../../../../analysis/production-thinking-signature-error-20260928.md)

本地证据：

- `tmp/prod-evidence/20260928-145914-152.53.243.159-thinking-signature/`

建议优化：

- 将 prior attempt failure 与 final stream failure 分开展示和存储。
- 补充第二次流读取错误的传输层脱敏观测字段。
- 区分原始上游 body bytes 与诊断片段 bytes。
- 为 stream read error 增加短期账号/模型/region 质量指标，但不要直接永久禁用账号。
- signature/payload guard/多块 thinking 改动必须先完成脱敏真实抓包矩阵。

未做事项：

- 未修改生产代码、配置、数据库、Redis、容器、进程或账号状态。
- 未修改本地业务代码。
- 未发布新版本。
- 未执行生产压测。

## 预期验收

1. 能还原该请求从入口、账号选择、上游发送、流式响应读取到最终 usage 的时间线。
2. 明确 `signature` 是在本地转换/重试/裁剪后失效，还是上游原始请求已拒绝。
3. 明确为什么内部状态为 HTTP 200，以及这是否会影响 Claude Code CLI 的继续/中断行为。
4. 明确账号 `#1423` 是否存在重复复现、冷却、错误计数或异常 token 状态。
5. 对每个结论标记证据来源；没有证据的部分保持为待验证假设。

## 证据目录

本次原始证据保存在本地 `tmp/prod-evidence/` 下，默认不打包、不上传、不写入生产。
