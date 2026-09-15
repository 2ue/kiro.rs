# 本地账号 429 狂暴轮换模式（计划文档）

> 状态：计划已定稿，待实现
> 适用范围：**仅本地账号**（`src/kiro/`），外部池（`src/external_pool/`）完全不受影响
> 分支：`feat/local-berserk-rotation`
> 设计基线：**沿用 ../kiro-rs-main 的 429 处理模式，额外增加狂暴模式开关**

---

## 一、需求背景

### 1.1 用户原始描述

> 分析 ../kiro-rs-main 这个项目中学习其中的调用端点，然后 429 暴力轮换，直到成功，做成配置开关。
>
> 我希望的是多个端点轮换调用，遇到 429 就换号（优先换号）重试，把所有账号重试 1 遍，直到所有的账号轮换一遍（所有的端点），这是狂暴模式，开启才这样做，可以设置轮换几轮，做成配置，只针对本地账号。

### 1.2 追加澄清

| # | 澄清内容 |
|---|---|
| 1 | **狂暴模式关闭后，理论上就不能影响现在调度逻辑** |
| 2 | **端点轮换可以加到普通模式去**（独立于狂暴模式的开关） |
| 3 | **端点轮换指的是调用上游的 baseUrl**（本地账号调度 AWS 上游），不是 ide/cli 协议实现 |
| 5 | **前端页面不需要配置具体端点，只需要开关**；端点按 kiro-rs-main 的来 |
| 4 | **按 kiro-rs-main 的模式来实现狂暴模式，额外加狂暴模式开关** |

### 1.3 已确认的设计决策

| 决策点 | 选择 |
|---|---|
| 轮换列表形态 | **端点内置不可配**：只在官方支持的 `us-east-1` / `eu-central-1` 间轮换，用户只有一个布尔开关（见 §4.2） |
| profileArn region 冲突 | **强制轮换，失败算一次尝试** |
| 换号退避 | 同轮内换号**完全不等待** |
| 429 冷却副作用 | **仅本次请求内 `excluded_ids` 临时排除，不写全局冷却** |
| 轮换顺序 | **优先换号**：先把所有账号在 region A 试完，再切 region B |

---

## 二、基线调研：kiro-rs-main 的模式

### 2.1 先澄清一个事实

**kiro-rs-main 没有多 baseUrl 轮换。** 已逐文件核实：

| 检查项 | 证据 |
|---|---|
| `api_url` 构造 | `src/kiro/endpoint/ide.rs:64-69`、`cli.rs:64-66`，单个 `format!("https://q.{}.amazonaws.com/...", self.api_region(ctx))` |
| region 来源 | `effective_api_region(ctx.config)`，每凭据解析出**一个**值 |
| config region 字段 | 仅 `region` / `auth_region` / `api_region` 三个**单值 String**（`config.rs:80-91`） |
| 列表类型 | `grep "Vec<String>"` 在 config.rs **零命中** |
| 轮换游标 | `grep rotate/rotation/next_region` 只命中 client-key 轮换与日志 rotate |
| baseUrl 覆盖 | kiro-rs-main **连 `kiro_upstream_base_url` 都没有**（本项目反而有） |

所以**在推理主链路上 region 轮换是本项目的新增设计**；从 kiro-rs-main 借鉴的是它的**429 错误分类与重试骨架**。

**但 kiro-rs-main 在 REST 辅助接口上确实有两端点回退**：
`rest_api_region_candidates`（`src/kiro/token_manager.rs:465`）对
getUsageLimits / ListAvailableModels / setUserPreference 就是在
`us-east-1` 与 `eu-central-1` 之间按 SSO 区域排序后做 403 回退，并注明
「这些接口**仅在这两个端点提供服务**」。本方案的端点集合与排序规则直接取自这里。

### 2.2 kiro-rs-main 的 429 三分法（本方案的基线）

`../kiro-rs-main/src/kiro/provider.rs:1007-1132`：

```
429 到达
 ├─ ① 账号级风控（is_account_throttled: "suspicious activity" + "temporary limits"）
 │    → 冷却 account_throttle_cooldown_secs（默认 1800s）
 │    → 有 Retry-After → 立即返回，不换号
 │    → remaining == 0  → 立即返回
 │    → 否则 continue（换账号）
 │
 ├─ ② 有可解析的 Retry-After（should_retry_locally() == false）
 │    → 直接返回 429 给客户端，遵守上游指示
 │
 └─ ③ 普通 429 / 408 / 5xx
      → 不禁用、不切换凭据
      → 429 用 retry_delay_throttle(1s→8s)；408/5xx 用 retry_delay(200ms→2s)
      → continue
```

其重试预算 `MAX_TOTAL_RETRIES = 4`，注释明确写了理由：**"过多重试会在账号间连环撞墙、放大限流"**。

### 2.3 从 kiro-rs-main 借鉴什么 / 不借鉴什么

| 机制 | kiro-rs-main | 本方案 |
|---|---|---|
| 429 三分法骨架 | ✅ | **✅ 完整沿用**，狂暴模式只接在第③类之后 |
| 风控型 429 不硬刚 | ✅ | **✅ 沿用**（本项目 `detect_risk_control_error` 覆盖面更广） |
| `Retry-After` 存在即停 | ✅ | **✅ 沿用**，狂暴模式也必须遵守 |
| `retry_delay_throttle` 429 长退避 1s→8s | ✅ | **✅ 移植**（本项目目前缺失，429 与 5xx 共用 200ms→2s 短退避） |
| 误判防护：子串预扫 + JSON 确认 `reason` | ✅ | ✅ 沿用 |
| 总重试上限 4 | ✅ | ❌ 狂暴模式下按 `账号 × region × 轮数` 放开 |
| 普通 429 同账号重试 | ✅ | ❌ 狂暴模式改为**优先换号**（用户明确要求） |
| 推理链路 region 轮换 | ❌ 不存在 | ✅ 本项目新增 |
| 两端点集合与排序规则 | ✅ `rest_api_region_candidates`（REST 接口） | **✅ 沿用**，移植到推理链路 |

---

## 三、本项目现状

### 3.1 上游 URL 构造链

```
KiroEndpoint::api_url(ctx)
  └─ base_url(ctx)
       ├─ config.kiro_upstream_base_url（若配置则直接覆盖，仅用于压测/staging）
       └─ 否则 format!("https://q.{}.amazonaws.com", api_region)
            └─ credentials.effective_api_region(config)
                 ├─ 1. credentials.api_region
                 ├─ 2. profile_arn 内嵌 region  ← arn:aws:codewhisperer:{region}:...
                 └─ 3. config.effective_api_region()（默认 us-east-1）
```

- `src/kiro/endpoint/ide.rs:33-48`、`src/kiro/model/credentials.rs:599-604`、`:431-438`

**关键约束**：`profile_arn` 内嵌 region 优先级高于全局配置，因此轮换必须通过**显式覆盖**生效。

### 3.2 本项目与 kiro-rs-main 的差异

| 能力 | kiro-rs-main | 本项目 |
|---|---|---|
| 风控 429 检测 | `is_account_throttled`（2 个关键词） | `detect_risk_control_error`（`provider.rs:12187`，覆盖 423/TEMPORARILY_SUSPENDED/多种变体，**更全**） |
| 429 专用长退避 | `retry_delay_throttle` 1s→8s | **缺失**，429 与 5xx 共用 `retry_delay` 200ms→2s |
| 429 冷却写入 | `report_account_throttled_for_request` | `report_transient_failure_kind`（`provider.rs:11958`，写全局冷却） |
| 请求级发送预算 | 无 | `InferenceAttemptBudget`，**硬 clamp 到 1..=10** |
| 账号临时排除 | 靠冷却间接实现 | 显式 `excluded_ids: HashSet<u64>` |

### 3.3 必须突破的四个限制

| # | 位置 | 限制 | 狂暴需求 |
|---|---|---|---|
| 1 | `provider.rs:12124` `max_retry_attempts` | 默认 3，忽略池大小 | 开启时 = 轮数 × 账号数 × region数 |
| 2 | `provider.rs:10197` | `.min(budget.available_attempts())`，clamp `1..=10` | **最大的坑**：只改 #1 会被静默截断到 10 |
| 3 | `provider.rs:7949` `maybe_exclude_after_transient_failure` | 无备选账号返回 false → 硬失败；账号排除后本请求内永不恢复 | 按轮次重置，改用 `(账号, region)` 已试集合 |
| 4 | `provider.rs:11958` `report_transient_failure_kind` | 429 写全局冷却，冷却账号第 2 轮不可见 | 狂暴模式跳过冷却写入 |

### 3.4 本地 / 外部池边界

外部池从不进入 `call_api_with_retry`（它有独立的 `external_pool/retry_pipeline.rs`，预算枚举 `InferenceAttemptKind::ExternalPool`）。**把改动限制在 `call_api_with_retry` 内，天然保证外部池零影响。**

---

## 四、方案设计

### 4.1 三层结构（沿用 kiro-rs-main 骨架 + 狂暴层）

```
429 到达
 ├─ ① 风控型 429  → 沿用现有 detect_risk_control_error 路径，不狂暴
 ├─ ② 有 Retry-After → 直接返回，遵守上游，不狂暴
 └─ ③ 普通 429/408/5xx
      ├─ 狂暴关闭 → 完全走现有逻辑（零影响）
      └─ 狂暴开启 → 优先换号 → 换 region → 换轮次
                    同轮不 sleep；跨轮用 retry_delay_throttle(1s→8s)
```

**①②③ 的判定顺序与 kiro-rs-main 完全一致**，狂暴模式只在第③类内部生效。

### 4.2 两个正交开关

```
开关 A：端点（region）轮换（普通模式亦可用）
  kiroUpstreamRegionRotationEnabled: false   ← 总开关，默认关
  端点由程序内置，无需配置

开关 B：狂暴模式（默认关闭）
  localBerserkModeEnabled: false
  localBerserkMaxRounds: 1
  localBerserkRoundDelayMs: 1000        跨轮退避，对齐 kiro-rs-main 的 1s 基线
```

可只开 A（普通模式多一层 region 容错）、只开 B（仅换号轮换）、或同时开（完整笛卡尔积）。

**A 只有一个布尔开关，端点列表不暴露给用户**：Kiro/Q 上游只在
`us-east-1` 与 `eu-central-1` 两个端点提供服务（依据 kiro-rs-main
`rest_api_region_candidates`，`src/kiro/token_manager.rs:465`——该项目对
getUsageLimits / ListAvailableModels 就是在这两个端点间做 403 回退）。
让用户填 region 只会引入「填错 → 打到不存在的域名」这一类新故障，
因此端点内置为 `KIRO_ROTATION_REGIONS`，用户只需要决定开或关。

轮换顺序同样沿用 kiro-rs-main 的规则：`eu-central-1` 或任意 `eu-*` 账号以
`eu-central-1` 为主端点、`us-east-1` 为回退；其余账号反之。这样 Enterprise / IdC
账号即使 SSO 区域不是 `us-east-1`，首次尝试也能命中正确端点。

关闭总开关后 `region_at()` 恒返回 `None`、`region_slots()` 恒为 1，
下游所有轮换判断统一退化为「沿用凭据自身的 region」，回退路径只有这一个收敛点。

两个开关**完全正交**：关闭 A 不影响 B 的换号轮换（此时 region 固定为改造前的端点），
关闭 B 不影响 A 在普通模式下的端点轮换重试。四种组合均有单测覆盖。

### 4.3 轮换顺序（优先换号）

```
轮次 1:
  region[0] : acct1 → acct2 → ... → acctN     ← 先把账号轮完
  region[1] : acct1 → acct2 → ... → acctN     ← 再换 region
  ...
       ↓ 一轮全失败，退避 retry_delay_throttle
轮次 2:
  region[0] : acct1 → ...

总尝试上限 = 账号数 N × region数 M × 轮数 R
同轮换号不 sleep；跨轮退避
```

### 4.4 停止条件

任一命中即停：

1. **成功**
2. 账号 × region × 轮数全部试完
3. **不可重试错误**：400 / 客户端校验错误 / 带 `Retry-After` 的 429 / 风控型 429 / 402 配额耗尽 / 账号封禁
4. **下游已提交**（流式已吐字节）
5. 全局超时

### 4.5 关闭时零影响的保证

结构性保证 + 测试锁定：

- 新逻辑全部包在 `if berserk.active()` / `if rotation.active()` 内，默认走原路径；
- `max_retry_attempts` 关闭时原样返回；
- `endpoint_for` 无 region 覆盖时走原有凭据固定逻辑；
- 测试 `berserk_disabled_matches_legacy_behavior_exactly`：相同 mock 账号 + 相同种子，开关关闭时**尝试序列、命中次数、错误信息逐项相等**（对标 `quality_scheduling_disabled_matches_legacy_selection_exactly`）。

---

## 五、配置项设计

沿用 `Config` 的 `#[serde(rename_all = "camelCase")]` + `default_*()` 模式。

| 字段（Rust） | JSON key | 类型 | 默认 | 说明 |
|---|---|---|---|---|
| `kiro_upstream_region_rotation_enabled` | `kiroUpstreamRegionRotationEnabled` | `bool` | `false` | 端点轮换总开关，关 = 使用改造前的端点；端点内置不可配 |
| `local_berserk_mode_enabled` | `localBerserkModeEnabled` | `bool` | `false` | 狂暴模式主开关 |
| `local_berserk_max_rounds` | `localBerserkMaxRounds` | `u32` | `1` | 轮数，校验 `1..=10` |
| `local_berserk_round_delay_ms` | `localBerserkRoundDelayMs` | `u64` | `1000` | 跨轮退避，对齐 kiro-rs-main |

需同步改的 6 处：

1. `src/model/config.rs` — 字段 + `default_*()` + 手写 `impl Default` + 默认值断言测试
2. `src/admin/types.rs` — 响应结构体 + 更新请求结构体（`Option<T>`）
3. `src/admin/service.rs` — 读入响应、`unwrap_or(current)`、**校验**、写回
4. `src/admin/service_tests.rs` — 校验测试
5. `src/anthropic/handlers.rs` — `RequestRuntimeConfig` 透传（对标 `LocalStreamRetryConfig::from_runtime_config`）
6. `src/kiro/provider.rs` — 运行时读取（循环内已有 `runtime_config()` 调用）

**region 格式校验**：复用 `credentials.rs:305` 的 `validate_kiro_region_host_label`，防止域名注入。

---

## 六、实现改动点

| # | 文件:行 | 改动 |
|---|---|---|
| 1 | `src/model/config.rs` | 4 个配置字段 + 默认函数 + Default impl + 断言测试 |
| 2 | `src/kiro/retry_pipeline.rs`（**新建**） | 轮换状态机 `RotationPlan`、`BerserkConfig::from_runtime_config()`、`active()`；纯函数便于单测。对标 `external_pool/retry_pipeline.rs` |
| 3 | `src/kiro/provider.rs:12138` | **移植 kiro-rs-main 的 `retry_delay_throttle`**（1s→8s，429 专用） |
| 4 | `src/kiro/endpoint/mod.rs:280` `RequestContext` | 新增 `region_override: Option<&str>`；`ide.rs`/`cli.rs` 的 `api_region()` 优先读它 |
| 5 | `src/kiro/provider.rs:7371` `endpoint_for` | 增加 `forced_region: Option<&str>` 透传 |
| 6 | `src/kiro/provider.rs:12124` `max_retry_attempts` | 狂暴开启时返回 `rounds × accounts × regions` |
| 7 | `src/kiro/provider.rs:10197` | **狂暴模式豁免 `InferenceAttemptBudget`**（否则静默截断到 10）；保留"下游已提交即停" |
| 8 | `src/kiro/provider.rs:11918` 429 分支 | 在第③类内插入狂暴分支：优先换号 → 换 region → 换轮次；跳过全局冷却写入；同轮不 sleep |
| 9 | `src/kiro/provider.rs:7949` | 狂暴下无备选账号时不直接失败，转而换 region / 下一轮 |
| 10 | `admin-ui/` + `ui/` 两套前端 | 三个开关 + 两个数值必须在管理页面可视化配置（类型、默认值、归一化、表单控件），不能只靠配置文件 |

---

## 七、测试设计

**先单测保证基本正确，再做 mock 账号真实调度测试**；调度测试不搞 1~几并发的无意义规模。

### 7.1 单元测试

| 模块 | 用例 |
|---|---|
| `src/kiro/retry_pipeline.rs` | 轮换序列 N×M×R 顺序正确且"优先换号"；空 region 列表退化为仅换号；轮数边界 0/1/10 |
| `src/model/config.rs` | 默认值断言；camelCase 序列化往返 |
| `src/admin/service_tests.rs` | 轮数越界拒绝；延迟越界拒绝；默认配置通过校验 |
| `src/kiro/provider.rs` | `max_retry_attempts` 开关取值；`retry_delay_throttle` 曲线（1s→8s、封顶、抖动范围）；429 三分法分类 |

### 7.2 Mock 上游真实调度测试

复用 `FakeBadRequestServer`（`provider.rs:1302`：axum + 临时端口 + `AtomicU64` + `Drop` abort），扩展为按 **region（Host/URL）与账号维度**返回不同状态。

沿用既有规模约定 `for pool_size in [1, 20, 60] { for round in 0..5 }`（`provider.rs:5019`）。

| # | 场景 | 断言 |
|---|---|---|
| S1 ✅ | 全账号全 region 429 | 命中 = N×M×R；最终失败；无 panic |
| S2 | 最后一个账号成功 | 恰好轮到即停，不多试 |
| S3 | region[0] 全 429、region[1] 正常 | **先轮完所有账号再换 region**（验证优先换号） |
| S4 | 429 后逐步恢复 | 第 2 轮成功，验证轮数生效 |
| S5 | 带 `Retry-After` 的 429 | **不狂暴**，直接返回并透传头 |
| S6 | 风控型 429（suspicious activity） | 走风控路径，不狂暴 |
| S7 | 混合 429+500+403+400 | 400/403 立即停；429/500 参与轮换 |
| S8 | **开关关闭** | 尝试序列与旧逻辑**逐项相等** |
| S9 | 仅开 region 轮换（普通模式） | 换 region 生效，但不做多轮笛卡尔积 |
| S10 | 狂暴 + 外部池同时配置 | 外部池行为与改动前完全一致 |
| S11 | 流式已提交后失败 | 不重试，避免重复吐字节 |
| S12 | 高并发（≥64 并发 × 多轮） | 无死锁、无预算泄漏、`excluded_ids` 不串请求 |

### 7.2.1 mock 调度测试实际发现的两个缺陷

单测全绿、结构审查也通过，但 mock 上游调度测试仍然抓到两个真实缺陷 ——
这正是「不能只做单元测试」的价值所在：

| # | 缺陷 | 症状 | 修复 |
|---|---|---|---|
| B1 | **狂暴模式被 `InferenceAttemptBudget` 静默截断在 10 次** | 豁免只加在 `max_retries` 上，但每次发送前还要过 `budget.reserve`，那里以预算自身上限为准。12 个账号的池子打满 10 次就报 `local inference routing limit reached`，第二个端点根本轮不到 | 新增 `raise_max_attempts_for_berserk`，把预算上限抬到 `账号×端点×轮数`；`max_attempts` 改为原子字段。只放宽计数上限，`reserve` 的「下游已提交」「为 fallback 预留」两条语义不变（S10/S11 锁定） |
| B2 | **带 `Retry-After` 的 429 也会触发狂暴轮换** | 三分法第 ② 类本应立即返回。但判定只看 `failure_kind == RateLimit`，对所有 429 一律成立；而狂暴又跳过了写冷却 —— 恰恰是唯一会消费 `Retry-After` 的地方，于是上游明确说了「7 秒后再来」还是被打满整个矩阵 | 引入 `berserk_applies = active && RateLimit && retry_after.is_none()`，并把轮换触发、冷却跳过、退避控制三处统一切到该判定 |

> B1 的教训：豁免一个限制时，必须找全该限制的**所有**执行点。
> 当时只审了 `max_retries` 一条路径就下了「结构上正确」的结论。

### 7.3 与既有逻辑的混合测试（不破坏原有逻辑）

- **质量感知调度**：狂暴不写全局冷却，须验证质量曲线不被污染
- **sticky session**：会话粘性账号被 429 后的排除语义不变
- **RPM / 并发容量**：狂暴轮换不得绕过并发 lease
- **流式预提交重试**（`LocalStreamRetryConfig`）：两层重试不得相乘放大

---

## 八、风险与缓解

| 风险 | 缓解 |
|---|---|
| 豁免 `InferenceAttemptBudget` 导致请求无限放大 | 上限严格 = N×M×R，R 校验 ≤10；下游已提交立即停 |
| 狂暴放大上游限流（kiro-rs-main 明确警告过） | 默认关闭；风控型与带 `Retry-After` 的 429 排除在外；跨轮用 1s→8s 长退避 |
| region 与 profileArn 不匹配产生无效请求 | 用户已确认接受；日志标注 region 来源便于排查 |
| 长时间占用并发 lease | 同轮不 sleep，整体可控；保留全局超时 |
| 破坏既有调度逻辑 | S8 零影响锁定 + S10 外部池回归 |

---

## 九、实施顺序

1. ✅ 建分支 `feat/local-berserk-rotation`
2. 配置项（config + admin + 校验 + 单测）
3. 移植 `retry_delay_throttle`（kiro-rs-main 1s→8s 曲线）
4. `src/kiro/retry_pipeline.rs` 轮换状态机（纯函数 + 单测）
5. `RequestContext` region 覆盖 + `endpoint_for` 透传
6. 主循环与 429 第③类分支接入
7. 单元测试补齐
8. Mock 真实调度测试（S1–S12）
9. 清理测试进程与产物，核对内存/磁盘余量
