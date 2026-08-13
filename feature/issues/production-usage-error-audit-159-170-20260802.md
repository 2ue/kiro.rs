# 159/170 现网 usage 错误审计与体验改进

Status: `read-only-evidence-collected / problem-clusters-recorded / no-new-runtime-fix-selected-yet`

Severity: `P1 pending evidence; promote to P0 only if request interruption or retry storm is confirmed`

Last reviewed: 2026-08-03 Asia/Shanghai

## 范围与目标

审计以下两台现网 `kiro.rs` 服务的 usage 错误日志、部署代码版本和磁盘上的脱敏 JSONL/证据：

- `152.53.243.159`
- `152.53.194.170`

目标是找出真正会打断下游任务、可以通过有限重试、请求处理、调度或诊断改进提升体验的错误类别，并区分：

1. 可以安全自动恢复的瞬时错误；
2. 应由请求处理或模型/工具兼容修复的确定性错误；
3. 必须直接返回给下游的真实请求错误；
4. 证据不足、不能为了“少报错”而修改的现象。

本专题只允许读操作：不重启现网服务、不修改运行配置、不写入 PostgreSQL/Redis、不启用或替换生产凭证、不删除远程证据。

## 用户可见现象与影响

本轮已对两台机器完成只读 evidence pass。两台机器都运行 `ghcr.io/2ue/kiro-rs:0.0.123`，主要错误来自外部池 5xx/400，另有旧版本外部池超长预检和 usage 标准字段旧行为。

关注的体验指标：

- 同一请求是否在首个可恢复失败后被安全重试；
- 是否存在本地凭证失败后没有进入已配置外部池的情况；
- 是否存在外部池已经可用却被本地预检或错误归一化提前拒绝；
- 是否把上下文超限、工具 schema、图片格式等确定性请求错误错误地重试；
- 是否出现 usage 记录成功但客户端失败，或客户端成功但 usage 归一化成错误；
- 错误弹层是否保留真实上游/处理诊断，而不只显示归一化文案。

## 2026-08-03 只读证据结果

证据根目录：

- `tmp/prod-evidence/20260803-025431-usage-audit-159-170/`

本轮没有重启、Compose 写操作、数据库写入、Redis 写入、配置修改或远程文件删除。

部署与版本：

| Host | Image | App status | Port |
| --- | --- | --- | --- |
| `152.53.243.159` | `ghcr.io/2ue/kiro-rs:0.0.123` | healthy | `59137 -> 8990` |
| `152.53.194.170` | `ghcr.io/2ue/kiro-rs:0.0.123` | healthy | `59137 -> 8990` |

6h usage fingerprint:

| Host | Top classes |
| --- | --- |
| `152.53.243.159` | external `server_error` 196; external `bad_request` 82; `network_error` 11; prompt-too-long preflight classes 8 total; `security_lock` 5; `auth_error` 5 |
| `152.53.194.170` | external `server_error` 199; external `bad_request` 121; `auth_error` 5; `client_dropped` 4; `network_error` 3; `rate_limit` 2; prompt-too-long preflight 1 |

已形成四个问题簇：

| Problem | Current interpretation | Treatment |
| --- | --- | --- |
| [P001 external prompt-too-long preflight](../../tmp/prod-evidence/20260803-025431-usage-audit-159-170/problems/P001-external-prompt-too-long-preflight/problem.md) | `v0.0.123` 在发送外部池前按估算输入硬 400；`externalAttempts=0` | 与 [20260801 外部池根因](20260801-production-external-errors-root-cause.md)一致，后续版本已取消此硬预检；等待生产升级后复查 |
| [P002 usage standard fields on v0.0.123](../../tmp/prod-evidence/20260803-025431-usage-audit-159-170/problems/P002-usage-standard-fields-v123/problem.md) | 错误行把 request estimate 放进 downstream-standard 输入字段；成功行 cache projection 接近 1m | 与 [Downstream standard usage field over 1m](downstream-usage-standard-field-over-1m-20260731.md)一致，后续版本已有 focused fix；等待生产升级后复查 |
| [P003 external retryable 5xx exhausted](../../tmp/prod-evidence/20260803-025431-usage-audit-159-170/problems/P003-external-retryable-5xx-exhausted/problem.md) | 已跨两个启用外部池重试，两个池均 502 后返回客户端 502；另有一个正样本证明跨池重试能成功 | 不选盲目增加重试；优先观察外部池健康/容量/cooldown |
| [P004 external 400 diagnostic gap](../../tmp/prod-evidence/20260803-025431-usage-audit-159-170/problems/P004-external-400-bad-request-diagnostics/problem.md) | 400 被正确标为 `retryable=false`，但 usage 只保存归一化错误和低粒度 metadata，缺少 Admin 可用的脱敏上游分类 | 不自动重试；后续若当前版本仍不足，做“真实上游/处理诊断”的脱敏结构化持久化，不泄漏 raw body |

关键样本：

- `152.53.243.159` `req_01QiZxVrczduKL5YzCvGTE2z`：`/cc/v1/messages`，`claude-opus-5`，`错误来源=external_prompt_too_long_preflight`，`externalAttempts=0`，standard `input_tokens=2,793,383`。
- `152.53.194.170` `req_0166sFWMjMccwFCfQza85zzV`：`/ha/v1/messages`，`claude-sonnet-5`，`错误来源=external_prompt_too_long_preflight`，`externalAttempts=0`，standard `input_tokens=1,394,935`。
- `152.53.243.159` `req_01aqAYbzZH6a9r3Ps2Y9GDDA`：external 502，`jinnyapi` 与 `kkkkyue` 两次都 `retry_next`，最终客户端 502。
- `152.53.194.170` `req_01rHLgUX8SLaLB8aHYq3zJN1`：external 502，`kkkkyue` 与 `jinnyapi` 两次都 `retry_next`，最终客户端 502。
- `152.53.243.159` `req_01xHarsYWjan2BxUWPViZvKw` 与 `152.53.194.170` `req_01q8AWWQo5fD1Ja7Ak86bmAg`：external 400，单池尝试，`retryable=false`，没有 `payloadBreakdown` / `payloadGuardReport` / tool-format JSONL 样本。

tool-format debug 配置在两台机器上都是启用且目录为 `logs/tool-format-debug`，但容器内未发现该目录，也没有这些 request id 的磁盘 JSONL 样本。当前源码只在更具体的 request-body/tool-use format 错误路径写该诊断；这些生产行是 generic external 400/502，因此 usage JSONB 是本轮权威证据。

## 当前已知关联材料

- [159/170/142 生产实例运行时卡死](prod-runtime-completion-storage-coupling-159-170-142-20260727.md)
- [2026-08-01 生产外部池两类错误根因补充](20260801-production-external-errors-root-cause.md)
- [本地账号额度耗尽导致 400](local-credential-exhausted-overage-disabled-400-20260731.md)
- [下游标准 usage 单字段超过 1m](downstream-usage-standard-field-over-1m-20260731.md)
- [上游错误诊断隐私与响应体边界](upstream-error-diagnostic-privacy-and-bounds.md)

## 证据采集计划

1. 只读确认两台机器当前部署版本、容器启动时间、服务端口和配置版本标识。
2. 在本地仓库和可访问的脱敏证据目录中按请求 ID、错误 ID、机器地址和日期检索 JSONL。
3. 按页面中文字段聚类 usage 错误，并保留原始上游/处理诊断的脱敏摘要。
4. 将错误与当前代码版本的请求流程、重试分类、凭证调度、外部池和 usage 写入路径逐一对照。
5. 如果本地外部池配置可安全拉取，则在隔离本地服务复制配置；否则使用脱敏固定证据和假上游复现，不调用生产账号。
6. 只有在证据证明能改善下游连续性且不会吞掉确定性错误时，才提出代码改动。

## 最小复现与对照矩阵

待证据采集后补齐，至少包括：

- 本地凭证成功；
- 本地凭证可恢复失败后重试；
- 本地凭证确定性 400；
- 本地无凭证 fallback 到外部池；
- 外部池真实 400/404/429/5xx；
- 上下文超限、工具 schema、图片和 WebSearch 请求错误；
- 流式首输出前失败与已提交输出后的终止错误；
- usage 写入成功/失败与客户端结果的组合。

## 根因判断边界

在没有请求级证据前，不得把“错误率下降”作为唯一成功标准。以下情况原则上不自动重试：

- 请求体、工具 schema、图片格式或上下文窗口确定性错误；
- 外部上游明确返回模型不存在或权限不足；
- 已经产生不可逆副作用的工具调用之后的流式失败。

以下情况才可能进入有限重试候选：

- 首输出前的连接建立、临时 429/5xx、token refresh 瞬时失败；
- 本地调度/Redis/PgSQL 瞬时故障且请求尚未提交上游；
- 本地凭证不可用但外部池配置明确允许 fallback，且外部池容量可用。

## 方案状态

当前不新增运行时代码改动：

- P001/P002 与已记录并后续发布的旧版本缺陷重合，先等待生产从 `v0.0.123` 升级后复查。
- P003 已经执行跨外部池重试；不选择盲目增加即时重试。
- P004 不选择 400 自动重试。真正可提升体验的是 Admin/usage 诊断增强：保存脱敏、低基数、受限的上游错误分类和处理信息，同时继续禁止 raw body 进入客户端错误、普通日志和默认 usage 明细。

如果当前本地/发布版本仍只给出归一化 400 而没有足够 Admin 诊断，后续可把 P004 拆成独立实现项并补 focused test。

## 验收矩阵

- 两台机器的错误类别、数量、时间窗和请求阶段均有脱敏证据。
- 每个候选修复都有代码路径、失败分类和不重试理由。
- 复现优先在隔离服务/假上游完成，不触碰 `9022` 和生产数据。
- 修复若落地，必须通过 focused Rust、CLI/HTTP 行为和相关 UI usage 诊断验证。
- 任何发布前状态都同步更新本问题单、当前问题索引和 plan-tree。

## 修复后结果

本轮是只读审计，不改现网、不发版。已完成问题聚类和处理选择：

- 不把所有报错都强行“解决”；
- 不对确定性 400 盲目重试；
- 不把已跨两个外部池失败的 5xx 再做无证据多次重试；
- 把旧版本已修复类问题留给生产升级后 recurrence check；
- 把 usage 弹层“真实上游/处理诊断”收敛为后端脱敏诊断持久化问题，而不是前端单独展示问题。

## 残余风险与回滚

- 生产证据可能只有归一化错误，无法证明真实上游根因；此时保留现状并补充观测，不强行重试。
- 外部池重试可能增加上游计费或重复副作用，必须以首输出/提交状态为边界。
- 任何改动都应可通过配置关闭或回滚，且不能改变 Raw 透传和确定性 400 的语义。
