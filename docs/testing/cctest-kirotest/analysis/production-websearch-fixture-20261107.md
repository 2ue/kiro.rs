# 生产抓包 WebSearch 字段对照

更新时间：2026-11-07（本机 UTC 证据）

## 结论

生产抓包确认当前实现使用的两个 WebSearch opaque 字段形状与既有部署输出一致：

- `encrypted_content` 是搜索摘要明文，允许单行、多行和空字符串；
- `encrypted_index` 是 Base64 编码的 `序号:URL`；
- 生产旧版本的 `server_tool_use` 与 `web_search_tool_result` 没有 `caller`，当前工作树已补 `caller={"type":"direct"}`；
- 生产抓包不能证明 CCTest 的 `websearch=0` 是由 opaque 字段导致，也不能证明这些值满足官方加密语义。

因此本轮不改 `encrypted_content`、`encrypted_index`，不伪造加密值。当前最高分 `82/110` 的 WebSearch 失败仍缺少评分器失败样本或官方 fixture 支持。

## 数据范围

只读顺序扫描本机生产抓包：

```text
/var/lib/docker/volumes/65265a2bb319768ff130ad0a88ee5d08aba49d9ba89286f2c0ebdf4a3b12bda7/_data/llm-capture/capture-20261106T074649Z-bd94a24b/events.jsonl
```

文件大小 `4,294,967,184` 字节，`459,688` 行。分析脚本只保留计数、长度、Base64 解码前缀和哈希前缀，没有复制、修改或删除原始抓包。

## 统计结果

| 字段/事件 | 统计 |
|---|---:|
| `encrypted_content` | 41 次 |
| `encrypted_index` | 21 次 |
| `encrypted_content` 单行摘要 | 34 次 |
| `encrypted_content` 多行摘要 | 6 次 |
| `encrypted_content` 空字符串 | 1 次 |
| `encrypted_index` 可解码为 `序号:URL` | 21 次 |
| `web_search_tool_result` 结果数组长度 10 | 3 次 |
| `web_search_tool_result` 结果数组长度 11 | 1 次 |
| 生产 `server_tool_use` 带 `caller` | 0/4 |
| 生产 `web_search_tool_result` 带 `caller` | 0/4 |

`encrypted_content` 多于结果数量，是因为抓包同时包含搜索结果块和 citation 关联内容；统计不能据此推断每个结果必须固定一个摘要长度。

## 字段形状

生产样本的 `encrypted_index` Base64 解码后形如：

```text
1:https://...
5:https://...
3:https://...
```

当前工作树生成逻辑位于 `src/anthropic/handlers/model_driven_websearch.rs`，同样编码 `format!("{number}:{}", source.url)`。`encrypted_content` 当前来自 `result.snippet`，与生产样本的明文摘要形状一致。

## 版本边界

该抓包来自 59138 生产实例的旧部署（镜像 `0.0.239`，OCI revision `30edc00c...`）。当前工作树为 `v0.0.240`，已增加 `caller` 字段等后续改动。生产抓包只能用于字段形状参照，不能作为当前代码全部行为的等价证明。

## 对评分假设的影响

1. opaque 字段形状与生产旧部署一致，随机替换为伪加密值没有证据收益。
2. `caller` 是生产旧版本缺失、当前代码已补齐的公开协议差异；Round 61 仍为 `websearch=0`，说明仅补 `caller` 不足以通过私有评分。
3. 抓包中没有发现搜索结果被第二轮原样回传上游的证据，无法验证评分器是否检查跨轮 opaque 字段回传。
4. 后续只有在获得 CCTest 失败样本 ID、官方 fixture 或明确分数共变实验后，才继续修改 WebSearch 字段。

## 复核命令

分析使用本地临时脚本 `/tmp/analyze_prod_websearch.py` 顺序读取事件文件，未触碰 59138 服务、数据库、Redis、账号状态或 Caddy 配置。
