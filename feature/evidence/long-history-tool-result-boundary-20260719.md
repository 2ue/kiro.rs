# Long-history tool-result boundary capture

Date: 2026-07-19

Status: `frozen fake-upstream pass / oracle corrected / real Claude long-session pending`

## Scope

This record resolves the latest long-history red item around captured upstream bodies containing the synthetic loadtest strings `old_history_entry_with_large_tool_result` and `summarized_history_entry_with_hash_and_excerpt`.

The important boundary is:

- internal protocol transcript markers are forbidden in upstream-visible text fields;
- ordinary user/tool_result content is user-owned data and may remain, subject to configured truncation/shaping.

The earlier violation detector was too broad because it treated arbitrary loadtest tool output strings as if they were internal protocol leakage. That would make a clean, intentionally preserved tool_result fail the gate and would also push the implementation toward modifying user/tool_result data, which violates the existing body-semantics contract.

## Environment

- Product binary: `/tmp/kiro-frozen-20260719-r8/kiro-rs`
- Product SHA-256: `131696bd81e1cdaeceaac6a45f9c76bf698eb559785b379a82fd77e2f742e631`
- Loadtest binary: `/tmp/kiro-frozen-20260719-r5/kiro_loadtest`
- Loadtest SHA-256: `23c04221deb72dde601d491452d8cc9a99211df99b2cd39a386272141f2db8e3`
- PostgreSQL: current-project isolated `kiro-final-20260718-pg`, loopback `127.0.0.1:50891`
- Redis: current-project isolated `kiro-final-20260718-redis`, loopback `127.0.0.1:50892/0`
- Protected port `9022`: skipped by numeric exclusion; not probed or touched.
- Raw temp roots: deleted after summary capture.

## Frozen runtime matrix

Summary file:

```text
/tmp/kiro-long-history-tool-result-probe-20260719-r1.json
sha256 927f33fecebf18e358f5fc7e27a1a86c4deb08bf9535a02d13351a3ed183f578
```

Cases:

| Case | Mode | Payload | Requests | Status | p95 TTFB ms | Max Kiro body bytes | Max history | Tool uses/results | Internal marker hits | Synthetic tool text present | Result |
| --- | --- | --- | ---: | --- | ---: | ---: | ---: | --- | ---: | --- | --- |
| `preemptive_large_tool_results` | `preemptive` | `large-tool-results` | 5 | `{"200":5}` | 37 | 37,985 | 12 | 4 / 4 | 0 | yes | pass |
| `preemptive_mixed_pathological` | `preemptive` | `mixed-pathological` | 5 | `{"200":5}` | 77 | 240,122 | 0 | 0 / 0 | 0 | no | pass |
| `preemptive_schema_key_mapping` | `preemptive` | `schema-key-mapping` | 5 | `{"200":5}` | 13 | 4,801 | 2 | 0 / 0 | 0 | no | pass |
| `on_too_long_large_tool_results` | `on_too_long` | `large-tool-results` | 5 | `{"200":5}` | 24 | 554,537 | 12 | 4 / 4 | 0 | yes | pass |

Forbidden markers scanned across captured Kiro text fields:

```text
user Continue
user Tool results provided
Tool results provided
<function_results>
</function_results>
[previous output]
[trimmed output]
[duplicate output]
```

All four cases had `0` forbidden marker hits and `0` orphan tool results. The synthetic tool-result strings remained only in legitimate historical `toolResults` content for the two large-tool-result cases. That is expected: payload shaping preserves a bounded head/tail summary by design and must not rewrite arbitrary tool output.

## On-too-long retry capture

Summary file:

```text
/tmp/kiro-on-too-long-retry-capture-20260719-r1.json
sha256 f85b3f9ad3282389fed10e079740f10417009732bc813d3c378d94d14036e513
```

The fake Kiro upstream returned `400 {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}` for the first inference request, then accepted the retry.

Observed:

- downstream load result: `1/1` success, `statusCounts={"200":1}`;
- inference sends: exactly `2`;
- first send body: `554,474` bytes, `historyLen=12`, `toolResults=4`;
- retry send body: `37,922` bytes, `historyLen=12`, `toolResults=4`;
- forbidden marker hits: `0` on both sends;
- unknown fake requests: `0`.

This proves the `on_too_long` path keeps the first send untrimmed, performs one bounded payload-guard retry after an upstream too-long signal, and sends the trimmed body without internal transcript markers.

## Interpretation

The long-history red item is not a product leak on the r8 frozen candidate. It was an over-broad test oracle:

- `old_history_entry_with_large_tool_result` and `summarized_history_entry_with_hash_and_excerpt` are synthetic loadtest text inside valid tool results.
- The proxy correctly preserves/truncates those as user-owned tool output.
- Actual internal transcript markers remained absent from upstream Kiro bodies across preemptive, on-too-long, mixed pathological and schema-key cases.

The release gate still remains open for real Claude Code long-session/resume/tool/search/image/MCP scenarios. This record only closes the fake-upstream long-history oracle correction and on-too-long retry capture for the frozen r8 binary.

