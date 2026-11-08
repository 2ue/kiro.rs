# cctest 调优阶段总结（截至 Round 103）

更新时间：2026-11-08（北京时间 13:56；证据时间 UTC 05:56）

## 当前结论

目标仍未完成。正确模型条件下当前最高分为 `82/110`，稳定失败项仍为 `stream_structure=5`、`websearch=0`、`signature_proto=5`。Round 75 恢复 thinking 起始块空 `signature`，Round 76 完整抓包并逐请求核对 13 个真实 Kiro signature，评分均保持 82。Round 43 临时打开 identityTurnReminder 后降为 64，回滚后非流分数恢复。59139 路由配置保持原值。

| 检测项 | 原始配置分数 | 已确认边界 |
|---|---:|---|
| 流结构 | 5/10 | 公开 SSE 完整；thinking start、estimated_tokens、keepalive 改动均未改善分数 |
| WebSearch | 0/10 | tool/result、caller、query delta、citations、usage 和终止事件完整；opaque 字段没有官方生成算法 |
| 结构化输出 | Round 50: 0；Round 51–54: 10 | 同一条件式 identity/platform 工具描述连续四轮完整测试得 10；对应请求均返回 `identity_platform=claude_code`，实现未硬编码枚举值。总体目标仍未满足 |
| 签名 | 5/10 | 真实 Kiro opaque signature 原样保留；不能离线生成 verifier 接受的签名 |
| 非流结构 | 10/10 | 开启 identityTurnReminder 会使非流响应增加 thinking block 并导致 0；原配置下恢复 10 |

Round 75–76 证据：[`round-75.md`](rounds/round-75.md)、[`round-76.md`](rounds/round-76.md)、[`issues.md`](analysis/issues.md)。

## Round 40–44

- Round 40 去掉 thinking start 空 signature；公开协议形状改善，评分仍 73。
- Round 41 完整抓包含 23 个完整客户端流、26 个 HTTP 200 上游响应，四个失败分项不变。
- Round 42 仅开启 identityPrompt，注入确实到达 Kiro，但总分仍 73。复核纠正了 Round 07 的身份值：它实际返回 `claude_code` 并得 10；Round 42 返回 `kiro` 并得 0。
- Round 43 仅开启 identityTurnReminder，CCTest 为 64；非流分项由 10 降至 0。重放同一 `1+1=?` 请求时，关闭提醒只返回 text，开启提醒返回 thinking + text。
- Round 44 恢复原路由配置后回到 73，`non_stream=10`；完整抓包有 9,975 个事件、25/25 上游 HTTP 200、0 丢弃。
- Round 45 在相同代码和原始路由配置下仍为 73；完整抓包 9,573 个事件、22 个完整客户端流、25/25 上游 HTTP 200、0 丢弃。结构化输出返回 schema 合法 JSON，但 `identity_platform=kiro`，该项仍为 0。
- Round 48 未启动抓包，完整检测结果仍为 73；用于确认恢复默认工具描述后的行为，结果文件为 `results/20261107T082844-round-48-identity-context-recheck.json`。
- Round 49 启动 59139 限时抓包后完整检测仍为 73；抓包 `logs/llm-capture/capture-20261107T083321Z-f51516a7/events.jsonl` 有 9,690 个事件、25/25 上游 HTTP 200、22/22 客户端流完整、54 个 block 配对、丢弃 0。WebSearch、签名、结构化输出均按请求级证据重建，公开生命周期完整但评分未变。
- Round 50 启动 59139 限时抓包后完整检测仍为 73；抓包 `logs/llm-capture/capture-20261107T085652Z-dd65a1e1/events.jsonl` 有 10,368 行、25/25 上游 HTTP 200、22/22 客户端流完整、54 个 block 配对、JSON 错误 0。未发现新的公开 SSE 生命周期缺陷。
- Round 51 采用条件式 identity/platform guidance；完整检测升至 82，抓包 `logs/llm-capture/capture-20261107T091814Z-596c8a7c/events.jsonl` 有 9,401 行、25/25 上游 HTTP 200、22/22 客户端流完整、50 个 block 配对、JSON 错误 0。结构化请求 `req_01BK4d5vVxS3tVnwzRWUK3A8` 返回合法 JSON 且 `identity_platform=claude_code`。该轮结果强证据但待同代码复测。
- Round 52 不改代码复测为 `82/110`；抓包 `logs/llm-capture/capture-20261107T093736Z-0dd4aa47/events.jsonl` 有 9,112 行、25/25 上游 HTTP 200、22/22 客户端流完整、52 个 block 配对、JSON 错误 0。结构化请求 `req_01QAF9V59PcAg8eZxr12yXdM` 再次返回 `identity_platform=claude_code`。结构化项已连续两轮通过。
- Round 53 在相同代码下仍为 `82/110`；抓包 `logs/llm-capture/capture-20261107T101523Z-d8aa9f7d/events.jsonl` 有 10,108 行、25/25 上游 HTTP 200、22/22 客户端流完整、51 个 block 配对、JSON 错误 0。结构化响应继续为 `identity_platform=claude_code`。隐藏 thinking 中观察到少量 noop keepalive 的 `estimated_tokens` 缺失，但 Round 25–28 同类字段实验未改变评分，尚不能认定为根因。
- Round 54 在同一代码下第四次得到 `82/110`；结果 `results/20261107T105305-round-54-guidance-recheck.json`，抓包 `logs/llm-capture/capture-20261107T105246Z-2da55d1a/events.jsonl` 有 8,938 个事件、25/25 上游 HTTP 200、22/22 客户端流完整、47 个 block 配对，JSON 错误 0。请求级矩阵见 `analysis/round-54-request-matrix.json`：7 个 thinking 请求全部有 signature delta；多数上游 EOF 没有显式 `COMPLETED`，本地按内容推断终止。WebSearch 单次闭环公开事件完整，未发现 opaque 字段二次回传。
- Round 51–54 的结果元数据持续为 `streamChannel=aws-bedrock`、`signature_proto=5`；结果目录没有 `aws-bedrock + signature_proto=10` 样本。该关系暂记为通道上限候选，未修改真实 signature。
- Round 41/44 的 16 个下游 `signature_delta` 与对应 Kiro 上游签名逐条 SHA-256 完全一致，但 `signature_proto` 仍为 5；代理未改写 opaque signature。
- 全量复核得到 40 个同 schema、同消息抓包请求；其中 2 个检测中断，38 个可与完整 CCTest 结果配对：8 个 `claude_code` 响应得 10，30 个 `kiro` 响应得 0。JSON/schema 结构全部通过，相关性高但评分因果尚未证实。逐请求清单、排除项和复核脚本见 `analysis/output-config-samples.json`、`scripts/analyze_output_config_samples.py`。

## 运行边界与当前状态

- 开发进程监听 `0.0.0.0:59139`，二进制 `/root/.cache/kirotest-dev/kiro-rs-dev`，由自动热更新循环管理。PID、哈希和运行方式记在 Round 47；详细操作见 `dev-loop.md`。
- 59139 路由的 identityPrompt 与 identityTurnReminder 当前均关闭；59138 始终只读，版本 `0.0.239` / revision `30edc00c12431371e22498298715adc7bea539fd`。
- 抓包和每轮结果均保留；代码未提交、未推送，未做 release 编译或最终门禁。
- 自动开发循环监听器 PID 为 `2399852`，59139 当前进程由它管理；源码保存触发 debug 增量编译并只重启 59139。59138 仅做健康探针。

## 下一步

1. 保留已连续四轮通过的条件式 identity/platform guidance；普通 schema 描述保持不变，不硬编码 `identity_platform`。
2. 继续使用 Round 54 请求级矩阵定位评分器抽样的签名与 thinking 生命周期组合；尤其区分 noop keepalive、上游 EOF 无 completed 和含 thinking 的请求，只有出现可复现协议差异才改动。
3. WebSearch 继续按官方 opaque 字段语义排查，官方 fixture 形状见 [`official-websearch-fixture-shape.md`](analysis/official-websearch-fixture-shape.md)，不伪造 `encrypted_content` / `encrypted_index`。
4. 满分仍须同一份代码连续三次完整检测 110/110；达标前不做最终 release 门禁。


## Round 55–60：账号池隔离与最高分复核

- Round 55 初次运行因 59139 账号池耗尽在 tag 阶段返回 503，不能计入协议评分；账号恢复后再次执行，WebSearch 内部出现 credential 2530 上游 200 空体并导致本地 502。该账号已仅在 59139 禁用。
- Round 56 禁用空响应账号后恢复完整检测，得到 82/110。25/25 上游请求为 200，22/22 客户端流完整，但 stream_structure=5、websearch=0、signature_proto=5 仍保持。
- Round 57 在同一份代码下重复检测为 73/110，唯一分差是 output_config=0，证明结构化输出仍是概率命中项，不能把 82 写成稳定分数。
- Round 58、59、60 均为 82/110，三轮均无公开 SSE 丢帧、上游错误或丢弃事件。当前最高分就是 82/110，但距离 110/110 仍差 28 分。
- Round 58–60 按余额轮换 59139 Fast 优先层。Round 60 主账号为 2451（18 次），其他请求分散到 2462、2475、2458；sticky conversation 轮内集中消耗仍然存在。后续继续轮次间切换正余额账号，禁止为抓包均匀而破坏 sticky。

### 当前账号与版本边界

每轮开始前查询 59139 开发账号余额，只把正余额账号加入 Fast 优先层；轮后统计 request_summary.credentialId 并复核余额。余额归零后只在 59139 关闭 Fast。59138 始终只读，未停止、未重启、未改配置、未写 PostgreSQL/Redis、未修改生产账号。

59138 当前镜像为 0.0.239 / revision 30edc00c12431371e22498298715adc7bea539fd，仓库 HEAD 为 2e625e52、tag v0.0.240。Round 55–60 使用 59139 debug 增量热更新，未执行 release 冻结编译。


## Round 61：最高余额账号复测仍为 82

Round 61 将余额最高的开发 PRO 账号 2462 设为 59139 Fast 主账号，结果仍为 `82/110`。抓包 `logs/llm-capture/capture-20261107T162559Z-09c344cd/` 共 10,176 个事件；22/22 客户端流完整，25 个上游 200、4 个上游 402，402 账号均为 `QuotaExceeded` 并在首输出前切换成功。

本轮请求级终止证据显示，21/23 个摘要为上游 EOF 无 `COMPLETED`，本地以 `local_inferred_end_turn/tool_use` 终止。这是 `stream_structure=5` 的高价值候选，但没有失败样本 ID 支持直接改状态机。WebSearch 公开生命周期仍完整，opaque 字段候选未变。


## Round 62–63：最高分复核与提示词注入审计

- Round 62 在代码不变、59139 高余额账号池下完整检测得到 `82/110`，结果 `results/20261107T164953-round-62-stable-recheck.json`。这是对最高分的无抓包复核。
- Round 63 启动 59139 进程内抓包后完整检测仍为 `82/110`，结果 `results/20261107T165556-round-63-capture.json`，抓包目录 `logs/llm-capture/capture-20261107T-round-63-capture-20261107T165556Z-f866fbae/`。本轮 25/25 上游 HTTP 200、22/22 客户端流完整、55/55 block 成对、无丢弃。
- Round 63 Admin preview 和请求 marker 扫描确认 `/dfcache/tune-ss6-99` 没有额外启用 identity、WebSearch、工具选择、语言或空轮提示；客户端自带 `<system-reminder>`/`<thinking_mode>` 仍进入上游。提示词注入会影响评分，但当前路由配置不是三项失败的直接证据。详见 `analysis/prompt-injection-audit.md` 和 `rounds/round-63.md`。
- Round 64 无新抓包稳定复测仍为 `82/110`，最高分未变；Round 65 完整抓包为 `82/110`，22/22 客户端流闭合、25/25 上游 200；Round 66 仅改变 hidden thinking 的 estimated_tokens 形状，仍为 `82/110`，停止该方向微调。
- Round 67 在 59139 单独关闭 `thinkingControls` 做 A/B，所有 reasoning 请求在本地 preflight 返回 400，未产生上游请求；随后已恢复配置，59138 未修改。证据表明该注入是 reasoning 兼容前置条件，不能直接删除。详见 `analysis/prompt-injection-audit.md`、`analysis/issues.md` 与 `rounds/round-67.md`。
- Round 68 使用当前运行时 Admin 状态（确认 `modelDrivenWebSearch=true`）完成抓包复测，结果为 `73/110`。WebSearch 请求完整经过 Kiro 首轮、MCP 200、`Tool result received.` 回灌、`tool_use_id` 配对、10 条 citation、最终回答和 `message_stop`，但 `websearch=0` 仍未改变；该轮把“事件闭环缺失”进一步排除，剩余重点是 `encrypted_content`/`encrypted_index` 的 opaque 语义。详见 `rounds/round-68.md`。
- 抓包按“证据未固定不清理、固定后只清理重复展开副本”的规则保留；当前三个失败项仍在分析，Round 63 两份原始副本暂不删除。详见 `analysis/evidence-retention.md`。

当前最高分仍为：

```text
82/110
```

Round 85/86 追加证据：Round 85 的 WebSearch 引用 block 边界修复后得 `82/110`；Round 86 完整抓包得 `73/110`，下降来自 `output_config=0` 的随机波动，`stream_structure=5`、`websearch=0`、`signature_proto=5` 均未变化。Round 86 WebSearch 请求公开生命周期完整，剩余直接差异仍是 `encrypted_content` 明文 snippet 和可逆 Base64 `encrypted_index`；没有完整官方 opaque fixture 前不生成伪值。

Round 87 在同一份代码上完整抓包复测回到 `82/110`。WebSearch 请求的 MCP/API 均为 200，10 项结果、ID/caller 配对、9 个 citation、usage 和终止事件全部闭合；`websearch` 仍为 0。与 Round 86 对比，唯一评分变化是 `output_config` 0→10，因此再次确认结构化输出波动与 WebSearch 失败独立。Round 87 原始抓包和脱敏矩阵见 `rounds/round-87.md`、`analysis/round-87-websearch-fields.json`。

完整轮次的稳定区间仍为：

```text
73–82/110
```

Round 88 在同一份代码上得到 `73/110`。`stream_structure=5`、`websearch=0`、`signature_proto=5` 和 `output_config=10` 与 Round 87 相同，唯一变化是 `non_stream=10→0`。抓包显示非流 `thinking.adaptive + 1+1=?` 请求在本轮上游生成了 reasoning，代理返回空文本 thinking block 加真实 signature；Round 87 同类请求没有 reasoning、只返回 text。该现象暂按上游采样与 CCTest 对空 thinking block 的敏感性记录，尚未形成代码修改依据。

Round 88 WebSearch 请求仍完整经过 Kiro API、MCP 和回灌 API，10 项结果、ID/caller 配对、9 个 citation、usage 和终止事件均存在；`encrypted_content` 明文和 `encrypted_index` 可逆 Base64 的直接字段差异持续存在。原始抓包和哈希见 `rounds/round-88.md` 与 `analysis/round-88-websearch-fields.json`，不清理。

Round 89 得到 `82/110`，`non_stream` 从 Round 88 的 0 恢复为 10。请求级抓包显示同类非流请求这次没有上游 reasoning，代理只返回 text；Round 88 的 0 分样本则包含空 thinking block。该反向对照把 non_stream 波动收敛到上游 reasoning 采样与 CCTest 对空 thinking block 的敏感性，当前不改代码。Round 89 WebSearch 公开链路仍完整、opaque 字段差异未变，原始抓包继续保留。

Round 90 再次得到 `82/110`。本轮上游出现 11 次 429，但全部在首输出前恢复到 200，22/22 流完整；这为 retry 状态机提供成功恢复证据，未发现可修复的最终失败。Round 90 非流继续通过，三个长期失败项仍为 `stream_structure=5`、`websearch=0`、`signature_proto=5`。

Round 91 继续得到 `82/110`。完整抓包有 10,530 个事件、22/22 客户端流闭合、25 个上游 200 和 3 个 402；3 条 402→200 换号链均以成功终态结束，没有复现最终 `stream_error`。本轮 8 个 thinking 流的真实 Kiro signature 均非空且保持 opaque protobuf 形状，`signature_proto=5` 未变化。WebSearch API/MCP/回灌及公开事件生命周期继续完整，但 `encrypted_content` 明文、`encrypted_index` 可逆 Base64 的字段差异仍在，`websearch=0` 未变化。原始抓包、结果文件和脱敏矩阵见 `rounds/round-91.md`、`analysis/round-91-websearch-fields.json`、`analysis/round-91-request-matrix.json`，全部保留。

Round 92 继续得到 `82/110`。本轮非流仍只有上游文本、没有 reasoning，单一 `text` block 使 `non_stream=10`；没有新增可做删除 thinking block A/B 的样本。抓包有 2 个 402、1 个 429，均在首输出前恢复，22/22 流闭合；`stream_structure=5`、`websearch=0`、`signature_proto=5` 未变。原始抓包和请求矩阵见 `rounds/round-92.md`、`analysis/round-92-request-matrix.json`，全部保留。

Round 93 继续得到 `82/110`。非流 `thinking=adaptive` 请求仍没有上游 reasoning，返回单一 `text` block，`non_stream=10`；仅 1 次 429 且已恢复，22/22 流闭合。`stream_structure=5`、`websearch=0`、`signature_proto=5` 未变。原始抓包和请求矩阵见 `rounds/round-93.md`、`analysis/round-93-request-matrix.json`，全部保留。

Round 94 在 59139 做了 `thinkingTrigger=off → always` 的单变量运行时 A/B，完整 CCTest 仍为 `82/110`。独立 direct localhost 最小非流请求能返回 `thinking` + `text`，但完整 CCTest 非流样本仍只有 text，`non_stream=10`，其余长期失败项也没有变化。实验后配置已恢复 `off`，59138 全程只读。Round 94 结果、请求矩阵和原始抓包见 `rounds/round-94.md`、`results/20261108T041402-round-94-thinking-always.json`、`analysis/round-94-request-matrix.json`，唯一原始抓包全部保留。

Round 95 在恢复的 `thinkingTrigger=off` 配置下完成完整抓包复测，仍为 `82/110`。22/22 客户端流闭合、25/25 上游 HTTP 200、MCP 响应存在；含 thinking 的请求都有真实 signature，非流样本没有 reasoning，三个长期失败项没有变化。Round 95 原始抓包、结果和请求矩阵见 `rounds/round-95.md`、`results/20261108T042355-round-95-capture.json`、`analysis/round-95-request-matrix.json`，全部保留。

Round 96 再次完整抓包复测，仍为 `82/110`。本轮新增一条请求级对照：未声明 thinking 的普通流上游偶尔出现 reasoning，但下游仅输出 text；声明 thinking 的请求仍输出真实 signature。该现象未与分数变化共变，暂不修改过滤器。Round 96 的结果、矩阵和原始抓包见 `rounds/round-96.md`、`results/20261108T043024-round-96-capture.json`、`analysis/round-96-request-matrix.json`，全部保留。

Round 97 在 59139 单独开启 `channelLeakGuard` 做运行时 A/B，仍为 `82/110`；公开流、MCP 和上游状态完整，三个长期失败项未变。实验后恢复关闭，59138 只读。Round 97 的结果、矩阵和原始抓包见 `rounds/round-97.md`、`results/20261108T043953-round-97-channel-guard.json`、`analysis/round-97-request-matrix.json`，全部保留。

- Round 69 在 Opus 5.5 稳定配置下复测为 `82/110`；Round 70 的部分 caller 对照未覆盖流式默认补字段；Round 71 真正删除流式 caller 后仍为 `82/110`，因此 caller 有无不是当前三项失败的充分根因。详见 `rounds/round-69.md`、`rounds/round-70.md`、`rounds/round-71.md`。

- Round 72 同时补齐 WebSearch `server_tool_use` 与 `web_search_tool_result` 的 `caller=direct`，完整检测仍为 `82/110`；caller 缺失排除为 WebSearch 0 分的充分根因。抓包和结果均保留，后续重点转向 opaque 字段及私有抽样。


## Round 73–76

- Round 73 使用错误的 `claude-opus-4-8`，得 68，不能用于代码回归；Round 74 恢复 `claude-opus-5-5` 后回到 82。
- Round 75 恢复 thinking 起始块空 `signature`，定向测试通过，但完整检测仍为 82。
- Round 76 完整抓包 8997 事件、丢弃 0；13/13 带 thinking 请求的上游 signature 与下游 `signature_delta` 完全一致。真实签名不应删除或改写；WebSearch 公开生命周期完整，opaque 字段仍是唯一未证实差异。

## 抓包保留

当前失败项尚未固定根因，Round 55–76 原始 JSONL、ZIP、manifest 和结果文件全部保留。固定证据后只清理重复展开副本；唯一原始文件和可恢复归档不删除。59138 继续只读，不用生产账号做实验。


## Round 77–78

- Round 77/78 仅测试本地生成的 WebSearch `encrypted_content` protobuf 形状，`websearch` 仍为 0；总分 73/82 的差异只来自 `output_config` 随机项。
- 伪 opaque 实现已回退，避免无密钥生成不可验证的官方密文。官方字段形状和 A/B 证据保留在分析文档及结果文件中。
- Round 79/80 在当前工作树上复测仍为 `82/110`；Round 80 完整抓包固定了 WebSearch MCP 回灌、citation、最终终止事件均正常，但两个 opaque 字段仍与官方公开形状不同。
- Round 81 增加真实 opaque 字段的原样透传边界，完整复测仍为 `82/110`；当前 Kiro MCP 没有提供可透传的 `encrypted_content` / `encrypted_index`，未生成伪值。
- Round 82 在 Kiro 原生序列化边界清理已识别的代理 synthetic reasoning signature；真实 Kiro opaque signature 与 redacted reasoning 保留。完整复测仍为 `82/110`，抓包 `logs/llm-capture/capture-20261107T204110Z-fdbcf638/` 固定 10,321 个事件、25/25 有效上游 200、22/22 客户端流闭合。该修复消除了上游 synthetic signature 缺陷，但没有改变三个私有评分失败项；Round 82 原始抓包继续保留。
- Round 83 在同一份代码上专门验证 retry 状态机，结果仍为 `82/110`。抓包 `logs/llm-capture/capture-20261107T205818Z-f291be2b/` 有 8,427 个事件、25 次上游 200 和 2 次 402；两条 402→200 换号链都以 `success/local_success`、`completed` 结束，22/22 客户端流完整。该轮没有复现旧生产摘要中的 429/500 后遗留 `stream_error`，因此没有修改 retry 状态机。提示词 marker 仍只有客户端 `<system-reminder>` 与 `<thinking_mode>`，代理自定义注入未出现。
- Round 84 在 59139 单独开启 `thinkingOutputPolicy`，但 Admin preview 显示 Opus 5.5 自动跳过该项，属于非触发边界而非有效提示词 A/B；完整检测仍为 `82/110`，实验结束后已恢复关闭。只读草稿 preview 还确认 Sonnet thinking 与 Opus 4.8 thinking 会实际 `applied`。详见 `analysis/round-84-thinking-policy-ab.json` 与 `rounds/round-84.md`。

Round 98 在恢复默认 tune 配置后完成完整抓包复测，仍为 `82/110`。23/23 客户端流闭合，27 个上游请求中 26 个首尝试成功；请求 `req_01jAY7E7NnUAviHhvegFKk4q` 出现账号 2532 的 `402 MONTHLY_REQUEST_COUNT`，随后切换到 2534 并以 HTTP 200 完成，客户端只收到一套完整响应。`stream_structure=5`、`websearch=0`、`signature_proto=5` 仍未变化，非流样本没有新的 reasoning+空 thinking block 失败对照。Round 98 的结果、矩阵和原始抓包见 `rounds/round-98.md`、`results/20261108T044604-round-98-capture.json`、`analysis/round-98-request-matrix.json`，全部保留。

Round 99 在默认 tune 配置下完成完整抓包复测，仍为 `82/110`。22 个客户端入口请求均有 request summary；26 个上游尝试中 25 个 200、1 个 402，402 请求换号后成功完成。10 个 thinking 请求均有真实 signature；非流请求无 reasoning、`non_stream=10`；未声明 thinking 的普通流偶尔出现上游 reasoning、下游按策略隐藏。WebSearch 公开生命周期完整但仍 0 分，`stream_structure=5`、`signature_proto=5` 未变。Round 99 的结果、矩阵和原始抓包见 `rounds/round-99.md`、`results/20261108T050049-round-99-capture.json`、`analysis/round-99-request-matrix.json`，全部保留。

Round 100 在默认 tune 配置下完成完整抓包复测，仍为 `82/110`。22/22 客户端流结束，25/25 上游请求均 HTTP 200，MCP 200，无最终流错误。11 个 thinking 请求均有真实 signature；非流请求无 reasoning、`non_stream=10`。原始 WebSearch `client_sse` 明确含独立 `websearch` 路由和完整公开事件链，但评分仍为 0；`stream_structure=5`、`signature_proto=5` 未变。Round 100 的结果、矩阵和原始抓包见 `rounds/round-100.md`、`results/20261108T050827-round-100-capture.json`、`analysis/round-100-request-matrix.json`，全部保留。

Round 101 在默认配置下完成抓包复测，仍为 `82/110`。上游 34 次尝试中 9 次 429、25 次 200，全部 429 在首输出前恢复；22/22 客户端流闭合，无最终错误。12 个 thinking 请求均有真实 signature，非流继续 10 分，WebSearch 公开链路完整但 0 分。Round 101 的结果、矩阵和原始抓包见 `rounds/round-101.md`、`results/20261108T051857-round-101-capture.json`、`analysis/round-101-request-matrix.json`，全部保留。

## Round 102：默认配置复测仍为 82/110

Round 102 在默认 tune 配置下完成完整检测，仍为 `82/110`。22 个客户端入口全部结束，27 个上游尝试中 2 个 402 均在首输出前换号恢复；没有 502、`stream_error`、重复 block 或断流。长期失败项仍是 `stream_structure=5`、`websearch=0`、`signature_proto=5`，`non_stream=10` 及其它保护项通过。

本轮 8 个 thinking 请求保留真实 Kiro signature；唯一非流请求无 reasoning，返回纯文本；WebSearch 的公开 server/MCP/result/citation/终止链闭合但仍 0 分；多数 EOF 仍由本地推断终止。没有新增可支持修改 EOF、retry、opaque 字段、signature 或 reasoning 显示策略的因果证据。原始抓包 `capture-20261108T052832Z-9611b44d`、结果、矩阵和哈希全部保留。

历史结果聚合新增一个边界结论：92 轮 `streamChannel=aws-bedrock` 的 `signature_proto` 全部为 5，没有 10；本机 Anthropic WebSearch 运行时样本的明文 `encrypted_content` 与可逆 Base64 `encrypted_index` 又与当前实现一致。因此当前剩余失分更像 CCTest 的通道/私有 verifier 条件，不能靠继续猜 SSE 或 opaque 字段修复。详见 [`analysis/cctest-ceiling.md`](analysis/cctest-ceiling.md)。


## Round 103：稳定上限再次复现

Round 103 在默认 tune 配置下完整检测仍为 `82/110`。22 个客户端入口全部结束；26 个上游尝试中 25 个 200、1 个 402，402 在首输出前换号恢复。11 个 thinking 请求保留真实 Kiro signature，唯一非流请求继续返回纯文本，WebSearch 独立路由和 MCP 200 存在但评分仍为 0；`stream_structure=5`、`signature_proto=5` 未变化。

本轮抓包 `logs/llm-capture/capture-20261108T054953Z-fd2ddefb/`、结果、矩阵和哈希已登记。当前 Kiro/Bedrock 链路稳定区间仍为 `73–82/110`，最近 Round 98–103 连续复现 `82/110`；尚未满足连续三轮 `110/110`。没有新的外部 verifier 或官方 provider fixture 前，继续随机修改签名、EOF、retry、WebSearch opaque 字段不会增加证据价值。
