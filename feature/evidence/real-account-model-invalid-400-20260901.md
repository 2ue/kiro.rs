# 真实本地账号模型 `400 invalid_request_error` 证据

Status: `evidence-complete-for-current-local-run / historical-root-cause-partially-attributed`

## 目的

本文件可以脱离当前会话阅读。它记录 2026-09-01 在本项目唯一长期测试实例上，
使用真实 Kiro 凭据进行的修复前后对照、账号边界、模型能力快照、请求结果和
可复现命令。原始 credential JSON、access token、refresh token、API key、
cookie、邮箱和数据库密码均未写入本文件或同目录证据。

问题分析正文见：
[real-account-model-invalid-400-20260901.md](../issues/real-account-model-invalid-400-20260901.md)。

## 运行身份和资源

| 字段 | 值 |
| --- | --- |
| 日期 | 2026-09-01 |
| 时间窗 | 10:36--14:53 UTC |
| listener | `127.0.0.1:19023` |
| 配置 | `tmp/thinking-budget-local/config.json` |
| PostgreSQL database | `kiro_thinking_budget_20260901` |
| Redis namespace | `127.0.0.1:26379/0` |
| 候选 binary | `/tmp/kiro-thinking-candidate.tf6Wsb/kiro-rs` |
| SHA-256 | `f02b9883f5b8ce831b4801f0b14802d9a53619bff44c6bcdb72dcc7c76a15ffd` |
| Git revision at capture | `41c3566b26e14da25ff87c98c9fe4181e6b15016` |
| source credential records | 220 |
| service-loaded records | 220 |
| external pools | 0 |

服务启动日志确认：

```text
已加载 220 个凭据配置
usage/dashboard PgSQL 已使用独立连接池
已订阅 Redis 运行时事件
启动 Anthropic API 端点: 127.0.0.1:19023
```

健康检查：

```http
GET http://127.0.0.1:19023/healthz
HTTP/1.1 200 OK

{"service":"kiro-rs","status":"ok"}
```

进程核验：

```text
PID 89324
/tmp/kiro-thinking-candidate.tf6Wsb/kiro-rs
  -c tmp/thinking-budget-local/config.json
  --credentials <operator-supplied-file>
```

## 证据文件清单

| 文件 | 内容 |
| --- | --- |
| [baseline-20x2.json](real-account-tests-20260901/baseline-20x2.json) | 10 秒并发汇总 |
| [baseline-20x2-detail.json](real-account-tests-20260901/baseline-20x2-detail.json) | 20 条逐请求结果 |
| [model-matrix-25.json](real-account-tests-20260901/model-matrix-25.json) | 5 模型 × 5 次逐请求结果 |
| [credentials-snapshot-20260901.json](real-account-tests-20260901/credentials-snapshot-20260901.json) | 220 条脱敏账号运行快照 |
| [model-capabilities-snapshot-20260901.json](real-account-tests-20260901/model-capabilities-snapshot-20260901.json) | 模型目录和 reasoning discovery 状态 |
| [usage-normalization-snapshot-20260901.json](real-account-tests-20260901/usage-normalization-snapshot-20260901.json) | 修复后 4 条 usage 记录的白名单字段 |

## 修复前结果

### baseline 汇总

`baseline-20x2.json`：

```text
base=http://127.0.0.1:19023/cc/v1/messages
concurrency=2
durationMs=10000
sent=13
completed=13
ok=0
failed=13
statusCodes: 400=11, 502=2
```

### baseline 逐请求

`baseline-20x2-detail.json`：

```text
total=20
status=400: 20
content-type=application/json
body-bytes=302
```

每条公开响应都是统一的 `invalid_request_error` 文案，只带脱敏后的 request ID
形态，未返回坏字段。

### 模型矩阵

`model-matrix-25.json`：

```text
generatedAt=2026-09-01T10:40:40.529Z
total=25
status=400: 25
```

按模型统计：

| requested model | 次数 | HTTP 400 |
| --- | ---: | ---: |
| `sonnet` | 5 | 5 |
| `claude-sonnet-4` | 5 | 5 |
| `claude-sonnet-4.5` | 5 | 5 |
| `claude-sonnet-4-6` | 5 | 5 |
| `claude-sonnet-4-5-20250929` | 5 | 5 |

这只能证明矩阵失败，不足以证明每条请求的具体坏字段相同。

## 账号快照

`credentials-snapshot-20260901.json` 是由 Admin API 白名单投影生成的 JSON，
没有 refresh token、access token、邮箱、哈希或密码字段。统计如下：

```text
total=220
available=220
disabled=0
globalInFlightRequests=0
queuedRequests=0
globalMaxConcurrentRequests=512
maxQueuedRequests=30
```

分类：

```text
authMethod: social=199, api_key=15, idc=6
endpoint: ide=205, cli=15
subscription: KIRO FREE=78, KIRO POWER=16, KIRO PRO MAX=126
hasProfileArn: true=199, false=21
effectiveApiRegion: us-east-1=220
```

本轮外部池数量为 0，所以所有真实请求均属于本地账号边界。

## 模型能力快照

`model-capabilities-snapshot-20260901.json`：

```text
available=true
source=kiro-list-available-models
modelCount=9
models:
  auto
  claude-haiku-4.5
  claude-sonnet-4
  claude-sonnet-4.5
  deepseek-3.2
  glm-5
  minimax-m2.1
  minimax-m2.5
  qwen3-coder-next
lastError=native reasoning capability discovery is incomplete (2/6 cohorts observed)
```

此快照说明模型目录存在，但 reasoning cohort 尚未完整发现；不能把目录中的
每个模型解释为每个账号都已授权且所有 thinking 形态均可用。

## 修复后真实请求

请求共同条件：

```text
endpoint=/cc/v1/messages
requested model=claude-sonnet-4.5
prompt=Reply with exactly: pong
真实本地账号路径
```

四个用例均 HTTP 200：

| case | 输入边界 | 结果 |
| --- | --- | --- |
| equality | `thinking.enabled`, budget 2048, max 2048 | 200；usage output 131，thinking 130 |
| bounded expansion | `thinking.enabled`, budget 4096, max 2048 | 200；usage output 257，thinking 256 |
| adaptive cleanup | `thinking.adaptive`, 同时携带 budget | 200；usage output 1 |
| disabled cleanup | `thinking.disabled`, 同时携带 budget | 200；usage output 1 |

`usage-normalization-snapshot-20260901.json` 中 4 条记录全部为 `status=success`
且无 `errorType/errorMessage`。记录的 `requestedMaxTokens` 为：

```text
2048, 2048, 4097, 2048
```

其中 `4097` 是 `budget=4096,max=2048` 的有界扩展结果，证明修复发生在第一次
发送之前，而不是靠第二次重试恢复。

### 当前实例回归（2026-09-01 15:55 UTC）

在同一个 `127.0.0.1:19023` 实例、同一份真实账号池上再次执行四个低频用例。
当前监听进程为 PID `89324`，二进制 SHA-256 为
`f02b9883f5b8ce831b4801f0b14802d9a53619bff44c6bcdb72dcc7c76a15ffd`。
四个请求均首次返回 HTTP 200：

| 用例 | 输入 | 结果 |
| --- | --- | --- |
| equality | enabled, `2048/2048` | thinking + text，`thinking_tokens=203` |
| bounded expansion | enabled, `4096/2048` | 服务规范化为 `4097/4096`，thinking + text，`thinking_tokens=156` |
| adaptive cleanup | adaptive，同时带 `budget_tokens=4096` | text，无 thinking，HTTP 200 |
| disabled cleanup | disabled，同时带 `budget_tokens=4096` | text，无 thinking，HTTP 200 |

逐请求脱敏证据见
[`current-regression-20260901.json`](real-account-tests-20260901/current-regression-20260901.json)。
该文件只保留请求边界、响应类型、usage 和 request ID，不包含凭据或完整请求体。

## 可复现和验证命令

### 健康与唯一实例

```bash
curl -fsS http://127.0.0.1:19023/healthz
lsof -nP -iTCP:19023 -sTCP:LISTEN
ps -p "$(lsof -nP -iTCP:19023 -sTCP:LISTEN -t | head -n1)" -o pid=,command=
```

### 源码聚焦回归

```bash
feature/tests/run-cargo-scoped.sh model-resolution -- \
  cargo test -q model_capabilities -- --nocapture
# 35 passed

feature/tests/run-cargo-scoped.sh thinking-normalization -- \
  cargo test -q request_facts -- --nocapture
# 22 passed

feature/tests/run-cargo-scoped.sh thinking-entry-normalization -- \
  cargo test -q messages_entry_normalizes_omitted_or_conflicting_thinking_controls \
  -- --nocapture
# 1 passed
```

所有 Cargo 命令均经过 scoped wrapper；三个 target 在命令结束后清理。

### 证据脱敏检查

```bash
rg -n -i \
  "authorization|bearer|refresh.?token|password|cookie|secret|refreshTokenHash|apiKeyHash|@" \
  feature/evidence/real-account-tests-20260901
```

允许命中 `authMethod=api_key` 这样的分类字段；不允许出现 token、密码、
完整 credential JSON 或邮箱地址。当前目录通过 `git diff --check`。

## 结论和限制

已证实：

1. 入口 thinking 边界非法组合可以在发送前修复；
2. 修复后真实本地账号的四种边界请求首次发送即可完成；
3. 显式 minor 不应静默换成另一个 minor；
4. 本轮真实请求全部是本地账号，外部池没有证据；
5. 历史 400 的统一公开文案不足以把全部样本归因给单一字段。

未证实：

1. 历史 20x2/25 失败是否全部由 thinking 字段触发；
2. 历史请求是否混入签名 transcript、工具 schema、图片或账号授权问题；
3. 外部池在相同请求体下的行为；
4. `2/6` reasoning cohorts 未完成时，各账号对 native reasoning 的完整能力。

因此后续诊断应优先保存 request-scoped、脱敏的字段事实和上游分类，
再决定是否按账号换号、跨池 fallback 或协议修复；不能用泛化 retry 代替根因分析。
