# P07 非流式响应按 "工具名 + 规范化 input" 去重结构化 tool_use，误删合法的重复调用；流式不去重，两条路径行为不一致

Status: open / documented / not-fixed
Severity: Medium
Area: response
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

> 2026-09-29 代码核对（HEAD a4227c1）：3a1306d 未改动非流式 tool_use 收集与 `stream.rs`，问题仍存在。`handlers.rs`、`stream.rs` 行号与 HEAD 一致。`src/anthropic/handlers/tests.rs` 在 3a1306d 中新增了约 155 行测试，夹具整体下移 45 行，已按 HEAD 更新（`HandlerEventStreamFault` `:3295`、router `:3848`、调用函数 `:3900`、`CompleteToolWithoutStatus` `:3662-3672`、EOF 矩阵 `:5035-5145`）。

## 问题与影响

非流式响应处理上游的 `toolUseEvent(stop=true)` 时，会先算出签名 `tool_use_signature(original_name, input)`，也就是 `name|canonical_json(input)`（`src/anthropic/stream.rs:886-892`），再用 `seen_tool_sigs.insert(sig)` 去重。签名重复的结构化 tool_use 会被直接丢掉，只打一条 debug 日志 "重复的结构化 tool_use 已跳过"（`src/anthropic/handlers.rs:10735-10749`）。EOF flush 路径也这样处理（`src/anthropic/handlers.rs:10956-10966`）。去重集合在 `src/anthropic/handlers.rs:10617` 初始化。

模型在一轮里发出两个 `toolUseId` 不同、但名字和参数都一样的调用，是完全合法的，例如：

- 同一轮并行跑两次 `Bash {"command":"date"}` 来测耗时或做对比；
- 两个 `Task`/`Agent` 子任务的 prompt 相同，但希望各自独立执行；
- 重复发出幂等的轮询请求（`Bash {"command":"sleep 5 && curl ..."}`）。

非流式客户端（Agent SDK 非流式调用、脚本、部分 IDE 插件）只会收到第一个。第二个 `toolUseId` 从响应里消失，客户端不会为它回 tool_result，所以不会出现配对错误，但**用户意图被静默改写**。usage 里的 output tokens 仍然包含被丢掉的那次调用。

流式路径对结构化 tool_use **不去重**：`process_tool_use` 对每个 `toolUseId` 都会发出 `content_block_start`（`src/anthropic/stream.rs:3512-3552`），stop 时只是把签名记下来（`src/anthropic/stream.rs:3616-3618`、`src/anthropic/stream.rs:3681-3683`）。同一个上游响应，stream=true 和 stream=false 得到的 tool_use 数量不一样。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

- Anthropic Messages API（已验证事实，官方文档）：`content` 是 `tool_use` 块的数组，每个块由 `id` 唯一标识。协议对多个块的 `name`/`input` 是否相同没有任何约束。并行工具调用时，客户端要为每个 `id` 分别回 `tool_result`。
- 官方响应里不会出现两个 `id` 相同的块。所以代理侧唯一合理的去重键是 `id`（对应 Kiro 的 `toolUseId`）。
- Kiro（经验推断，没有找到样本）：上游在重传或断点续传时，可能用**同一个 `toolUseId`** 把同一个调用再发一次。本仓库没有找到 "上游用不同 `toolUseId` 重复发送同一个调用" 的报文证据。

## 源码链与根因

去重逻辑的来源（git 历史）：

- `5270d2b`（2026-06-16，"fix: harden Kiro protocol and Claude Code tool handling"）一次性加入了 `seen_tool_sigs`、结构化 tool_use 的签名去重，以及 `append_recovered_non_stream_blocks` 里针对 "字面 `<invoke>` 泄漏恢复出来的 tool_use" 的签名去重（`git show 5270d2b -- src/anthropic/handlers.rs`）。同一个 commit 给 `stream.rs` 加了约 1300 行字面 invoke 泄漏恢复逻辑。
- 真正要解决的问题是：**上游在输出结构化 `toolUseEvent` 的同时，又把同一个调用以 `<invoke …>` 文本泄漏到正文里**。代理从正文恢复出 tool_use 后，不能和结构化那份重复。流式侧对应的就是 `queue_leaked_tool_use` / `emit_queued_leaked_tool_uses` 只跳过 "已被结构化调用占用的签名"（`src/anthropic/stream.rs:3053-3091`），有单测 `literal_tool_protocol_dedupes_later_structured_tool_use_for_five_rounds` 锁定（`src/anthropic/stream.rs:7213-7236`）。
- 非流式实现时，同一个 `seen_tool_sigs` 集合被**同时**用于 "结构化 vs 结构化" 和 "泄漏 vs 结构化" 两种去重。前者是附带造成的，并没有对应的上游故障样本。

非流式执行顺序（这也是修复能保住泄漏去重的前提）：

1. 事件循环里先处理所有结构化 tool_use，把签名写入 `seen_tool_sigs`（`src/anthropic/handlers.rs:10677-10751`）。
2. EOF flush 未完成的 tool buffer（`src/anthropic/handlers.rs:10917-10968`）。
3. 最后在 `append_non_stream_reasoning_and_text` → `append_recovered_non_stream_blocks` 里从正文恢复字面 invoke，用同一个集合跳过重复（`src/anthropic/handlers.rs:9670-9704`、`src/anthropic/handlers.rs:9707-9814`）。

根因：去重键选错了。结构化事件本身有唯一 ID，应该按 ID 去重；按内容签名去重只适用于 "没有可信 ID 的泄漏恢复块"。

审计原述核对：结论成立。行号 `~10735-10749` 准确。另外 EOF flush 路径（`src/anthropic/handlers.rs:10956-10966`）有同样的问题，审计没有提到。"流式不去重" 对**结构化** tool_use 成立；流式对**泄漏恢复**的 tool_use 是会做签名去重的。

## 复现

### 最小复现（单测）

放进 `src/anthropic/handlers/tests.rs`，复用已有的 fake upstream 夹具：`HandlerEventStreamFault`（`src/anthropic/handlers/tests.rs:3295`）、`eventstream_test_frame`（`src/anthropic/handlers/tests.rs:49`）、`handler_eventstream_fault_router`（`src/anthropic/handlers/tests.rs:3848`）、`call_handler_eventstream_fault`（`src/anthropic/handlers/tests.rs:3900`），写法参照 `CompleteToolWithoutStatus`（`src/anthropic/handlers/tests.rs:3662-3672`）。

先在 enum 里加一个 `DuplicateStructuredToolUses` 变体，响应分支如下：

```rust
HandlerEventStreamFault::DuplicateStructuredToolUses => {
    let tool = |id: &str| {
        eventstream_test_frame(
            "toolUseEvent",
            json!({
                "name": "Bash",
                "toolUseId": id,
                "input": "{\"command\":\"date\"}",
                "stop": true
            }),
        )
    };
    let mut body = tool("toolu_dup_a");
    body.extend(tool("toolu_dup_b"));
    // 同 ID 重发：应当只保留一份
    body.extend(tool("toolu_dup_b"));
    handler_eventstream_bytes_response(body)
}
```

测试：

```rust
#[test]
fn audit_p07_distinct_tool_use_ids_with_same_input_are_all_kept_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("audit-p07-duplicate-tool-use", || async {
        for stream in [false, true] {
            for round in 1..=5 {
                let upstream = HandlerEventStreamFaultUpstream::start(
                    HandlerEventStreamFault::DuplicateStructuredToolUses,
                )
                .await;
                let (app, _usage) = handler_eventstream_fault_router(&upstream.base_url);
                let (status, _request_id, body) = call_handler_eventstream_fault(app, stream).await;
                assert_eq!(status, StatusCode::OK, "stream={stream} round={round} body={body}");
                // 修复前 stream=false 只有 toolu_dup_a
                assert!(body.contains("toolu_dup_a"), "stream={stream} round={round}");
                assert!(body.contains("toolu_dup_b"), "stream={stream} round={round}");
                if !stream {
                    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
                    let ids = value["content"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|block| block["type"] == "tool_use")
                        .map(|block| block["id"].as_str().unwrap().to_string())
                        .collect::<Vec<_>>();
                    assert_eq!(ids, ["toolu_dup_a", "toolu_dup_b"], "round={round}");
                }
            }
        }
    });
}
```

修复前：stream=false 时 `toolu_dup_b` 缺失，断言失败。stream=true 时结构化块都发出了，但同 ID 重发的行为需要结合 `state_manager.handle_content_block_start` 对已存在 index 的处理一起确认（这个测试会顺带暴露出来）。

### 端到端复现

1. 在测试实例上用非流式请求（`"stream": false`）让模型 "并行执行两次完全相同的 `date` 命令，不要合并"，工具列表里提供 `Bash`。
2. 用 debug 日志对比上游事件里的 `toolUseEvent` 数量和响应 `content` 里的 tool_use 数量，同时 grep 日志 "重复的结构化 tool_use 已跳过"。
3. 相同 prompt 用 `"stream": true` 再跑一次，对比 tool_use 块数量。

## 修复方案

### 候选方案

| 方案 | 做法 | 评价 |
| --- | --- | --- |
| A. 结构化按 `toolUseId` 去重（推荐） | 结构化路径新增 `seen_tool_ids: HashSet<String>`，只有 ID 重复才跳过；签名仍写入 `seen_tool_sigs`，专门供泄漏恢复去重 | 语义正确，改动小，泄漏去重行为不变 |
| B. 完全删除结构化去重 | 结构化事件全部透传 | 上游同 ID 重发时会出现重复 `id`，违反协议 |
| C. 签名计数（多重集） | 泄漏恢复时按次数抵消 | 可以处理 "两个相同结构化调用 + 一个泄漏回显" 的情况，但复杂度收益比低 |

### 推荐方案

采用方案 A：

1. `src/anthropic/handlers.rs:10617` 旁边新增 `let mut seen_tool_ids: HashSet<String> = HashSet::new();`。
2. `src/anthropic/handlers.rs:10735-10749` 改成：先 `seen_tool_sigs.insert(sig)`（忽略返回值），再在 `seen_tool_ids.insert(tool_use.tool_use_id.clone())` 为 true 时 push；否则打 debug 日志 "重复 toolUseId 的结构化 tool_use 已跳过"。
3. EOF flush（`src/anthropic/handlers.rs:10956-10966`）同样处理。
4. `append_recovered_non_stream_blocks` 保持签名去重不变。
5. 流式侧：确认同一 `toolUseId` 在 stop 之后又出现时，不会再发一次 `content_block_start`。`tool_block_indices` 会复用 index（`src/anthropic/stream.rs:3512-3519`），如果 state manager 没有拦截，就要补一个 `completed_tool_ids` 集合，让两边语义一致。
6. 最好把非流式 tool_use 收集逻辑抽成纯函数（输入事件序列，输出 blocks），方便不经过 HTTP 夹具直接做单测。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 新增 `audit_p07_distinct_tool_use_ids_with_same_input_are_all_kept_for_five_rounds`（stream 和非流式都跑，每种 5 轮）。
- 保留并重跑 `literal_tool_protocol_dedupes_later_structured_tool_use_for_five_rounds`（`src/anthropic/stream.rs:7213`）。补一个非流式的对等用例：结构化 `Bash{"command":"ls"}` 加上正文里的字面 `<invoke name="Bash">` 泄漏，结果必须只有 1 个 tool_use。
- 同 `toolUseId` 重发：非流式和流式都只输出 1 个块。
- 回归 `run_handler_non_stream_untrusted_eof_matrix` 和 `CompleteToolWithoutStatus` / `IncompleteToolWithoutStatus` 相关矩阵（`src/anthropic/handlers/tests.rs:5035-5145`）。

## 兼容性与风险

- 如果上游确实会用**不同** `toolUseId` 重复发送同一个调用（本仓库没有样本），修复后非流式会多出一个 tool_use，和流式现状一致。先在 debug 日志里统计 "重复 toolUseId" 和 "同签名不同 ID" 两类计数跑一周，再决定是否需要额外防护。
- 客户端会真实执行两次相同的命令。这本来就是模型的意图，也是官方 API 的行为。
- 回滚：恢复签名判断即可，没有状态迁移。
