# 现网错误优化计划

更新时间：2026-06-09 13:05 CST

本文档用于在脱离当前对话的情况下继续执行现网错误分析、修复设计、实现、验证和发布。文档只记录可复用的技术上下文、证据、代码位置、优化目标和执行步骤；不保存 SSH 密码、Admin Key、API Key 等敏感凭证。执行远端查询前，应从安全凭据渠道获取访问凭证。

## 适用范围

本计划覆盖以下问题：

- 外部备用池流式请求在约 180 秒附近断流。
- 本地 Kiro 上游流式请求出现 `upstream stream idle timeout`。
- 外部备用池返回 `429`、`500`、`database is locked`、`Invalid token`、`channel affinity disabled` 等错误时的分类、冷却、切池和自动禁用策略。
- `Context window is full`、`Input is too long`、`CONTENT_LENGTH_EXCEEDS_THRESHOLD` 类请求过大问题。
- `Improperly formed request`、`Invalid message sequence: tool_use and tool_result blocks must be correctly paired and ordered` 类工具序列问题。
- 模型映射和外部池模型透传/映射的可观测性。
- 外部池 usage/cache 整形、成本兜底、输出放大和最终下游上报一致性。
- 使用记录和统计查询在日志量较大时的性能与数据保留策略。

不包含：

- 立即修改现网配置。
- 重启现网服务。
- 清理现网数据。
- 发布新版本。

以上操作必须由后续明确指令触发。

## 现网上下文

已知现网服务：

- 主机：`152.53.194.170`
- 部署目录：`~/docker-compose/kiro-rs-2ue-59137`
- 服务端口：`59137`
- 应用容器：`kiro-rs-2ue-59137-app`
- Postgres 容器：`kiro-rs-2ue-59137-postgres`
- Redis 容器：`kiro-rs-2ue-59137-redis`
- 当前查询到的应用版本：`kiro-rs 0.0.39`
- 镜像：`ghcr.io/2ue/kiro-rs:latest`
- 服务状态：healthy

远端操作约束：

- 只能执行只读查询，除非用户明确要求修改。
- 允许命令示例：`docker compose ps`、`docker logs`、`psql SELECT`、`docker exec ... --version`。
- 禁止默认执行：`docker restart`、`docker compose up -d`、`UPDATE/DELETE/INSERT`、清理日志、改配置、发布、重建容器。

本地仓库上下文：

- 本地路径：`/Users/yuanfeijie/Desktop/procode/kiro.rs`
- 主分支：`main`
- `Cargo.toml` 当前本地版本曾查询为 `0.0.40`
- 本地工作区存在未提交的外部池 output uplift 相关改动；这些改动不等于现网已生效。
- 现网 `0.0.39` 的行为不能直接用本地 dirty 代码解释。

## 只读查询命令

远端查询服务状态：

```bash
cd ~/docker-compose/kiro-rs-2ue-59137
docker compose ps
docker exec kiro-rs-2ue-59137-app /app/kiro-rs --version
```

远端查询最近错误日志：

```bash
docker logs --since 3h --tail 2500 kiro-rs-2ue-59137-app 2>&1 \
  | grep -Ei 'error|Bad Request|timeout|external pool|Input is too long|CONTENT_LENGTH|tool_use|tool_result|model_not_found|database is locked|Improperly|Context window|decoding response body|invalid message sequence|HTML response|Too many requests|429|403|502|503|504' \
  | tail -n 220
```

远端查询使用记录表结构：

```bash
docker exec kiro-rs-2ue-59137-postgres psql \
  -U kiro_rs_59137 \
  -d kiro_rs_59137 \
  -Atc "
select tablename
from pg_tables
where schemaname='public'
order by tablename;

select column_name || ':' || data_type
from information_schema.columns
where table_schema='public'
  and table_name='usage_records'
order by ordinal_position;
"
```

远端查询错误聚合：

```bash
docker exec kiro-rs-2ue-59137-postgres psql \
  -U kiro_rs_59137 \
  -d kiro_rs_59137 \
  -P pager=off \
  -Atc "
select coalesce(status,'?') || '|' || coalesce(error_type,'?') || '|' || count(*)
from usage_records
where created_at < now() - interval '3 hours'
  and created_at >= now() - interval '24 hours'
  and status not in ('success','client_dropped')
  and deleted_at is null
group by status,error_type
order by count(*) desc
limit 30;

select left(coalesce(error_message,error_detail,''), 220)
       || '|' || count(*)
       || '|avg_ms=' || coalesce(round(avg(duration_ms))::text,'')
from usage_records
where created_at < now() - interval '3 hours'
  and created_at >= now() - interval '24 hours'
  and status not in ('success','client_dropped')
  and deleted_at is null
group by left(coalesce(error_message,error_detail,''), 220)
order by count(*) desc
limit 40;
"
```

远端查询外部池错误：

```bash
docker exec kiro-rs-2ue-59137-postgres psql \
  -U kiro_rs_59137 \
  -d kiro_rs_59137 \
  -P pager=off \
  -Atc "
select coalesce(data->>'externalPoolName','?')
       || '|' || coalesce(error_type,'?')
       || '|' || left(coalesce(error_message,''), 180)
       || '|' || count(*)
       || '|avg_ms=' || coalesce(round(avg(duration_ms))::text,'')
from usage_records
where created_at >= now() - interval '24 hours'
  and data::text like '%external%'
  and status not in ('success','client_dropped')
  and deleted_at is null
group by data->>'externalPoolName',error_type,left(coalesce(error_message,''), 180)
order by count(*) desc
limit 30;
"
```

## 已观察到的远端错误证据

远端 `0.0.39` 在 2026-06-09 查询时观察到：

- `external stream read error: error decoding response body`
  - 约 201 次。
  - 平均耗时约 `179772ms`。
  - 几乎贴近 180 秒。
- `upstream stream idle timeout`
  - 约 100 次。
  - 平均耗时约 `212733ms`。
- `external pool request send failed: error sending request for url (http://31.58.226.99:18473/v1/messages)`
  - 约 35 次。
  - 平均耗时约 `81036ms`。
- 外部池 `kkk` 返回：
  - `500 Internal Server Error`
  - `429 Too Many Requests`
  - `model_not_found ... database is locked (SQLITE_BUSY)`
  - `Invalid token`
  - `channel affinity has been disabled`
  - `Context window is full`
  - `Invalid message sequence: tool_use and tool_result blocks must be correctly paired and ordered`
- 本地凭证池存在 429 后换凭证成功的链路，例如 `#109(429)>#115(200)`。
- 远端 payload guard 已经把很多请求裁剪到 `max_bytes=460800` 附近，常见 `final_bytes` 在 `450KB` 左右。
- 当前远端样本中 `claude-opus-4-8` 已正常解析到 `claude-opus-4.8`，不能再用“4.8 一定被打到 4.5”解释当前错误。

## 关键代码位置

外部池固定 180 秒请求超时：

- `src/external_pool.rs`
- 常量：`DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS`
- HTTP client 创建位置：`ExternalPoolManager::new`

外部池流式转发：

- `src/external_pool.rs`
- 函数：`forward_once`
- 流式分支：`response.bytes_stream()` 后的 `futures::stream::unfold`

外部池调度、重试、冷却：

- `src/external_pool.rs`
- 函数：`forward_with_failover`
- 函数：`select_pool`
- 函数：`acquire_pool`
- 函数：`handle_capacity_unavailable`
- 函数：`classify_external_error`
- 函数：`auto_disable_pool_if_configured`

本地凭证 fallback 到外部池：

- `src/anthropic/handlers.rs`
- 函数：`classify_local_error_for_external_fallback`
- 函数：`maybe_forward_external_after_local_error`

payload guard：

- `src/anthropic/payload_guard.rs`
- 函数：`guard_kiro_request`
- 函数：`repair_request`
- 函数：`repair_orphan_tool_results`
- 函数：`remove_unpaired_tool_uses`
- 函数：`trim_oldest_history_unit`

模型映射：

- `src/anthropic/model_capabilities.rs`
- 函数：`resolve_model_with_catalog_mapping_and_mode`
- 函数：`pick_version_equivalent_available`
- 函数：`family_model_candidates`

cache/usage 整形：

- `src/anthropic/cache.rs`
- `src/anthropic/prompt_cache_creation_control.rs`
- `src/external_pool.rs`
- 重点函数：`project_usage_value`
- 重点函数：`external_pool_billing`

使用记录与统计：

- `src/anthropic/usage.rs`
- Postgres 表：
  - `usage_records`
  - `usage_rollup_time_buckets`
  - `usage_rollup_totals`
  - `usage_cache_read_rollup_time_buckets`
  - `usage_cache_read_totals`
  - `usage_credential_cost_summary`

## 优化目标

目标 1：外部池长流式请求不应因为固定 180 秒总请求 timeout 被切断。

目标 2：外部池错误分类要能区分瞬态、限流、鉴权、配置错误、模型维度不可用、池级不可用。

目标 3：payload guard 不应频繁把请求裁到上限边缘，应保留安全余量，减少 `Input is too long` 和接口层 body 限制错误。

目标 4：工具序列错误要有可诊断信息；修复逻辑要保守，不能破坏正常 tool/tool_result 语义。

目标 5：外部池 usage/cache 整形后，最终返回给下游的 usage 与后台成本统计一致，避免只在后台 floor 成本而下游看不到对应 usage。

目标 6：模型映射以同步上游模型列表为第一参照；内置候选只做兜底，页面和日志能显示实际解析路径。

目标 7：使用记录查询和 dashboard 统计不能因日志量大拖慢管理后台，清理历史使用记录后 rollup 统计不应丢失。

## 分阶段优化计划

### 阶段 1：外部池流式 timeout 拆分

问题：

当前外部池使用同一个 `reqwest::Client`，设置了固定 `.timeout(180s)`。这个 timeout 对流式请求是总请求时长限制，不是 idle timeout。远端 `external stream read error: error decoding response body` 平均约 `179772ms`，高度吻合这个限制。

改进方案：

- 新增外部池 timeout 配置：
  - `externalPoolRequestTimeoutSecs`
    - 非流式请求总超时。
    - 默认 `180`。
  - `externalPoolStreamRequestTimeoutSecs`
    - 流式请求总超时。
    - 默认 `0`，表示不限制总时长。
  - `externalPoolStreamIdleTimeoutSecs`
    - 流式空闲超时。
    - 默认 `180`。
  - `externalPoolConnectTimeoutSecs`
    - 建连超时。
    - 默认 `10`。

实现要点：

- 外部池非流式请求继续使用总 timeout。
- 外部池流式请求不能使用固定 180 秒总 timeout。
- 流式读取循环中维护 `last_chunk_at`。
- 如果超过 `externalPoolStreamIdleTimeoutSecs` 没有任何 chunk，则记录 `external_pool_stream_idle_timeout`。
- 如果配置了 `externalPoolStreamRequestTimeoutSecs > 0`，才限制整条流的最大时长。
- usage 记录中区分：
  - `external_pool_request_timeout`
  - `external_pool_stream_idle_timeout`
  - `external_pool_stream_read_error`
  - `external_pool_connect_error`

测试：

- 单元测试：stream 每 30 秒一个 chunk，持续超过 180 秒，不应被总 timeout 切断。
- 单元测试：stream 180 秒无 chunk，应触发 idle timeout。
- 集成测试：外部池 mock server 延迟流式响应，验证日志和 usage status。
- 回归测试：非流式请求仍受 `externalPoolRequestTimeoutSecs` 控制。

风险：

- 如果流式总时长默认无限制，客户端断开时必须正确释放 lease。
- 必须确保 `Drop` 路径释放 Redis lease。

### 阶段 2：外部池错误分类、冷却和自动禁用

问题：

远端外部池返回了多类错误，但当前分类粒度不足：

- `429 Too Many Requests`
- `500 Internal Server Error`
- `database is locked (SQLITE_BUSY)`
- `Invalid token`
- `channel affinity has been disabled`
- `model_not_found`

改进方案：

新增或细化错误类型：

- `external_pool_rate_limit`
  - HTTP 429 或 body 包含 `Too many requests`。
  - 短 cooldown，默认 5-30 秒。
- `external_pool_server_error`
  - HTTP 5xx 或外部池内部 500。
  - 短 cooldown，默认 10-60 秒。
- `external_pool_database_busy`
  - body 包含 `database is locked`、`SQLITE_BUSY`。
  - 瞬态错误，短 cooldown，不自动禁用。
- `external_pool_auth_error`
  - body 包含 `Invalid token`。
  - 可自动禁用。
- `external_pool_channel_disabled`
  - body 包含 `channel affinity has been disabled`。
  - 可自动禁用或长 cooldown。
- `external_pool_model_unavailable`
  - body 包含 `model_not_found` 或 `Failed to get available channel for model`。
  - 不建议禁用整个池；建议记录为“池 + 模型”的短期不可用状态。

新增状态建议：

- Redis 维护 `external_pool:{pool_id}:model:{model}:cooldown`
- TTL 默认：
  - rate limit：`15s`
  - database busy：`20s`
  - model unavailable：`60s`
  - server error：`30s`
  - auth/channel disabled：按自动禁用策略或长 cooldown

实现要点：

- `select_pool` 时同时检查池级 cooldown 和池模型级 cooldown。
- `classify_external_error` 返回更结构化的分类。
- `record_external_failure` 记录具体 `error_type`。
- 多个外部池存在时，当前池瞬态失败后立即排除当前池并尝试下一个池。

测试：

- 外部池 A 返回 429，外部池 B 可用，应切到 B。
- 外部池 A 对 sonnet 返回 `model_not_found`，对 opus 仍可用，不应禁用整个 A。
- 外部池返回 `Invalid token`，达到阈值后自动禁用。
- 只有一个外部池时，429 后按配置 wait/cooldown，不应无限快速重试。

风险：

- `model_not_found` 有时是外部池临时数据库锁造成，不要永久禁用模型。
- 自动禁用策略必须可配置，并在页面明确展示。

### 阶段 3：payload guard 安全余量和触发策略

问题：

远端大量请求被裁剪到 `460800` 附近。例如 `final_bytes=458803`、`460175`、`456774`。这太接近上限，provider 层后续注入字段、压缩差异、endpoint metadata 都可能让最终上游 body 超过限制。

改进方案：

- 保留现有 `payloadGuardMaxBytes`。
- 新增 `payloadGuardSafetyMarginBytes`：
  - 默认 `32768`。
  - 实际目标字节数为 `payloadGuardMaxBytes - payloadGuardSafetyMarginBytes`。
- 或将默认 `payloadGuardMaxBytes` 从 `460800` 调整为更保守的 `430080` / `409600`。
- 页面明确区分：
  - 全局必然执行：协议修复、tool pair 修复、thinking 压缩等。
  - 条件触发：超过 max bytes 才裁剪历史。
  - on-too-long 触发：上游返回 too long 后才重试裁剪一次。

推荐默认值：

- `payloadGuardEnabled=true`
- `payloadGuardMode=preemptive`
- `payloadGuardMaxBytes=460800`
- `payloadGuardSafetyMarginBytes=32768`
- 实际裁剪目标约 `428032`
- `payloadGuardTrimHistory=true`
- 当前 user message 默认不裁剪，除非用户明确开启当前内容裁剪策略。

测试：

- 构造 600KB Kiro request，最终 body 应小于 `428032`。
- 构造只有当前图片或当前 document 超限的请求，默认不删除当前内容，但记录 `still_oversized`。
- on-too-long 模式下首次不裁，收到 too long 后只重试一次。
- 验证 usage record 中 payload diagnostics 正确记录 original/final bytes。

风险：

- 更保守的目标会丢弃更多历史，影响长会话连续性。
- 需要在页面上说明这是 Kiro HTTP payload 限制，不等同于模型 context window。

### 阶段 4：tool_use/tool_result 序列诊断和保守修复

问题：

远端仍出现：

- `Improperly formed request`
- `Invalid message sequence: tool_use and tool_result blocks must be correctly paired and ordered`

当前 Kiro payload guard 已做修复，但不一定覆盖所有 Anthropic 原始消息序列、裁剪后边界、多 agent 插入、MCP 工具调用等场景。

改进方案：

- 新增“消息序列诊断器”，发上游前对转换后的 Kiro request 做最终检查。
- 诊断器记录：
  - 第几个 history entry 出现孤立 tool_result。
  - 第几个 assistant tool_use 没有后续 user tool_result。
  - 当前 user tool_result 是否对应最后 assistant tool_use。
  - 裁剪前后 tool_use/tool_result 数量变化。
- 默认只诊断和保守修复，不激进改写当前合法工具调用。
- 对 Anthropic 原始请求也可增加只读诊断，判断下游传入本身是否不合法。

实现要点：

- 不要把所有 400 都自动改写。
- 不要随意删除当前 user 的合法 tool_result。
- 不要把当前工具结果文本化后继续保留 tool_result。
- 如果修复后仍可能非法，日志和 usage detail 必须能显示诊断原因。

测试：

- assistant tool_use + user tool_result 成对，不能被误删。
- 裁剪掉 assistant 后，孤立 user tool_result 应文本化或移除。
- 裁剪掉 user result 后，前一个 assistant tool_use 应被移除。
- 多个 tool_use、多 MCP、多 agent 历史交错场景。
- 当前 user tool_result 对应最后 assistant tool_use 的场景。

风险：

- 过度修复会破坏 Claude Code CLI 工具调用连续性。
- 必须优先保证合法工具链路不被改坏。

### 阶段 5：外部池 usage/cache 整形和成本一致性

问题：

远端 usage 样本显示当前存在：

- `rawUsage`
- `reportedUsage`
- `rawCostUsd`
- `reportedCostUsd`
- `billableCostUsd`
- `costFloorApplied`

样本中出现：

- `rawCostUsd=0.13234875`
- `reportedCostUsd=0.12119575`
- `billableCostUsd=0.13234875`
- `costFloorApplied=true`

这说明后台账务把 billable cost floor 到 raw cost，但如果下游响应里的 usage 仍是 `reportedUsage`，则“后台计费不亏”和“下游看到的 usage”不一致。用户需求是最终上报给调用方的 usage 本身就应经过整形和放大，使按系统价格计算后不长期亏本。

目标行为：

- `pass_through`
  - 完全透传外部池响应。
  - 不改 usage。
  - 不做 cache 整形。
  - 不做 output uplift。
- `current_path_policy`
  - 把外部池当作普通本地凭证一样，根据当前请求路径走本地缓存模拟规则。
  - 忽略外部池本身返回的 cache read/create。
  - 先生成 shaped usage。
  - 对 cache read/create 做约 25% 放大。
  - 如果 output_tokens 超过配置阈值，再对 output 做放大。
  - 最终返回给下游的是放大后的 usage。

配置设计：

全局 usage 输出放大：

```json
{
  "reportedUsage": {
    "outputUplift": {
      "enabled": false,
      "minTokens": 1000,
      "percent": 25
    }
  }
}
```

外部池覆盖策略：

```json
{
  "externalPools": {
    "externalPoolOutputUpliftMode": "inherit",
    "externalPoolOutputUpliftMinTokens": 0,
    "externalPoolOutputUpliftPercent": 0
  }
}
```

模式：

- `inherit`
  - 外部池使用全局 output uplift。
- `disabled`
  - 外部池禁用 output uplift。
- `override`
  - 外部池使用自己的阈值和百分比。

必须避免：

- 同一请求被全局和外部池重复 uplift。
- `pass_through` 被全局 output uplift 影响。
- 后台 `billableCostUsd` 和响应给下游的 usage 不一致。

记录字段建议：

- `rawUsage`
- `rawCostUsd`
- `shapedUsage`
- `shapedCostUsd`
- `upliftedUsage`
- `upliftedCostUsd`
- `reportedUsage`
- `reportedCostUsd`
- `profitUsd = reportedCostUsd - rawCostUsd`
- `usageProjectionMode`
- `outputUpliftApplied`
- `cacheUpliftPercent`

测试：

- 外部池 pass-through：响应 usage 与外部池原始 usage 完全一致。
- 外部池 current-path-policy：忽略外部池 cache，按本地策略生成 cache。
- current-path-policy + cache uplift 25%：cache read/create 被放大。
- output 小于阈值：不放大 output。
- output 大于阈值：只放大 output，不改 input/cache。
- inherit/disabled/override 不重复执行。
- 页面利润计算等于 `reportedCostUsd - rawCostUsd`。

风险：

- output uplift 会使下游看到更高输出 token，必须在配置中明确。
- 如果外部池真实成本高于系统同步价格估算，仍可能亏；需要成本分析页面持续观测。

### 阶段 6：模型映射可观测性和外部池模型策略

问题：

现网样本已显示 `claude-opus-4-8 -> claude-opus-4.8` 正常，但仍需要避免未来模型升级时依赖硬编码。

目标：

- 上游同步模型列表是第一参照。
- 精确匹配优先。
- 没有精确匹配时，版本等价匹配，例如 `4-8` 与 `4.8`。
- 再没有时走显式映射规则。
- 最后才走 family fallback。
- 如果配置关闭映射且无规则，则透传模型。

改进方案：

- Admin 增加模型解析预览接口：
  - 输入：请求模型。
  - 输出：上游模型、命中来源、命中规则、是否透传。
- 使用记录中继续记录：
  - 请求模型。
  - 上游模型。
  - 解析来源。
  - 解析说明。
- 外部池模型策略单独配置：
  - 默认透传用户请求模型。
  - 可选使用本地模型映射。
  - 可选使用外部池专属模型映射。

测试：

- 同步模型列表包含 `claude-opus-4.8` 时，`claude-opus-4-8` 应映射到 `claude-opus-4.8`。
- 请求本身是 `claude-opus-4.8` 时，应精确匹配，不二次改写。
- 同步模型列表为空时，只允许 seed fallback。
- 关闭 mapping 时，未知模型直接透传。

风险：

- 外部池可能支持的模型集合不同于 Kiro 本地池，不能强制共用本地映射。

### 阶段 7：使用记录、统计和清理

问题：

使用记录多会导致页面慢；用户也希望偶尔手动分批清理旧记录，同时顶部统计不丢失。

现状：

远端数据库已有 rollup 表：

- `usage_rollup_time_buckets`
- `usage_rollup_totals`
- `usage_cache_read_rollup_time_buckets`
- `usage_cache_read_totals`
- `usage_credential_cost_summary`

优化方向：

- Dashboard 聚合优先查 rollup 表。
- 使用记录列表必须分页，并按 `created_at desc` 使用索引。
- 下拉筛选账号/外部池时，列表接口不要全表扫。
- 清理历史记录前确认 rollup 已包含这些记录。
- 清理采用分批 soft delete 或 hard delete 前，应保证统计不丢。

推荐清理页面能力：

- 选择清理范围：
  - 7 天前
  - 30 天前
  - 90 天前
  - 自定义时间
- 显示将清理的数量，不要求用户填写未知批次数。
- 点击开始后按批次清理，例如每批 `1000` 条。
- 页面展示进度：
  - 已清理数量。
  - 剩余估算。
  - 当前批耗时。
- 可停止。

测试：

- 清理前后 dashboard 总量不变。
- 使用记录列表减少。
- 查询耗时下降。
- 清理过程中服务请求不受影响。

风险：

- 如果 rollup 写入有漏记，清理后统计会丢。
- 清理任务必须低优先级、分批、可暂停。

## 推荐默认配置

payload guard：

```json
{
  "payloadGuardEnabled": true,
  "payloadGuardMode": "preemptive",
  "payloadGuardMaxBytes": 460800,
  "payloadGuardSafetyMarginBytes": 32768,
  "payloadGuardTrimHistory": true
}
```

外部池调度：

```json
{
  "externalPoolsEnabled": true,
  "externalPoolCapacityMode": "wait",
  "externalPoolDispatchMaxWaitSecs": 30,
  "externalPoolRetryMaxAttempts": 0,
  "externalPoolGlobalMaxConcurrentRequests": 0,
  "externalPoolMaxQueuedRequests": 30
}
```

外部池 timeout：

```json
{
  "externalPoolRequestTimeoutSecs": 180,
  "externalPoolStreamRequestTimeoutSecs": 0,
  "externalPoolStreamIdleTimeoutSecs": 180
}
```

说明：

- `externalPoolRequestTimeoutSecs` 只作用于非流式外部池请求；`0` 表示不限制总时长。
- `externalPoolStreamRequestTimeoutSecs` 只作用于流式外部池请求的总时长；默认 `0`，避免长流式请求被固定 180 秒总 timeout 截断。
- `externalPoolStreamIdleTimeoutSecs` 作用于流式外部池请求的空闲时间；默认 `180`，超过该时间没有任何 chunk 才中断。
- 当前实现没有单独的 `externalPoolConnectTimeoutSecs` 字段。

外部池 usage 整形：

```json
{
  "externalPoolUsageProjectionMode": "current_path_policy",
  "externalPoolUsageProjectionUpliftPercent": 25,
  "externalPoolOutputUpliftMode": "inherit"
}
```

全局 output uplift：

```json
{
  "reportedUsage": {
    "outputUplift": {
      "enabled": false,
      "minTokens": 1000,
      "percent": 25
    }
  }
}
```

说明：

- 默认不建议开启全局 output uplift，避免影响本地凭证正常 usage。
- 外部池如需成本补偿，优先在外部池 current-path-policy 下配置 inherit/override。
- pass-through 外部池必须保持完全透传。

## 实施顺序

建议按以下顺序实现，避免一次改动过大：

1. 外部池 timeout 拆分。
2. 外部池错误分类和池模型级 cooldown。
3. payload guard safety margin。
4. tool_use/tool_result 诊断增强。
5. 外部池 usage/cache 整形与 output uplift 两层配置。
6. 模型解析预览和外部池模型策略。
7. 使用记录清理和 rollup 查询确认。

每个阶段完成后必须：

- 更新对应文档。
- 增加单元测试。
- 跑完整测试。
- 本地真实请求验证。
- 确认没有影响无外部池场景。
- 确认没有影响普通本地凭证调度。

## 本地验证命令

Rust：

```bash
cargo fmt
cargo test --locked --no-default-features
cargo check --locked --no-default-features
```

前端：

```bash
node tools/check-admin-ui-api-parity.mjs
pnpm --dir admin-ui build
pnpm --dir admin-ui-daisy build
```

如果 macOS 链接器环境需要显式指定：

```bash
CC=/usr/bin/clang \
CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/clang \
cargo test --locked --no-default-features
```

真实请求验证建议：

- 使用 `ccman` 切换到本地服务。
- 模型优先用 `sonnet`，因为本地凭证池可能只支持少量 Claude Code 系列模型。
- 覆盖：
  - 单轮普通请求。
  - 多轮连续会话。
  - 长会话。
  - tools 调用。
  - MCP 调用。
  - agent 场景。
  - 外部池 fallback。
  - 外部池 pass-through。
  - 外部池 current-path-policy。
  - 请求过大触发 payload guard。
  - 工具序列异常诊断。

## 发布前检查清单

- `git status` 中只包含本次目标相关文件。
- 不提交敏感凭证。
- 不把现网 SSH 密码/Admin Key 写入文档或配置。
- 无外部池配置时，本地凭证池请求仍正常。
- 外部池关闭时，不影响本地调度。
- 外部池 pass-through 时，响应 body 和 usage 不被改写。
- 外部池 current-path-policy 时，usage 整形和后台记录一致。
- payload guard 开关、模式、阈值、余量在新旧 UI 均能显示和保存。
- dashboard/useage 自动刷新默认关闭，localStorage 持久化不影响请求调度。
- 使用记录查询走分页和索引。
- 发版后给出版本号、tag、变更摘要、测试结果。

## 现网升级建议

在实现并发布新版本后，现网升级应分两步：

第一步，只升级服务并保持大部分策略默认值：

- 开启外部池 stream idle timeout。
- 不默认开启全局 output uplift。
- 不默认改变 pass-through 外部池 usage。
- payload guard 使用安全余量。

第二步，观察 1-2 小时使用记录和错误：

- `external stream read error` 是否下降。
- `external_pool_request_timeout` 与 `external_pool_stream_idle_timeout` 是否可区分。
- `Context window is full` 是否下降。
- `Improperly formed request` 是否有明确诊断。
- 外部池 `rawCost`、`reportedCost`、`profit` 是否符合预期。

如果观察正常，再开启或调整：

- 外部池 current-path-policy。
- cache uplift 25%。
- output uplift 阈值和百分比。
- 池模型级 cooldown。
- 自动禁用策略。

## 需要避免的错误做法

- 不要把 `1M context` 理解成可以无限发送 1MB 以上 JSON body。模型上下文和 Kiro HTTP payload 限制是两回事。
- 不要对所有 400 都 fallback 外部池。请求格式错误、工具序列错误、schema 错误、上下文过长都不应该盲目 fallback。
- 不要为了成本补偿只改后台 `billableCostUsd`，而不改最终下游 usage。否则页面账务和调用方看到的数据不一致。
- 不要让 pass-through 外部池被全局整形影响。
- 不要把外部池 `model_not_found` 直接等同于整个池不可用。
- 不要在没有明确用户指令时改现网配置、重启服务或清理数据。

## 验收标准

阶段 1 验收：

- 外部池流式请求超过 180 秒但持续有 chunk 时不再被固定总 timeout 切断。
- 仍能在空闲超过配置值时触发 idle timeout。
- 后台配置和备用号池 tab 都能保存 `externalPoolRequestTimeoutSecs`、`externalPoolStreamRequestTimeoutSecs`、`externalPoolStreamIdleTimeoutSecs`。
- `payloadGuardSafetyMarginBytes` 会把实际裁剪目标从 `payloadGuardMaxBytes` 中扣除；`payloadGuardMaxBytes=0` 时仍然不做大小限制。

阶段 2 验收：

- 外部池 `429`、`database is locked`、`Invalid token`、`channel affinity disabled` 在 usage 记录中有不同 error_type。
- 多外部池时，单池瞬态失败会尝试其他池。

阶段 3 验收：

- 大请求最终 body 与上限之间有安全余量。
- `Input is too long` / `Context window is full` 频率下降。

阶段 4 验收：

- `Improperly formed request` 类错误能在日志中看到具体消息序列诊断。
- 正常工具调用不被破坏。

阶段 5 验收：

- 外部池 pass-through 完全透传。
- 外部池 current-path-policy 下，最终下游 usage、后台 reported usage、reported cost 一致。
- 利润计算明确为 `reportedCostUsd - rawCostUsd`。

阶段 6 验收：

- `claude-opus-4-8`、`claude-opus-4.8`、未来 `4-9/4.9` 能按同步模型列表优先解析。
- 页面可预览模型解析结果。

阶段 7 验收：

- 使用记录列表分页查询稳定。
- dashboard 统计不依赖全量扫描 `usage_records`。
- 清理旧记录不丢失 rollup 统计。
