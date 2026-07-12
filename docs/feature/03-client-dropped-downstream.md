# 下游客户端断开（非缺陷，仅排除误判）

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

## 性质判定：客户端行为，非程序问题

- 这是**客户端行为**（用户取消、客户端超时、页面关闭等），不是代理或上游的缺陷。
- 生产分析报告本身已声明：此类"不作为大模型接口根因，只保留样本用于排除误判"。

## 程序可规避性

- ❌ 不适用。代理无法阻止客户端断开。
- **唯一注意点**：统计接口成功率 / 错误率时，应把 `client_dropped` 从"服务端错误"中剔除，避免污染真实故障率指标。当前分类已正确独立成类，符合预期。

## 复现说明

客户端行为，可平凡复现（发起流式请求后立即断开连接），但无验证价值。无需纳入回归。

## 关联

- 生产证据：`tmp/analysis-usage-llm-errors/root-causes/03-client_dropped_downstream/`。
