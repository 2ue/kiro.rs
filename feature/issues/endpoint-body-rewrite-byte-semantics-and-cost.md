# Endpoint body 重写的字节语义与重复序列化成本

Status: `no-op-identity-single-pass-and-escaped-key-fixed / provider-byte-pass / post-fix-release-probes-pass / final-rebind-pending`

Severity: P0/P1

## 问题、现象与影响

修复 JSON whitespace helper 后继续逐阶段审计发现，CLI endpoint 的 `rewrite_cli_body` 即使没有任何 `origin` 或 unsupported thinking 字段需要改变，也会把完整 Kiro JSON 解析成 `serde_json::Value` 再序列化。真实红测中对象 `z,a` 被重排为 `a,z`，`1e+02` 变成 `100.0`，`\u00e9` 变成 literal `é`。因此即使关闭 whitespace compression，无操作的 endpoint stage 仍会改变 body bytes。

当确实需要修改字段且同时需要注入 profile ARN 时，CLI 和 IDE 原实现还会连续执行两个完整 parse/serialize pass。大请求、长工具历史或异常接近 50 MiB 上限时，这会放大 CPU、临时内存和 tail latency。

继续对 marker fast path 做对抗测试又发现第二个问题：有效 JSON 可把 key 写成 `"orig\u0069n"`、`"additionalModelRequest\u0046ields"` 或 `"output\u005fconfig"`。旧 substring prefilter 看不到这些语义 key，导致 CLI origin/thinking 清理或 IDE thinking 注入被静默绕过。该问题不是 tool hash 指纹，也不会表现成固定泄漏文本。

50 MiB mutation probe 还暴露第三个资源问题：`serde_json::to_string` 的增长式输出 buffer 最终保留约 100 MiB capacity。修复前 CLI/IDE 每轮累计约 200 MiB allocation、内部峰值 live 约 150 MiB，语义虽正确但突发下内存放大不可接受。

## 根因与源码链

[`src/kiro/endpoint/cli.rs`](../../src/kiro/endpoint/cli.rs) 的 `rewrite_cli_body` 无条件 parse/serialize，随后 `inject_profile_arn` 再做一次。[`src/kiro/endpoint/ide.rs`](../../src/kiro/endpoint/ide.rs) 先运行 `inject_ide_thinking_fields`，随后独立运行 profile 注入，同样最多两次。

这与 Anthropic raw body passthrough 不同：本地路径已经把请求转换成 Kiro JSON，确实需要按 endpoint 修改 origin、thinking wrapper 或 profile。正确合同是“无实际字段变更时 exact identity；有变更时只改变声明字段并保持其他 JSON Value 语义”，而不是要求所有本地转换保持原始 Anthropic 字节。

## 复现方法

CLI no-op fixture 包含格式化空白、未知字段、`z,a` 顺序、`1.0`/`1e+02` 和 escaped Unicode，但没有可改的 `origin` 或 `additionalModelRequestFields`。执行 `rewrite_cli_body` 并逐字节比较，旧实现五轮第一轮即失败。

变更 fixture 同时包含 current/history origin、thinking/output_config/unknown model fields、profile、nested unknown root 和 escape。CLI/IDE 各执行五轮，将输出解析后与“只手工修改声明字段”的 expected Value 完整比较。

escaped-key 红测分别使用 `orig\u0069n`、大小写十六进制 `output\u005Fconfig` 和混合多个 `\u`；负样本覆盖 value/普通字符串、nested schema key、escaped backslash、UTF-8 key、无效 hex、截断字符串和错误 key。每类五轮。near-limit probe 使用 1 KiB、100 KiB、1 MiB、5 MiB、50 MiB，CLI/IDE 的 escaped-no-marker 与 mutation 每档五轮。

## 选定修复与性能方案

- CLI 先做低成本 marker prefilter；无目标字段且无 profile 时不解析，直接返回原 body。
- 普通 raw marker miss 且 body 含反斜杠时，调用共享的无分配 lexical object-key detector；它只识别冒号前的语义 ASCII key，不把 value 或字符串内容误判为 key。无效 JSON 即使保守命中，正式 parse 失败后仍 raw identity。
- mutation helper 返回 `changed`；marker 只是普通字符串、字段已是目标值或 JSON 无适用结构时仍返回原 body。
- CLI/IDE 的正式 `transform_api_body` 各自只 parse 一次，在同一个 `Value` 上合并 origin、thinking 和 profile mutation，最多 serialize 一次。
- mutation serialization 按原 body 长度加 thinking/profile 的最坏新增字节预分配一个 `Vec<u8>`，再执行一次 `serde_json::to_writer`；避免 50 MiB 输出增长到约 100 MiB capacity。
- profile 注入只允许根 object；scalar/invalid JSON fail-safe 原样返回，不使用可能 panic 的字符串 index mutation。
- 有实际变更时以完整 Value diff 验证只改声明字段；不以重排后的字符串相等充当语义证明。

## 验证结果

CLI no-op exact 红测按预期失败，明确输出了键顺序、指数和 Unicode escape 改写。escaped semantic key 红测随后证明 substring fast path 会漏掉真实目标字段。两项修复和预分配优化后，endpoint focused tests 为 37/37 通过、2 个独立 perf probe ignored；CLI/IDE no-op、已规范化 origin/thinking/profile exact identity、escaped key、nested false positive、deep/malformed identity、combined mutation、URL/header/API-key/region/profile/thinking 均通过。

raw TCP fake upstream 的实际 provider API 矩阵为 80/80：IDE/CLI、API key/profile、compression off/on、stream/non-stream 每格五轮，path、content-type、长度、SHA-256 和原始 bytes 全匹配；预分配优化后重跑仍为 80/80，证明 wire bytes 未改变。

修复前 release probe 记录了资源红基线：50 MiB escaped-no-marker 的 CLI/IDE p95 分别 45.087/44.638 ms，只有一次 50 MiB owned output；50 MiB mutation 的 CLI/IDE p95 分别 163.429/112.861 ms，累计 allocation 均约 200 MiB、内部峰值 live 约 150 MiB、返回 output capacity 约 100 MiB。该 release binary 不含之后的预分配优化，不能作为最终绿结果。

修复后当前 debug binary 的 50 MiB 五轮结果：CLI allocation ops `17`，累计 104,860,111 B，峰值 live 104,860,088 B，output capacity 52,428,800 B，max RSS 166,658,048 B；IDE allocation ops `18`，累计 104,860,289 B，峰值 live 104,860,265 B，output capacity 52,428,928 B，max RSS 164,151,296 B。相对红基线，总分配约下降 50%，内部峰值 live 约下降 33%，输出 capacity 约下降 50%。debug latency 不与 release latency横向比较。

`cargo check --tests` 为 0 error、0 warning。最终冻结候选必须重跑 post-fix release 50 MiB p95/RSS；本专项不会再次在共享 target 触发十分钟 LTO。

## 残余风险与回滚

### 2026-07-17 post-fix release 复核

[Body / Payload identity matrix](../evidence/body-payload-identity-matrix-20260717.md) 在同一个 scoped target 内先通过 CLI `11/11` 与 IDE `19/19` debug 模块，再显式执行 post-fix release probe：CLI/IDE x 1 KiB、100 KiB、1 MiB、5 MiB x escaped-no-marker/mutation x 5 轮，共 16 个 release test invocations、80 次 transform。provider raw TCP 也重新完成 80/80 send，覆盖 endpoint、compression、stream 和 profile 组合。

所有 probe 断言通过，证明 no-op identity 与声明字段 mutation 在这些尺寸和轮次上保持合同。执行器截断了中段 `CLI_ENDPOINT_BODY_PERF` / `IDE_ENDPOINT_BODY_PERF` 行，因此本轮不能给出完整精确 p50/p95/p99 或 allocation vector，也不会把先前 pre-fix release mutation 数字冒充本轮结果。最终冻结 SHA 的性能分布、并发 near-limit event-loop/RSS 与 MCP wire capture仍是发布门禁。批次结束时约 2.30 GiB 的 scoped debug/release target 已自动删除且 reservation 释放。

实际变更路径仍需构造一个 `Value` tree 和一个预分配输出，因此大字符串 mutation 的内部峰值约为两份 body，外加调用者持有的原 body；这是当前结构化字段修改的 O(n) 成本。无变更路径因为 trait 返回 owned `String`，仍必须有一份输出 clone。若未来需要签名字节、顺序敏感 JSON 或更低的近上限内存，应改用经过完整 JSON 边界验证的 lexical root/path patch；不能用 ad hoc 字符串替换。

escaped-key detector 只用于决定是否进入正式 parse，不自行修改 body；因此保守 false positive 只增加一次 parse，不能改变内容。其 50 MiB worst escaped-no-marker release p95 约 45 ms，最终总负载门禁仍需确认多并发时不会阻塞 Tokio worker。同步 CPU 段不可被 abort 中途抢占的残余风险记录在 JSON whitespace issue。

回滚可将 endpoint 设为完全不修改，但会破坏 CLI origin、IDE adaptive thinking 或 profile 兼容；不得恢复无条件 no-op reserialization 或双 pass。
