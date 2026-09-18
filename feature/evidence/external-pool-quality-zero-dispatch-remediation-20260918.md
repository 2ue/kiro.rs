# External Pool Quality Zero-Dispatch Remediation Evidence

Date: 2026-09-18 Asia/Shanghai

Scope: `main` worktree at `5313bc1 chore(release): 0.0.163`

This is a local code/test evidence package. It does not contain production
credentials, production Redis keys, raw account data, or upstream response
bodies.

## Findings Confirmed In Code

- `external_pool_quality_aware_scheduling_enabled` had an incompatible default
  of `true`. Rust, UI, and Admin UI defaults now resolve to `false`; explicit
  `true` remains supported.
- Ordinary transient failures no longer escalate through
  `external_pool_transient_failure_cooldown_threshold` into pool hard cooldown.
  The transient streak remains a short-lived ranking signal.
- Quality sample and probation writes use a separate Redis connection manager
  and a bounded semaphore of 32 detached tasks. Saturated optimization work is
  dropped instead of blocking coordinator, lease, or dispatch-fence commands.
- Relative probation is evaluated from the current request's full candidate
  cohort after pool enabled/auto-disable, route admission, model admission,
  cooldown, concurrency, and runtime-coordinator checks.
- The probation baseline uses the cohort window error-rate median. The score
  path retains its EWMA/latency baseline and hard priority-tier semantics.
- When the quality switch is explicitly enabled but no candidate has enough
  samples, selection falls back exactly to the legacy selector.
- A selector invariant violation with `available_pools > 0` and no selected pool
  now returns a bounded synthetic 503 instead of spinning until the dispatch
  deadline.

## Focused Verification

All Cargo commands were run through `feature/tests/run-cargo-scoped.sh`.

| Command scope | Result |
| --- | --- |
| `fmt-small -- cargo fmt --all` | pass |
| `quality-selection-final -- cargo test --bin kiro-rs quality_scheduling --locked` | `15 passed / 0 failed` |
| `quality-relative -- cargo test --bin kiro-rs relative_degradation --locked` | `4 passed / 0 failed` |
| `quality-config -- cargo test --bin kiro-rs quality_config --locked` | `1 passed / 0 failed` |
| `transient-cooldown -- cargo test --bin kiro-rs external_pool_repeated_soft_failures_never_create_pool_cooldown --locked` | `1 passed / 0 failed` |
| `check-quality-fix-final -- cargo check --all-targets --locked` | pass |
| `git diff --check` | pass |

The focused tests cover:

- switch-off exact legacy selection;
- explicit-on cold start with no samples;
- path/model-eligible cohort relative probation;
- window-error-rate baseline rather than a single-account absolute red line;
- all-candidate global degradation without probationing every pool;
- ordinary repeated transient failures without pool cooldown;
- default/API camel-case configuration contract.

## Service-Level Failure Matrix (2026-09-18)

为了验证“外部池仍有流量、质量降级不能把候选集清空、普通失败不能硬冷却”，本轮使用
项目自有的 PostgreSQL/Redis 容器，但为每一批创建了唯一 PostgreSQL database，
Redis 仅使用隔离的 DB14/DB15。上游全部是本地 HTTP fake，不调用真实第三方池，也不消耗
真实 Kiro 配额。每个 case 串行执行，Cargo 均通过 scoped wrapper，避免构建目录和测试状态
互相污染。

### 第一批：错误/恢复与高并发质量调度

15/15 passed, 0 failed:

- `external_pool_repeated_soft_failures_never_create_pool_cooldown`
- `external_pool_mock_error_matrix_limits_repeated_failures_and_preserves_recovery`
- `external_pool_one_wave_all_pools_transient_502_does_not_blackout_recovery`
- `external_pool_one_wave_account_capacity_and_quota_errors_soft_recover`
  （401/403/402/429）
- `external_pool_mock_sporadic_failures_recover_without_long_blackout`
- `external_pool_all_pools_sustained_502_recovers_without_long_blackout`
- `external_pool_high_concurrency_sustained_primary_502_transfers_to_backup_without_cooldown`
- `external_pool_high_concurrency_random_mixed_status_turbulence_transfers_to_healthy_pools`
- `external_pool_high_concurrency_network_turbulence_transfers_to_healthy_pools`
- `l2_quality_disabled_keeps_legacy_priority_routing_under_load`
- `l2_quality_shifts_traffic_to_the_faster_pool_within_one_priority_tier`
- `l2_all_pools_degraded_still_serves_traffic_without_emptying_candidates`
- `l2_single_remaining_pool_is_never_starved_even_when_failing`
- `l2_quality_never_promotes_a_lower_priority_tier_under_load`
- `l2_failing_pool_is_probationed_then_recovers_and_regains_traffic`

其中 L2 质量调度波次使用 `L2_WAVE_SIZE=256`；恢复轨迹测试实际观察了失败池进入
probation、仍获得探测流量、上游恢复后重新获得主流量。该批次没有出现候选池清空或
恢复黑洞。

### 第二批：准入、硬冷却与协议错误边界

8/8 passed, 0 failed:

- `external_pool_static_eligibility_snapshot_singleflights_models_and_body_modes`
- `external_pool_fallback_eligibility_uses_model_not_body_mode`
- `external_pool_model_unavailable_cooldown_is_model_scoped_and_does_not_queue`
- `external_pool_route_mode_is_applied_per_pool_for_selection_and_eligibility`
- `external_pool_atomic_acquire_honors_pool_cooldown_and_fails_closed_on_bad_state`
- `external_pool_not_found_invalid_model_uses_default_cross_pool_retry_status_codes`
- `external_pool_bad_request_model_unavailable_fails_over_without_same_pool_replay`
- `external_pool_retry_after_header_records_soft_failure_without_pool_cooldown`

这些用例验证了：路径和模型准入先于质量比较；模型不可用只对对应模型做 cooldown，
不会把整个池或其它模型一并挡住；池级 hard cooldown 仍然只在显式不可用/准入边界上
生效；普通 `Retry-After`/429 和重复 5xx 只留下软失败调度证据。

### 结果解释

- “正常”：质量关闭时 legacy priority 路由和质量开启时健康池分流均通过。
- “持续异常”：单池、主池、全池持续 502/网络异常均有界结束；恢复后可以重新发送。
- “部分异常”：混合状态码、间歇失败、失败池与健康备池混合均通过，流量转向健康池。
- “突然恢复”：`fail_first` 夹具恢复后，失败池保留探测流量并重新获得份额。
- “部分不可用/全部不可用”：模型/路径/池 cooldown 组合按范围拒绝；普通瞬态错误不会
  在最外层把全部候选变成 hard cooldown。唯一剩余池持续失败时仍继续尝试，确保仍有
  上游命中而不是静默变成 `upstream hit=0`。

本轮是服务级 fake-upstream 证据，不等同于生产复现，也不等同于真实第三方外部池
验证。生产问题仍需用脱敏 coordinator/lease/Redis 延迟和 usage 证据闭环。

## Production/Reproduction Boundary

The reported production symptom remains credible: downstream traffic can
continue while external upstream hit count stays at zero if runtime snapshot,
coordinator, lease acquisition, or dispatch fencing fails before the external
HTTP call. This local pass did not access production and did not reproduce a
stable coordinator/Redis contention wave.

The remaining evidence needed from the affected deployment is limited to
redacted coordinator timeout/breaker events, lease/snapshot latency, Redis
slowlog/command latency, quality task dropped/timeout counters, and the
corresponding PostgreSQL usage rows.

## Resource Hygiene

- No project `kiro-rs`, Cargo, Rust compiler, or fake-upstream process remained
  after the service-level runs.
- `feature/tests/run-cargo-scoped.sh --reap-stale` reported
  `active=0 removed=0 failed=0` after the final cleanup.
- Scoped validation targets were removed; repository `target` was `0B` after
  cleanup.
- Disk availability remained about `26 GiB` after the two compile batches.
- The two temporary PostgreSQL databases used by the service-level batches were
  explicitly dropped; no `kiro_quality_*` database remained.
- The isolated Redis namespaces used for external-pool tests were deleted from
  DB14/DB15 (`6` keys from DB14 and `28` keys from DB15); no
  `kiro_rs:test:external_pool:*` key remained.
- No temporary `kiro-rs` service or long-lived project port was created.
- A Chrome DevTools MCP process tree was observed under the user's existing
  Grok process. It was not treated as a project test child and was left alone
  to avoid interrupting the user's external session.
