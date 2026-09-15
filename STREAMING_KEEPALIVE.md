# 流式响应保活机制说明

## 背景

当服务部署在 Cloudflare 或其他有超时限制的反向代理后，长时间的流式响应可能因为无数据传输而被中间代理认为连接已死，导致 120 秒超时断开。

## 当前实现

项目已实现多层保活机制，优先级从高到低：

### 1. 空 Delta 保活（推荐，最可靠）

**工作原理：**
- 每 5 秒发送一个空的 `content_block_delta` 事件
- 根据当前活跃的块类型发送对应的空 delta：
  - `text` 块 → `text_delta` with `"text": ""`
  - `thinking` 块 → `thinking_delta` with `"thinking": ""`
  - `tool_use` 块 → `input_json_delta` with `"partial_json": ""`

**优势：**
- ✅ 完全符合 Anthropic Messages API 规范
- ✅ 不会被解析为有效内容（空字符串）
- ✅ 中间代理（包括 Cloudflare）将其识别为有效 SSE 数据
- ✅ 客户端可以正确处理这些事件

**实现位置：**
- `src/anthropic/stream.rs:3287` - `claude_code_noop_delta_keepalive_event()`
- `src/anthropic/handlers.rs:9168` - ping 定时器处理

### 2. Ping 事件保活（降级方案）

**工作原理：**
- 如果没有活跃的内容块（极少见情况），发送标准 SSE ping 事件
- 格式：`event: ping\ndata: {"type": "ping"}\n\n`

**问题：**
- ⚠️ 某些 CDN/反向代理可能不将 ping 事件视为有效流量
- ⚠️ 仅在边缘情况下使用

## 配置参数

### 保活间隔

**环境变量配置（推荐）：**

```bash
export KIRO_STREAM_KEEPALIVE_INTERVAL_SECS=15
```

**默认值：**
```rust
// src/anthropic/handlers.rs:7822
const PING_INTERVAL_SECS: u64 = 5;
```

**建议值：**
- **Cloudflare (120s 超时):** 5-30 秒
- **AWS CloudFront (60s):** 5-15 秒  
- **普通 nginx (默认 60s):** 10-30 秒
- **无中间代理:** 30-60 秒

**配置说明：**
- 环境变量优先级高于默认值
- 有效范围：1-300 秒
- 建议保持在代理超时的 1/4 以下（对于 CF 120s，即 ≤ 30 秒）
- 值越小保活越频繁，但不会影响性能（空 delta 非常轻量）

### 上游空闲超时

```rust
// src/anthropic/handlers.rs:7824
const DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS: u64 = 180;
```

上游 Kiro API 如果超过此时间没有发送任何数据，将被视为超时。

## 工作流程示例

```
客户端请求
    ↓
[0s] message_start 事件
    ↓
[0s] content_block_start (index=0, type=text)
    ↓
[5s] ← 空 text_delta 保活 (text="")
    ↓
[10s] ← 空 text_delta 保活 (text="")
    ↓
[12s] 真实 text_delta (text="Hello")
    ↓
[15s] ← 空 text_delta 保活 (text="")
    ↓
[18s] 真实 text_delta (text=" World")
    ↓
[20s] ← 空 text_delta 保活 (text="")
    ↓
[22s] content_block_stop
    ↓
[22s] message_stop
```

## 验证方法

### 1. 本地测试

```bash
# 启动服务
cargo run

# 使用 curl 测试流式响应
curl -N -X POST http://localhost:3000/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: YOUR_API_KEY" \
  -d '{
    "model": "claude-opus-5",
    "max_tokens": 1024,
    "stream": true,
    "messages": [{
      "role": "user",
      "content": "请写一个很长的故事，中间要思考"
    }]
  }' | while IFS= read -r line; do
    echo "$(date '+%H:%M:%S') $line"
  done
```

观察是否有空 delta 事件定期出现。

### 2. Cloudflare 生产环境测试

```bash
# 测试长时间流式响应（应该不会在 120 秒断开）
time curl -N -X POST https://your-domain.com/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: YOUR_API_KEY" \
  -d '{
    "model": "claude-opus-5",
    "max_tokens": 4096,
    "stream": true,
    "messages": [{
      "role": "user",
      "content": "请非常详细地解释量子计算的原理，包含大量思考过程"
    }]
  }' > response.log

# 检查是否完整接收
grep "message_stop" response.log
```

## 常见问题

### Q: 为什么还是超时？

**可能原因：**

1. **Cloudflare 缓存层级问题**
   - 确认 Cloudflare 的 "Proxy status" 是否为 "Proxied"
   - 检查是否有自定义的 Page Rules 覆盖超时设置

2. **上游没有及时响应**
   - 检查 Kiro API 自身是否稳定
   - 增加日志确认是否有上游超时：
     ```bash
     tail -f logs/kiro.log | grep "upstream.*timeout\|idle"
     ```

3. **Ping 间隔过长**
   - 将 `PING_INTERVAL_SECS` 调整到 10-15 秒

### Q: 空 delta 会不会影响客户端？

不会。标准的 Anthropic API 客户端（官方 SDK、LangChain 等）都会忽略空内容的 delta 事件，因为：
- `text: ""` 不会追加任何字符
- `thinking: ""` 不会追加任何思考内容
- `partial_json: ""` 不会改变 JSON 解析状态

### Q: 如何完全禁用保活？

不推荐禁用，但如果确实需要：

```rust
// src/anthropic/handlers.rs
const PING_INTERVAL_SECS: u64 = 3600; // 设置为很大的值（1小时）
```

## 进一步优化建议

### 方案 A: Docker 部署配置示例

**docker-compose.yml:**
```yaml
services:
  kiro:
    image: kiro-rs:latest
    environment:
      # Cloudflare 部署建议 15-20 秒
      KIRO_STREAM_KEEPALIVE_INTERVAL_SECS: 15
    ports:
      - "3000:3000"
```

### 方案 B: Systemd 服务配置

**/etc/systemd/system/kiro.service:**
```ini
[Unit]
Description=Kiro AI Gateway
After=network.target

[Service]
Type=simple
User=kiro
WorkingDirectory=/opt/kiro
Environment="KIRO_STREAM_KEEPALIVE_INTERVAL_SECS=20"
ExecStart=/opt/kiro/target/release/kiro-rs
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

### 方案 C: Cloudflare Workers 前置处理

如果您使用 Cloudflare Workers 作为前端：

```javascript
// worker.js
addEventListener('fetch', event => {
  event.respondWith(handleRequest(event.request))
})

async function handleRequest(request) {
  const response = await fetch(request)
  
  // 确保 Cloudflare 不缓存流式响应
  const newHeaders = new Headers(response.headers)
  newHeaders.set('Cache-Control', 'no-cache, no-store, must-revalidate')
  newHeaders.set('X-Accel-Buffering', 'no')
  
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: newHeaders
  })
}
```

在 `message_start` 后立即发送一个空的 `text_block_start` + 空 `text_delta`，确保从一开始就有内容块可用于保活：

```rust
// 伪代码示例
fn initial_keepalive_block() -> Vec<SseEvent> {
    vec![
        SseEvent::new("content_block_start", json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        })),
    ]
}
```

## 相关代码位置

- 保活逻辑：`src/anthropic/handlers.rs:9168-9192`
- 空 Delta 生成：`src/anthropic/stream.rs:3287-3355`
- Ping 事件生成：`src/anthropic/handlers.rs:8342-8344`
- 配置常量：`src/anthropic/handlers.rs:7820-7824`

## 更新日志

- **2026-09-08**: 移除 User-Agent 检测，所有客户端默认使用空 delta 保活
- **2026-09-08**: 新增 `create_keepalive_text_block_event` 辅助方法
