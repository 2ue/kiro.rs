# Cloudflare 120秒超时解决方案 - 快速指南

## 问题描述

当 Kiro 部署在 Cloudflare 后，流式响应在超过 120 秒后被强制中断。

## 已实现的解决方案

✅ **自动空 Delta 保活机制**已集成到项目中，无需任何代码修改即可使用。

### 工作原理

系统每 5 秒自动发送空的 `content_block_delta` 事件：
```json
{
  "type": "content_block_delta",
  "index": 0,
  "delta": {
    "type": "text_delta",
    "text": ""  // 空字符串，不影响输出
  }
}
```

这些空 delta 被 Cloudflare 识别为有效数据流，保持连接活跃。

## 配置（可选）

### 调整保活间隔

如果默认 5 秒对您的场景来说过于频繁，可以通过环境变量调整：

```bash
# 设置为 15 秒（推荐用于 Cloudflare）
export KIRO_STREAM_KEEPALIVE_INTERVAL_SECS=15

# 启动服务
cargo run --release
```

或在 Docker 中：
```yaml
environment:
  KIRO_STREAM_KEEPALIVE_INTERVAL_SECS: 15
```

### 建议值

| 部署环境 | 超时时间 | 建议间隔 | 说明 |
|---------|---------|---------|------|
| Cloudflare | 120s | 5-30s | 默认 5s 已足够 |
| AWS CloudFront | 60s | 5-15s | 较短超时需要更频繁 |
| Nginx | 60s | 10-30s | 可根据配置调整 |
| 直连 | 无限制 | 30-60s | 可适当放宽 |

## 验证测试

运行自动化测试脚本：

```bash
# 设置 API 配置
export KIRO_API_URL="https://your-domain.com"
export KIRO_API_KEY="your-api-key"

# 运行测试（需要 3-5 分钟）
./scripts/test_keepalive.sh
```

或手动测试：

```bash
curl -N -X POST https://your-domain.com/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: YOUR_KEY" \
  -d '{
    "model": "claude-opus-5",
    "max_tokens": 2048,
    "stream": true,
    "messages": [{
      "role": "user",
      "content": "请详细解释量子计算，包含大量思考"
    }]
  }' | grep -E "delta.*text.*\"\"|message_stop"
```

应该看到：
- 定期出现的空 delta 事件（如 `"text":""` 或 `"thinking":""`）
- 最后的 `message_stop` 事件（表示完整接收）

## 常见问题

### 仍然超时？

1. **检查 Cloudflare 设置**
   - 确认 DNS 记录是 "Proxied" 状态（橙色云朵）
   - 检查 Page Rules 是否有自定义超时设置

2. **减小保活间隔**
   ```bash
   export KIRO_STREAM_KEEPALIVE_INTERVAL_SECS=10
   ```

3. **查看日志**
   ```bash
   # 检查是否有上游超时
   tail -f logs/kiro.log | grep -i "timeout\|idle"
   ```

### 空 Delta 会影响客户端吗？

不会。所有标准 Anthropic API 客户端都会忽略空内容：
- `text: ""` 不追加任何字符
- `thinking: ""` 不追加任何思考内容
- `partial_json: ""` 不改变 JSON 状态

### 如何禁用保活？

不建议禁用，但如果需要：
```bash
export KIRO_STREAM_KEEPALIVE_INTERVAL_SECS=3600  # 1小时
```

## 详细文档

参见 [STREAMING_KEEPALIVE.md](./STREAMING_KEEPALIVE.md) 获取完整技术细节。

## 相关代码

- 保活逻辑: `src/anthropic/handlers.rs:9168-9192`
- 空 Delta 生成: `src/anthropic/stream.rs:3287`
- 配置函数: `src/anthropic/handlers.rs:7828`

## 更新记录

- **2026-09-08**: 实现通用空 Delta 保活机制，移除 User-Agent 限制
- **2026-09-08**: 添加环境变量配置支持
- **2026-09-08**: 创建自动化测试脚本
