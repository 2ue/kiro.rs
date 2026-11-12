# 端点轮换与版本生命周期发布门禁 2026-11-12

- 候选提交：`52f354b8`；发布版 `kiro-rs` sha256 前缀 `3ebe35900c5e0598`，`kiro_loadtest` 前缀 `51216aa80f0ea46e`。
- 全量测试：native-tls 与 rustls 各 3046 + 31 通过、0 失败（带真实 PostgreSQL/Redis 存储测试）；时间敏感 10/10；clippy 基线通过；`ui` 与 `admin-ui` 构建通过。

## L3（fake upstream，冻结发布版二进制）：passed=True

| 场景 | 状态码 | TTFB p95 ms | 总耗时 p95 ms | RSS 起/止 MB | FD 起/止 |
| --- | --- | --- | --- | --- | --- |
| l3_normal_c1_r5 | {"200": 5} | 14 | 15 | 33/43 | 42/42 |
| l3_normal_c5_r20 | {"200": 20} | 26 | 26 | 43/48 | 41/46 |
| l3_normal_c10_r50 | {"200": 50} | 25 | 25 | 67/75 | 41/51 |
| l3_spike_c40_r100 | {"200": 100} | 37 | 38 | 75/91 | 41/81 |
| l3_recovery_after_spike_c3_r10 | {"200": 10} | 8 | 9 | 92/92 | 41/44 |
| l3_recovery_after_error_burst_c12_r40 | {"200": 40} | 252 | 252 | 33/77 | 42/53 |
| l3_post_error_recovery_normal_c3_r12 | {"200": 12} | 29 | 29 | 77/78 | 41/44 |
| l3_invalid_tool_burst_c20_r40 | {"400": 40} | 47 | 47 | 33/64 | 42/61 |
| l3_invalid_tool_recovery_normal_c3_r12 | {"200": 12} | 16 | 16 | 79/80 | 41/45 |

## L4（fake upstream，冻结发布版二进制）：passed=True

| 场景 | 状态码 | TTFB p95 ms | 总耗时 p95 ms | RSS 起/止 MB | FD 起/止 |
| --- | --- | --- | --- | --- | --- |
| l4_proxy_restart_during_long_stream | {"200": 8, "transport_error": 72} | 537 | 4633 | 33/71 | 42/38 |
| l4_proxy_restart_recovery_normal_c3_r12 | {"200": 12} | 22 | 22 | 49/59 | 40/43 |
| l4_rate_limit_burst_c20_r40 | {"429": 40} | 739 | 739 | 33/80 | 42/61 |
| l4_rate_limit_recovery_normal_c3_r12 | {"200": 12} | 19 | 19 | 80/82 | 41/44 |
| l4_server_error_burst_c20_r40 | {"429": 19, "502": 21} | 728 | 728 | 33/82 | 42/61 |
| l4_server_error_recovery_normal_c3_r12 | {"200": 12} | 17 | 17 | 82/83 | 41/44 |
| l4_invalid_tool_burst_c20_r40 | {"400": 40} | 22 | 22 | 33/63 | 42/61 |
| l4_invalid_tool_recovery_normal_c3_r12 | {"200": 12} | 23 | 23 | 78/79 | 41/44 |
| l4_client_drop_c20_r40 | {"200": 40} | 26 | 26 | 32/66 | 42/41 |
| l4_client_drop_recovery_normal_c3_r12 | {"200": 12} | 7 | 7 | 67/67 | 41/44 |
| l4_mixed_chaos_c24_r96 | {"200": 47, "429": 46, "502": 3} | 8271 | 8271 | 33/67 | 42/64 |
| l4_mixed_chaos_recovery_normal_c3_r12 | {"200": 12} | 9 | 9 | 73/76 | 40/43 |

## L2 真实上游低并发（项目根目录 FREE 账号，8 个启用，只用 claude-haiku-4.5）

| 场景 | 状态码 | TTFB p95 ms | 总耗时 p95 ms |
| --- | --- | --- | --- |
| v1-stream | {"200": 10} | 2896 | 3150 |
| v1-non-stream | {"200": 10} | 1049 | 1049 |
| cc-stream | {"200": 10} | 1267 | 1267 |
| thinking-stream | {"200": 6} | 1037 | 1137 |
| tool-use-stream | {"200": 6} | 1123 | 1123 |
| alias-model | {"200": 4} | 762 | 947 |
| invalid-model | {"503": 3} | 3 | 3 |
| dfcache-missing | {"404": 3} | 1 | 1 |
| burst-c6 | {"200": 8, "429": 16} | 758 | 841 |
| post-burst | {"200": 6} | 1603 | 1779 |

说明：
- `burst-c6` 的 429 来自本地准入 RPM（requestAdmission.rpm=40）；首次 post-burst 仍在同一分钟窗口被准入拒绝，窗口过后复测 6/6 成功。
- `invalid-model` 返回 503（没有账号支持该模型），`/dfcache/not-configured` 返回 404。
- WebSearch 端到端 3/3 返回 10 条结果；Profile 补全对已有值的账号跳过；账号信息刷新 5/5；版本 pinned 1.4.154 保持；启用账号后 3 秒内模型目录重新同步。
- 测试结束后启用的 8 个账号已禁用回去（240 个全部 disabled），压测数据库与 Redis 测试键已清理，原始报告已删除。
