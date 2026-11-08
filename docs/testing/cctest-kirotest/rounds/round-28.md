# Round 28：noop keepalive thinking_delta 字段实验

时间：2026-11-06（UTC 19:28–19:32；北京时间 03:28–03:32）

## 环境

- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139 开发实例。
- 模型：`claude-opus-5-5`。
- 代码工作区：`2e625e52d2b2225c487d6ff027ce29110f997511`。
- 代码变化：noop keepalive 命中隐藏 thinking block 且 beta 字段开关有效时，发送
  `thinking_delta` 并显式携带 `estimated_tokens: null`。
- 环境变量：`KIRO_RS_CCTEST_NULL_THINKING_ESTIMATED_TOKENS=1`、`KIRO_RS_CCTEST_NO_PLAIN_EMPTY_THINKING_DELTA=1`。
- 抓包：`logs/llm-capture/capture-20261106T192849Z-9ce45cb9/events.jsonl`。
- 抓包统计：9881 行、8733512 字节、25 个 upstream response，全部 HTTP 200；含 22 个完整 message 流。

59138 未重启、未改配置、未写数据库；本轮只对开发实例 59139 编译和重启。

## 结果

结果文件：`results/20261106T192858-round-28-keepalive-null.json`

总分仍为 `73/110`，分项没有变化：

| stream_structure | signature_proto | websearch | output_config | server_tool |
|---:|---:|---:|---:|---:|
| 5 | 5 | 0 | 0 | 10 |

其余保护项全部 10 分。

## 抓包证据

Round 28 的 thinking delta 字段统计：

- `estimated_tokens=50`：16 次；
- `estimated_tokens=null`：8 次；
- 缺少 `estimated_tokens`：0 次。

因此本轮已统一过滤器和 noop keepalive 两条路径的字段形状。普通流、工具续接、WebSearch、结构化输出请求均能完成合法终止，抓包未出现因字段实验导致的 HTTP 错误。

典型 WebSearch 请求 `req_016gdcrMC8xRr64SzCRsHQ7L` 仍包含：

- `server_tool_use` 与 `web_search_tool_result` 的 ID 配对；
- `input_json_delta` 传递 query；
- 10 个结果、10 个 citations；
- `message_delta.stop_reason=end_turn` 与 `message_stop`。

典型 structured output 请求 `req_019MmRoRSrDfPTutMKBSmCpk` 返回单一 text block，拼接后是合法 JSON，顶层字段精确为 `identity_platform`、`desc`，但 cctest 仍给 `output_config=0`。

## 结论

noop keepalive 是否携带 `estimated_tokens` 不是 `stream_structure` 或 `signature_proto` 的充分根因；Round 25–28 连续四轮均为 73/110。后续不再围绕该字段继续微调，转向请求级状态机、结构化输出内部工具回放和 WebSearch opaque 字段的证据对照。

## 资源

本轮使用开发模式增量编译，限制为 `feature/tests/run-cargo-scoped.sh cctest-kirotest-dev -- cargo build --bin kiro-rs -j2`；可用内存约 9 GiB，未触发暂停阈值。59139 运行期间未改动生产实例。

## 账号池

59139 在本轮使用已清理后的 9 个可调度账号。若后续开发池不足，可从 59138 只读导出新的账号及其绑定代理，复制到 59139；不得在 59138 执行删除、禁用、优先级、配额、刷新或其他写操作。
