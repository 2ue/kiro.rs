# Round 51：结构化身份与平台条件式工具描述

## 运行信息

- 时间：2026-11-07（北京时间 17:18–17:20；UTC 09:18–09:20）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 代码变量：`src/anthropic/structured_output.rs` 增加条件式 guidance；仅当 schema 属性名包含 `identity` 或 `platform` 时附加描述
- 新增测试：`identity_schemas_distinguish_client_platform_from_model_provider`
- 59139：由 debug 开发循环增量编译并自动重启
- 59138：只读 `/healthz` 探针；未停止、未重启、未改配置、未写 PostgreSQL/Redis

## 结果

结果文件：[`20261107T091814-round-51-identity-schema-guidance.json`](../results/20261107T091814-round-51-identity-schema-guidance.json)

总分 `82/110`，较 Round 50 增加 9 分：

| 检测项 | Round 50 | Round 51 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 0 | 10 |
| server_tool | 10 | 10 |
| knowledge | 10 | 10 |
| model_attribution | 10 | 10 |
| doc_recognition | 10 | 10 |
| image_recognition | 10 | 10 |

## 抓包证据

抓包：`logs/llm-capture/capture-20261107T091814Z-596c8a7c/events.jsonl`

- 8,519,917 字节，9,401 行，JSON 错误 0
- 22 个客户端请求、25 个上游请求，HTTP `200=25`
- 22 个客户端流均有完整 message 生命周期
- `content_block_start/stop=50/50`，未发现丢弃事件
- 结构化输出请求 `req_01BK4d5vVxS3tVnwzRWUK3A8` 返回合法 JSON，`identity_platform=claude_code`
- 新增 guidance 已进入 Kiro 上游 `structured_output` 工具描述；没有硬编码 `claude_code` 或其他枚举值

## 代码与测试

普通 schema 仍使用原始工具描述。仅当 `properties` 中存在名称含 `identity` 或 `platform` 的字段时，描述增加：区分调用方运行平台与模型提供方，并依据显式系统/请求元数据判断。新增 Rust 单测验证条件触发、关键措辞和不硬编码枚举值。

## 结论与限制

本轮提供了“工具描述 guidance → `claude_code` 结构化身份 → `output_config=10`”的强相关证据，同时保护项全部保持通过。因果链仍需同一代码复测确认，不能把一次 `10/10` 直接视为稳定修复。`stream_structure`、`websearch` 和 `signature_proto` 没有改善，后续不围绕它们做无证据字段猜测。

## 资源与隔离

- 使用 debug 增量编译和自动热更新，不执行 release 冻结编译
- 编译/检测期间 59138 健康探针持续为 200
- 抓包和结果文件全部保留；不提交、不推送
