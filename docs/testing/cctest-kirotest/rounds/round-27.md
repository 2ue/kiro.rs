# Round 27：beta thinking_delta 字段统一实验

时间：2026-11-06（UTC 19:14–19:18；北京时间 03:14–03:18）

## 环境

- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139 开发实例。
- 模型：`claude-opus-5-5`。
- 代码工作区：`2e625e52d2b2225c487d6ff027ce29110f997511`。
- 本轮开发模式增量编译后二进制 SHA-256：`a5a02fb560ebf3dc6573b8ff99e468ac1919568a573dfb04b25dd1f163b8861c`。
- 代码变化：beta 开启时将 `emit_null_estimated_tokens` 设为真，使过滤器在无数值估算时也携带 `estimated_tokens: null`。
- 环境变量：`KIRO_RS_CCTEST_NULL_THINKING_ESTIMATED_TOKENS=1`、`KIRO_RS_CCTEST_NO_PLAIN_EMPTY_THINKING_DELTA=1`。
- 抓包：`logs/llm-capture/capture-20261106T191423Z-9de0cf6e/events.jsonl`。
- 抓包统计：10303 行、8899484 字节、25 个上游响应，全部 HTTP 200；`client_sse` 4913 个，含 22 个完整 message 流。

59138 未重启、未改配置、未写数据库；只做端口和鉴权只读探针。

## 结果

结果文件：`results/20261106T191423-round-27-all-estimate-fields.json`

总分仍为 `73/110`，分项与 Round 26 完全一致：

| stream_structure | signature_proto | websearch | output_config | server_tool |
|---:|---:|---:|---:|---:|
| 5 | 5 | 0 | 0 | 10 |

其余保护项全部 10 分。

## 抓包证据

- 过滤器生成的 delta 已统一为带 `estimated_tokens`：数值估算为 50，无估算为 null。
- 抓包仍可看到少量不带字段的空 thinking delta；该事件来自 keepalive noop，位于过滤器之外。
- WebSearch 公开生命周期完整且全部 upstream HTTP 200，但仍为 0 分。
- output_config 请求仍为 `stream=true`、`output_config.format.type=json_schema`，内部使用 `structured_output` 工具重写为单一 JSON text block，仍为 0 分。

## 结论

统一过滤器字段形状没有改善 cctest。`stream_structure=5` 的主因不能只归结为 `estimated_tokens`；下一步优先验证 keepalive noop 事件是否应带 null 或改发 ping，并对 output_config 做流/非流与旧版字段矩阵。WebSearch 继续保留 opaque 字段未证实结论。

## 资源

开发模式编译使用 `feature/tests/run-cargo-scoped.sh cctest-kirotest-dev -- cargo build --bin kiro-rs -j2`，可用内存约 9 GiB，构建期间未超过暂停阈值；59139 运行期间未改动生产实例。
