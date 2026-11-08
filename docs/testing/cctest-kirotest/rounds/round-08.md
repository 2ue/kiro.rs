# Round 08：合并 WebSearch 最终连续文本块

时间：2026-11-06（北京时间）

## 变更

在 `src/anthropic/handlers/model_driven_websearch.rs` 合并最终答案中的连续 `text` blocks，并去重合并 citations；server tool 与结果 block 的顺序不变。依据是 Round 07 搜索响应将最终答案拆成 10 个 text block，尝试验证客户端只取首块的可能性。

## 结果

结果文件：`results/20261106T113714-round-08-coalesced-websearch.json`，总分 `73/110`。

| 检测项 | Round 07 | Round 08 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 10 | 0 |
| server_tool | 10 | 10 |
| knowledge | 10 | 10 |
| model_attribution | 10 | 10 |
| doc_recognition | 10 | 10 |
| image_recognition | 10 | 10 |

`websearch` 无改善；`output_config` 再次由 10 变为 0，进一步支持采样或模型输出敏感的判断。

## 抓包与结论

抓包：`logs/llm-capture/capture-20261106T113708Z-844cd9ec/events.jsonl`，6959 行、约 7.5 MiB，JSON 均可解析，上游 HTTP 200 共 25 次。

WebSearch 请求 `req_0147gMph9oRF6RevV5SfWmxQ` 最终变为一个 text block，仍完整包含 citations、结果块、搜索 usage 和终止事件，但分数仍是 0。因此“多个 text block”不是评分失败的充分原因；该改动没有证据证明能提升兼容性，后续若其破坏段落/引用定位，则应回退。

## 资源与保护范围

- 使用 59139 开发实例，59138 未变更。
- 开发模式迭代；本轮没有 release 编译。
- 资源监控峰值未形成连续采样，后续补记录运行前后样本。

注：Round 08 的 `output_config=0` 与 Round 07 的 10 不同，不能把单轮下降归因于 text block 合并；后续按采样波动处理。
