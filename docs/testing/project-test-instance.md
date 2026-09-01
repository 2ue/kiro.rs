# Project Test Instance Policy

## Normative Rule

本仓库的本地验证、真实账号导入、协议回归、负载测试和故障复现必须使用
**一个项目归属的长期测试实例**（single project-owned validation instance）。
“隔离”表示本项目与其他项目的运行资源边界清晰，不表示每个测试用例都创建
一个新的 `kiro.rs` 服务。

本规则适用于常规功能验证、真实账号调用、协议回归、负载/混沌测试以及问题
复现。测试目标本身若是“多进程/多实例协调、故障接管或跨实例一致性”，可按
该专项测试的明确要求启动额外实例；这不是常规测试默认行为，必须在专项
报告中声明额外实例的端口、配置、存储、生命周期和清理结果。无论是否存在
专项多实例测试，本项目的常规验证实例始终只有下表中的一个。

本项目的指定实例如下：

| Resource | Value |
| --- | --- |
| Service | `kiro.rs` owned by this repository |
| Listen address | `127.0.0.1:19023` |
| Runtime config | `tmp/thinking-budget-local/config.json` |
| PostgreSQL database | `kiro_thinking_budget_20260901` |
| Redis endpoint | `127.0.0.1:26379/0` |
| Evidence/log root | `tmp/thinking-budget-local/` |

凭据文件、API key、refresh token、cookie 和代理密码不属于可复制的测试证据；
文档和报告只能记录脱敏摘要、哈希、账号数量、账号类型和请求/错误 ID。

## Required Execution Semantics

1. 所有 case 复用上述同一个 `kiro.rs` 进程和监听端口。不得为每个 case、
   每轮压测或每个账号批次启动新的 `kiro.rs` 实例。
2. 不得把请求发送到其他项目的服务端口，也不得把其他项目的进程当作本项目
   的测试实例。开始测试前必须核对端口监听 PID、进程命令、配置路径和存储
   归属。
3. 真实账号导入必须直接调用该实例的 Admin API。批量导入应等待服务端完成
   全部条目处理，并记录 `total/success/skipped/failed`；客户端连接超时或
   断开不能直接推断服务端已停止处理，必须再次查询账号总数和审计记录。
4. case 之间的隔离通过唯一 run/case ID、限定查询时间窗、受控并发、顺序执行、
   结果清理和证据目录完成。不得以启动第二个代理服务代替这些控制。
5. 需要重启或换用新构建时，只允许停止并重新启动本项目已核验的指定实例，
   使用相同端口、配置、PostgreSQL、Redis namespace 和日志目录；不得并行
   启动第二个项目实例。
6. fake upstream、fake proxy 和 Claude CLI 的 HOME/config 可以是独立进程或
   临时目录；它们不构成第二个 `kiro.rs` 实例。临时 fake 进程必须在 case
   结束后停止。
7. 对常规测试，只有在某个 case 会不可逆破坏共享测试数据库、Redis 状态或
   账号状态，且无法通过清理/顺序控制完成时，才允许使用临时 `kiro.rs`
   实例。多进程/多实例协调专项不受此条的“常规测试”限制，但必须遵守
   本节前述专项声明和资源清理要求。任何临时实例都必须在测试报告中记录
   原因、端口、配置、存储、启动/停止时间和清理结果。

## Preflight Checklist

执行任何测试前，记录并核验：

- 指定实例健康检查返回成功；
- `lsof -nP -iTCP:19023 -sTCP:LISTEN` 对应的 PID 和命令属于本仓库；
- 进程使用 `tmp/thinking-budget-local/config.json`；
- PostgreSQL 数据库和 Redis namespace 与本项目一致；
- 其他项目的端口（例如 `9022`）未被使用；
- 本轮的 `run_id`、case 数量、并发上限和证据目录已确定。

验证完成后，只清理本轮生成的报告、临时 fake 进程和本轮标记的数据。除非
测试本身验证重启/恢复，否则不要停止指定实例。
