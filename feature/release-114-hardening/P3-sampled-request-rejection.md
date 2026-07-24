# P3 - Usage 中的 sampled request rejection

## 现象

生产 Usage 明细中出现：

- `errorType=request_rejection`
- `errorMessage=sampled request rejection`
- `model=unknown`
- tokens/cost 均为 0

用户视角看起来像“奇怪报错”。

## 代码语义

这是 request API key admission 层的采样诊断记录，由 `sampled_request_rejection_usage_record` 构造。

它用于在拒绝量较高时低成本记录部分样本，避免每一次入站拒绝都写完整 UsageRecord。记录内容包括：

- `errorMetadata.sampled=true`
- `errorMetadata.reason`
- `errorMetadata.stage`
- `errorMetadata.observedCount`
- `errorMetadata.observedCountIsExact=false`

设计上：

- 不代表请求已经打到上游模型；
- 不代表本地账号或外部池失败；
- 不应产生计费；
- `observedCount` 是采样时看到的单调计数，不是该记录代表的精确请求数。

## 生产证据

在 24h 聚合中，`request_rejection` 采样记录约 41 条，主要原因包括：

- `admission_rpm`
- `admission_queue_timeout`
- `request_entry_invalid`

同一窗口内，成功外部池 0 计费有数万条，因此 `sampled request rejection` 不是 0 计费主因。

## 复现方法

本地可通过 request API key admission 相关测试复现：

- 对同一 request API key 构造超过 RPM 或队列等待上限的请求；
- admission 层返回 429；
- 采样器按幂次/预算策略记录部分 `request_rejection` UsageRecord。

相关测试已有：

- `rejection_micropressure_is_bounded_and_power_of_two_sampled_for_five_rounds`
- `rejection_sampling_isolated_by_key_and_reason_for_five_rounds`
- `high_cardinality_rejection_logs_are_globally_bounded_for_five_rounds`

## 处理结论

本轮不把它作为代码缺陷修复，原因：

1. 它是故意的 admission 观测记录；
2. 数量远小于外部池流式 0 计费主因；
3. 它不应计费，tokens/cost=0 符合设计；
4. 需要继续观察的是 admission 配额是否配置过低，或是否存在某个 request API key 短时打满 RPM/队列。

## 后续建议

如果仍认为 UI 明细里“sampled request rejection”容易误导，可以后续做显示层优化：

- 在 Usage 明细里把 `request_rejection` 显示为“网关准入拒绝采样”；
- 在详情里展示 `reason/stage/observedCountIsExact=false`；
- 不把它归类为上游错误或账号错误。

