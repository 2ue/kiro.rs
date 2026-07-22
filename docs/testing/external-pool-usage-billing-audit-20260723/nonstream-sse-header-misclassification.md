# 非流式响应被 SSE Header 误分流问题 - 2026-07-23

## 问题

修复外部池非流式 0 计费问题时，代码审查发现一个比“非流式 parser 字段不够”更贴近生产复核现象的分支问题：

```text
response_is_stream = route.is_stream() || response_headers_look_like_sse(response_headers)
```

这意味着：

```text
下游请求是非流式；
上游返回 HTTP 200；
上游响应头声明 text/event-stream；
但上游 body 实际是普通 JSON message body；
=>
kiro.rs 会按 stream 分支转发。
```

stream 分支按 SSE event 解析 usage。普通 JSON body 不会被 SSE parser 识别，因此内部 `ExternalUsageCapture` 为空，最终 `UsageRecord` 会缺少 `rawUsage` 和 `externalPoolBilling`。

同时，由于 body 是按 stream body 原样下发的，下游仍可能看到一个包含标准 `usage` 的普通 JSON body。这样就出现了生产复核里的矛盾：

```text
下游 body 有标准 usage；
DB/Redis billing 却缺失。
```

## 旧假设

原设计把问题主要建模为：

```text
非流式 JSON body 已进入 maybe_project_non_stream_usage；
但是 parser 没识别出 usage，或 projection/billing 没生成。
```

这个假设不能完整解释“最终 body 有顶层 Anthropic usage 但 billing missing”的复核样本，因为旧源码理论上能解析顶层 `$.usage`。

## 新代码事实

源码里非流式请求是否走 stream 分支，不只由请求 `stream` 决定，还由上游响应头决定。

如果上游错误或兼容层返回：

```text
content-type: text/event-stream
body: {"type":"message","content":[...],"usage":{...}}
```

旧代码会：

```text
走 stream branch
不读完整 body 做 JSON usage parser
按 SSE event 处理普通 JSON body
下游仍收到 body
record_success 时 billing=None
```

这正好补上了生产证据链中的缺口。

## 修复设计更新

非流式请求不再因为上游响应头像 SSE 就直接进入 stream branch。

新规则：

```text
if route.is_stream():
    走 stream branch

if !route.is_stream():
    一律读完整 body
    如果 body 是 JSON model response:
        按非流式 usage processor 处理
        必要时重写 usage / 注入 usage / 生成 billing
        如果上游 content-type 错标为 text/event-stream:
            下游 content-type 修正为 application/json
    如果 body 是真正 SSE 文本:
        归类为 success protocol error
        retry 其他池或按外部池失败处理
```

这比只补 parser path 更符合目标：

```text
非流式下游请求应返回非流式 JSON；
正常 JSON 200 不能因为上游 header 错标而漏 billing；
真正 SSE-on-non-stream 不应被当作正常非流式 JSON 成功。
```

## 新增测试

新增模拟上游完整接入测试：

```text
external_pool_fake_upstream_non_stream_json_with_sse_header_records_billing
```

测试形态：

```text
fake upstream HTTP 200
content-type: text/event-stream
body: Anthropic JSON message with top-level usage
pool.usage_projection_mode=current_path_policy
route.stream=false
```

断言：

```text
downstream HTTP 200
downstream content-type=application/json
downstream usage 被 current_path_policy 整形
UsageRecord.status=success
UsageRecord.stream=false
rawUsage present
externalPoolBilling present
billing.rawUsage 保留上游 raw usage
billing.reportedUsage 等于下游 body usage
```

本测试需要 `KIRO_RS_TEST_POSTGRES_URL` 和 `KIRO_RS_TEST_REDIS_URL` 才能完整跑 `ExternalPoolManager + Postgres pool definition + Redis lease` 路径。没有测试库时按现有集成测试约定早退。

## 修复文件

```text
src/external_pool.rs
src/external_pool/tests.rs
src/anthropic/usage.rs
```

## 修复状态

已按上述方案完成代码修复并通过验证。

最终实现要点：

```text
route.stream=true:
  继续走 stream branch。

route.stream=false:
  一律读完整 body。
  JSON body 按非流式 usage processor 处理。
  上游误报 text/event-stream 但 body 是 JSON 时，向下游修正 content-type=application/json。
  真正 SSE 文本进入非流式路径时，归类为 success protocol error。
```

已通过验证：

```text
cargo fmt --check
git diff --check
cargo test external_pool:: -- --nocapture
cargo test
cargo build --release
```

完整 fake upstream + backup pool 集成测试：

```text
external_pool_fake_upstream_non_stream_json_with_sse_header_records_billing
```

该测试覆盖了：

```text
downstream body 保持正常 JSON
downstream content-type 修正
usage projection 生效
UsageRecord.rawUsage present
externalPoolBilling present
```
