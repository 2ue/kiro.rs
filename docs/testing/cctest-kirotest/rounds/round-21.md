# Round 21：固定 Opus 5.5 连续三次

时间：2026-11-06（UTC 17:26–17:35；北京时间 01:26–01:35）

## 环境与边界

- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139 开发实例。
- 三次均固定 `claude-opus-5-5`，代码未在三次之间变化。
- 抓包：`logs/llm-capture/capture-20261106T172613Z-0121b911/events.jsonl`。
- 抓包统计：29339 个事件、26057035 字节、75 个上游响应；75 个均 HTTP 200，无 429/500/502。
- 59138 全程只读，未重启、未改配置、未写生产 PostgreSQL/Redis。

## 结果

| 结果文件 | 总分 | stream_structure | signature_proto | websearch | output_config |
|---|---:|---:|---:|---:|---:|
| `results/20261106T172613-round-21-opus55-repeat3.json` | 73 | 5 | 5 | 0 | 0 |
| `results/20261106T172903-round-21-opus55-repeat3.json` | 73 | 5 | 5 | 0 | 0 |
| `results/20261106T173208-round-21-opus55-repeat3.json` | 82 | 5 | 5 | 0 | 10 |

三次共同通过 `tag_check`、`non_stream`、`server_tool`、`knowledge`、`model_attribution`、`doc_recognition`、`image_recognition`；其余采样项仍未达标。

## 抓包结论

1. 普通工具续接请求均为 HTTP 200，事件顺序完整；历史 thinking block 中存在 Kiro 原生 opaque signature，不能把所有签名当作 synthetic。
2. thinking 事件可见空 `thinking_delta`，部分事件含 `estimated_tokens=50`；签名通过独立 `signature_delta` 发送。需要分别去掉 estimated token、合并空 delta、去掉 start 内 signature 做单变量实验。
3. WebSearch 有 3 个内部请求，均出现 HTTP 200 和 MCP 结果；结果 block 中有 `encrypted_content` 与 `encrypted_index`。当前实现使用明文 snippet 和代理自定义 Base64 索引，属于未被官方公开算法证明的私有检查候选。
4. `output_config` 在固定模型下仍出现 0、0、10，说明至少有模型输出或采样请求差异；不能把结构化输出失败归因于 Caddy、账号池或上游 HTTP 状态。

## 账号与链路结论

本轮没有新增账号删除。59139 账号池已有近期成功请求；Caddy/Cloudflare → 59139 链路正常。59138 版本仍为 0.0.239，工作区为 0.0.240，生产未部署当前 HEAD。
