# Claude CLI Thinking / Effort Ingress Capture

Date: 2026-07-17

Last updated: 2026-07-18

Role: A09/D07 第一层证据，回答 Claude Code CLI 实际发送什么；不替代 kiro.rs 最终 Kiro wire 或真实上游 thinking 输出

Status: `pass / ingress-closed / final-wire-and-upstream-open / no-9022-probe-regression-closed`

## 范围

本证据使用真实 Claude Code CLI 连接 loopback fake Anthropic server，捕获顶层字段的结构化摘要。它不启动 kiro.rs，不连接 PostgreSQL、Redis 或 Kiro 上游，不读取任何真实 credential，也不触碰现有 `127.0.0.1:9022`。

捕获器只保留 model、stream、thinking、output_config、顶层 key、body 大小和 SHA-256。原始 system prompt、messages、metadata value 和假 API key 均不落盘、不进入报告。

## 身份

- Git HEAD：`401473ca1649997bdeccf4468e3add1bdb187248`，dirty working tree。
- Claude Code CLI：`2.1.197`。
- Runner：[thinking-effort-claude-cli-capture.mjs](../tests/thinking-effort-claude-cli-capture.mjs)。
- 当前 runner SHA-256：`8e0a922499e8864a99b7215ed48b6bf359819f738656958814a5d0a0571c0fae`。
- 当前 signal test SHA-256：`5b22cfb65497c508fd6609bf5b64992e54228dd16062c2f6444e3b83d7c5b72f`。
- 旧字段捕获对应 SHA：`512aefb67218e98f200462cd424fd5b796f0b49a5567f202cda07a5660a4a036`、`510938b3823392bb310de6255933a1b5986faad9bd911693b07a8df3018c3bd1`。这些旧版本的产品字段结论仍保留，但其中曾把既有 9022 listener 的前后 PID 作为隔离证明；该做法违反当前安全合同，不能再作为 release 隔离证据。
- 每个 case 使用独立 HOME、CLAUDE_CONFIG_DIR、project 和 session ID。
- `--bare --print --output-format stream-json --no-session-persistence --tools ""`，fake key，关闭非必要流量、更新、错误上报和遥测。

执行命令：

```bash
node feature/tests/thinking-effort-claude-cli-capture.mjs
```

## 三次完整运行

| Run | Cases | Messages hits | Unknown/invalid | 产品字段结果 | Runner wall time | Cleanup |
| --- | ---: | ---: | ---: | --- | ---: | --- |
| `1784252958406-69006-3fa59c` | 30 | 30 | 0/0 | 六档一致 | 72.1s | pass |
| `1784253625240-40198-b9b569` | 30 | 30 | 0/0 | 六档一致 | 15.27s | pass |
| `1784254782359-7348-6b7e2e` | 30 | 30 | 0/0 | 六档一致 | 13.48s | pass |

第一次运行的产品请求本身约 12.2 秒完成，但 runner 没有取消每个 case 的 60 秒 timeout timer，导致 Node 空等。该测试工具缺陷已修复，随后完整重复两次：第二次 `cliDurationMsTotal=14581`、`wallDurationMs=15270`；最终 runner SHA 对应的第三次为 `cliDurationMsTotal=12759`、`wallDurationMs=13476`，均不存在同类空等。第一轮不用于性能结论，仅作为字段一致性的独立重复。

三次合计：

- 90 个独立 Claude CLI session。
- 90 个 Messages 请求，严格每 session 1 hit。
- 0 个 count_tokens 请求。
- 0 unknown endpoint，0 invalid JSON，0 CLI timeout。
- 所有 fake response 均被 CLI 正常消费。

## Cleanup 竞态修复后的重复验证

随后 signal gate 真实复现了两个测试基础设施缺陷：父测试只按 PID 判断后代会受 PID reuse 干扰；更重要的是 runner 只等待 Claude process-group leader，后代可能在 TEMP_ROOT 删除后重新创建配置目录。增加 PID start identity 后仍复现 TEMP_ROOT 残留，证明第二项不是假红。最终修复包括 signal shutdown gate、完整 PGID drain，以及 normal/signal 路径都在删目录前确认 owned group 归零。

早期修复后执行结果曾额外记录 `protected9022Unchanged` 和 9022 PID。该字段本身不是产品协议事实，只是测试隔离探针；按当前 skill 安全合同，测试不应读取 live 9022 listener。2026-07-18 已删除该探针，当前 runner 只按端口数值排除 `9022`，只验证自己创建的 fake port。

当前执行结果：

- `node --test feature/tests/thinking-effort-claude-cli-capture-signal.test.mjs` 通过，2 个 subtest 全绿；HUP/INT/TERM 各 3 轮，共 `9/9` signal case，退出码分别为 `129/130/143`，owned TEMP_ROOT、owned fake port、owned Claude 后代均清理。
- `node feature/tests/thinking-effort-claude-cli-capture.mjs` 通过；六档各 5 轮，共 `30/30` session 与 `30/30` Messages hit；0 count_tokens、0 unknown、0 invalid JSON、0 timeout。
- 输出的隔离字段为 `forbiddenPorts:[9022]`、`protected9022ProbeSkipped:true`；cleanup 字段为 `childrenStopped:true`、`portReleased:true`、`tempRemoved:true`、`protected9022ProbeSkipped:true`。
- 当前轮字段矩阵与历史三次完全一致：adaptive 恒存在，absent 默认 high，显式 `max` 原样发送为 `output_config.effort=max`。

因此 cleanup 修复没有改变本节的产品字段结论，也没有通过延长等待掩盖残留。该结果仍只证明 Claude CLI 入站，不证明 kiro.rs final wire。

## 入站字段矩阵

下表每格是三次运行各 5 轮、合计 15 轮的共同结果：

| CLI effort | `thinking` | `output_config` | model | stream |
| --- | --- | --- | --- | --- |
| absent | `{"type":"adaptive"}` | `{"effort":"high"}` | `claude-opus-4-8` | `true` |
| low | `{"type":"adaptive"}` | `{"effort":"low"}` | `claude-opus-4-8` | `true` |
| medium | `{"type":"adaptive"}` | `{"effort":"medium"}` | `claude-opus-4-8` | `true` |
| high | `{"type":"adaptive"}` | `{"effort":"high"}` | `claude-opus-4-8` | `true` |
| xhigh | `{"type":"adaptive"}` | `{"effort":"xhigh"}` | `claude-opus-4-8` | `true` |
| max | `{"type":"adaptive"}` | `{"effort":"max"}` | `claude-opus-4-8` | `true` |

因此可以关闭两个入站假设：

1. 当前 Claude CLI 并未把 `max` 截断成 `high`；显式五档逐值原样发送。
2. 当前 Claude CLI 并未遗漏 adaptive；六档都发送 `thinking.type=adaptive`，未显式给 effort 时默认 `high`。

这不证明 kiro.rs 后续映射正确，也不证明 Kiro 上游要求同时收到两个字段。

## 当前项目静态链路

当前工作树静态事实：

- `MessagesRequest` 同时解析 `thinking` 和 `output_config`；effort normalizer 接受 `low/medium/high/xhigh/max`，未知值回退 `high`。
- native reasoning 模型表是本项目按 model ID 硬编码的。Opus 4.7/4.8 接受五档；Opus/Sonnet 4.6 表不含 `xhigh`，现有逻辑把不支持值映射到列表最后一项 `max`。
- native converter 生成 `additionalModelRequestFields.output_config.effort`，并把 `thinking` 设为 `None`。
- CLI endpoint 明确删除 `additionalModelRequestFields.thinking`。
- IDE endpoint 在存在 `output_config` 且没有 `thinking` 时注入 `{"type":"adaptive","display":"summarized"}`。

这形成了需要 final wire runner 验证的 endpoint 差异，但“CLI 不带 thinking”本身尚不能定性为 bug。

## 官方 Kiro IDE Bundle 交叉证据

本机安装的官方 Kiro IDE：

- extension version：`1.0.165`。
- app product commit：`fe9e4a263ce2dbc2c52128a05e44f1336297dee9`。
- bundle：`/Applications/Kiro.app/Contents/Resources/app/extensions/kiro.kiro-agent/dist/extension.js`。

该 bundle 从 `ListAvailableModels.additionalModelRequestFieldsSchema` 动态读取 effort enum、default 和 schema path，并只构造以下二选一字段：

```js
{ output_config: { effort } }
{ reasoning: { effort } }
```

在模型调用点，它把这个对象作为 `additionalModelRequestFields` 传给 GenerateAssistantResponse。当前 bundle 中没有找到模型请求用的字面 `{"type":"adaptive"}` 注入。

这是一条强静态交叉证据：当前 Kiro IDE 把 adaptive effort 的上游能力视为动态 schema path + effort，不足以支持“必须总是注入 thinking.type=adaptive”的预设。它仍不是网络抓包，最终事实必须由 fake Kiro wire 和受控真实上游小样本确认。

该交叉证据同时指出本项目的漂移风险：本项目使用硬编码 model/effort 表，而官方客户端使用上游 `ListAvailableModels` schema。模型新增、默认变化或 schema 从 `output_config` 切到 `reasoning` 时，硬编码可能静默回退、clamp 或丢能力。

## 清理与隔离

当前 runner 满足：

- fake port 释放。
- 所有 Claude child process group 停止。
- 独立 HOME/config/project TEMP_ROOT 删除。
- 报告不含 fake key 或 temp path。
- 不读取或比较 live `9022` PID；报告只声明 `protected9022ProbeSkipped:true`，并在随机端口分配时拒绝选择数值端口 `9022`。

## 未关闭项

- 真实 Claude CLI 经当前 kiro.rs 后，CLI/IDE endpoint 最终 Kiro body 六档各 5 轮。
- converter 动态使用上游 effort schema，还是继续维护硬编码兼容表。
- `thinking` absent/disabled/enabled+budget/adaptive、未知/空白/大小写等 API 组合。
- tool 前决策、thinking alias、`think hard`/`ultrathink`、长会话和自动触发。
- 最终 response thinking block/delta、signed/redacted history 和 `thinking_tokens`。
- Kiro 400/429/500/partial、恢复与 attempt/RPM 上限。
- 受控真实 Kiro 上游对各 schema/value 的支持矩阵。

这些项目关闭前，A09/D07 及发布状态继续为 `NO-GO`。
