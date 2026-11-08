# Round 81：opaque 字段保留边界复测

## 时间与版本

- 时间：2026-11-07（北京时间）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 代码：当前工作树 `v0.0.240`，开发循环已热更新 59139
- 结果文件：[`20261107T201008-round-81-opaque-preserve.json`](../results/20261107T201008-round-81-opaque-preserve.json)
- 59138：只读健康检查，未停止、未重启、未修改

## 单变量修改

WebSearch 结果结构新增可选字段：

- 如果上游结果提供 `encrypted_content`，对外原样保留；
- 如果上游结果提供 `encrypted_index`，citation 原样保留；
- Kiro MCP 当前不提供这两个字段，仍使用既有兼容回退；
- 没有生成随机密文，也没有改变当前 Kiro MCP 响应的字段值。

该修改只建立了未来真实 opaque 字段的透传边界，不把本地生成的值伪装成 Anthropic 官方密文。

## 结果

总分 **82/110**，与 Round 79/80 相同。

| 检测项 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 10 |
| server_tool | 10 |
| knowledge | 10 |
| model_attribution | 10 |
| doc_recognition | 10 |
| image_recognition | 10 |

## 结论

透传边界没有造成保护项回归，也没有改善当前 WebSearch 分数，因为 Kiro MCP 返回的结果对象没有真实 opaque 字段。当前三个失败项仍未达到可修复证据门槛：

- `stream_structure=5`：公开下游生命周期完整；上游 EOF 无 `COMPLETED` 仍是候选，但不能伪造上游终止事件；
- `signature_proto=5`：真实 Kiro signature 仍应原样透传；没有 verifier 失败样本支持重编码；
- `websearch=0`：完整 MCP 回灌和 citation 已固定，仍缺少官方 opaque fixture 或多轮评分失败样本。

## 验证

- `cargo fmt --check` 通过；
- WebSearch 定向测试通过；
- Round 81 完整 CCTest 通过并返回 `done`；
- 59139 `/healthz`：200；59138 `/healthz`：200。
