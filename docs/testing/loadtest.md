# Kiro Loadtest 使用说明

`kiro_loadtest` 是独立压测和异常复现工具，不会随主服务启动。它用于验证本地代理、fake Kiro server、streaming、thinking、tool-use、高缓存路由、错误归一化、延迟和资源占用。

## 编译环境注意

当前机器的 PATH 中可能存在非系统 `cc`。如果编译时报：

```text
error: unknown command '.../symbols.o'
```

使用系统编译器运行：

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --bin kiro_loadtest
```

## 基础 smoke test

启动内置 fake server，并直接对 fake server 发 5 个流式请求：

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --fake-listen 127.0.0.1:19080 \
  --base-url http://127.0.0.1:19080 \
  --route /v1/messages \
  --requests 5 \
  --concurrency 2 \
  --scenario normal-stream \
  --report target/loadtest/smoke.json
```

## 测本地代理

优先使用隔离测试代理，例如 `19022`。只有在明确要验证日常开发服务时，才把 `base-url` 改成 `9022`。

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:19022 \
  --route /cc/v1/messages \
  --model claude-sonnet-4-20250514 \
  --requests 100 \
  --concurrency 10 \
  --scenario normal-stream \
  --auth-key admin123 \
  --report target/loadtest/local-cc.json
```

## thinking 测试

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:19022 \
  --route /cc/v1/messages \
  --model claude-sonnet-4-20250514 \
  --requests 30 \
  --concurrency 3 \
  --scenario normal-stream \
  --thinking true \
  --auth-key admin123 \
  --report target/loadtest/thinking.json
```

验收重点：

- `firstThinkingMs.p95` 应有值。
- `firstTextMs.p95` 应有值。
- 成功请求不应返回 tool-use 格式错误。

## tool-use 测试

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:19022 \
  --route /cc/v1/messages \
  --model claude-sonnet-4-20250514 \
  --requests 30 \
  --concurrency 3 \
  --scenario normal-stream \
  --tool-use true \
  --auth-key admin123 \
  --report target/loadtest/tool-use.json
```

## `/dfcache/*` 测试

已配置路由：

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:19022 \
  --dfcache-route /dfcache/cc/v1/messages \
  --requests 20 \
  --concurrency 2 \
  --scenario normal-stream \
  --auth-key admin123 \
  --report target/loadtest/dfcache-configured.json
```

未配置路由应返回错误，并且报告中应出现非 2xx 状态：

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:19022 \
  --dfcache-route /dfcache/not-configured/v1/messages \
  --requests 5 \
  --concurrency 1 \
  --scenario normal-stream \
  --auth-key admin123 \
  --report target/loadtest/dfcache-missing.json
```

## 异常场景 fake server

只启动 fake server：

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --fake-listen 127.0.0.1:19080 \
  --fake-only true \
  --scenario slow-thinking-then-text
```

可用场景：

- `normal-stream`
- `normal-non-stream`
- `slow-first-byte`
- `slow-thinking-then-text`
- `stream-idle-timeout`
- `json-exception200`
- `rate-limit429`
- `server-error500`
- `invalid-tool-format`
- `malformed-sse`
- `client-drop`
- `recovery-after-burst`

## 真实上游保护

如果要明确进行真实上游压测，必须同时加参数和环境变量：

```bash
KIRO_LOADTEST_ALLOW_REAL_UPSTREAM=1 \
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --real-upstream true \
  --base-url http://127.0.0.1:19022 \
  --route /cc/v1/messages \
  --requests 20 \
  --concurrency 2 \
  --auth-key admin123 \
  --report target/loadtest/real-upstream.json
```

不要在没有明确目的时增加并发。真实上游测试优先从低并发开始。

## 报告字段

报告是 JSON：

- `requests`：完成的请求数。
- `success`：成功请求数。
- `errors`：失败请求数。
- `statusCounts`：HTTP 状态码分布。
- `ttfbMs`：代理请求到下游收到第一个字节的耗时。
- `firstThinkingMs`：第一个 thinking delta 的耗时。
- `firstTextMs`：第一个可见 text delta 的耗时。
- `totalLatencyMs`：请求总耗时。
- `memory`：目标进程 RSS 的开始、峰值、结束值。
- `fileDescriptors`：目标进程 FD 数的开始、峰值、结束值。
- `requestIds`：采样 request id，最多 100 个。
- `errorIds`：采样 error id，最多 100 个。

## 大并发建议

- 先用 fake server 做 100 并发验证工具本身。
- 再用本地隔离代理从 `concurrency=5` 开始。
- 如果 `ttfbMs.p95` 随并发明显升高，优先降低单账号并发或缩短 dispatch wait。
- 如果错误主要是 429，检查单账号 RPM、全局并发和排队长度。
- 如果 `memory.peak` 持续上涨且结束后不回落，需要进一步查 stream 是否释放资源。
