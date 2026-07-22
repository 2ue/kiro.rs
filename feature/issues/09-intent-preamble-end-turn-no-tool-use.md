# 模型意图开场白后 end_turn 空转（看似"卡住/中断"）

Status: `usage-observability-focused-pass / long-session-statistical-gate-pending`

Severity: P1 experience/observability

- 状态：根因已用现网数据坐实；2026-07-13 已实现 usage-only 诊断字段，待真实长会话观察
- 严重级别：中 —— 非错误、不影响计费与稳定性，但影响 agent 交互体验（看似卡住）
- 影响端点：全部 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1`、`/dfcache/*`（共用请求处理逻辑）
- 性质：**模型行为**，非代理缺陷、非上游错误。代理只能"观测/软引导"，不能硬修复。

## 现象

Agent 会话中，助手输出一句"意图开场白"（如"先摸清楚当前进程和端口状态""让我找一下历史文件"）后就停住：
- 无后续文本、无工具调用、无报错；
- 约几秒后 UI 停在转圈或直接结束；
- 客户端（Claude Code）没有自动发起下一轮。

用户观感是"说要做却中断了"。

## 现网证据（已坐实，非推断）

线上库 `usage_records`（部署 `kiro-rs-2ue-59137`），本次会话 `conversation_id = 4633d467-317c-4620-9545-a26f2d81eb66`，145+ 条记录 **全部 `status=success`**，无一条 error/stream_error/upstream_timeout。

断流轮（15:36:55）`data` 关键字段：
```
status                  = success
terminalReason          = completed
downstreamStopReason    = end_turn      ← 决定性
outputTokens            = 77
firstOutputDeltaMs      = 1477          ← 首字 1.5s，无卡顿
firstVisibleTextDeltaMs = 1477
duration_ms             = 3499          ← 3.5s 正常结束
```

同会话最近轮次呈 `end_turn` 与 `tool_use` 交替，全部 `completed`：
```
15:56:27 tool_use  completed   ← 正常发出工具调用
15:53:31 end_turn  completed   ← 纯文本收尾（观感"中断"）
15:52:19 end_turn  completed
15:49:47 tool_use  completed
```

**结论**：每一次"中断"在服务侧都是一次干净的、成功的 `end_turn`。模型吐出意图开场白后主动结束该轮、未在本轮产出 tool_use。代理如实转发 `message_delta(stop_reason=end_turn)` + `message_stop`。

## 根因

模型（尤其 Opus）有时把"声明意图"的开场白单独作为一轮 `end_turn` 输出，把真正的工具调用留到下一轮。正常情况 Claude Code CLI 会自动接续下一轮；在某些时序下（典型：用户在模型输出/工具执行途中插话，打断了 CLI 的自动续轮），下一轮未触发，表现为"卡住"。

- 根源：模型生成层行为（不确定性）。
- 诱因：CLI 续轮时序（本次三次复现均伴随用户中途插话）。
- 代理：无责，无中断能力（见下）。

## 代理无"静默断流"路径（代码佐证）

`src/anthropic/handlers.rs:5584` 的流式 `tokio::select!` 循环，所有结束路径都发终止帧：

| 路径 | 位置 | 客户端收到 |
|---|---|---|
| 上游正常 EOF | `handlers.rs:5738` None 分支 | `message_delta`(stop_reason)+`message_stop` |
| 读流错误 | `5722` Some(Err) | SSE `error` 事件 |
| 空闲超时(180s) | `5819` idle_sleep | SSE `error` 事件 |
| 上游 JSON 错误体 | `5739`/`5757` | SSE `error` 事件 |

异常路径统一走 `record_stream_error` → `generate_final_events`(`stream.rs:2735`) 发 `error` 事件。**不存在"不发任何东西掐流"的路径。** 本现象走的是正常 EOF + `end_turn`。

## 诊断方案（2026-07-13 已实施，零风险）

目标：在 usage `data` 里标记"疑似意图开场白空转"轮次，便于按会话统计频率、定位场景，**不改变任何流行为、不注入 prompt、不影响 prompt cache**。

### 可用信号（均已存在，无需新造）

- `StreamStateManager.has_tool_use`（`stream.rs:1004`）：本轮是否发出过 tool_use。
- `StreamStateManager.has_non_thinking_blocks()`（`stream.rs:1072`）：本轮是否产出非 thinking 内容（即有无实际文本）。
- `downstream_stop_reason()`（`stream.rs:1504`）→ `get_stop_reason()`（`1099`）：最终 stop_reason。
- `StreamContext` 已持有 `known_tools`（`new_with_..._known_tools`，`stream.rs:1375+`）：本次请求是否带工具定义。
- 写入链路：`handlers.rs:2703` `set_downstream_stop_reason` + usage `data` jsonb（`usage.rs:310` `downstream_stop_reason`）。

### 判定条件

```
downstreamStopReason == "end_turn"
  && has_tool_use == false            // 本轮未调工具
  && known_tools 非空                 // agent 场景：本可调工具
  && has_non_thinking_blocks == true  // 确实吐了文本（排除空轮/纯 thinking）
  && outputTokens < 阈值(建议 200)     // 短开场白特征（可选，降噪）
```

命中 → 在 usage `data` 写：
- `suspectedIntentPreambleEndTurn: true`
- （可选）计数字段，便于聚合。

### 落点

- 判定在流终结处（`stream.rs` `generate_final_events` 附近，已能拿到全部信号），或在 `handlers.rs:2703` 组装 usage 前。
- 仅追加 `data` 字段，不动 SSE 输出、不动 stop_reason。

## 排查用查询（诊断上线后）

```sql
SELECT to_char(created_at,'MI:SS') t, model, output_tokens,
       data->>'downstreamStopReason' stop,
       data->>'suspectedIntentPreambleEndTurn' flag
FROM usage_records
WHERE conversation_id = '<会话UUID>'
ORDER BY created_at;
```
按 `flag='true'` 聚合，可回答：多频繁、集中在哪些模型/场景、是否都伴随插话。

## 复现方案

在隔离 Claude Code CLI session 中提供可调用工具，连续进行至少 20 轮，并在 assistant 输出或工具执行途中插入 follow-up。服务端同时记录 stop reason、tool_use、known tools、可见文本、output tokens 与 risk 字段；CLI JSONL 和 usage 以 request ID 对齐。对照组是不插话的相同任务。每组至少 5 个 session，不能把任意短 `end_turn` 都标成空转。

## 不做什么（已评估否决）

- **代理自动续轮**：❌ 代理无状态转发、不持有完整上下文与工具结果，自续会破坏 turn 语义、导致重复/乱序/自续循环。风险 >> 收益。
- **全局 system 软引导**："在同一轮内完成说明与工具调用"过弱、且污染所有请求 prompt、破坏 prompt cache、与既有注入叠加。**暂缓**，待诊断数据证明高频且集中，再针对性、条件性注入。

## 回归清单

- [x] 单测：`end_turn + 无 tool_use + 有工具定义 + 有文本` → 命中标记。
- [x] 单测：`tool_use` 轮不命中。
- [x] 诊断字段仅出现在 usage `latencyTrace`，不改变 SSE 输出与 stop_reason。
- [ ] 现网按会话查询能聚合出该 flag。

## 关联

- 代理/上游各类错误归档：`feature/README.md`。
- 流式终止路径：`feature/issues/02-stream-upstream-idle-timeout.md`（同为流式，但那是真超时错误，本文是正常 end_turn）。
- 现网证据：`usage_records`，`conversation_id=4633d467-317c-4620-9545-a26f2d81eb66`（部署 `kiro-rs-2ue-59137`）。

## 残余风险与回滚

当前规则只是诊断启发式，可能把合法短回答标为 low/medium risk；它不能证明模型本应调用工具，也不能驱动代理自动续轮。回滚可移除新增诊断字段，但不得改写 stop_reason 或自动生成下一轮。是否增加语言/任务软提示只能由更大样本统计决定，并应保持可独立开关。
