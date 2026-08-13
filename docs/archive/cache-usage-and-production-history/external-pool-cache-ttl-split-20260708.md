# 外部池流式 usage TTL split 异常计费分析

日期：2026-07-08

## 结论

这次问题不是“配置上限显示异常”，也不是 sub2api UI 展示误差。sub2api 的 `usage_logs.cache_creation_5m_tokens` 和 `usage_logs.cache_creation_1h_tokens` 是真实写入日志并参与成本计算的字段。

现网样本显示，sub2api 通过 kiro.rs 外部池调用同一个 Kiro 兼容上游时，出现了同一条流式请求里：

- 聚合 `cache_creation_tokens` 已经是 kiro.rs 按路径整形后的小值；
- 但 5m/1h 明细仍是上游 raw usage 里的大值；
- sub2api 后续按 5m/1h 明细计费，造成异常高额 cache write 成本。

这属于下游可见 usage 字段混合口径的问题：同一个 response usage 对象里，聚合字段和 TTL 明细字段不是同一套策略输出。

## 现网证据

只读查询窗口：`2026-07-08 13:30:00+08` 到 `14:12:00+08`。

sub2api 高额样本：

| sub2api id | 时间 | account | model | output | cache_creation | 5m | 1h | total_cost |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| `14394484` | 2026-07-08 14:04:47 | 2113 | claude-opus-4-7 | 639 | 1,584 | 3,222,479 | 0 | 20.1566637500 |
| `14394002` | 2026-07-08 14:01:00 | 2113 | claude-opus-4-8 | 135 | 7,782 | 0 | 1,998,336 | 19.9867435000 |
| `14393941` | 2026-07-08 14:00:50 | 2129 | claude-opus-4-8 | 62 | 37,202 | 1,300,180 | 0 | 8.1276935000 |
| `14394019` | 2026-07-08 14:01:03 | 2129 | claude-opus-4-8 | 464 | 37,224 | 0 | 139,594 | 1.4075470000 |

其中 `cache_ttl_overridden=false`，说明 sub2api 没有在记录阶段强制改写 TTL 类型。

kiro.rs 与 sub2api 可对齐样本：

- sub2api `14393941`：`14:00:50.525`，output `62`，`cache_creation=37202`，`5m=1300180`。
- kiro.rs B 同秒请求 `req_01o4692sLMnGZoj6q4omWHKe`：`14:00:50.520`，output `62`，记录列 `rec_creation=37202`，`rec_5m=37202`，外部池 raw usage aggregate `1300180`，reported usage aggregate `37202`。

这说明 sub2api 记录的聚合字段对上了 kiro.rs reported usage，但 sub2api 的 5m 明细对上了上游 raw usage。

另一个同样模式：

- sub2api `14394019`：`14:01:03.269`，output `464`，`cache_creation=37224`，`1h=139594`。
- kiro.rs B 同秒请求 `req_01rju2nSJajyNfmU2acM3odE`：output `464`，reported aggregate `37224`，raw aggregate `139594`。

因此，异常不是 sub2api 独立制造出来的；它是从 kiro.rs 下游响应里看到了一套混合 usage 口径后按字段计费。

## 代码机制

sub2api 的流式解析逻辑在 `../sub2api/backend/internal/service/gateway_service.go`：

- `message_start` 会读取 `message.usage.cache_creation.ephemeral_5m_input_tokens` 和 `ephemeral_1h_input_tokens`，包括 0。
- `message_delta` 会读取 `usage.cache_creation.ephemeral_5m_input_tokens` 和 `ephemeral_1h_input_tokens`，只要大于 0 就覆盖。
- `buildRecordUsageLog` 会把 `ForwardResult.Usage.CacheCreation5mTokens/CacheCreation1hTokens` 写入 `usage_logs`。

kiro.rs 外部池流式逻辑在 `src/external_pool.rs`：

- `event_passthrough` 不是字节级完全透传；它会按完整 SSE event drain。
- 如果当前外部池配置 `usage_projection_mode=current_path_policy`，usage event 会进入路径策略投影。
- 历史风险点是：投影后的 flat aggregate 和 raw nested TTL split 可能在不同 event 或同一 event 内混合，被下游合并解析后形成 `cache_creation_tokens` 小、`cache_creation_5m/1h_tokens` 大的记录。

Anthropic Messages usage 支持 `cache_creation` nested TTL 明细；该明细必须和 `cache_creation_input_tokens` 同口径，否则任何按 TTL split 计费的下游都会产生错账。

## 修复策略

本次修复不把问题处理成“删除 5m/1h 明细”。原因是下游确实可能需要 Kiro/Anthropic 兼容的 TTL split，而且用户要求外部池按入口路径整形后的 usage 返回给下游。

修复后的规则：

1. 外部池 raw usage 捕获会读取 nested `usage.cache_creation.ephemeral_5m_input_tokens/ephemeral_1h_input_tokens`。
2. 当 `usage_projection_mode=current_path_policy` 命中时，下游可见 usage 的 flat 字段和 nested TTL split 都来自同一个 projected/reported usage。
3. 投影后的 response 会保证：
   - `cache_creation_input_tokens = cache_creation.ephemeral_5m_input_tokens + cache_creation.ephemeral_1h_input_tokens`
   - 不再把上游 raw nested split 留给下游。
4. 当 usage projection 关闭时，外部池仍保持真实透传，不改写 nested split。

对应代码：

- `src/external_pool.rs::cache_usage_from_value`：读取 nested TTL split，并在只有 nested、flat aggregate 为 0 时把 split 之和作为 raw cache creation。
- `src/external_pool.rs::apply_projected_cache_creation_breakdown`：把 projected usage 的 5m/1h 明细写回 `usage.cache_creation`，并清理旧 flat split 字段。
- 新增单测覆盖 130 万 5m、199 万 1h、nested-only、projection disabled 透传四类情况。

## 对现网数据的解释

`14394002` 贵，是因为 sub2api 真实记录并计费了约 199.8 万 1h cache write tokens。这个事实成立。

问题不在“配置上限允许 2M”本身，而在下游看到了 raw 1h split 与 shaped aggregate 的混合 usage。即使某个上游 raw 请求真的写入了约 199.8 万 1h cache tokens，只要 kiro.rs 配置要求按入口路径整形，就不应该把 raw 1h split 和整形后的 aggregate 同时返回给下游。

修复后，如果路径整形结果只有 7,782 cache creation，那么 nested 5m/1h 明细之和也必须是 7,782；如果路径整形结果没有 cache creation，则 nested `cache_creation` 会被移除。

