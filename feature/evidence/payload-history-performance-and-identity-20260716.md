# Payload History Performance And Raw Identity Evidence

Date: 2026-07-16

Status: `focused-local-pass / release-gates-pending`

## 问题与修复前机制

Anthropic 和 Kiro payload guard 的历史裁剪都按“删除一个逻辑 turn -> repair 整个历史 -> 序列化整个请求 -> 再判断大小”循环。需要删除 `N` 个 turn 时，处理量接近剩余 body 大小之和，长历史下呈近二次增长。该问题没有固定的 tool hash 指纹；它表现为大 body 的 CPU/分配增长、请求入口延迟和突发流量下的队列压力。

第一次批量实现仍有一个隐藏的二次路径：每选择下一个逻辑 turn，都会重新计算整个已选前缀的 JSON 字节。2,000 条历史的两项 debug 测试合计约 `7.23s`，因此没有把第一次实现判为完成。

最终聚焦实现只对每条即将删除的 message 执行一次 `CountingWriter` 计数，并增量维护数组逗号开销。选出满足 soft target 的完整前缀后，一次 drain、一次 repair、一次完整序列化。当前消息与 active tool-result pair 仍受原有边界保护。

## 可重复命令与结果

```text
cargo test history_batch_trim_matches_exact_json_reduction -- --nocapture
2/2 passed; 200-turn fixtures; serialized array reduction与估算完全一致

cargo test large_history_uses_one_trim_pass_and_constant_serializations -- --nocapture
2/2 passed; 每条路径 1,000 user + 1,000 assistant history
test execution: 0.06s
Anthropic: history_trim_passes=1, guard_serializations<=2, final<=40,000 bytes
Kiro:      history_trim_passes=1, guard_serializations<=3, final<=40,000 bytes

cargo test anthropic::payload_guard::tests:: -- --nocapture
60/60 passed（批量实现后第一次聚焦；新增后需在最终共享 revision 重跑）

cargo test clean_anthropic_raw_body_is_byte_identical_at_representative_sizes -- --nocapture
1/1 passed; 内部覆盖 1 KiB/100 KiB/1 MiB/5 MiB，各 3 轮
每轮 guarded bytes 与原始非规范 JSON 完全相同
futureField 保留
guard_serializations=0
history_trim_passes=0
```

以上命令运行于未提交共享工作树，基线 HEAD 为 `401473c` / `v0.0.109`。它们是聚焦证据，不是最终 release revision 证据。

## 结构验收

- `json_array_prefix_reduction` 同时计算被删 item JSON bytes 与数组 separator 差值，测试与真实 `serde_json::to_vec` 前后差完全一致。
- 大历史只移除完整的 user/assistant/tool-result 逻辑单元。
- active current tool result 没有下一完整 user turn 时返回 `removed=0`，不会为达到 soft target 拆掉 active pair。
- `PayloadGuardReport` 新增 `guardSerializations` 和 `historyTrimPasses`，slow timing log 同步记录序列化次数，便于生产复核。
- clean normalized/raw path 在没有 repair/shaping 时直接复用原始 `Bytes`，未知顶层字段和原始空白/字段顺序保持不变。

## Escaped Unicode Prefilter Red/Green

后续源码复核发现 raw transcript sanitizer 的预筛把“body 中出现任意 `\\u`”视为可能污染。即使 1 MiB 正常中文只是由客户端编码为 `\\u4E2D`，也会进入完整 JSON DOM parse、clone 和 assistant-history projection；这不会改变 clean 输出，但违反小请求/clean body 不承担整树处理成本的合同。

修复后 prefilter 仍只做一次线性扫描，但用固定 4 个 marker 匹配状态解析 ASCII JSON escape，不分配 DOM、不复制 body。四类 marker 的每个字符分别改成大小写 hex `\\uXXXX` 仍可命中；escaped backslash 和约 1 MiB 正常 Unicode escape body 不命中。

```text
cargo +1.92.0 test raw_request_prefilter_ -- --nocapture
5/5 passed; 1319 filtered out; focused test execution 0.53s
```

该结果证明 marker 兼容与 fast-path 分支，不是 release-mode B05 延迟/RSS 证据。

## 尚未完成

- B05 每档 100 轮 release-mode p50/p95/p99 与 CPU/RSS 对照尚未执行。
- 50 MiB route gate 的五路由 `413`、统一错误 envelope、0 provider 仍在本轮实现/复验中。
- `50 MiB + burst` 的峰值 RSS、排队与回落尚未执行。
- current-fit 最多 64 次收缩的最坏输入仍需单独测量和进一步收敛。
- 真实 Claude Code CLI 长会话、120k history、resume 和 L5 soak 尚未执行。
