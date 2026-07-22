# 外部池 prompt 超长（上游硬限制 + 本地高负载两段式）

Status: `classification-and-preflight-focused-pass / real-route-revalidation-pending`

Severity: P1

- 状态：已定性；2026-07-13 已做错误分类、用量估算与外部池 max-input 预检
- 严重级别：低 —— 生产近 12 小时 7 条（占非成功请求 2.5%）
- 分类来源：`tmp/analysis-usage-llm-errors` root-cause `04-external_pool_prompt_too_long`

## 现象（分类标题有误导性）

分类标题只写了"外部池 prompt 超长"，但全量样本显示这是**两段式根因链**：

1. 本地账号先尝试 **3 次**（`credentialAttemptCount:3`），**全部 500 瞬态**：
   ```
   500 Internal Server Error {"message":"Encountered unexpectedly high load when processing the request..."}
   errType: transient_error
   ```
2. 本地瞬态重试耗尽 → `fallbackReason: local_transient_exhausted` → fallback 到外部池 → 外部池以硬限制拒绝：
   ```
   bad_request: {"error":{"message":"prompt is too long: > 1000000 maximum ..."},"type":"error"}
   errorStatusCode: 400, routeKind: external_pool, routeSubtype: external_fallback_after_local_attempts
   ```

全量特征：7 条全部 `model=claude-opus-4-8`、`requestedMaxTokens=64000`、`credentialAttemptCount:3`、`externalAttemptCount:1`。

## 根因与性质判定：两层都是上游问题

| 层 | 错误 | 性质 |
|---|---|---|
| 本地 Kiro | 500 高负载 `unexpectedly high load` | 上游容量/瞬态 |
| 外部池 | `prompt is too long: > 1000000 maximum` | 上游硬上限（1M token） |

内容确实超过外部池 1M token 上限 —— 这部分**不可规避**（是真实的上下文过长）。

## 程序可规避性：仅体验优化，非根除

- ❌ 无法让超过 1M 的 prompt 被接受（上游硬限制）。
- ✅ **已改进**：parsed external route 不再把 `request_input_tokens` 恒置为 0；`prompt is too long` 已纳入 payload/context too-long 分类；外部池对外 public message 改为清晰的上下文过长语义，但仍不透出外部池原文。
- ✅ **已增加预检**：运行时配置新增 `externalPoolMaxInputTokens`，默认 `1,000,000`，`0` 表示关闭。若本次请求已有 input token 估算且超过该上限，代理直接返回稳定 `invalid_request_error`，记录 external failure usage，不占用外部池并发、不发外部池请求。
- ⚠️ **后续可选增强**：如果不同外部池或不同模型有不同上限，可继续扩展为 per-pool / per-model 上限；当前先用全局默认解决现网样本里的 1M 硬限制。
- ⚠️ 本地连续 3 次 500 高负载属**调度/容量**问题，与本请求内容无关；可关注账号池健康度，但不属于本请求可修复项。

## 复现说明

- 依赖上游瞬态（本地 500 高负载）+ 真实 >1M 上下文 + 外部池配置，**无法在本地稳定复现**。
- 外部池超长部分理论上可构造（拼一个 >1M token 的请求发到配了外部池的路由），但真实大流量会消耗资源；本轮先用定向分类/预检单测覆盖，真实服务回归只做低量协议验证。

## 回归清单

- [x] `prompt is too long` 被识别为 payload/context too-long。
- [x] 外部池 public error 使用上下文过长语义，但不泄露外部池 raw message。
- [x] parsed external route 携带非 0 `request_input_tokens`。
- [x] `externalPoolMaxInputTokens` 预检：超过上限时本地返回 `invalid_request_error`，不发外部池。
- [ ] 真实服务低量回归：正常外部池请求不受影响；超限请求本地快速失败。

## 残余风险与回滚

全局 `externalPoolMaxInputTokens` 不能表达不同 pool/model 的真实上限，token estimate 也可能与外部提供方 tokenizer 有偏差；因此预检必须保守且错误可解释。回滚可以关闭该预检（配置 0）以恢复上游判定，但不得恢复外部 raw 错误泄漏、无限等待或把确定性 too-long 当可换池重试。最终仍需正常/near-limit/over-limit 每类 5 轮和错误后恢复。

## 关联

- 生产证据：`tmp/analysis-usage-llm-errors/root-causes/04-external_pool_prompt_too_long/`。
- 外部池设计：`docs/external-fallback-pools-design.md`。
