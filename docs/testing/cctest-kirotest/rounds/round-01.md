# Round 01：HEAD 开发实例冒烟与链路诊断

时间：2026-11-06（北京时间）

## 代码与实例

- 代码：仓库 HEAD `2e625e52`，`v0.0.240`。
- 59139：使用 `feature/tests/run-cargo-scoped.sh cctest-kirotest-dev -- cargo run` 启动，未使用 release 冻结。
- 59138：生产实例未重启、未改配置；仍是 `0.0.239`。
- 资源：本轮未执行 release 编译；开发实例已由 scoped Cargo 会话运行。

## 开发实例冒烟

抓包：`logs/llm-capture/capture-20261106T094529Z-e2a47b36/events.jsonl`，115 行、约 69 KiB；原始抓包保留。

| 请求 | 结果 | 证据 |
|---|---|---|
| 普通非流 | HTTP 200，`stop_reason=end_turn`，usage 非零 | `req_01HeMn9u3uGN7aPtGmQU1XvU` |
| 普通流 | `message_start → content_block_start(text) → delta → content_block_stop → message_delta(end_turn) → message_stop` | `req_01iW2QgrG3ncT2qcBJH2FVod` |
| WebSearch 流 | 4 个连续 block，工具 ID 配对，最终 `end_turn`/`message_stop` | `req_016LEiSsttw6BwuYgwD4xKVw` |

WebSearch 具体序列：

```text
message_start
content_block_start index=0 type=text
content_block_stop index=0
content_block_start index=1 type=server_tool_use id=srvtoolu_... name=web_search
content_block_stop index=1
content_block_start index=2 type=web_search_tool_result tool_use_id=srvtoolu_...
content_block_stop index=2
content_block_start index=3 type=text
content_block_stop index=3
message_delta stop_reason=end_turn usage.server_tool_use.web_search_requests=1
message_stop
```

`server_tool_use.id` 与 `web_search_tool_result.tool_use_id` 完全相等；上游 3 次请求全部 HTTP 200；没有 upstream 400/429/500。

## Caddy 链路

已备份 `/root/docker-compose/caddy/Caddyfile` 到 `Caddyfile.bak-20261106-kirotest-before`，并追加唯一站点：`kirotest.okmcode.com → host.docker.internal:59139`。`caddy validate` 通过。

当前公网仍返回 HTTP 525。原因已确认：正在运行的 `caddy-caddy-1` 使用 Caddy 2.10.2，配置通过 `admin off` 禁用管理 API；CLI `caddy reload` 因 `localhost:2019` 拒绝连接而未加载新增站点。Caddy 2.10.2 的 `SIGHUP`/`SIGUSR1` 也不是可用 reload 通道，不能把未加载的配置误认为已生效。

生产 `kiro2.okmcode.com` 仍由运行中的旧配置提供服务，未重启 Caddy、未修改其代理目标。后续需要在不影响 59138 的前提下，为 Caddy 提供可回滚的配置加载路径；在加载成功并确认 `kirotest` HTTPS 后，才能跑公网 cctest。

## 本轮结论

1. 当前 HEAD 的普通流和 WebSearch 直接协议形状完整，不能据此宣布 cctest 四项全部修复。
2. 开发抓包没有生产中 31 次 synthetic signature 400 的现象；需要用带历史 thinking 的请求继续验证。
3. 公网 525 是 Caddy 配置未加载造成，不是 59139 服务故障。
4. 已添加可复用脚本 `scripts/cctest.py`，结果默认保存到 `docs/testing/cctest-kirotest/results/`，密钥只从环境读取。
