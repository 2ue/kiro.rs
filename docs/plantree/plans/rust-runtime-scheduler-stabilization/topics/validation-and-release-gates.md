# 未完成验证与发版门禁

Last reviewed: 2026-07-28 Asia/Shanghai

## 当前已完成的本轮验证

已完成：

- 外部池 cached/no-wait gate 新增真实 PgSQL/Redis 集成测试。
- handler fallback、raw external、preflight rescue 防回环回归。
- `cargo fmt --check`。
- `cargo check --all-targets --locked`。
- clippy baseline：`811 <= 849`。
- `git diff --check`。
- build artifact inventory gate：`targets=0 reservations=0 target_processes=0 blockers=0`。

未完成项如下。

## P0-1：外部池/本地凭证 load + chaos 矩阵

需要覆盖：

- 外部池关闭，本地账号正常。
- 外部池开启 + 直连。
- 外部池开启 + 非直连 + 本地账号正常。
- 外部池开启 + 非直连 + 本地账号容量满。
- 外部池开启 + 非直连 + 本地 RPM 满。
- 外部池开启 + 非直连 + 本地 Redis scheduler degraded。
- 外部池开启 + 非直连 + 无本地账号。
- 外部池一个可用、多个可用、全部满、全部冷却、坏配置、模型不支持。
- 外部池 429 burst、500 burst、protocol error、timeout、network error。
- 外部池错误后 local rescue，确认不回环。
- 大量客户端断开、长流集中结束、慢首字、慢 thinking。

验收：

- 正常请求在错误 burst 后能恢复。
- 本地 ready 请求不因外部池 PgSQL/Redis 慢而阻塞。
- RSS/FD/连接数在流量停止后回落。
- usage 中 routeKind/routeSubtype/attempts 能解释行为。
- 内部 upstream sends 不因 fallback/rescue 无界放大。

## P0-2：真实 Claude Code CLI 回归

需要覆盖：

- `/cc/v1/messages` stream。
- `/v1/messages` stream/non-stream。
- 多轮长会话。
- tools。
- MCP。
- WebSearch/MCP auxiliary。
- thinking 主动/被动触发。
- payload guard 触发前后。
- 外部池开启/关闭/直连/非直连。

验收：

- Claude Code CLI 不出现协议错误。
- thinking signature 不出现误删/重排/strip-all 导致的异常。
- tool_result/tool_use 历史不被污染。
- usage 输入/输出/cache/费用符合配置预期。

## P0-3：真实上游低并发安全验证

限制：

- 不对生产压测。
- 真实上游必须低并发、小样本、硬上限。
- 不打印凭据。

需要覆盖：

- 正常 stream。
- 正常 non-stream。
- thinking stream。
- tool-use stream。
- invalid model/request。
- 外部池 1 个可用池的真实 fallback。

验收：

- 请求成功和失败都能正常记录 usage。
- 错误响应不泄露内部 pool/credential/private scheduler 细节。
- 外部池失败不会污染本地凭证状态。

## P0-4：发布前质量门禁

每次发版前至少需要：

- `cargo fmt --check`
- `cargo check --all-targets --locked`
- clippy baseline
- 相关 Rust filter 测试
- 真实 PgSQL/Redis 集成测试（若改调度/存储）
- build artifact inventory gate
- 前端 build/typecheck（若改 UI）
- 根据改动决定是否跑 Claude Code CLI 真实回归

验收：

- 所有 scoped target 清理完成。
- 没有本地/远端 tag 误判。
- 发布版本号以真实已发布镜像/真实 release 状态为准，不只看 tag。
- CI 中重复质量检查需要去重或明确职责，避免两个 workflow 重复跑同一慢门禁。

## P1：dashboard/usage 验证

需要覆盖：

- dashboard 分接口加载。
- 查询慢不能阻断主业务。
- 时间范围影响所有应联动指标。
- 本地成本、外部池成本、实际计费、Kiro 积分消耗同时展示。
- 账号质量维度：成功率、错误率、费用、TTFB、冷却、封控风险。

验收：

- 数据多时 dashboard 可以慢，但不能影响 `/v1/messages`、`/cc/v1/messages`。
- 慢查询要返回明确 busy/partial 状态，不要整个页面一起失败。
- usage 聚合和主业务 Redis/PgSQL 热路径隔离。
