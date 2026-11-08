# Round 85：WebSearch 引用文本 block 边界实验

## 时间与边界

- 时间：2026-11-08 10:25–10:27（北京时间；结果文件时间字段为 UTC 02:25–02:27）。
- 59139 使用 debug 增量开发循环；59138 只读，未重启、未改配置、未写 PostgreSQL/Redis、未改账号状态。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`。
- 本轮未启动完整 LLM capture，只有 CCTest 结果文件；不把本轮当作新的原始抓包证据。

## 代码变量

修改 `src/anthropic/handlers/model_driven_websearch.rs` 的 `coalesce_text_blocks`：

- 只合并两个都没有 `citations` 的相邻 `text` block；
- 带引用的文本 block 保持独立，保留 citation 段边界；
- 其它 WebSearch block、caller、ID、usage 和终止逻辑不变。

定向测试通过：

```text
model_driven_websearch：13 passed，0 failed
```

## 结果

结果文件：[`20261108T022513-round-85-websearch-blocks.json`](../results/20261108T022513-round-85-websearch-blocks.json)

| 检测项 | 分数 |
|---|---:|
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 10 |
| server_tool | 10 |
| 其它保护项 | 40 |
| **总分** | **82/110** |

## 结论

引用段保持独立是对官方流式 block 形状的兼容修复，但本轮 `websearch=0` 没有改变。由于没有抓包，不能据此判断本轮的公开 block 是否已被 CCTest 采样；Round 86 使用同一代码启动完整 capture 复核。

## 资源与生产边界

- 59139 由开发循环自动增量编译并重启；
- 59138 `/healthz` 保持 200，只执行只读探针；
- 没有 release 冻结编译；
- 没有清理任何原始抓包或账号数据。
