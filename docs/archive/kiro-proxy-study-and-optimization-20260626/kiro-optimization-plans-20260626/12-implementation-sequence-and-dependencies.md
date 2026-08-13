# 总实施顺序、依赖与发布门禁

## 适用范围

本方案定义所有优化方案的实施顺序、依赖关系、发布策略、回滚策略和验收门禁。

任何后续开发都必须先确认自己属于哪个阶段，避免把高风险调度改动、UI 重构、错误归一化、缓存试验混在一起发布。

## 总原则

- 先测试工具，后热路径优化。
- 先观测，后策略改变。
- 先默认关闭，后小流量启用。
- 先内部记录，后管理端展示。
- 先保持兼容，后清理历史字段。
- 每个阶段必须可单独回滚。

## 阶段划分

### Phase 0：文档与基线

内容：

- 完成方案文档。
- 记录当前默认配置。
- 记录当前压测基线。

门禁：

- 文档拆分完成。
- 当前代码无业务改动混入。

### Phase 1：压测与异常工具

对应文档：

- `04-loadtest-and-chaos-test-harness.md`

必须先做，因为后续所有调度、stream、缓存改动都需要它验证。

交付：

- fake Kiro server。
- loadtest CLI。
- 报告 JSON。
- 手动真实上游测试说明。

门禁：

- fake server 覆盖 normal、slow、429、500、200 JSON exception、idle、client drop。
- 能采集 TTFB、first thinking、first text、total latency、RSS、FD。

### Phase 2：错误归一化与失败原因

对应文档：

- `02-selection-failure-reasons.md`
- `10-observability-trace-and-error-normalization.md`

先做失败原因和统一错误，是因为后续调度优化需要可解释。

交付：

- `SelectionFailureSummary`。
- `ErrorDiagnostic`。
- 统一英文对外错误。
- request id / error id 全链路。

门禁：

- 对下游不出现内部概念。
- 内部 usage 可查原始错误摘要。
- 高并发错误记录不阻塞接口。

### Phase 3：协议和 stream 稳定性

对应文档：

- `05-tool-use-malformed-regression.md`
- `06-stream-idle-and-upstream-exception.md`

交付：

- payload audit。
- tool-use 回归矩阵。
- stream phase trace。
- idle timeout。
- 200 JSON exception 分类。

门禁：

- Claude Code CLI 长会话、thinking、tool-use 真实测试通过。
- stream 异常路径全部释放 lease。
- thinking delta 真实输出。

### Phase 4：调度模块拆分

对应文档：

- `01-token-manager-module-split.md`

交付：

- `token_manager` 目录模块。
- 对外 API re-export。
- 行为不变测试。

门禁：

- 默认配置选择结果不变。
- Redis key 不变。
- 管理端 snapshot schema 不变。

### Phase 5：调度策略和健康分

对应文档：

- `03-scheduler-strategies-health-score.md`

交付：

- `SchedulerScoreBreakdown`。
- 可选 `weighted_least_inflight`。
- 管理端解释字段。

门禁：

- 默认策略行为不变。
- 新策略默认关闭。
- 压测证明大并发负载更均匀。

### Phase 6：profileArn 与缓存增强

对应文档：

- `07-profile-arn-region-self-heal.md`
- `08-cachepoint-and-cache-normalization.md`

交付：

- profileArn source/state。
- 受控自愈。
- prompt cache entry 上限。
- cache fingerprint normalization。
- 默认关闭的 cachePoint 试验。

门禁：

- 自愈默认不持久化写回。
- cachePoint 默认关闭。
- `/dfcache/*` 安全规则通过测试。
- cache entry 有上限。

### Phase 7：Endpoint failover

对应文档：

- `09-endpoint-failover-policy.md`

交付：

- 默认关闭 failover。
- retryable 分类。
- endpoint attempt trace。

门禁：

- 已输出 stream 不 failover。
- invalid request 不 failover。
- 默认配置行为不变。

### Phase 8：管理端 UI 重构

对应文档：

- `11-admin-account-ux-and-cache-bounds.md`

交付：

- 账号术语统一。
- 配置页重分组。
- 固定顶部 nav。
- 账号卡片重构。
- 真实版本号展示。

门禁：

- 路由点击正常。
- 截图验收通过。
- 无原生 select、confirm、alert。
- 不出现内部晦涩概念。

## 依赖关系

必须遵守：

- Phase 1 是 Phase 3、Phase 5、Phase 6、Phase 7 的前置。
- Phase 2 是 Phase 5 和 Phase 7 的前置。
- Phase 3 是 thinking、tool-use、stream 相关发布的前置。
- Phase 4 必须在 Phase 5 前完成，否则调度策略会继续堆在大文件里。
- Phase 6 的 cachePoint 必须依赖 Phase 1 的真实上游测试。
- Phase 8 可以和后端阶段并行，但不得改业务接口语义。

## 发布策略

每个 Phase 独立发布。

发布前必须：

```bash
cargo test
cargo clippy --all-targets --all-features
```

涉及 UI 时必须：

```bash
npm test
npm run build
```

如果项目 UI 使用其他包管理器，则使用项目现有命令。

涉及热路径时必须运行：

```bash
cargo run --bin kiro_loadtest -- --scenario normal_stream
cargo run --bin kiro_loadtest -- --scenario stream_idle_timeout
cargo run --bin kiro_loadtest -- --scenario recovery_after_burst
```

真实上游测试必须手动加双开关。

## 回滚策略

必须有配置开关：

- `selection_failure_record_enabled`
- `error_diagnostic_enabled`
- `payload_audit_enabled`
- `scheduler_score_breakdown_enabled`
- `scheduler_weighted_least_inflight_enabled`
- `profile_arn_self_heal_enabled`
- `profile_arn_self_heal_write_back_enabled`
- `kiro_cache_point_enabled`
- `kiro_endpoint_failover_enabled`

回滚优先级：

1. 先关闭新功能开关。
2. 再回滚本阶段代码。
3. 不跨阶段回滚无关改动。

## 数据兼容要求

- 新增 usage metadata 字段必须可选。
- 历史 usage 没有 error diagnostics 时页面必须正常显示。
- 配置缺失新字段时必须使用默认值。
- 管理端不得假设新字段一定存在。
- 数据库迁移必须可重复执行。

## 安全要求

不得记录：

- access token。
- refresh token。
- authorization header。
- cookie。
- 完整 prompt。
- 完整 response。
- 完整上游错误 body。

允许记录：

- request id。
- error id。
- status code。
- 原始错误摘要。
- body hash。
- route。
- model。
- account id。
- 延迟指标。

## 验收总标准

最终完整优化后必须满足：

- 正常请求成功。
- stream 请求成功。
- thinking 请求有真实 thinking 输出。
- tool-use 长会话正常。
- `/cc`、`/ha`、`/na` 行为不变。
- `/dfcache/*` 已配置可用，未配置报错。
- 单账号 RPM 生效。
- 单账号并发生效。
- 全局并发生效。
- 上游异常有统一对外错误。
- 内部 usage 保留原始诊断。
- 高并发无明显内存泄漏。
- 管理端术语统一，版本号真实。

## 不得做的事项

- 不得跳过压测直接改调度策略。
- 不得把多个高风险 Phase 合并成一个大版本。
- 不得默认启用 cachePoint、endpoint failover 或新调度策略。
- 不得让 UI 重构修改后端业务语义。
- 不得把内部概念暴露给下游。

## 后续可选扩展

完成所有 Phase 后，可以再评估：

- OpenAI Responses API。
- OTel exporter。
- 管理端 error id 搜索页。
- per-route scheduler strategy。
- 更细粒度 cachePoint 策略。

