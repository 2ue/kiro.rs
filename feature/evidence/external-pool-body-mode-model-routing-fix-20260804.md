# 外部池正文模式/模型路由 focused 验证（2026-08-04）

Status: `focused-verification-complete / production-observation-pending`

Scope: 当前工作树中外部池候选选择、Raw/标准处理 body 处理解耦、Raw 入口重选标准
处理池、默认 `anthropic-version` header，以及 parsed fallback entrypoint 的模型
资格判断。

本轮没有修改生产配置、PostgreSQL、Redis、容器或服务进程；生产 142 的只读现场证据
仍记录在 [142 外部池直连、模型映射与重试证据](external-pool-direct-model-retry-142-20260804.md)。

## 验证结论

当前工作树通过 focused gate：

- 外部池候选选择不再按“请求正文模式”过滤；`模型` 继续参与候选资格和重选。
- `请求正文模式` 只在选中外部池后决定 body 处理：Raw 透传池保留原始 body，
  标准处理池构造标准 `MessagesRequest`。
- Raw 入口保留 `effective_raw_body`，当重选到标准处理池时可以延迟解析并发送
  标准处理 body。
- Raw 池 502 后可以按模型重选到标准处理池，不会因为 Raw/标准处理不同而直接
  得到“外部池不可用”。
- 外部池运行时请求在客户端未提供时默认补
  `anthropic-version: 2023-06-01`；客户端显式传入时保留客户端值。
- `body_mode_filter` 仍作为 route/body-processing hint 保留在请求上下文和诊断
  边界里，但不再决定外部池候选集合。

## 代码覆盖

主要变更面：

- `src/anthropic/handlers.rs`
- `src/anthropic/handlers/tests.rs`
- `src/external_pool.rs`
- `src/external_pool/body_pipeline.rs`
- `src/external_pool/tests.rs`
- `src/storage/postgres.rs`

关键行为锁定：

- `ExternalPoolEligibility` 不再携带 `request_body_mode`；
- PgSQL 加载 eligibility 时仍校验 `request_body_mode` 合法性，但不把它写入候选筛选
  投影；
- `has_cached_eligible_pool_for_model` / `has_eligible_pool_for_model` 等 readiness 路径
  使用模型资格，不使用正文模式资格；
- Raw route 可通过 `effective_raw_body` 为标准处理池构造 body；
- `forward_headers` 默认补 `anthropic-version`，并覆盖 Bearer 与 `x-api-key` 两种
  外部池鉴权方式。

## 命令结果

```text
cargo fmt --all -- --check
=> pass
```

```text
feature/tests/run-cargo-scoped.sh external-pool-body-mode-model-raw-rerun2 -- cargo test --locked raw_route_
=> 2 passed / 0 failed
```

```text
feature/tests/run-cargo-scoped.sh external-pool-body-mode-model-eligibility-rerun2 -- cargo test --locked eligibility
=> 7 passed / 0 failed
```

```text
feature/tests/run-cargo-scoped.sh external-pool-after-body-mode-model -- cargo test --locked external_pool::tests
=> external_pool::tests: 218 passed / 0 failed
```

```text
feature/tests/run-cargo-scoped.sh handlers-model-only-eligibility -- cargo test --locked all_parsed_external_fallback_entrypoints_share_model_only_eligibility
=> 1 passed / 0 failed
```

## 边界

- 本轮没有做生产业务 Messages 真实调用；生产升级后的 recurrence 仍待观察。
- 本轮只关闭 P0：默认 `anthropic-version` 和“选池/body 处理解耦”。可配置池模式
  重试、手动恢复/临时不可调度、候选筛选原因和模型字段展示仍是后续 P1/P2。
- Raw body 只有在能解析为标准 `MessagesRequest` 时才能重选到标准处理池；无法解析
  的非标准 Raw body 仍应只走能接受 Raw 的池，不能强行改成标准处理。
