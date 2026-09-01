# P004：raw 路由顶层 `max_tokens` 类型和范围未统一校验

Status: `fixed / local-runtime-verified`

Severity: P1

## 问题

请求入口已经对 `thinking.budget_tokens` 做规范化，但 raw external
passthrough 仍可能只依赖轻量 probe 的 `Option<i64>` 结果。`null`、浮点数或
超出 `i32` 范围的值会被表示成 `None` 或大整数，若没有统一校验，就可能绕过
入口并把确定性的格式错误交给路由、账号重试或外部池处理。

这不是 transient upstream failure，也不是 retry budget 问题。请求格式在首次
发送前就可以确定，应该在本地明确拒绝。

## 根因

`RawMessagesBodyProbe` 同时记录：

- `max_tokens_present`：顶层字段是否出现；
- `max_tokens: Option<i64>`：只有能够按 JSON 整数解析时才有值。

旧的 raw reasoning validator 只在 `probe.max_tokens` 为 `Some` 时参与
`budget_tokens < max_tokens` 比较，没有检查“字段存在但不是整数”，也没有检查
顶层 `max_tokens` 的 `1..=i32::MAX` 范围。

## 修复

修改 [`src/anthropic/request_facts.rs`](../../src/anthropic/request_facts.rs)：

1. 顶层 `max_tokens` 存在时，必须能解析为 JSON 整数；
2. 值必须处于 `1..=2147483647`；
3. 缺失字段不在这里拒绝，继续由既有 missing-max-tokens 配置策略处理；
4. 校验发生在 raw external 路由和本地账号路由共用的入口验证阶段；
5. 不增加重试、不切换账号、不改写 messages、tools、图片或历史 thinking。

## 验证结果

在同一个项目长期测试实例 `127.0.0.1:19023` 上，使用当前 release 构建和真实
本地账号池发送 5 个请求。5/5 首次返回 HTTP 400，且返回的是明确字段错误：

| 用例 | 输入 | HTTP | 错误 |
| --- | --- | ---: | --- |
| `max-null` | `max_tokens: null` | 400 | `max_tokens must be an integer` |
| `max-float` | `max_tokens: 1.5` | 400 | `max_tokens must be an integer` |
| `max-zero` | `max_tokens: 0` | 400 | `max_tokens must be between 1 and 2147483647` |
| `max-negative` | `max_tokens: -1` | 400 | `max_tokens must be between 1 and 2147483647` |
| `max-overflow` | `max_tokens: 2147483648` | 400 | `max_tokens must be between 1 and 2147483647` |

这些请求没有进入上游调用，因此没有账号重试或 external pool fallback。

## 验证命令

```bash
feature/tests/run-cargo-scoped.sh thinking-budget-focused -- \
  cargo test --locked request_facts::tests::raw_reasoning_protocol -- --nocapture
# 9 passed

cargo fmt --check
git diff --check
node feature/tests/inventory-build-artifacts.mjs --gate --no-docker
# release-gate result=pass
```

运行实例核验：

```text
listener: 127.0.0.1:19023
PID: 81106
binary: /tmp/kiro-thinking-candidate.dlJWU9/kiro-rs
SHA-256: d7764aea6ea97abe55decfd182732db771f719fe51f60462488f1c5fb543b623
healthz: {"service":"kiro-rs","status":"ok"}
```

原始响应头、响应体和请求样例保存在本地临时证据目录：

`tmp/thinking-budget-local/evidence/current-regression-20260902/`

该目录不包含凭据、token、密码或完整账号信息。
