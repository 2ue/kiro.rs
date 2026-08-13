# Payload Guard Semantics, Limits, And Performance

Status: `focused-body-matrix-and-release-probes-pass / frozen-load-and-cli-pending`

Severity: P0/P1

## 问题集合

- repair 会把 orphan/mismatch/duplicate tool result 原文文本化。
- trim 按单条 history 删除并在每轮 repair/serialize，可拆逻辑 turn，并呈近二次成本。
- `payloadGuardMaxBytes` 的合同曾在不同权威面相互矛盾：旧 `config.rs` 注释称“最大字节数”，但部署文档和两套 UI 明确称“本地裁剪目标/经验预算”，并明确允许 `still_oversized` 透传。100 KB 配置下可保留约 607 KB tool schema 证明它不是 hard limit，但按现有部署合同不能直接定性成实现 bug。绝对入站上限并非缺失：`anthropic/router.rs` 已在 body 提取前配置固定 50 MiB `DefaultBodyLimit`；待验证缺口是所有 messages 路由覆盖、413/0-upstream 行为以及 burst/排队时内存边界。
- current image 可追加重复 omission placeholder；5 MiB 上游图片限制当前按 base64 字符/JSON source 长度判断，不是 decoded source bytes，因此约 3.75 MiB 至 5 MiB 的合法图片会被提前误判超限。
- current fit 最多多轮完整序列化，clean raw 入口还有额外 DOM parse/clone。

## 根因

payload repair、history pairing、trim、图片归一化和最终 size check 曾由多轮“修改后重新 repair/serialize”循环共同承担，缺少逻辑 turn 与字段所有权；soft shaping target、路由 absolute body limit 和上游媒体限制又共用了“max bytes”近似命名。图片使用编码后字符串长度而非 decoded bytes，raw 路径也没有 clean-marker fast path。这些所有权混淆同时造成正文语义变化、错误归因和近二次性能。

## 单点复现

使用 deep schema、大工具定义、active tool pair、大 result/document/user text、有效超预算图片和 1 KB 至 5 MB clean body。每个 fixture 先只开一个 shaping 能力，再测组合；同时比较输入/输出 JSON 的字段级 diff，不能只看 HTTP 状态。

图片边界必须按 decoded bytes 构造：`5 MiB - 1`、`5 MiB`、`5 MiB + 1`，分别生成带/不带空白、普通 base64/data URL、PNG/JPEG/WebP/GIF 形态；不能用编码字符串长度冒充图片大小。soft payload target 则分别验证 `still_oversized`、显式 shaping 和上游真实错误，不把它写成 hard-limit 断言。

## 方案

- 逻辑 turn 原子 trim，先计算候选单元大小再批量/二分删除。
- malformed output 使用中性占位/错误，不复制原文。
- 保持 `payloadGuardMaxBytes` 为兼容的 soft shaping target，并统一源码注释、API 和 UI 文案。保留路由层独立 50 MiB absolute request limit，补覆盖与 413/0-upstream/内存测试；只有证据表明 50 MiB 不合适时才另行配置化。不能把 soft target 突然改成 hard reject，避免误伤本来可由上游接受的正文、工具 schema 或图片。
- 图片在 converter 已校验内容后按 decoded bytes 判断 5 MiB；raw/external 路径使用精确、无大额二次分配的 base64 decoded-length 计算并对非法编码 fail closed。每类 omission 只生成一个汇总占位。
- 增加 serialize count、stage latency、original/final bytes 观测；clean body 使用廉价 marker prefilter。

## 性能验收

按 B04/B05 和 L5 执行。clean payload p95 增量必须在记录的预算内，CPU/内存随 body 近线性，RSS/FD 在 idle 后回落；不得因入口处理时序制造 dispatch_queue 429。

## 当前实现与聚焦证据

2026-07-16 当前工作树已修正 5 MiB 图片判断为 decoded bytes，并把同一 Anthropic 消息内多个超限图片收敛为一条有序汇总占位。红绿测试和边界结果见 [Payload 图片 decoded-byte 证据](../evidence/payload-image-decoded-boundary-20260716.md)。

历史 trim 已从逐逻辑 turn 全量 repair/serialize 改为增量计算完整 turn 前缀后批量删除。第一次批量实现仍因反复计算整个前缀而在 2,000 条历史测试中耗时约 `7.23s`；修正为每条 message 只计数一次后，同一两项测试执行部分为 `0.06s`。当前结构化报告记录 `guardSerializations/historyTrimPasses`；1,000 user + 1,000 assistant 的 Anthropic/Kiro fixture 均为一个 trim pass，完整序列化分别不超过 2/3 次。1 KiB、100 KiB、1 MiB、5 MiB clean raw 各 3 轮保持 byte-identical、未知字段保留且 guard 内 0 次序列化。详见 [Payload 历史性能与 raw identity](../evidence/payload-history-performance-and-identity-20260716.md)。

后续又发现 transcript raw prefilter 对任意 `\\u` escape 都进入 JSON DOM；正常转义中文长 body 会无污染也 parse/clone。当前已改成固定状态的 escape-aware marker scanner：四类 marker 每个字符转义位置仍可识别，约 1 MiB clean `\\u4E2D` body 不进 DOM；聚焦 5/5 通过。最终 B05 仍需用 release candidate 测实际分位。

文件路径复核又确认两个与 Messages shaping 不同、但同属 payload/resource 边界的问题。第一，文件内容上限与全路由 `DefaultBodyLimit` 原来同为 50 MiB，multipart 边界和字段头使“恰好 50 MiB 文件”必然先被外层错误拒绝。当前文件集合 POST 路由单独保留 1 MiB multipart 外壳预算，Messages 仍严格维持 50 MiB；真实 Router 对 50 MiB 与 50 MiB+1 各执行 5 轮，前者 5/5 上传成功并删除，后者 5/5 到达文件内容 guard 后明确 413。第二，文件删除原来只移除 `HashMap` 项、不移除 FIFO `VecDeque` ID，上传后删除 churn 会让元数据无界增长并把未来 eviction 变成大扫描；当前同步清理 FIFO，5 轮 x 1,000 次插入/删除后 `files/order/total_bytes` 均归零。聚焦命令：

```text
cargo test file_delete_churn_keeps_fifo_metadata_bounded_for_five_rounds -- --nocapture
cargo test file_upload_route_accepts_exact_file_limit_and_rejects_one_byte_over_for_five_rounds -- --nocapture
```

两项分别 `1/1` test 通过，测试体耗时约 `0.03s` 与 `3.14s`。首次组合命令因 Cargo 只接受一个 TESTNAME 参数而立即失败，随后按真实测试入口分别执行；该调用错误不计入通过证据。文件并发 upload 的真实慢 body、全局 2 permit、RSS 回落和 256 MiB store eviction 仍需 L3/L5 验证。

### 新确认的 413 错误来源误归因风险

当前五类 Messages 路由的 50 MiB 上限由 Axum `DefaultBodyLimit` 执行；认证中间件随后用“响应状态是 413 且 Content-Type 为 `text/plain` 或缺失”来猜测它是框架 body-limit rejection，并改写成 Anthropic JSON `invalid_request_error`。这个条件不是框架响应的可靠身份：external pool 或其他 handler 合法返回的纯文本 413 也会被改写为 `The request body exceeds the 50 MiB limit.`，造成错误分类、usage/排障误导和上游响应语义丢失。

当前自有 `MessagesBody` extractor 已在 body-limit 所有权边界生成规范 413，并删除认证中间件的来源猜测。五类 Messages 路由超限各 3 轮，以及 JSON/plain/untyped 下游 413 保真各 5 轮已有聚焦测试。最终候选仍需带真实 provider/external hit 计数、burst RSS 与所有 profile 重跑，因此不能只凭组件结果关闭本专题。

### 2026-07-17 Body / Payload 合并复核

修复后使用 Rust `1.92.0` 在唯一 scoped target 中合并执行 debug 与 release 矩阵。payload guard 模块为 `67/67` 通过、1 个 release probe 在普通发现中 ignored；其中 clean Anthropic/Kiro 的 1 KiB、100 KiB、1 MiB、5 MiB 各 100 轮，leading assistant 的 1,000/4,000/16,000 条各 5 轮，20/100 tool cycles、decoded image 三边界、current 四图批量裁剪及 tool result/document/schema 均按用例至少 5 轮。clean Anthropic 保持 exact bytes、同一 `Bytes` pointer、未知字段和 0 serialization；clean Kiro 固定 1 serialization；Kiro/Anthropic 四图裁一图均固定 3 serializations 且只有一个汇总占位。

同一批次还通过 body processing `20/20`、request body `3/3`、router `5/5`、十路由多模态 handler、external raw/normalized、provider 80/80 wire capture 和 CLI/IDE endpoint 语义模块。release payload size probe 覆盖 clean/dirty Anthropic 与 clean Kiro 的四档尺寸、每格 5 轮并通过。完整命令、精确计数和 `size_kib=2410696 removed=true reservation_released=true` 的清理证据见 [Body / Payload identity matrix](../evidence/body-payload-identity-matrix-20260717.md)。

本轮 3,600 行输出的中段被执行器截断，所以不能从本轮完整回收并宣称一套新的精确 p50/p95/p99；没有为补显示日志再制造一个冷构建。既有精确性能数字仍只归属于原证据记录的 binary。真实 Claude CLI C2-C4、50 MiB 并发 RSS/event-loop 与 L5 仍未完成，因此本专题保持部分关闭。

## 残余风险与回滚边界

2026-07-19 追加 r8 frozen fake-upstream 捕获，明确区分合法 tool_result 内容保留与内部 transcript 泄漏。`preemptive` 的 large-tool-results、mixed-pathological、schema-key-mapping 以及 `on_too_long` large-tool-results 共 20/20 请求成功，internal transcript marker 命中为 0；`on_too_long` 首发 too-long 400 后 exactly one retry，body 从约 554 KiB 降至约 37 KiB。详见 [长历史 tool_result 边界证据](../evidence/long-history-tool-result-boundary-20260719.md)。这证明当前 r8 fake-upstream 下 payload guard 不会因长历史复现内部 scaffold，但也确认 shaping 仍会保留合法 tool_result head/tail，测试不得把任意工具输出字符串当泄漏。

50 MiB extractor 与纯文本 413 已有聚焦合同证据，2026-07-17 合并批次也重新证明 1 KiB 至 5 MiB identity、批量 image fit 和序列化计数；但所有 external profile、真实上游 hit、文件慢上传/burst、50 MiB 进程 RSS 与真实 CLI 仍未完成端到端验证。回滚可以撤销单项 shaping 优化，但不能恢复 orphan 原文 textify、按编码字符串误判图片、文件 FIFO 元数据泄漏，或把 soft target 改写为未经兼容评估的 hard reject。最终候选若 clean body p95/RSS 不满足 B05/L5，必须先定位 serialize/clone 次数再发布。
