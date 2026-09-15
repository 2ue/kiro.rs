# sub2api 质量感知调度：现状勘察与方案修正

> 勘察时间：2026-09-15，代码基线 `/Users/yuanfeijie/Desktop/procode/sub2api`。
> 结论：**sub2api 的现状与最初方案假设差异很大**，不能照搬 kiro.rs 的实现。

## 1. 最重要的发现：OpenAI 路径已经有质量感知调度了

`backend/internal/service/openai_account_scheduler.go` 已实现一套与我在
kiro.rs 中设计的方案高度同构的机制，且同样是**被动采样**：

| 能力 | sub2api OpenAI 路径 | 位置 |
| --- | --- | --- |
| 错误率 EWMA | ✅ `alpha = 0.2`（与 kiro.rs 完全一致） | `:231-262` |
| 首字 EWMA | ✅ 首样本直接赋值，避免从 0 爬升 | `:244-261` |
| 无样本区分 | ✅ `ttftEWMA` 初始化为 NaN，`hasTTFT` 标识 | `:207, :278` |
| 被动上报 | ✅ `ReportResult(accountID, success, firstTokenMs)` | `:122, :1819, :2440` |
| 组内归一化 | ✅ TTFT 按候选集 min-max 归一 | `:918-930, :990-992` |
| 权重可配 | ✅ 10 项权重，支持 DB 覆盖 | `:2571-2614` |
| Top-K 随机 | ✅ `openAIWSLBTopKForRequest` | `:1032-1038` |
| 质量触发逃逸 | ✅ `shouldEscapeStickyAccount`（首字/错误率超阈值时脱离粘性） | `:632-643` |

**因此 Task #5 的目标必须重写**：不是"给 sub2api 加质量感知调度"，
而是"把 OpenAI 路径已有的能力，补齐到其它路径 + 补上缺失的劣化/恢复语义"。

## 2. 真正的缺口

### 缺口 A：Anthropic / Gemini 路径是「逐级硬过滤」，完全没有质量因子

`gateway_scheduling.go: SelectAccountWithLoadAwareness`（`:100`）走的是另一条路：

```
filterByMinPriority  →  filterByMinLoadRate  →  filterBySoonestReset  →  selectByLRU
        :1589                 :1609                   :1632                :1661
```

每一步都是**硬过滤**（只保留 `== min` 的账号），最后 LRU 兜底。
没有评分、没有错误率、没有首字，`openAIAccountRuntimeStats` 完全不参与。
**这才是用户需求真正未被满足的地方。**

注意：这条路径的硬过滤链条意味着质量因子不能简单"加权进去"——
需要决定它插在哪一级，且不能破坏 `filterByMinPriority` 的优先级硬分层语义
（与 kiro.rs 中 L2-14 守住的是同一条不变量）。

### 缺口 B：没有"持续劣化 → 临时降级 → 逐步恢复"的时间维度

这是用户明确要求的部分（"持续多少分钟，直接对账号降级多少分钟"）。
sub2api 现有的只是**瞬时** EWMA 评分：质量差 → 当前这次少选它；
但没有 kiro.rs 的 probation（避让期）、指数退避、探测配额、恢复爬坡。

现有的 `shouldEscapeStickyAccount` 只作用于粘性会话，且是瞬时判断。

### 缺口 C：`buildOpenAIAccountSchedulerScoreSnapshot` 的因子是死值

`openai_account_scheduler.go:2686-2688, 2751-2752`：

```go
errorRate: 0, ttft: 0, hasTTFT: false,   // 从不填充
errorFactor := 1.0                        // 硬编码
ttftFactor := 0.5                         // 硬编码
```

这个函数服务于**快照/预览展示**（`scheduler_snapshot_service.go`、admin API），
不在真实选路上（真实选路在 `:980-1029`，因子是活的）。
后果：管理端看到的评分与真实调度评分不一致，
`weights.ErrorRate` / `weights.TTFT` 在快照里对所有账号贡献相同、完全抵消。
**这是一个独立的展示层 bug，可单独修复，风险低、价值明确。**

## 3. TTFT 数据可得性

| 路径 | 是否已有 firstTokenMs | 备注 |
| --- | --- | --- |
| OpenAI | ✅ 已接入 `ReportResult` | — |
| Antigravity（Claude/Gemini/compat） | ✅ 流式中已采集 | `antigravity_gateway_*.go`，但**未喂给调度器** |
| Anthropic 原生 / Gemini 原生 | ❓ 待确认 | 需进一步勘察 |

Antigravity 那几条路已经算出了 `firstTokenMs` 并塞进 usage 上报，
只是没有转交调度器——接线成本低。

## 4. 修正后的实施顺序（按价值/风险比排序）

1. **S1 — 修 `buildOpenAISchedulerScoreSnapshot` 死因子**（低风险、独立）
   让快照复用真实选路的 `stats.snapshot()`，管理端所见即所得。
2. **S2 — 给非 OpenAI 路径接入质量因子**（核心价值）
   在 `SelectAccountWithLoadAwareness` 的过滤链中引入质量维度，
   必须保住 `filterByMinPriority` 的硬分层不变量。
3. **S3 — 补劣化/恢复时间维度**（用户明确要求）
   把 kiro.rs 的 probation + 指数退避 + 探测配额 + 恢复爬坡移植过来，
   两个项目的参数语义与命名保持一致，便于运维统一理解。
4. **S4 — 设置项/DTO/前端**：沿用既有 `weightOverrides` 覆盖机制扩展。
5. **S5 — 复杂场景测试**：与既有因子（粘性会话、限流、临时不可调度、
   故障转移排除）混合，对齐 kiro.rs 的 L2 测试矩阵。

## 5. 已确认的决策（2026-09-15，用户确认）

- **范围：A + B + C 全做**，按 S1→S5 顺序推进。
- **probation 状态存 Redis**：sub2api 线上是**多副本部署**。
  因此不能沿用现有 `openAIAccountRuntimeStats` 的进程内 `sync.Map` 风格，
  避让状态必须跨实例共享，与 kiro.rs 保持一致——
  否则同一个坏账号会在每个实例上各自重新踩一遍坑，
  且避让/恢复的时间语义在副本间互相矛盾。
  > 注意：EWMA 统计本身是否也要搬去 Redis 是一个独立问题。
  > 进程内 EWMA 在多副本下只是"每实例各自收敛"，尚可接受；
  > 但 probation 是**带时间语义的状态机**，必须共享。
- **base 分支既有 5 个红灯：顺手修掉**，作为独立提交，不混入本特性。

## 6. 待确认项

- [ ] Anthropic 原生 / Gemini 原生路径是否采集首字
- [ ] openspec 变更目录命名与既有 `add-*` 约定对齐
