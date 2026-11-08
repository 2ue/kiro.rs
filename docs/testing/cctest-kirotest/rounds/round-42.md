# Round 42：identityPrompt 单变量验证

## 运行信息

- 时间：2026-11-07（北京时间 11:58–12:01）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 开发实例：59139；运行二进制 SHA-256 `db4dfe587b72cb84cd6f223044a0b3651cb42c2ce6db938f52eec14f8a30b061`
- 59138：只读，未重启、未改配置、未写 PostgreSQL/Redis

## 实验变量

仅对 59139 的 `/dfcache/tune-ss6-99` 临时设置 `identityPrompt.enabled=true`。原路由值为 `false`；WebSearch、thinking、usage 等配置保持不变。管理端 preview 确认仅 identityPrompt 为 applied，抓包也确认默认 identity prompt 到达结构化输出对应的 Kiro 上游请求。

配置在测试结束时恢复；恢复后 GET 确认 identityPrompt 和 webSearchGuidance 均为关闭状态。没有重启实例。

## 结果

结果文件：`results/20261107T035810-round-42-identity-prompt.json`

| 项目 | 分数 |
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
| 总分 | 73/110 |

## 抓包证据

完整抓包：`logs/llm-capture/capture-20261107T035810Z-9a7656b8/events.jsonl`；保留 `manifest.json` 与 `capture.zip`。

- 9,814 个事件，8,712,627 字节；JSON 错误 0、丢弃事件 0。
- 22 个客户端请求、25 个上游请求；25 个上游响应全部为 HTTP 200。
- 22 个客户端流的 `message_start/message_delta/message_stop` 各 22 次；54 个 block start/stop 成对。
- 结构化输出请求 `req_01ZnhtqgFMr23xDcJwBv1HTD` 使用 `stream=true`、`format.type=json_schema`，客户端文本可解析且包含 schema 要求的键；响应 `identity_platform=kiro`，该项仍为 0。
- 同一请求中可在 Kiro 上游 body 找到 identity prompt 文本，说明注入确实生效；它没有改变模型的身份回答，也没有改变总分。

另有一次运行前未加载 `CCTEST_TARGET_API_KEY` 的空抓包会话 `capture-20261107T035541Z-f6b075f1`，0 个事件、0 字节，已保留。正式运行只从本地 cctest 环境文件读取需要的 API key，没有加载其中的生产 PostgreSQL 命令。

资源监控 17 次：最低 `MemAvailable=9.35 GiB`、最低 `SwapFree=10.89 GiB`、最高 1 分钟 load `0.61`；59138 `/healthz` 探针均为 HTTP 200。未触发暂停阈值。

## 结论

identityPrompt 单变量没有改善分数：注入到达上游后，结构化输出仍返回 `identity_platform=kiro`，该分项为 0。后续复核 Round 07 原始抓包发现，Round 07 实际返回 `identity_platform=claude_code` 并得 10；此前记录的 Round 07 身份值有误。当时列出的 Round 07、21、29–31、35、39、41、42、44 共 12 个样本只是近期子集，其中 5 个 `claude_code` 样本得 10，7 个 `kiro` 样本得 0。全量复核后的 40 个请求、38 个完整配对样本见下文和 `analysis/output-config-samples.json`；身份语义目前是高相关候选，但私有评分规则尚未公开，不能据此硬编码响应字段。

后续全量复核补充：当时列出的 12 个样本只是近期轮次子集。全量找到 40 个同 schema、同消息请求，其中 2 个检测中断；38 个完整配对样本为 8 个 `claude_code`/10 分、30 个 `kiro`/0 分；逐请求清单与排除项见 `analysis/scoring.md` 和 `analysis/output-config-samples.json`。本轮单变量实验结果不变。
