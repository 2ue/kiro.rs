# Round 45：原配置基线与结构化输出语义复核

## 运行信息

- 时间：2026-11-07（北京时间 12:52–12:55；UTC 04:52–04:55）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 工作区 HEAD：`2e625e52`；本轮没有代码或路由配置改动
- 59139 二进制：`/tmp/kirotest-dev/kiro-rs-cctest-round40-thinking-start`
- 二进制 SHA-256：`db4dfe587b72cb84cd6f223044a0b3651cb42c2ce6db938f52eec14f8a30b061`
- 59138：仅健康探针读取；未改进程、配置、数据库、Redis 或日志

## 结果

结果文件：[`20261107T045201-round-45-baseline-restored.json`](../results/20261107T045201-round-45-baseline-restored.json)

总分 `73/110`，与 Round 44 相同：

| 检测项 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 0 |
| server_tool | 10 |
| knowledge | 10 |
| model_attribution | 10 |
| doc_recognition | 10 |
| image_recognition | 10 |

## 抓包与结构检查

完整抓包保留于 `logs/llm-capture/capture-20261107T045201Z-95980d12/`，包含 `events.jsonl`、`manifest.json` 与 `capture.zip`。

- 9,573 个事件，8,574,203 字节；JSON 错误 0、丢弃事件 0。
- 22 个客户端请求、25 个上游请求；上游响应全部 HTTP 200。
- 22 个流均有一次 `message_start/message_delta/message_stop`；49 个 block start/stop 配对，index 连续。
- 对 block 生命周期、index 和 delta/block 类型配对的逐请求校验为 22/22 通过；没有发现公开 SSE 形状差异能解释 `stream_structure=5`。
- 8 个下游 `signature_delta` 均可与同请求 Kiro `reasoningContentEvent.signature` 的 SHA-256 对上，代理未重编码签名。
- 抓包记录 29 个 WebSearch 路由 SSE 事件；本轮 `websearch` 仍为 0。

## output_config 交叉样本

结构化输出请求 `req_01TGsWjyyo7Shvfhy7oWZBj3` 与之前样本使用相同用户消息哈希 `4ee3036002a9304a616fc15d085908446751e3282b658acefc09d684db2a23cb`、schema 哈希 `93136f64e9d34e3d5bfdaee2e7d0d04173d03b435325748bea2dd9fbe13f77c0`。Kiro 收到 `structured_output` 工具，Anthropic 响应是单一 text block；拼接文本是合法 JSON，字段、类型、必需键、enum 均符合 schema，`identity_platform=kiro`，`desc` 长度 983。该分项仍为 0。

最初的近期轮次子集统计将它记为第 13 个样本。随后全量复核发现实际有 40 个同 schema、同消息请求，其中 2 个检测中断；38 个完整配对样本里，8 个 `claude_code` 响应得 10，30 个 `kiro` 响应得 0。Round 45 属于后者。它继续支持响应语义与分数高度相关，但没有公开判分细节证明身份值就是评分条件。完整逐请求清单和排除条件见 `analysis/scoring.md` 与 `analysis/output-config-samples.json`。

## 资源与生产隔离

- 监控 18 次：最低 `MemAvailable=9.05 GiB`、最低 `SwapFree=10.99 GiB`、最高 1 分钟负载 `1.06`。
- 59138 `/healthz` 探针全部 HTTP 200；未触发暂停阈值。
- 59139 继续使用开发实例；没有重新编译或重启服务。

## 参考资料

- 官方资料：Anthropic《Structured outputs》：<https://platform.claude.com/docs/en/build-with-claude/structured-outputs>；访问时间 2026-11-07 04:46 UTC。该文档说明 `output_config.format` 的 JSON Schema 请求及 text block 响应形状。Round 45 的请求与响应满足观察到的公开形状；它不能说明 CCTest 私有语义评分。

## 结论

Round 45 再次确认原配置分数为 `73/110`。当前四个失败项不能由本轮发现的 SSE 生命周期错误、上游 HTTP 错误或结构化 JSON/schema 形状解释。继续按请求级证据调查 `stream_structure`、`signature_proto`、WebSearch opaque 字段和 output_config 语义评分；不伪造签名、WebSearch opaque 字段，也不硬编码模型生成字段。
