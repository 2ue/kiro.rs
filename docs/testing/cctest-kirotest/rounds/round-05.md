# Round 05：开启代码执行配置后的复测

时间：2026-11-06（北京时间）

## 变更

- Rust 源码仍未修改。
- 59139 开发实例通过 Admin API 开启独立 runtime_config：
  - `bodyConversion.codeExecution.enabled=true`；
  - `bodyConversion.codeExecution.pathOverrides["/dfcache/tune-ss6-99"]=true`。
- `/dfcache/tune-ss6-99` 的模型驱动 WebSearch 开关保持开启。
- 59138 生产实例未访问、未重启、未写配置。

配置变更脱敏记录：`/tmp/kirotest-dev/artifacts/round05-code-execution-config.json`。

## cctest 结果

结果文件：`results/20261106T105750-round-05-code-execution.json`。

| 检测项 | Round 04 | Round 05 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 0 | 0 |
| server_tool | 0 | 0 |
| knowledge | 10 | 10 |
| model_attribution | 10 | 10 |
| doc_recognition | 10 | 10 |
| image_recognition | 10 | 10 |

总分仍为 `64/110`。仅开启代码执行配置没有让 `server_tool` 得分，说明请求形状和输入 schema 仍不完整，不能只靠 runtime 开关触发原生代码执行。

## 抓包证据

抓包：`logs/llm-capture/capture-20261106T105736Z-e47a710f/events.jsonl`，约 8.3 MiB、9678 行，全部 JSON 可解析；上游 HTTP `200=25`，没有错误响应。

cctest 代码执行请求带有原生工具声明：

```json
{
  "type": "code_execution_20250825",
  "name": "code_execution"
}
```

当原生工具声明没有输入 schema 时，Kiro 返回的 `toolUseEvent` 的 `input` 为空对象。代理只能把它转换成执行失败结果：缺少 `code` 或 `command`。因此客户端虽然收到了 `server_tool_use` 和结果 block，实际执行没有成功。

## 根因与结论

1. 配置开关负责允许代码执行路径，但不会替代理解原生工具输入契约。
2. `code_execution_20250825` / `bash_code_execution_20250825` 需要明确的输入 schema 和描述，才能让 Kiro 生成可执行输入。
3. 下一轮采用最小代码改动，在工具转换边界补齐 schema；保持执行器、usage 和生产服务不变。

## 资源与证据

- 使用开发模式 `cargo run`，未执行 release 编译。
- 抓包原始 JSONL 和 ZIP 均保留。
- 生产 59138 全程未变更。
