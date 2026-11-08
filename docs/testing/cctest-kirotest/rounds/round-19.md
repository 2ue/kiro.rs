# Round 19：开发库账号清理后复测

时间：2026-11-06（UTC 17:10；北京时间 01:10）

## 环境与边界

- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139 开发实例。
- 运行方式：开发模式，未执行 release 冻结。
- 59138 仅做只读探针，未重启、未改配置、未写 PostgreSQL/Redis、未调用生产 Admin 写接口。
- 本轮使用固定模型 `claude-opus-4-8`；与后续 Opus 5.5 轮次不直接比较。

## 结果

结果文件：`results/20261106T171014-round-19-dev-accounts-clean.json`

总分：`68/110`。

| 检测项 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 0 |
| output_config | 10 |
| server_tool | 10 |
| knowledge | 10 |
| model_attribution | 0 |
| doc_recognition | 10 |
| image_recognition | 10 |

## 账号处理

上一轮从 59139 独立数据库删除了有明确 `auth`/额度失败证据的凭据；本轮没有继续按单次限流或普通网络错误删除账号。可用账号均有近期成功调用，生产 59138 状态未改变。

## 结论

`server_tool=10` 继续保持，说明代码执行工具 schema 修复有效。`output_config=10` 说明结构化输出存在模型/请求随机性。`signature_proto=0` 与模型切换相关，不能据此断定固定 Opus 5.5 的协议行为；后续固定模型复测。
