# 真实压测与异常测试工具实施方案

## 适用范围

本方案用于建立可复现的压测、异常注入、长会话、streaming、thinking、高缓存、自定义路由、RPM、并发、恢复能力测试工具。

该工具是后续调度、缓存、streaming、错误归一化优化的前置条件。没有该工具，不得合并会改变热路径的调度和重试改动。

## 来源项目与学习点

- `kiroxy`：有独立 loadtest 工具思路，覆盖 streaming 和延迟统计。
- `cp-coder9/kiro-gateway/tests/README.md`：测试矩阵组织清晰，适合把协议转换、thinking、tool use、错误路由分开验证。
- 当前项目：已有真实 usage 记录、request id、error id，可作为压测结果核对来源。

## 当前项目现状

当前项目已经可以通过单测覆盖部分转换和 usage 逻辑，但不足以回答：

- 大并发下接口是否变慢。
- 哪些参数会导致排队变慢。
- 上游突然慢、突然恢复时，调度是否正常。
- 单账号 RPM 是否真实生效。
- stream 卡死是否释放 lease。
- thinking 是否有真实思维流输出。
- 错误是否被归一化，同时内部日志是否保留原始原因。

## 目标

- 提供一个本地可运行的压测 CLI。
- 提供一个 fake Kiro server，用于稳定复现异常。
- 支持真实本地账号调用，但必须显式开启，避免误打现网。
- 每轮测试输出机器可读报告。
- 测试必须能验证 usage、日志、错误 ID、RPM、并发、内存、FD、延迟。

## 非目标

- 不把压测工具作为生产服务的一部分启动。
- 不默认请求真实上游。
- 不在 CI 中默认跑长时间真实上游测试。
- 不要求压测结果绝对固定，但必须有明确阈值。

## 涉及文件

建议新增：

```text
src/bin/kiro_loadtest.rs
tests/support/fake_kiro_server.rs
tests/support/loadtest_client.rs
tests/loadtest_scenarios.rs
docs/testing/loadtest.md
```

可选新增：

```text
xtask/src/loadtest.rs
```

如果项目没有 `xtask`，优先使用 `src/bin/kiro_loadtest.rs`。

## CLI 设计

命令：

```bash
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:9022 \
  --route /cc/v1/messages \
  --model claude-sonnet-4-20250514 \
  --concurrency 20 \
  --requests 500 \
  --scenario normal_stream \
  --report target/loadtest/report.json
```

必须支持参数：

```text
--base-url
--route
--model
--concurrency
--requests
--duration-secs
--scenario
--stream true|false
--thinking true|false
--tool-use true|false
--cache-control true|false
--dfcache-route
--rpm-target-account
--timeout-secs
--report
--real-upstream true|false
--auth-key
```

`--real-upstream=true` 必须要求额外环境变量：

```text
KIRO_LOADTEST_ALLOW_REAL_UPSTREAM=1
```

没有该环境变量时必须拒绝运行真实上游测试。

## Fake Kiro Server 场景

fake server 必须支持：

| 场景 | 行为 |
| --- | --- |
| `normal_stream` | 立即返回合法 SSE |
| `slow_first_byte` | 延迟 N 秒后返回第一个 event |
| `slow_thinking_then_text` | 先输出 thinking delta，再延迟输出 text |
| `stream_idle_timeout` | 返回 headers 后长时间不输出 event |
| `json_exception_200` | HTTP 200 但 body 是 JSON exception |
| `rate_limit_429` | 返回 429 |
| `server_error_500` | 返回 500 |
| `invalid_tool_format` | 模拟 Kiro 返回 Invalid tool use format |
| `malformed_sse` | 输出不完整 SSE frame |
| `client_drop` | 客户端中途断开 |
| `recovery_after_burst` | 前 N 秒慢或报错，之后恢复正常 |

fake server 必须在每次请求响应里带唯一 upstream request id，便于核对代理 trace。

## 测试场景矩阵

必须至少覆盖：

1. 普通非流式请求。
2. 普通流式请求。
3. thinking 流式请求，必须检查真实 thinking delta 输出。
4. thinking-only 后 end_turn。
5. 长会话多轮请求。
6. tool_use + tool_result 正常配对。
7. tool_result 缺失。
8. tool_result 多余。
9. `/cc/v1/messages` 高缓存路由。
10. `/ha/v1/messages` 高缓存路由。
11. `/na/v1/messages` 路由策略。
12. `/dfcache/{name}/v1/messages` 已配置路由。
13. `/dfcache/{name}/v1/messages` 未配置路由。
14. 单账号 RPM 限制。
15. 单账号并发限制。
16. 全局并发限制。
17. 上游慢首字。
18. 上游 429。
19. 上游 500。
20. HTTP 200 JSON exception。
21. stream idle timeout。
22. 客户端主动断开。
23. 突发大流量。
24. 突然全部异常。
25. 突然恢复正常。

## 指标采集

报告 JSON 必须包含：

```json
{
  "scenario": "normal_stream",
  "startedAt": "...",
  "durationMs": 0,
  "requests": 0,
  "success": 0,
  "errors": 0,
  "statusCounts": {},
  "ttfbMs": {"p50": 0, "p95": 0, "p99": 0},
  "firstThinkingMs": {"p50": 0, "p95": 0, "p99": 0},
  "firstTextMs": {"p50": 0, "p95": 0, "p99": 0},
  "totalLatencyMs": {"p50": 0, "p95": 0, "p99": 0},
  "memory": {"rssStartBytes": 0, "rssPeakBytes": 0, "rssEndBytes": 0},
  "fileDescriptors": {"start": 0, "peak": 0, "end": 0},
  "requestIds": [],
  "errorIds": []
}
```

指标定义：

- `ttfbMs`：从代理收到请求到下游收到第一个字节。
- `firstThinkingMs`：从代理收到请求到下游收到第一个 thinking delta。
- `firstTextMs`：从代理收到请求到下游收到第一个 text delta。
- `totalLatencyMs`：从代理收到请求到响应结束。
- RSS 必须从进程采样，不得只依赖 Rust 内部计数。
- FD 必须采样打开文件描述符数量。

## 性能验收阈值

本地 fake server 场景建议阈值：

- `normal_stream` P95 TTFB 小于 300ms。
- `normal_non_stream` P95 total latency 小于 500ms。
- 100 并发、1000 请求后 RSS 增长小于 100MB。
- 测试结束 10 秒后 FD 数量回落到开始值的 110% 以内。
- client drop 后 lease 必须释放。
- stream idle timeout 后 lease 必须释放。

真实上游场景不设固定延迟阈值，但必须输出趋势：

- P50/P95/P99 TTFB。
- 首个 thinking delta 时间。
- 首个 text delta 时间。
- 每个账号请求数。
- 每个账号错误数。
- RPM 命中次数。

## 实施步骤

1. 新增 fake Kiro server。
2. 新增 loadtest client。
3. 先支持普通 stream 和非 stream。
4. 增加 thinking、tool use、cache_control 请求模板。
5. 增加 route 矩阵。
6. 增加内存和 FD 采样。
7. 增加 usage 反查：根据 request id 查询系统 usage，确认记录存在。
8. 增加报告输出。
9. 把高风险测试放入手动命令，不放入默认 CI。

## 测试方案

新增集成测试：

- `loadtest_fake_server_normal_stream_passes`
- `loadtest_fake_server_slow_first_byte_records_ttfb`
- `loadtest_fake_server_stream_idle_releases_lease`
- `loadtest_fake_server_json_exception_200_is_classified`
- `loadtest_rpm_limit_blocks_target_account`
- `loadtest_dfcache_unconfigured_route_returns_404`
- `loadtest_thinking_stream_contains_thinking_delta`

手动真实测试命令必须记录在 `docs/testing/loadtest.md`。

## 验收标准

- 工具能在本地稳定复现异常。
- 报告能定位是代理排队慢、上游首字慢、thinking 慢还是可见文本慢。
- 能验证 RPM 生效。
- 能验证高并发下无明显内存泄漏。
- 能验证错误 ID 写入下游响应和系统 usage。

## 风险与回滚

风险：

- 压测误打真实上游。
- 长测占用本机资源。

规避：

- 真实上游必须双开关。
- 默认并发和请求数设置保守。
- 报告目录写到 `target/loadtest/`。

回滚：

- 压测工具独立于生产代码，可直接移除 bin 和 tests/support。

## 不得做的事项

- 不得默认启用真实上游测试。
- 不得把测试 token 写入报告。
- 不得把压测工具挂到生产服务自动启动。
- 不得用单测替代真实并发测试。

## 后续可选扩展

后续可以接入 Grafana 或 OpenTelemetry，但第一阶段必须优先保证本地命令可复现。

