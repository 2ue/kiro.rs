# Round 03：打开模型驱动 WebSearch 后复测

时间：2026-11-06（北京时间）

## 变更

没有修改 Rust 源码。通过开发实例本地 Admin API，仅修改 59139 独立 runtime_config：

```text
routeFeatures.pathOverrides["/dfcache/tune-ss6-99"].modelDrivenWebSearch: false → true
```

59138 生产实例未访问、未重启、未写配置。

## cctest 结果

结果文件：`results/20261106T103514-round-03-websearch-enabled.json`。

| 检测项 | Round 02 | Round 03 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 0 | 0 |
| server_tool | 0 | 0 |
| knowledge | 10 | 10 |
| model_attribution | 10 | 10 |
| doc_recognition | 10 | 10 |
| image_recognition | 10 | 10 |

总分仍为 `64/110`，判定 `reversed`。仅打开模型驱动 WebSearch 没有改变 cctest 结果，说明当前失败不只是开关问题；cctest 的配置可能在独立请求中覆盖了路径、或 server_tool/websearch 检查还要求更严格的请求/响应闭环。

## 配置根因证据

Round 02/03 之前的开发 runtime_config 明确显示：

```json
{
  "routeFeatures": {
    "pathOverrides": {
      "/dfcache/tune-ss6-99": {"modelDrivenWebSearch": false}
    }
  }
}
```

这解释了 Round 02 抓包中出现的模板式正文（`I'll search...`、`Here are the search results...`）。打开后，用相同的 Claude Code 子请求形状（最后一条 user 文本为 `Perform a web search for the query: ...`，`tools=[]`）手工验证已进入模型驱动协议：

- 非流响应：`server_tool_use` → `web_search_tool_result` → text，ID 以 `srvtoolu_` 开头且完全配对；
- usage 含 `server_tool_use.web_search_requests=1`；
- 文本 block 含 citations；
- 流响应有连续 block、`message_delta(end_turn)`、`message_stop`。

因此实现路径本身已经能产生 cctest 要求的主要 WebSearch 结构，Round 03 的 0 分需要关联 cctest 实际请求的 route、工具声明和历史回传，不能继续只改配置。

## Caddy 状态

- `/root/docker-compose/caddy/Caddyfile` 已通过 `caddy validate`；
- `https://kirotest.okmcode.com/v1/models`：HTTP 401；
- `https://kiro2.okmcode.com/v1/models`：HTTP 401；
- 401 是后端鉴权响应，证明 Caddy、Cloudflare 和两个后端都可达。

## 下一步

1. 开发实例启动一次短抓包，重新执行 Round 03 的 cctest，按 cctest 开始/结束时间精确截取请求；
2. 对每个 `client_request` 记录 endpoint、tools、最后一条 user 文本前缀和 `modelDrivenWebSearch` 解析结果；
3. 检查 cctest server_tool 的普通工具请求是否有下一轮 `tool_result` 闭环；
4. 检查 cctest output_config 是否要求非流请求或严格 schema 验证；
5. 同一思路已经连续两轮没有分数改善，后续改代码前回到请求级抓包，不再盲改 SSE 序列。

## 资源与证据

- 没有 release 编译；开发实例继续以 `cargo run` 运行。
- 生产 59138 全程未变更。
- 已停止 Round 02 的开发抓包会话，原始 JSONL 与 ZIP 均保留。
