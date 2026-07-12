# 外部池 prompt 超长（上游硬限制 + 本地高负载两段式）

- 状态：已定性；主体为上游问题，仅有体验优化空间
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

## 性质判定：两层都是上游问题

| 层 | 错误 | 性质 |
|---|---|---|
| 本地 Kiro | 500 高负载 `unexpectedly high load` | 上游容量/瞬态 |
| 外部池 | `prompt is too long: > 1000000 maximum` | 上游硬上限（1M token） |

内容确实超过外部池 1M token 上限 —— 这部分**不可规避**（是真实的上下文过长）。

## 程序可规避性：仅体验优化，非根除

- ❌ 无法让超过 1M 的 prompt 被接受（上游硬限制）。
- ⚠️ **可优化**：走外部池前，用已有的 input token 估算做**预检**。若已知超过外部池上限，直接返回清晰的"上下文过长"错误，省掉一次注定失败的外部池往返（降低延迟与外部池无效计费）。
- ⚠️ 本地连续 3 次 500 高负载属**调度/容量**问题，与本请求内容无关；可关注账号池健康度，但不属于本请求可修复项。

## 复现说明

- 依赖上游瞬态（本地 500 高负载）+ 真实 >1M 上下文 + 外部池配置，**无法在本地稳定复现**。
- 外部池超长部分理论上可构造（拼一个 >1M token 的请求发到配了外部池的路由），但本地测试账号与外部池配置不具备，暂不纳入回归。

## 关联

- 生产证据：`tmp/analysis-usage-llm-errors/root-causes/04-external_pool_prompt_too_long/`。
- 外部池设计：`docs/external-fallback-pools-design.md`。
