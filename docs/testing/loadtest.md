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
- `long-stream`
- `json-exception200`
- `rate-limit429`
- `server-error500`
- `invalid-tool-format`
- `malformed-sse`
- `client-drop`
- `recovery-after-burst`

`long-stream` 用于模拟上游长时间占用流式连接。首包延迟由 `--fake-delay-ms` 控制，后续 chunk 数量和间隔由 `--fake-stream-chunks`、`--fake-stream-chunk-delay-ms` 控制。

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --fake-listen 127.0.0.1:19080 \
  --fake-only true \
  --scenario long-stream \
  --fake-delay-ms 3000 \
  --fake-stream-chunks 80 \
  --fake-stream-chunk-delay-ms 250
```

## Payload 热点专项 case

这些 case 用于压 payload guard、converter、usage 诊断和 prompt-cache 模拟路径，不用于证明上游模型能力。建议只打隔离代理端口，例如 `19022`，不要打日常 `9022`。

如果需要让压测工具同时启动 fake upstream，在命令里加 `--fake-listen 127.0.0.1:19080`，并确保隔离代理的上游配置指向这个地址；否则先用上面的 `--fake-only true` 单独启动 fake upstream。

可用 `--payload-case`：

- `text-history`：普通长文本 history，对应历史消息裁剪和 token 估算。
- `large-tool-results`：历史 tool_result 大块内容，对应 tool_result 扫描、裁剪、序列化和 diagnostics。
- `deep-tool-input`：深层 tool_use input，对应深层 JSON 遍历、clone/deser 和 schema 处理。
- `many-tools`：大量工具定义和嵌套 schema，对应工具 schema 规范化、payload shaping、prompt-cache profile。
- `mixed-pathological`：长 history、大 tool_result、深层 input、多工具混合，用于复现最坏本地处理路径。

长文本 history：

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:19022 \
  --route /cc/v1/messages \
  --auth-key admin123 \
  --requests 200 \
  --concurrency 8 \
  --scenario normal-stream \
  --payload-case text-history \
  --long-context-chars 600000 \
  --long-context-messages 30 \
  --target-pid <proxy-pid> \
  --report target/loadtest/payload-text-history.json
```

大 tool_result，并叠加高首字延迟：

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:19022 \
  --route /cc/v1/messages \
  --auth-key admin123 \
  --requests 200 \
  --concurrency 8 \
  --scenario slow-first-byte \
  --fake-listen 127.0.0.1:19080 \
  --fake-delay-ms 3000 \
  --payload-case large-tool-results \
  --tool-result-chars 200000 \
  --tool-result-count 6 \
  --target-pid <proxy-pid> \
  --report target/loadtest/payload-large-tool-results-slow-ttfb.json
```

深层 tool input：

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:19022 \
  --route /cc/v1/messages \
  --auth-key admin123 \
  --requests 200 \
  --concurrency 8 \
  --scenario normal-stream \
  --payload-case deep-tool-input \
  --tool-input-depth 80 \
  --target-pid <proxy-pid> \
  --report target/loadtest/payload-deep-tool-input.json
```

多工具 schema：

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:19022 \
  --route /cc/v1/messages \
  --auth-key admin123 \
  --requests 200 \
  --concurrency 8 \
  --scenario normal-stream \
  --payload-case many-tools \
  --tool-count 120 \
  --tool-input-depth 10 \
  --cache-control true \
  --target-pid <proxy-pid> \
  --report target/loadtest/payload-many-tools.json
```

混合最坏形状，并叠加长流式占用：

```bash
CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo run --bin kiro_loadtest -- \
  --base-url http://127.0.0.1:19022 \
  --route /cc/v1/messages \
  --auth-key admin123 \
  --requests 100000 \
  --duration-secs 120 \
  --concurrency 12 \
  --scenario long-stream \
  --fake-listen 127.0.0.1:19080 \
  --fake-delay-ms 2500 \
  --fake-stream-chunks 80 \
  --fake-stream-chunk-delay-ms 250 \
  --payload-case mixed-pathological \
  --long-context-chars 600000 \
  --long-context-messages 30 \
  --current-user-chars 80000 \
  --system-chars 50000 \
  --tool-result-chars 200000 \
  --tool-result-count 6 \
  --tool-input-depth 64 \
  --tool-count 120 \
  --target-pid <proxy-pid> \
  --report target/loadtest/payload-mixed-long-stream.json
```

报告的 `requestProfile` 会记录实际生效的请求形状参数。对比 payload guard 开/关时，优先看 `cpuPercent.peak/end`、`memory.peak/end`、`fileDescriptors.peak/end`、`ttfbMs.p95/p99` 和 `totalLatencyMs.p95/p99`。

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
