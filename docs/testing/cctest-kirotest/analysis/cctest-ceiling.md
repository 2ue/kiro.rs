# CCTest 当前外部评分上限证据

## 结论

当前代码和 Kiro 账号链路稳定可复现 `82/110`，但已有结果显示 `signature_proto=10` 不是这条 Bedrock 上游链路可以自然得到的结果：历史结果中 92 轮检测被标记为 `streamChannel=aws-bedrock`，这 92 轮的 `signature_proto` 全部为 5，没有一轮为 10。带 `streamChannel=null` 的早期结果没有得到更高签名分，反而多为 0。

这说明剩余 5 分不是公开 SSE 生命周期、签名是否为空或签名是否被代理改写造成的，而是 CCTest 对签名来源/通道的私有判定。Kiro 上游返回的是 AWS Bedrock/Kiro 原生 opaque signature；没有 Anthropic 官方通道签名、验证算法或可验证 fixture，不能通过修改字段把它变成官方签名。

## 统计证据

对 `docs/testing/cctest-kirotest/results/*.json` 的只读聚合：

| 条件 | 结果数量 | `signature_proto=5` | `signature_proto=0` |
|---|---:|---:|---:|
| `streamChannel=aws-bedrock` | 92 | 92 | 0 |
| `streamChannel=null` | 8 个有分数结果 | 0 | 8 |

Round 102 继续满足第一行：

```json
{
  "streamChannel": "aws-bedrock",
  "scores": {
    "stream_structure": 5,
    "websearch": 0,
    "signature_proto": 5
  }
}
```

## WebSearch 官方运行时对照

本机保存的 `/tmp/wscc_stream.out` 和 `/tmp/wscc_nonstream.out` 是 Anthropic WebSearch 运行时样本。其字段形状与 Round 102 当前响应一致：

- `server_tool_use.name = "web_search"`；
- `web_search_tool_result.content` 为 10 个 `web_search_result`；
- `encrypted_content` 为搜索摘要明文；
- `encrypted_index` 为 `Base64(序号:URL)`；
- `web_search_result_location` citation 携带相同形状的 `encrypted_index`；
- `server_tool_use.web_search_requests = 1`；
- `message_delta(end_turn)` 和 `message_stop` 闭合。

Round 102 还额外补了 `caller={"type":"direct"}`；Round 69–72 已做 caller 有无 A/B，评分均保持 `websearch=0`。因此“字段必须是官方 protobuf opaque 值”不能继续作为已证实根因；官方运行时样本本身也展示了明文摘要与可逆 Base64 索引。

当前 WebSearch 0 分更可能来自 CCTest 的私有请求/通道判定、模型回灌条件或结果内容采样，公开响应差异不足以支持再次改写字段。

## 工程边界

保留真实 Kiro signature、官方公开 WebSearch 字段和完整事件生命周期，继续保持 59139 开发模式；不随机生成 Anthropic signature，不把 Kiro signature 重编码成伪官方值，不修改 59138。

如果硬性目标仍要求 110/110，需要新增可验证的 Anthropic 官方通道对照（官方凭据或 provider-issued signature fixture），或取得 CCTest 失败请求级 verifier 证据。仅靠当前 Kiro/Bedrock 账号和公开转换代码无法证明可以达到该目标。

## Round 103 复核

Round 103 在相同默认 tune 配置下再次得到 `82/110`，结果仍标记 `streamChannel=aws-bedrock`，`signature_proto=5`、`websearch=0`、`stream_structure=5`。11 个 thinking 请求的真实 Kiro signature 均非空，WebSearch 的公开链路和 MCP 200 均完整；因此 Round 103 没有改变“Bedrock 通道当前可复现上限候选”的判断。
