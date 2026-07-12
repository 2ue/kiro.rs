# LLM 调用异常根因索引

基于 `tmp/analysis-usage-llm-errors` 生产证据（app `0.0.101` / revision `737f9f1`，最近 12 小时窗口，50405 条 usage / 281 条非成功）逐类分析、复现、归档。本目录每份文档对应一个独立根因，含现象、根因、复现 case、修复方案与回归清单。

## 分析窗口口径

- UTC 窗口：`2026-07-11 21:46` → `2026-07-12 09:46`
- 非成功记录：281 条；status 分布 `error 216 / upstream_timeout 41 / client_dropped 17 / stream_error 7`

## 两大类

- **程序缺陷（可根除）**：输入决定、必现，代理侧透传/校验缺失导致。工具与 schema 边界值三类，合计约占非成功请求的 74%。
- **上游 / 客户端行为（只能优化，不可根除）**：依赖上游瞬态状态或客户端行为，无法在本地稳定复现，程序侧只能提升容错或体验。

## 文档清单

### A. 程序缺陷（优先修复）

| 文档 | 根因 | 生产条数 | 上游 reason | 复现 | 可规避 |
|---|---|---|---|---|---|
| [empty-tool-description-400-invalid-tool-use-format.md](./empty-tool-description-400-invalid-tool-use-format.md) | 问题 A：工具 `description` 为空串 | 201（71.5%） | Kiro `REQUEST_BODY_INVALID` | ✅ 已复现并修复 | ✅ 已填非空占位符 |
| ↑ 同文档 问题 B | `input_schema` 显式为 `null` | 7（2.5%） | 入口反序列化 400 | ✅ 已复现并修复 | ✅ 已容忍 null |
| [tool-property-key-invalid-400-tool-schema-invalid.md](./tool-property-key-invalid-400-tool-schema-invalid.md) | 工具属性键不匹配 `^[a-zA-Z0-9_.-]{1,64}$` | 另有生产样本 | Bedrock `TOOL_SCHEMA_INVALID` | ✅ 已复现并修复 | ✅ 默认可逆 sanitize；支持 reject/disabled |

三类同源：对客户端工具/schema 字段的边界值（空串 / null / 非法键）缺乏入口清洗与兜底，直接透传上游被拒。2026-07-12 已修复空 `description` 与 `input_schema:null`；非法 property key 采用默认可逆 `sanitize`：只清理不匹配正则的 key，发给上游前映射为唯一 `key<hash>`，并在 stream / non-stream 工具调用响应中还原为客户端原始 key。需要硬拒绝时可切换 `toolSchemaKeyMapping=reject`；需要旧透传行为时可切换 `disabled`。

### B. 上游 / 客户端行为（优化，非根除）

| 文档 | 根因 | 生产条数 | 性质 | 本地复现 | 程序可做 |
|---|---|---|---|---|---|
| [02-stream-upstream-idle-timeout.md](./02-stream-upstream-idle-timeout.md) | 流式上游空闲满 180s 被掐断 | 41 | 上游静默为主 | ❌ 依赖上游状态 | ⚠️ 首字前安全重试 + 调超时（**唯一有实质价值**） |
| [03-client-dropped-downstream.md](./03-client-dropped-downstream.md) | 下游客户端提前断开 | 17 | 客户端行为，非缺陷 | ❌ | ❌ 忽略，不计入接口根因 |
| [04-external-pool-prompt-too-long.md](./04-external-pool-prompt-too-long.md) | 本地 3 次 500 高负载耗尽 → 外部池撞 1M 上限 | 7 | 上游（高负载 + 硬限制） | ❌ | ⚠️ 外部池前 token 预检省一次往返 |
| [06-stream-upstream-status-error.md](./06-stream-upstream-status-error.md) | 流中途上游返回错误事件 | 5 | 上游瞬态 | ❌ | ⚠️ 有限（首字后不可安全重试） |
| [07-stream-internal-read-error.md](./07-stream-internal-read-error.md) | 流 body 解码失败 | 2 | 上游/网络瞬态 | ❌ | ⚠️ 有限 |
| [08-image-format-unsupported-400.md](./08-image-format-unsupported-400.md) | 坏图/伪图无法被上游解码 | 1 | 上游为主 | ✅ 坏图/伪图已复现 | ⚠️ 可选完整解码校验提前拦截 |

## 重要更正记录

- **08 图片**：早期误判「1×1 图片被拒」。实测更正：合法图片（64×64 / 512×512）正常返回 200；1×1 是被上游静默忽略（非拒绝）；真正触发 `IMAGE_FORMAT_UNSUPPORTED` 的是坏图/伪图。详见该文档。
- **04 标题误导**：分类名为「外部池 prompt 超长」，实际根因链是本地账号先 3 次 500 高负载、fallback 后才撞外部池 1M 上限。

## 优先级建议

1. **高**：工具/schema 三类（A）——必现、纯程序、约 74%，合并修复。
2. **中**：02 首字前安全重试 —— 需严格区分首字前/后，涉及流式重试安全边界。
3. **低**：04 外部池预检、08 图片解码校验 —— 体验优化，量小。
4. **忽略**：03（客户端行为）、06/07（上游瞬态、量极小）。

## 公共前置（复现用）

- 本地服务：`127.0.0.1:9022`，API Key `sk-kiro-rs-local-debug`（见 `config.json`）。
- 本地测试账号仅支持 sonnet，模型统一用 `claude-sonnet-4-20250514`。
- 生产证据目录：`tmp/analysis-usage-llm-errors/`。
