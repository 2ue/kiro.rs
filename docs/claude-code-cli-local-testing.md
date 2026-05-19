# 本地 Claude Code CLI 真实测试指南

本文档用于测试 `kiro-rs` 暴露给 Claude Code CLI 的真实服务能力。测试目标不是只验证 429，而是验证长会话、持续会话、工具调用、流式输出、账号故障转移、sticky 迁移、usage 可观测性和高缓存连续性。

## 测试原则

1. 必须使用真实 `claude` CLI 走当前配置的 Claude Code 服务商，不用 mock HTTP 代替。
2. 每次测试都要保存 `stream-json` 输出和服务日志，便于回放。
3. 账号级错误出现时，期望行为是转移到下一个可调度账号；只要存在健康账号，不应返回“服务不可用”。
4. sticky 会话只能作为健康账号偏好。sticky 账号冷却、认证失败、额度失败、模型不支持或本次请求已排除时，必须解绑或跳过。
5. 高缓存行为必须跨 resume 保留。账号转移不能导致 Claude Code CLI 的会话上下文和本地 prompt-cache usage 统计丢失。
6. 全局 429 波动是观测信号，不是健康账号调度的阻断条件。

## 前置检查

确认 Claude Code CLI 当前指向本地服务：

```bash
ccman cc current
```

期望能看到类似：

```text
当前 Claude Code 服务商
URL: http://127.0.0.1:9022
```

确认服务和 Admin API 可用：

```bash
curl -sS http://127.0.0.1:9022/api/admin/config/load-balancing \
  -H 'x-api-key: sk-admin-local-debug'
curl -sS http://127.0.0.1:9022/api/admin/credentials \
  -H 'x-api-key: sk-admin-local-debug'
```

如果端口或 admin key 不同，以本地 `config.json` 为准。

## 清理测试现场

只在需要干净样本时清理。排查偶发账号异常时，建议先保留 Redis 调度状态和历史 usage，以免清掉关键证据。

```bash
mkdir -p .local-run/claude-tests
: > .local-run/backend-9022.log
rm -f .local-run/claude-tests/*.jsonl

curl -sS -X POST http://127.0.0.1:9022/api/admin/usage-records/clear \
  -H 'x-api-key: sk-admin-local-debug'
```

不要默认清空 Redis 的 `kiro:sched:v1:*` key。只有做完全隔离的冷启动测试时才清理 Redis，因为这些 key 记录账号冷却、sticky 和全局波动事件。

## CLI 基本约束

`stream-json` 输出需要配合 `--print` 和 `--verbose`：

```bash
claude --print --verbose --output-format stream-json 'hello'
```

首次创建指定会话用 `--session-id <uuid>`。继续同一会话必须用 `--resume <uuid>`，不要再次传同一个 `--session-id`，否则 Claude Code CLI 会报 session already in use。

非交互工具调用建议在隔离测试目录中运行，并显式设置权限模式：

```bash
--permission-mode bypassPermissions --tools 'Read,Bash,Write,Edit'
```

如果测试环境不是隔离目录，改用更窄的 `--allowedTools`。

## 场景 A：新会话基础可用性

目标：验证 CLI 能通过本服务完成一次完整请求，并产生 usage 记录。

```bash
SID_A="$(uuidgen | tr '[:upper:]' '[:lower:]')"

claude --print --verbose --output-format stream-json \
  --session-id "$SID_A" \
  --model sonnet \
  '用两句话说明当前工作目录是什么项目，不要修改文件。' \
  > ".local-run/claude-tests/a1-new-session-$SID_A.jsonl" 2>&1
```

检查点：

1. CLI 输出包含 assistant message 或 result。
2. 服务端 usage 出现一条 success 或可解释的 error。
3. usage 记录里有 `credentialId` 或 `lastAttemptedCredentialId`。
4. 如果前序账号异常，`attemptedCredentialIds` 应出现多个账号，而最终不应在有健康账号时失败。

## 场景 B：同一会话 resume

目标：验证 sticky、Claude Code 会话续接和高缓存连续性。

```bash
claude --print --verbose --output-format stream-json \
  --resume "$SID_A" \
  --model sonnet \
  '继续上一轮回答，补充这个项目最可能的入口文件路径。不要修改文件。' \
  > ".local-run/claude-tests/a2-resume-$SID_A.jsonl" 2>&1
```

检查点：

1. CLI 没有创建全新上下文，能理解上一轮任务。
2. usage 的 `conversationId` 能稳定关联同一会话。
3. `stickyBound=true` 是正常现象，但如果原 sticky 账号进入冷却，后续记录应出现 `fallbackFromSticky=true` 或最终绑定到其他账号。
4. `usageSource=local_prompt_cache` 时，第二轮通常应出现 `cacheReadInputTokens`，而不是每次都只有 `cacheCreationInputTokens`。

## 场景 C：真实工具调用

目标：验证 Claude Code CLI 的工具调用、服务的流式协议转换和 usage 归因。

```bash
SID_TOOLS="$(uuidgen | tr '[:upper:]' '[:lower:]')"

claude --print --verbose --output-format stream-json \
  --session-id "$SID_TOOLS" \
  --model sonnet \
  --permission-mode bypassPermissions \
  --tools 'Read,Bash,Write,Edit' \
  '请读取 README.md 和 docs/account-scheduling-429-failover-strategy.md，运行 pwd，然后在 .local-run/claude-tests/tool-smoke.txt 写入一行测试摘要。' \
  > ".local-run/claude-tests/c1-tools-$SID_TOOLS.jsonl" 2>&1
```

检查点：

1. `stream-json` 中能看到 tool use 和 tool result。
2. 文件 `.local-run/claude-tests/tool-smoke.txt` 被创建。
3. usage 记录仍能写入 `credentialId`、`attemptedCredentialIds` 和 cache 字段。
4. 工具调用过程中如果发生账号级错误，后续请求不能继续打同一个冷却账号。

## 场景 D：长会话和高缓存

目标：验证较长上下文下的本地 prompt-cache/high-cache 统计不会因为 resume 或账号迁移消失。

```bash
SID_LONG="$(uuidgen | tr '[:upper:]' '[:lower:]')"

claude --print --verbose --output-format stream-json \
  --session-id "$SID_LONG" \
  --model sonnet \
  '阅读 README.md 和 docs/cache-behavior-analysis.md，整理 12 条要点。不要修改文件。' \
  > ".local-run/claude-tests/d1-long-$SID_LONG.jsonl" 2>&1

claude --print --verbose --output-format stream-json \
  --resume "$SID_LONG" \
  --model sonnet \
  '基于上一轮内容，再补充 8 条关于 high-cache 的测试观察点。不要修改文件。' \
  > ".local-run/claude-tests/d2-long-resume-$SID_LONG.jsonl" 2>&1
```

检查点：

1. 第二轮应该能承接第一轮内容。
2. usage summary 中 `localPromptCacheRequests` 增长。
3. 长上下文或 resume 场景应有 `cacheReadInputTokens`，达到阈值时 `highCacheRequests` 增长。
4. 如果账号转移发生，缓存统计仍应来自会话上下文和本地模拟，不应归零成完全无缓存请求。

## 场景 E：账号故障转移

目标：验证异常账号不会拖垮服务。这里不要求异常一定是 429，认证错误、额度错误、上游 5xx、代理错误都应按分类处理。

建议测试方法：

1. 先确认账号池里至少有一个已知健康账号。
2. 保留前两个容易进入冷却的账号，让真实请求自然触发异常，不要把所有账号都手工禁用。
3. 连续跑 5 到 10 轮 Claude CLI 请求，观察是否从异常账号转移到健康账号。
4. 若需要强制构造，优先用凭据级测试或临时代理故障隔离单个账号，不要破坏所有账号配置。

循环请求示例：

```bash
for i in 1 2 3 4 5 6 7 8; do
  sid="$(uuidgen | tr '[:upper:]' '[:lower:]')"
  claude --print --verbose --output-format stream-json \
    --session-id "$sid" \
    --model sonnet \
    "第 $i 轮：读取 README.md 的项目名称并返回一句话。不要修改文件。" \
    > ".local-run/claude-tests/e-failover-$i-$sid.jsonl" 2>&1
done
```

通过条件：

1. 存在健康账号时，最终不应持续全部 429 或全部服务不可用。
2. 异常账号应出现在 `rateLimitedCredentialIds` 或错误链路字段中。
3. 健康账号应成为最终 `credentialId`。
4. `schedulerBlocked=true` 只能出现在确实没有可调度账号，或本次请求已排除所有可用账号的情况下。
5. 全局波动事件不能让健康账号被跳过。

## 场景 F：sticky 迁移

目标：验证持续会话不会一直粘在坏账号上。

步骤：

1. 用 `--session-id` 创建新会话并成功完成一轮，记录最终 `credentialId`。
2. 让该账号进入冷却或临时不可调度。
3. 用 `--resume` 继续同一会话。
4. 观察 usage。

通过条件：

1. 第二轮不应继续命中冷却账号。
2. usage 应能看到 `fallbackFromSticky=true` 或 attempted 链路中先出现旧账号再出现健康账号。
3. 成功 fallback 后，后续同一会话应优先绑定成功账号，除非原账号已恢复健康。
4. 高缓存字段不能因为 sticky 迁移而消失。

## 结果采集

采集 usage summary：

```bash
curl -sS http://127.0.0.1:9022/api/admin/usage-summary \
  -H 'x-api-key: sk-admin-local-debug' \
  > .local-run/claude-tests/usage-summary.json
```

采集最近 usage records：

```bash
curl -sS 'http://127.0.0.1:9022/api/admin/usage-records?limit=1000' \
  -H 'x-api-key: sk-admin-local-debug' \
  > .local-run/claude-tests/usage-records.json
```

没有 `jq` 时可用 Node 提取关键字段：

```bash
node -e '
const fs = require("fs");
const raw = JSON.parse(fs.readFileSync(".local-run/claude-tests/usage-records.json", "utf8"));
const records = Array.isArray(raw) ? raw : raw.records || raw.data || [];
for (const r of records.slice(-30)) {
  console.log(JSON.stringify({
    id: r.id,
    status: r.status,
    conversationId: r.conversationId,
    credentialId: r.credentialId,
    attemptedCredentialIds: r.attemptedCredentialIds,
    rateLimitedCredentialIds: r.rateLimitedCredentialIds,
    lastAttemptedCredentialId: r.lastAttemptedCredentialId,
    schedulerBlocked: r.schedulerBlocked,
    stickyBound: r.stickyBound,
    fallbackFromSticky: r.fallbackFromSticky,
    usageSource: r.usageSource,
    cacheReadInputTokens: r.cacheReadInputTokens,
    cacheCreationInputTokens: r.cacheCreationInputTokens,
    errorType: r.errorType,
    errorMessage: r.errorMessage
  }));
}
'
```

## 常见失败判读

1. **同一会话重复传 `--session-id` 失败**：这是 CLI 使用方式错误。继续会话应使用 `--resume <uuid>`。
2. **有健康账号但返回所有凭据不可调度**：调度策略有缺陷，重点查全局 backoff 是否被当成调度阻断、sticky 是否未解绑、`excluded_ids` 是否误包含健康账号。
3. **连续全部 429**：先区分全局上游波动和账号池状态。看是否还有账号成功过、是否所有账号都在 `rateLimitedCredentialIds` 中、是否健康账号被错误冷却。
4. **前两个账号频繁冷却**：这是账号级状态，后续请求应避开它们并使用健康账号；如果仍反复命中，说明账号级 cooldown 或 sticky 检查没有生效。
5. **工具调用成功但 usage 没有 credential trace**：provider 错误或成功路径没有把 `CredentialAttemptTrace` 传到 handler。
6. **resume 后 cache read 消失**：检查 `conversationId` 提取、Claude CLI session 是否真的 resume、以及 `promptCacheSimulationMode` 是否仍为 `high-cache`。
7. **`schedulerBlocked=true` 太多**：只有无可调度候选时才合理。若同时存在健康账号，说明调度筛选或全局波动处理有 bug。

## 最低通过标准

一次完整复测至少应包含：

1. 2 个以上新会话。
2. 1 个会话至少 3 轮 resume。
3. 1 次真实工具调用。
4. 1 次长上下文/high-cache 检查。
5. 5 到 10 轮连续请求观察账号转移。
6. usage records 和 backend log 的归档。
7. 对 `attemptedCredentialIds`、`rateLimitedCredentialIds`、`schedulerBlocked`、`stickyBound`、`fallbackFromSticky`、cache 字段的结论。
