# 现网 59137 `/dfcache/minea` 本地账号缓存偏低根因分析（2026-10-07）

## 结论

1. 下游看到的命中率按 sub2api 口径计算：`read / (input + read + creation)`（`sub2api/frontend/src/components/charts/TokenUsageTrend.vue`）。creation 越大，命中率越低。
2. 主因在代码里：`current_high_cache` 放大缓存总量时，只放大了总量，没有放大"读"。差额全部算成"写"，所以每一轮都会把约 2 倍上下文重新记成缓存写入（根因 A）。
3. 这些写入很快用完 creation control 的窗口额度。剩余额度按几何级数缩小，于是出现 1、6、179 这样的小写入（根因 B，由 A 引发）。
4. 前缀追踪器只放在进程内存里。重启或换实例后，正在进行的会话会整段重新写入（根因 C）。
5. 现网数据在 10-07 03:30 UTC（北京时间 11:30）有一个明显拐点。在此之前，input 没有被整形（下游 input 等于原始 input），命中率在 19%~43%。之后 input 按 `sample-max 20000` 整形，命中率升到 62%~83%。这个时间点和审计日志里的 `update_runtime_config`（03:29、03:30）重合，但审计 detail 是空的，看不到具体改了什么。sub2api 图表里的 40%~50% 主要来自拐点之前的数据。
6. 修掉 A、B、C 后，按现网 24 小时流量模型估算，流式命中率约 95%，写入只体现增量，不会再出现几十、几个的写入。

## 证据

### 现网（只读查询，库 `kiro_rs_59137`，镜像 0.0.185 / 01bbb97）

24 小时流式 `local_prompt_cache` 共 7832 条：

| 维度 | 命中率 |
| --- | --- |
| 全部流式 | 0.390 |
| 会话首请求（331 条） | 0.042 |
| 后续请求、read=0（543 条，前缀整段失配） | 0.000 |
| 后续请求、read>0 且 write>read/2（2939 条，重复写） | 0.237 |
| 非流式（3347 条，haiku 标题/配额探测，`max_tokens=1`，没有 cache_control） | 0.138，只占 input 的约 4% |

- `(read+write)/raw_in` 中位数是 1.89。也就是说，每次请求的缓存总量接近 2~4 倍原始上下文，其中写入经常占一半以上。
- 写入值分布：1~9 有 199 条，10~99 有 165 条，100~999 有 264 条。读入值几乎没有小值（1~999 只有 4 条）。小值问题集中在写入。
- 按小时看：03:00 之前 `unshaped`（compat == raw）是 100%；04:00 起是 0%，命中率随之从 0.3~0.4 升到 0.62~0.83。
- 拐点之前 opus-5 有 735 条 50 万~100 万的超长上下文。input 原样计入，而缓存总量受 `maxSimulatedInputTokens=600000` 软上限约束，单条最高也只能到约 37%。

### 本地复现（release 构建，现网 runtime config 原样导入，20 个真实账号，claude-sonnet-4.5，Claude Code 2.1.283 走 `/dfcache/minea`）

同一会话 25 次请求，整段命中率 0.699。请求成对交替：

| raw_in | input | read | write | 说明 |
| --- | --- | --- | --- | --- |
| 29769 | 10697 | 43806 | 62959 | 前缀增长：read 约 1 倍上下文，write 约 2 倍上下文 |
| 29836 | 3599 | 112215 | 0 | 写入被 creation control 拦下，转进 input，再经 moveDelta 进 read |

后段写入序列：`75132, 61166, 1808, 0, 179, 6, 0, 1, 0`，就是窗口额度几何衰减的轨迹。

## 根因

### A. 放大只作用在总量，读没有被放大（`src/anthropic/cache.rs` `to_target_ratio_usage`）

- `prompt_cache.rs` `compute_with_bounds` 算出的 `matched_tokens` 基于未放大的上下文（约 1 倍）。
- `CacheAmplification::apply` 把缓存总量放大到 `tokenScale` 倍（minea 是 3.0）。
- `(true, true)` 分支：`read = min(read_未放大, target_cached_已放大)`，`creation = target_cached - read`。
- 结果：只要前缀有一点增长（哪怕几百 token），写入就是"放大后总量 − 未放大读"，约等于 2 倍完整上下文，每轮都重复。真实 Anthropic 每轮写入只有新增部分。
- 引入时间：2026-05-16 `96a9052`、05-25 `4afb3fa`。这不是近期改动，但 `tokenScale` 越大，越显著。

### B. creation control 窗口额度几何衰减，产生极小写入（`src/anthropic/prompt_cache_creation_control.rs`）

- `allowed_creation_tokens` 对窗口剩余额度调用 `jittered_creation_limit(remaining)`，每次只放行剩余的 88%~97%。
- 因为根因 A 每次写约 6 万，`maxCreationTokensPerWindow=600000 / 300s` 在约 8~10 次请求内耗尽。剩余额度依次变成 6 万 → 1.8k → 179 → 6 → 1，原样输出给下游。
- 代码没有下限：真实 Anthropic 不会出现低于模型最小可缓存长度（1024/2048/4096）的非零写入，这里会。
- 被拦下的写入加回 input（`with_allowed_creation`），再由 `moveDeltaToCacheRead` 搬进 read，形成"一次大写、一次大读"的锯齿。

### C. 前缀追踪器只在进程内存（`PromptCacheTracker.entries`、`PromptCacheCreationController.states`）

- 容器在 2026-10-07 07:00:43 UTC 重新启动（日志可见），所有会话前缀丢失。之后每个会话的下一次请求都会落到 `creation_only_usage`，整段记为写入。
- 这是 UI 上看不到的因素：每次发版、重启都会出现一段命中率低谷。多实例部署时也无法共享。
- 543 条"后续请求 read=0"里，除了重启，还有 Claude Code 压缩、子代理切换 system 等真实前缀变化。我没有逐条区分这两类。

### 不是根因的

- 账号、粘性绑定：`stickyBound=false`（换号）和 true 的命中率相近（0.40 对 0.36）。
- 非流式同步请求：token 占比约 4%，排除后命中率几乎不变，和用户观察一致。

## 解决方案（未实施）

| 编号 | 改动 | 预期效果 |
| --- | --- | --- |
| S1 | `to_target_ratio_usage` 的 `(true,true)` 分支按未放大的读占比（`matched / target_tokens`）缩放到放大后的总量，写入只保留放大后的增量 | 写入从约 2 倍上下文降到约 3 倍增量（通常几千到两万）；单此一项，后续请求命中率可到 85% 以上 |
| S2 | creation control：剩余额度低于模型最小可缓存长度（或 `minCreationDeltaTokens`）时直接记为 0 并累计到 pending，不再输出小额；去掉对剩余额度的几何抖动，改为对单次上限抖动 | 不再出现 1~999 的写入 |
| S3 | 追踪器状态写入 Redis（按 scope+fingerprint，TTL 跟随 entryTtl）；或在 scope 失配、但请求含历史 assistant 消息时，把已确认的历史前缀按读处理 | 重启、发版后不再整段重写 |
| S4 | 放大软上限不低于原始上下文：`basis = max(raw, min(raw*scale, cap))` | 100 万级上下文的命中率不再被压在 37% 以内 |

综合 S1~S4，按现网 24 小时流式流量重放估算：命中率约 0.95（`tmp/prod-cache-20261007/quantify.py`，模型假设 input 整形后单条约 3000、读等于上一轮缓存总量）。

需要确认：S1、S2、S4 都会改变下游可见的 cache read/creation 数值。之前定过"不改 usage 计算"的原则，所以实施前需要你明确授权。

## 验证范围与限制

- 本地只测了 claude-sonnet-4.5（Claude Code 多轮 + 工具调用）。haiku-4.5 没有单独跑。opus 系列按要求没有测。
- 现网只做了只读 SQL 和配置拉取，没有改动现网。
- 拐点之前的 input 整形配置无法从审计日志还原，结论只基于 usage 数据本身。
- 没有更早（48 小时以前）"80%~90%"时期的数据，无法确认当时的流量构成是否相同。
- 证据文件：`tmp/prod-cache-20261007/`（`minea_24h.csv`、`local_test_records.csv`、`cache_config_snapshot.json`、分析脚本）。已去掉密钥和凭据。

## 实施结果（v0.0.186）

新增路由级配置 `hitShaping`（UI 名称：命中率与小值整形），对 `current_high_cache`、`kiro_rs_tool`、`stable_segment_cache` 三种策略生效，`no_cache` 不生效。

默认关闭。关闭时下游 usage 和 creation control 都保持原有逻辑，有逐轮对比的回归测试保证（`hit_shaping_disabled_matches_legacy_usage_pipeline_round_by_round`）。现网升级后，在需要的路径上手动开启。

开启后的规则，优先级从高到低：

1. 写入频次：是否写入只由 creation control 决定。被拦下的请求写入为 0，计费差额记进 input，这时 input 可以超过 reportedUsage.input 的上限。creation control 改用稳定窗口：抖动只作用于窗口总额度，部分放行低于 1024 时整笔延后。
2. 不亏损：计费不低于原始用量计费 × `minCostRatio`（默认 1）。
3. 读写上限：读取不超过原始输入 × `maxReadMultiplier`；放行写入时，写入不低于原始输入 × `minCreationRatio`。两项默认都是 0，即不限制。
4. 命中率：按 `targetHitRatio` + `ratioJitter` 分配读取，会话首请求不受约束。
5. 小值：非零读写低于 `minCacheReadTokens` / `minCacheCreationTokens` 时抬到最小值。

| 配置项 | 默认值 |
| --- | --- |
| enabled | false |
| targetHitRatio / ratioJitter | 0.9 / 0.04 |
| minCacheReadTokens / minCacheCreationTokens | 1024 / 1024 |
| minCostRatio | 1 |
| maxReadMultiplier / minCreationRatio | 0 / 0 |

本地真实账号验证（现网 runtime config 原样导入，minea 开启 hitShaping，读取上限 6 倍，最小写入 1 倍）：

| 模型 | 请求数 | 命中率 | 读取最大 | 写入最低 | 1~1023 读写 | 计费 / 原始 |
| --- | --- | --- | --- | --- | --- | --- |
| sonnet-4.5 | 22 | 0.819 | 6.00 倍 | 1.00 倍 | 0 | 2.11 倍（单次最低 1.85） |
| haiku-4.5 | 21 | 0.829 | 6.00 倍 | 1.00 倍 | 0 | 2.02 倍（单次最低 1.28） |

命中率上限约为 读取上限 ÷（读取上限 + 最小写入 + input），想要 0.9 需要相应放宽读取上限或降低最小写入。

这轮验证发生在"写入频次只由 creation control 决定"之前，当时被拦下的请求仍会被计费下限强制写入。改为由 creation control 决定后，只做了单元测试和 handler 级测试，上线开启时需要再观察写入频次。

未覆盖：根因 C（追踪器状态只在内存）没有改；外部池路径没有改。
