# Thinking/Effort Development Wire Verification

Status: `development-wire-pass / frozen-fake-upstream-wire-pass / real-upstream-pending / release-NO-GO`

## Scope And Conclusion

The first part of this evidence closes the development-level request path from the Anthropic
request model through the production converter, `KiroRequest` serialization,
provider, CLI/IDE endpoint transform, and actual loopback HTTP request bytes.
It does not use Docker, PostgreSQL, Redis, a running kiro.rs service, real Kiro
credentials, or a real Kiro upstream.

For an authoritative fake model schema that advertises
`output_config.effort = [high, max]`, the final captured body was semantically:

```json
{
  "additionalModelRequestFields": {
    "output_config": { "effort": "max" }
  }
}
```

The value remained `max`; it was not clamped to `high`. The final wire did not
contain `additionalModelRequestFields.thinking`. That absence is intentional
for this schema: Claude's inbound `thinking.type=adaptive` selects native
reasoning, while the Kiro field name and allowed values come from the
authoritative model-discovery schema. The proxy must not invent a second field
that the upstream schema did not advertise.

Current endpoint behavior is also now symmetric and non-inventing: the CLI
transform preserves existing model-owned fields while changing only declared
origin/profile paths, and the IDE transform preserves existing fields while
changing only profile ARN when needed. The previous CLI delete and IDE
adaptive-injection behavior is no longer present.

## Reproduction Matrix

The development matrix covers:

- Anthropic `thinking`: adaptive, enabled with budget, disabled, absent,
  malformed/unsupported, model-suffix defaults, and automatic trigger signals;
- effort: absent, low/medium/high/xhigh/max, authoritative default, missing
  default, unsupported enum, reasoning-path schema, and heterogeneous cohort;
- response: stream/non-stream, native/synthetic reasoning, signed/redacted,
  tool boundary, truncation/overflow, usage, and external normalized SSE;
- endpoint: CLI and IDE, profile mutation/no mutation, compression on/off,
  stream/non-stream, five internal rounds, byte length and SHA-256;
- final max composition: converter to provider to actual loopback HTTP bytes,
  CLI/IDE x stream/non-stream x five rounds, 20 requests total.

No test contacted or inspected the protected service port. Ephemeral ports are
allocated from the OS and port 9022 is rejected by value without querying its
listener state.

## Commands And Results

Current pure Node runner gates after the no-probe change:

```text
node --check <five runner files>
PASS 5/5

node --test feature/tests/runtime-validation-paths.test.mjs \
  feature/tests/thinking-effort-kiro-wire-contract.test.mjs
PASS 11/11, current-source duration 0.574s

node --test feature/tests/thinking-effort-kiro-wire-signal.test.mjs
PASS 42/42, current-source duration 56.363s
```

Scoped Rust results, all with Rust 1.92.0 and `--locked`:

```text
thinking-focus-r2
104/104 passed
target size_kib=1669264
removed=true reservation_released=true

effort-focus-r1
17/17 passed
target size_kib=1669768
removed=true reservation_released=true

provider-wire-bytes-r2
1/1 top-level passed; 80 captured HTTP requests internally
target size_kib=1669268
removed=true reservation_released=true

provider-max-wire-r2
1/1 top-level passed; 20 captured HTTP requests internally
dynamic test duration 0.40s
target size_kib=1669600
removed=true reservation_released=true
```

The provider byte matrix compares path, Content-Type, Content-Length, SHA-256,
and retained request bytes. The max composition test additionally parses the
captured final JSON and asserts exact `output_config.effort=max`, absence of an
invented `thinking` field, and endpoint-specific origin for every request.

## Red And Invalid Runs

The following runs are preserved and are not counted as passes:

- `thinking-focus-r1`: Cargo rejected `--lib` because this package has no
  library target. The outer tool timeout interrupted wrapper reporting; the
  stale reaper then removed the 32 KiB owned target and its 12 GiB reservation.
- `provider-wire-bytes-r1`: `running 0 tests` because `--exact` was used without
  the fully qualified test name. Its compilation is not dynamic evidence;
  `provider-wire-bytes-r2` is the valid replacement.
- `provider-max-wire-r1`: compile error because the test tried to serialize
  `ConversionResult` instead of constructing the production `KiroRequest`.
  No HTTP request ran. The corrected test follows the same construction as
  `local_body_pipeline` and passed as `provider-max-wire-r2`.

Every red/invalid scope also reported `removed=true` and
`reservation_released=true`; no scoped target remains.

## Protocol Decision

The mapping authority is the per-model Kiro discovery contract:

- explicit effort is preserved only when present in the advertised enum;
- omitted effort uses the advertised default, not a hard-coded `high`;
- enabled token budgets map to a supported effort or fail explicitly;
- explicit effort with unknown, absent, invalid, or heterogeneous capability
  fails closed instead of disappearing or silently falling back to a prompt;
- a discovered `reasoning.effort` path is used as advertised instead of always
  forcing `output_config`;
- Anthropic `thinking` is not copied verbatim unless Kiro advertises an
  equivalent field contract.

Synthetic XML/prompt compatibility remains a separate fallback for models
without verified native reasoning and is controlled independently from the
operator prompt master. It is not allowed to forge a client-selected effort.

## Performance And Body Integrity

The production change in this slice is test-only; the request hot path did not
gain a scan, DOM round trip, retry, lock, or network request. Existing endpoint
tests retain byte identity when no declared path changes and use one bounded
parse/serialize only when origin/profile mutation is required. The 80-request
provider matrix covers compression on/off and both endpoints; the 20-request
max matrix completed in 0.40 seconds after compilation.

## Frozen Fake-Upstream Kiro Wire Gate

2026-07-18 追加执行真实 Claude Code CLI ingress 后的 Kiro wire gate。该 gate 使用仓库外冻结 `kiro-rs` binary、当前仓库专属隔离 PostgreSQL/Redis、fake Kiro upstream 和独立 Claude config/home；不访问受保护的 `127.0.0.1:9022`，runner 只按端口值拒绝 9022，不读取现有 listener PID。

结果：

```text
Claude Code CLI version=2.1.197
endpoints=cli,ide
efforts=absent,low,medium,high,xhigh,max
rounds=5
message_cases=60/60 passed
unknown_ingress_other=0
cc_head_probes=60
```

验证结论：

1. Claude CLI 入站仍会为 absent/low/medium/high/xhigh/max 发送 `thinking: { type: "adaptive" }`；absent 的 `output_config.effort` 默认 high，显式 max 仍为 max。这与单独 ingress capture `30/30` 一致。
2. Kiro outbound wire 保留 `additionalModelRequestFields.output_config.effort`，包括 `max`；没有发生 `max -> high` 截断。
3. fake model schema 未声明 `thinking` 字段时，Kiro outbound wire 不发明 `additionalModelRequestFields.thinking.type=adaptive`。这是当前协议决策：按上游 schema 映射 effort/reasoning，不把 Claude 入站 adaptive 原样伪造为未声明 Kiro 字段。
4. Claude CLI 2.1.197 每个 case 会先发一个 `HEAD /cc` 探针；runner 已把它记录为 `cc_head_probe` 协议事实，仍要求未知 ingress `other=0`。

无效中间结果：

- 旧 wire runner 曾把 `HEAD /cc` 归为 unexpected other，导致 false red；修正分类后重跑 60/60 通过。

该 gate 关闭“frozen fake-upstream handler 是否截断 effort 或发明 thinking”的问题；真实 Kiro upstream 的 native reasoning delta、final usage、主动/被动长会话、tool/search/image/MCP 和异常流仍是 release blocker。

## Remaining Gates

The frozen fake-upstream service runner has passed for the effort/thinking wire
contract above. This is not a real-upstream pass and does not prove native Kiro
reasoning deltas or production usage accounting.

Still open for the release gate:

- active/passive trigger, tool-decision and long-session/resume through the
  complete HTTP handler rather than converter/provider composition only;
- real native reasoning deltas and final usage from a controlled upstream;
- malformed field, 400/429/500/partial-stream recovery and shared attempt
  budgets through the complete service;
- external raw/normalized profiles and final release RSS/FD/latency gates.

Accordingly this topic is a development-wire pass, not a release pass. The
overall release remains `NO-GO`.
