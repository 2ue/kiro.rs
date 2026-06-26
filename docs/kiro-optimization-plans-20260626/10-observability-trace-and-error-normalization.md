# 观测链路与错误归一化实施方案

## 适用范围

本方案处理 request id、error id、usage diagnostics、内部原始错误保留、对下游统一英文错误、日志去重、异步记录、性能边界和可选 OpenTelemetry。

## 来源项目与学习点

- `kirocc-prox`：OTel 和 tracing 思路值得学习。
- 当前项目 `src/anthropic/envelope.rs`：已有 request id 和 error id 基础。
- 当前项目 `src/external_pool.rs`：已经保留外部账号原始错误并对下游归一化，是本方案的直接基础。

## 当前项目现状

已有能力：

- `request-id` / `anthropic-request-id` headers。
- `x-kiro-rs-error-id`。
- usage record。
- usage latency trace。
- 外部账号错误 diagnostics。
- 异步 PgSQL usage 写入队列。

需要加强：

- 错误信息需要统一英文口径。
- 对下游不能出现内部概念。
- 内部原始错误要完整，但不能重复。
- 每条错误日志要能通过唯一 ID 关联 usage 和系统日志。
- 记录不能影响接口性能。

## 目标

- 每个请求必须有 request id。
- 每个错误必须有 error id。
- 下游响应必须能拿到 error id。
- 内部 usage 必须能通过 error id 找到原始错误摘要。
- 对下游不得出现 pool、fallback、external、credential、backup 等内部词。
- 记录必须异步或有界，不能卡住接口。

## 非目标

- 不把所有日志都改成 OTel。
- 不记录完整请求体。
- 不记录 token。
- 不把内部错误分类直接暴露给下游。

## 涉及文件

- `src/anthropic/envelope.rs`
- `src/anthropic/usage.rs`
- `src/kiro/call_trace.rs`
- `src/external_pool.rs`
- `src/kiro/provider.rs`
- `src/model/config.rs`

## 术语规范

对下游允许使用：

- `request`
- `account`
- `model`
- `endpoint`
- `administrator`
- `error ID`

对下游禁止使用：

- `credential`
- `external pool`
- `fallback pool`
- `backup pool`
- `sticky fallback`
- `scheduler`
- `lease`
- `capacity snapshot`
- `upstream raw body`

内部日志可以使用代码概念，但必须放在内部 diagnostics 中。

## 对外错误消息表

所有 message 必须是英文。

| 场景 | HTTP 状态 | message |
| --- | --- | --- |
| 请求体格式错误 | 400 | `The request body is invalid.` |
| tool sequence 错误 | 400 | `The request body has an invalid tool-use sequence.` |
| 模型不可用 | 400 | `The requested model is not available for this endpoint.` |
| 未配置自定义路由 | 404 | `The requested endpoint is not configured.` |
| 暂无账号可接请求 | 429 | `No account is ready for this request right now. Please retry shortly.` |
| 队列已满 | 429 | `The request queue is full right now. Please retry shortly.` |
| 等待超时 | 429 | `No account became ready before the dispatch wait timeout.` |
| 上游账号无法完成 | 502 | `The upstream account could not complete this request.` |
| 上游 stream 异常 | 502 | `The upstream account returned an invalid stream.` |
| 上游 stream idle | 504 | `The upstream account did not produce data before the stream timeout.` |
| 内部未分类错误 | 500 | `The request could not be completed.` |

最终 message 必须追加：

```text
If this continues, contact the administrator with error ID: {error_id}
```

## 内部错误诊断结构

新增或统一结构：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDiagnostic {
    pub error_id: String,
    pub request_id: String,
    pub public_error_type: String,
    pub public_status_code: u16,
    pub internal_source: String,
    pub internal_reason: String,
    pub upstream_status_code: Option<u16>,
    pub upstream_request_id: Option<String>,
    pub raw_message: Option<String>,
    pub raw_message_truncated: bool,
    pub retryable: Option<bool>,
    pub account_id: Option<u32>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}
```

去重规则：

- `error_id` 只在顶层出现。
- `request_id` 只在顶层出现。
- `raw_message` 是唯一原始 message 字段。
- `upstream_status_code` 是唯一上游状态码字段。
- `metadata` 不得重复放入以上字段。
- `metadata` 只放补充信息，例如 cooldown seconds、retry after、body hash。

## 异步记录与性能边界

要求：

- usage/error 写入使用有界队列。
- 队列满时不得阻塞下游接口。
- 队列满时允许丢弃详细 diagnostics，但必须增加 dropped counter。
- request id 和 error id 生成必须 O(1)。
- 错误 metadata 必须有大小上限。

新增配置：

```rust
pub error_diagnostic_enabled: bool, // 默认 true
pub error_diagnostic_max_raw_message_bytes: usize, // 默认 2048
pub error_diagnostic_max_metadata_bytes: usize, // 默认 8192
pub error_diagnostic_queue_capacity: usize, // 默认复用 usage 队列或 4096
```

## OTel 可选方案

新增 feature flag：

```text
otel
```

默认不启用。

启用后 span 命名：

- `kiro.request`
- `kiro.dispatch`
- `kiro.upstream`
- `kiro.stream`
- `kiro.usage_record`

span attribute 不得包含 token、完整 prompt、完整 response。

## 实施步骤

1. 梳理所有错误出口。
2. 将对外 message 映射集中到 `envelope.rs` 或新增 `error_mapping.rs`。
3. 统一生成 error id。
4. 将 raw upstream error 写入 `ErrorDiagnostic`。
5. 修改外部账号和本地账号错误路径，使用同一 mapping。
6. 增加去重测试。
7. 增加日志字段 request id 和 error id。
8. 可选接入 OTel，不作为第一阶段必需项。

## 测试方案

新增测试：

- `public_error_does_not_contain_internal_terms`
- `public_error_is_english`
- `error_id_is_present_in_body_and_header`
- `request_id_is_present_in_body_and_header`
- `raw_upstream_error_is_recorded_in_diagnostic`
- `external_account_raw_error_is_not_returned_downstream`
- `error_diagnostic_does_not_duplicate_error_id`
- `error_diagnostic_truncates_raw_message`
- `diagnostic_queue_full_does_not_block_response`

真实测试：

- 上游 400 invalid tool use。
- 上游 429。
- 上游 500。
- 外部账号错误。
- 未配置 `/dfcache/*`。
- 无账号可用。

## 验收标准

- 下游所有错误都是统一英文。
- 下游错误都带 error id。
- usage 中能用 error id 找到内部原始错误摘要。
- 对下游不出现内部模块词。
- 诊断记录不重复。
- 高并发错误场景下接口不被日志写入拖慢。

## 风险与回滚

风险：

- 错误映射集中化时漏掉某个出口。
- metadata 过大影响写入。

规避：

- 用 `rg` 检查所有 `error_response` 和 `api_error` 出口。
- metadata 设置上限。
- 保留原日志作为短期 fallback。

回滚：

- 关闭 `error_diagnostic_enabled`。
- 保留 request id 和 error id，不回滚对外统一口径。

## 不得做的事项

- 不得把外部账号原始错误直接返回给下游。
- 不得把内部模块名写进对外 message。
- 不得记录完整请求体或 token。
- 不得让日志写入阻塞接口。
- 不得用中文作为对外 API 错误 message。

## 后续可选扩展

后续可以做管理端 error id 搜索页，输入 error id 后展示 request id、账号、route、model、内部原因和原始错误摘要。

