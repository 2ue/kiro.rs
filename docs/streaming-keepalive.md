# 流式保活与代理超时

`claude-pool` 在上游空闲时向下游写入 SSE 注释帧，帮助连接经过反向代理时持续传输数据。当前实现不生成空内容块或空 delta。

## 保活与超时

注释帧为：

```text
: keepalive

```

标准 SSE 客户端忽略注释，不会把它追加到正文、thinking 或工具参数。`src/account/mod.rs` 中的 `account_stream_keepalive_interval` 按以下规则选择间隔：

- 配置了上游空闲超时：`clamp(streamIdleTimeoutSecs / 2, 1, 5)` 秒。
- 空闲超时关闭时：5 秒。
- 有上游数据时推进空闲计时；保活不代表上游产生了语义输出，也不延长上游空闲或首输出预算。

相关参数由 `accounts` 设置，账号 `overrides` 可覆盖：

```json
{
  "accounts": {
    "requestTimeoutSecs": 180,
    "streamIdleTimeoutSecs": 180,
    "streamFirstOutputTimeoutSecs": 120
  }
}
```

`streamIdleTimeoutSecs` 为 `0` 时关闭上游空闲超时；`streamFirstOutputTimeoutSecs` 为 `0` 时关闭首输出超时。首输出前还存在缓冲和换号阶段，因此需要分别检查到达代理的首字节与上游首个语义输出。

当前实现没有单独设置保活间隔的环境变量。调整超时使用上述配置字段，通过控制台或管理 API 保存运行配置；已有数据库运行配置时，修改启动文件不会覆盖这些值。

## 反向代理

Nginx 可按部署需求配置流式转发：

```nginx
location / {
    proxy_pass http://127.0.0.1:8990;
    proxy_http_version 1.1;
    proxy_buffering off;
    proxy_cache off;
    proxy_read_timeout 240s;
    proxy_send_timeout 240s;
}
```

Cloudflare 等 CDN 的空闲、首字节和总时长限制应分别核查。保活可减少空闲断连，但不能取消平台的硬性请求时长限制；代理缓冲开启时，下游也可能看不到及时发出的帧。

## 验证

使用已接入账号支持的模型，记录完整 SSE：

```bash
curl -N --fail-with-body http://127.0.0.1:8990/cc/v1/messages \
  -H 'content-type: application/json' \
  -H 'x-api-key: sk-downstream-change-me' \
  -d '{"model":"claude-sonnet-4-5","max_tokens":1024,"stream":true,"messages":[{"role":"user","content":"解释异步任务取消的处理方式"}]}' \
  > response.sse
```

在上游空闲窗口检查 `: keepalive`；正常完成应收到 `message_stop`。通过代理复测时对照直连结果，分别查看首字节延迟、注释帧间隔、最后事件和服务端超时记录。
