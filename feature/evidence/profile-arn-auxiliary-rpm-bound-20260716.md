# ListAvailableProfiles Auxiliary RPM Bound

Status: `focused-pass / unified-candidate-and-multi-instance-gates-pending`

Role: 记录 inference 前 enterprise `profileArn` 自愈造成的 auxiliary RPM 放大、红灯复现、修复边界和聚焦验证。

## 现象与红灯

`call_api_with_retry`、MCP、模型目录和 Admin 单凭据调用都会在真正发送请求前调用 `ensure_profile_arn_for_context`。external IdP 凭据缺少真实 `profileArn` 时，旧实现直接请求 `ListAvailableProfiles`；403、500、空 profiles 或网络失败不会持久化结果，也没有同 credential singleflight 或失败 backoff。

因此同一账号的并发请求会把一个下游 burst 放大成同等数量的辅助 HTTP。这个调用发生在 inference attempt budget reserve 之前，单看 inference ledger 会漏掉它。

先加入真实本地 HTTP fake endpoint，再运行：

```bash
cargo +1.92.0 test concurrent_profile_discovery_for_one_credential_is_singleflight -- --nocapture
```

修复前稳定红灯：16 个同 credential 并发 caller 命中 `ListAvailableProfiles` 16 次，断言期望 1 次，实际为 16。测试使用 `127.0.0.1:0` 的 Axum endpoint 和真实 reqwest transport，不访问官方 Kiro，也不替换 provider 内部函数。

## 根因

- 缺失/无效 `profileArn` 没有按 credential 合并正在进行的 discovery。
- 403 被降级为 fallback、其他错误也被 best-effort 吞掉，但下一请求会立即重复 discovery。
- 数值 credential ID 不能单独作为 negative-cache key；删除后 ID 复用会继承旧状态。
- discovery 在 inference reserve 前执行是正确的分账边界，但此前没有独立计数语义，导致 auxiliary 风暴在 inference RPM 统计中不可见。

## 实现边界

`src/kiro/provider.rs` 现在使用进程内、按 credential 隔离的异步 gate：

- key 为 credential ID 加 SHA-256 认证身份指纹；状态不保存 refresh/access token 明文。
- 优先以 refresh token 建立稳定身份，access token 轮换不会拆分同一 gate；access-token-only 凭据仍能在替换后生成新身份。
- 同 credential 只有 leader 持有 Tokio async mutex 跨 HTTP await；parking_lot 同步锁只用于短状态读写，不跨 await。
- 不同 credential 使用不同 gate，不做 provider 全局串行。
- 失败 backoff 从 5 秒指数增长，最大 60 秒；backoff 到期后下一批仍由一个 leader 探测恢复。
- 成功结果保留 30 秒 handoff，只用于把结果交给 leader 持久化前已取得旧 context 的 waiters；后续正常 context 带真实 ARN，在任何 key/hash/lock/HTTP 之前直接返回。
- 状态表硬上限 2048，新增身份时只淘汰没有活跃引用的 LRU entry；如果所有 entry 都活跃，新的 auxiliary discovery 被抑制并继续既有 fallback，而不是绕过 singleflight 发 HTTP。
- profile ARN 400 分支会清除对应 discovery 状态，避免短成功 handoff 重新注入已确认失效的 ARN。
- `kiro_upstream_base_url` 现在与其他 Kiro endpoint 一样只覆盖 transport destination；逻辑 AWS Host header 仍按 region 生成。

独立计数和日志字段包括 `auxiliary_channel=profile_arn_discovery`、`operation=ListAvailableProfiles`、`auxiliary_attempt`、`inference_attempt_consumed=false`、success/negative/coalesced/backoff/state-capacity 计数。它们不消耗 request-scoped inference budget。backoff suppression 的 debug 日志只在累计次数为 1、2、4、8… 时输出，错误 burst 下保留计数但不产生线性日志写入。

## 聚焦验证

最终聚焦命令：

```bash
cargo +1.92.0 fmt --all
cargo +1.92.0 test 'kiro::provider::tests::' -- --nocapture
```

最后一次 debug 专项运行的可核对 manifest：

```text
base HEAD:          401473ca1649997bdeccf4468e3add1bdb187248
provider diff SHA: d14c08c21520a6e6a7936aa1fb79d3ff9853baeb766174059feeea2af7e94fa8
debug test binary: target/debug/deps/kiro_rs-a8445619ee052375
binary SHA-256:     6bd43c0bfbc8ac85a59fb29d734022de0f3a88305b3f3aee84f52aed10e9b0a4
```

`provider diff SHA` 包含同文件已有的并行 shared-attempt 修改，不代表本专项独占 diff；工作树未冻结，因此这些值只绑定 focused debug 证据，不是 release manifest。

结果：provider 模块 31/31 通过，专项和既有 model discovery、400 分类、MCP、completion 测试一起运行；耗时 23.23 秒，不含编译。

附加静态门禁：`cargo +1.92.0 fmt --all -- --check`、`git diff --check` 和 `cargo +1.92.0 check --tests` 通过。本专项没有新增编译 warning；仓库统一 Clippy baseline 仍失败于 `745 > 711`，涉及多个并行专题及 provider shared-attempt API，不能更新 baseline 掩盖，因此整体发布仍为 NO-GO。早期检查曾记录 `127.0.0.1:9022` PID 未变化；该 listener 探针按当前安全合同不再作为 release 隔离证据。

优化构建复核：`cargo +1.92.0 test --release profile_discovery -- --nocapture` 为 8/8，测试本体 3.39 秒；已有 ARN 的独立 release 测试 5000 次调用为 0.22 秒、0 HTTP、0 state entry。该 0.22 秒还包含每次从内存 token manager 获取 context，约 44 微秒/调用，只能作为 focused 上界，不替代服务级 p95/p99。

| 场景 | 轮次与 caller | 实际 auxiliary HTTP | 结论 |
| --- | --- | --- | --- |
| 同 credential 成功并发 | 5 轮 x 16 | 5，总是每轮 1 | singleflight，所有 waiter 得到 ARN |
| 同 credential 403 | 5 轮 x (16 首波 + 32 短重试) | 5；每轮短重试 0 | negative cache 生效 |
| 同 credential 500 | 5 轮 x (16 首波 + 32 短重试) | 5；每轮短重试 0 | error backoff 生效 |
| 500 后恢复 | 5 轮 x (1 + 8 抑制 + 16 恢复 + 1 快路) | 10；每轮失败/恢复各 1 | backoff 有界且恢复批次重新合并 |
| 1/20/60 credential | 每档 5 轮，每账号 2 caller，共 810 caller | 405，严格每账号每轮 1 | 不同账号并发，不全局串行 |
| ID 删除/复用模拟 | 相同数值 ID、不同认证身份 | 旧身份 1、新身份 1 | 新身份不继承旧 backoff |
| 跨 region ARN 失效清理 | 5 轮，每轮发现、清理、再发现 | 10；每轮 2 | key 不依赖发现后会变化的 ARN region |
| 状态 LRU/硬上限 | 32 个复用 ID 的不同身份，test max=8 | 表长始终 <=8 | 删除/替换残留有界可清理 |
| 所有 entry 活跃时饱和 | max=2，3 个 slow identity 并发 | 2，第三个 0 | 饱和时 fail closed for auxiliary HTTP |
| 已有真实 ARN | 5 轮 x 1000 调用 | 0；状态表始终空 | 正常快路无新锁/hash/HTTP |
| inference 分账 | 5 轮，每轮 profile success + inference 400 | 每轮 auxiliary 1、inference 1 | budget consumed/local=1，不把 auxiliary 算成 inference |

专项总计执行 481 次本地 `ListAvailableProfiles` HTTP，覆盖 403、500、成功、恢复和跨 region 失效清理；其中 1/20/60 矩阵为 405 次。所有 fake server 使用临时端口并在 Drop 时 abort，未使用真实 credential 或官方 endpoint。

## 性能与资源解释

正常已有 ARN 路径的控制流在 fingerprint、状态表和 async gate 之前返回；debug/release 的 5000 次聚焦调用均保持 0 HTTP、0 state entry、全零 auxiliary counter，release 测试本体为 0.22 秒。缺失 ARN 路径增加一次 SHA-256 和 O(1) bounded HashMap lookup；只有同 credential leader 执行网络，waiter 在 per-entry async mutex 排队。状态最多 2048 entry，不创建无界时间戳数组或按账号全表扫描；negative suppression 日志按 2 的幂采样，避免 debug 模式下随请求量线性写日志。

本专项没有用单测冒充 L3/L5：尚未采集统一 release binary 下的 RSS、FD、p95/p99、客户端 retry burst 和 15 分钟 soak。最终候选仍需将该路径放进错误 burst 与恢复负载，确认 Tokio waiter 和日志量在高并发下有界。

## 残余风险与发布判定

- singleflight/backoff 是进程内的。N 个实例可在同一窗口各发 1 次同 credential discovery；需要双实例测试量化，若仍超预算再设计分布式辅助 admission，不能把 Redis 同步热路径直接引入正常 inference。
- 独立原子计数和结构化日志已存在，但 usage 记录尚未持久化 auxiliary channel attribution；生产报表仍不能只靠 usage 重算 discovery/inference 比率。
- state capacity 饱和会保留既有 fallback profile 行为；这是避免辅助 HTTP 风暴的保守策略，不保证缺 ARN 的非流式上游一定接受 fallback。
- fake endpoint 覆盖真实 HTTP 协议和 provider 调用链，但不是官方 external IdP/Kiro 账号验证。
- 当前结果来自 dirty focused source，不绑定统一 release binary SHA。统一候选、真实 CLI、双实例、L3/L5 和生产只读 recurrence 未完成前，本专题不能标记 final pass，也不能据此承诺以后绝不出现其他 auxiliary RPM 放大。
