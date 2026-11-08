# 官方 WebSearch fixture 字段形状

更新时间：2026-11-07（本机保存的 Anthropic 官方文档页面）

## 结论

官方 Anthropic WebSearch 文档明确要求 `encrypted_content` 和 `encrypted_index` 是不透明值，并要求多轮对话原样回传。官方示例的 Base64 前缀可以解码出 protobuf 风格的外层结构；当前代理仍把 `encrypted_content` 直接填成 MCP 明文 snippet，因此存在明确的公开形状差异。

该文档没有给出生成算法、密钥或完整 fixture。不能据此离线复造官方密文，也不能把随机 Base64 当作兼容实现。当前应把它列为最高优先级候选，同时保留原始抓包，等待可验证的官方样本或评分共变实验。

## 来源与本地文件

- 页面：Anthropic WebSearch 文档，保存为 `/tmp/anthropic_websearch.html`。
- 流式 Messages 文档，保存为 `/tmp/anthropic-messages-streaming.html`。
- 文档示例摘要和流事件正文同时保留在 `/tmp/wscc_stream.out`、`/tmp/wscc_nonstream.out`；这些临时文件未写入仓库，仓库只记录字段结论和路径。

官方示例：

```json
{
  "type": "web_search_result",
  "encrypted_content": "EqgfCioIARgBIiQ3YTAwMjY1Mi1mZjM5LTQ1NGUtODgxNC1kNjNjNTk1ZWI3Y..."
}
```

官方 citation 示例：

```json
{
  "type": "web_search_result_location",
  "encrypted_index": "Eo8BCioIAhgBIiQyYjQ0OWJmZi1lNm.."
}
```

## 可复核的字节证据

对官方示例的可见 Base64 前缀解码：

```text
EqgfCioIARgBIiQ3YTAwMjY1Mi1mZjM5LTQ1NGUtODgxNC1kNjNjNTk1ZWI3Y
→ 12 a8 1f 0a 2a 08 01 18 01 22 24 7a 30 30 32 36 35 32 2d ...

Ev0DCioIAxgCIiQ3NmU4ZmI4OC1k
→ 12 fd 03 0a 2a 08 03 18 02 22 24 37 36 65 38 66 62 38 38 2d ...

Eo8BCioIAhgBIiQyYjQ0OWJmZi1l
→ 12 8f 01 0a 2a 08 02 18 01 22 24 32 62 34 34 39 62 66 66 2d ...
```

可见结构稳定包含：

- 外层 protobuf 字段 `0x12` 和长度 varint；
- 内层字段 `0x0a 0x2a`，其中包含 `0x08 <序号>`、`0x18 <版本/类型>` 和 `0x22 0x24 <36 字符 UUID>`；
- 后续字段被文档 `...` 截断，不能推断摘要、URL 或加密正文的编码方式。

## 与当前代码对照

当前代码：

- `src/anthropic/websearch.rs::web_search_result_block` 将 `result.snippet` 明文写入 `encrypted_content`；
- `src/anthropic/handlers/model_driven_websearch.rs::citation` 将 `number:URL` 做普通 Base64，作为 `encrypted_index`。

Round 76 的公开生命周期、ID 配对、citations、usage 和终止事件均完整，但 `websearch=0`。因此事件状态机和提示词注入已不能解释全部失败；`encrypted_content` 的官方 opaque 形状是剩余最高价值候选。

## 处理边界

1. 不删除唯一原始抓包；opaque 字段失败样本仍需要按请求回放。
2. 不随机生成 protobuf 或密文；没有完整 fixture 时，结构仿造会污染评分证据。
3. 若取得完整官方值，先做非流单变量 A/B，再做流式单变量 A/B；每轮记录评分、上游请求、历史回传和保护项。
4. `encrypted_index` 的 `number:URL` 可逆 Base64 与官方示例的 opaque protobuf 前缀明显不同，但 caller 实验已证明字段缺失不是唯一根因；两字段仍需分别验证。
