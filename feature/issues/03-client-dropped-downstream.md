# 下游客户端断开（非缺陷，仅排除误判）

Status: `historical-classification-valid / cleanup-and-resource-gate-pending`

Severity: P2 informational

- 状态：已定性，无需修复
- 严重级别：无 —— 非接口缺陷
- 数量：生产近 12 小时 17 条（占非成功请求 6.0%）
- 分类来源：`tmp/analysis-usage-llm-errors` root-cause `03-client_dropped_downstream`

## 现象

```
client_dropped: downstream client dropped before upstream stream completed
errorSource: downstream_client
errorType: client_dropped
```

上游流尚未完成前，下游客户端（用户侧 / 调用方）主动断开连接。

## 根因与性质判定：客户端行为，非程序问题

- 这是**客户端行为**（用户取消、客户端超时、页面关闭等），不是代理或上游的缺陷。
- 生产分析报告本身已声明：此类"不作为大模型接口根因，只保留样本用于排除误判"。

## 程序可规避性

- ❌ 不适用。代理无法阻止客户端断开。
- **唯一注意点**：统计接口成功率 / 错误率时，应把 `client_dropped` 从"服务端错误"中剔除，避免污染真实故障率指标。当前分类已正确独立成类，符合预期。

## 复现说明

客户端行为可稳定复现：向隔离临时服务发起流式请求，在 response headers、thinking、text 和 tool_use 四个提交点分别关闭客户端 socket。虽然断开本身不是服务缺陷，但 cleanup、usage 分类、permit/lease 释放具有回归价值，必须纳入 F01/L4。

## 处理方案

- 不把 `client_dropped` 计入服务端或上游错误率，但保留独立 usage/terminal reason。
- 客户端 drop 后取消下游 writer 和不再需要的上游读取任务，释放 request-key permit、credential/external lease、socket 和 buffer。
- 已提交后不得因客户端断开触发服务端换号重试；否则会产生无消费者的内部 RPM。

## 验证与证据

历史生产样本证明分类存在，不证明资源释放完整。最终候选需在四个提交点各 5 轮，记录 upstream hit、cancel latency、RSS、FD、active permit/lease；停止流量并 idle 后资源回到 F01/F02 阈值。

## 残余风险与回滚

当前尚缺 release candidate 的 client-drop chaos 和长流资源回落证据。回滚不能把该类重新归为 success 或 generic 500，也不能在 drop 后继续自动 retry；若主动取消上游导致兼容问题，可保留 bounded drain，但必须受 timeout 和资源上限约束。

## 关联

- 生产证据：`tmp/analysis-usage-llm-errors/root-causes/03-client_dropped_downstream/`。
