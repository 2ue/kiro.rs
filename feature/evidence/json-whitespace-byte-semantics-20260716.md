# JSON whitespace byte-semantics focused evidence - 2026-07-16

Scope: `src/http_client.rs::maybe_compress_json_whitespace`

Candidate state: dirty working tree. Helper production logic is release-profile measured; final tag binding and MCP wire capture remain release gates.

## Red result

Command: `cargo test http_client::tests::json_whitespace_compression -- --nocapture`

Result: 1 passed, 2 failed as designed. The old `Value` round trip changed
`{"z":1.0,"a":1e+02,"z":18446744073709551616}` into
`{"a":100.0,"z":1.8446744073709552e+19}`. This proves key order, duplicate-key and number-token loss.

## Green result

The first lexical version passed 3/3, then a separate pointer/capacity test failed because that version still allocated a second body-sized buffer. The final in-place suite passed 5/5, with two isolated performance probes ignored in normal test runs. Every semantic and allocation fixture ran at least 5 rounds. Deep valid/malformed JSON ran at depths 127, 128, 256 and 4096 for 5 rounds each, followed by a normal recovery transform each round.

Validated contracts:

- disabled and invalid bodies remain byte-identical;
- object order and duplicate keys remain intact;
- number spellings, Unicode literals and escape spellings remain intact;
- only `SP`, `HTAB`, `CR` and `LF` outside strings are removed;
- already compact JSON returns through a no-output-allocation fast path;
- whitespace-bearing valid JSON is compacted inside the input allocation with the same pointer and capacity;
- no `serde_json::Value` tree is materialized.
- current `serde_json 1.0.148` validates `IgnoredAny` through iterative `ignore_value`; deep validation does not recurse on the Rust call stack.

## Release size/allocation/RSS matrix

Binary: `target/release/deps/kiro_rs-8e21067b2ccc5c02`

SHA-256: `375f682c19462aae922d6fe0a7b9c947bb293f10e5745c30ebc7bfd2937e4bec`

The binary contains the final in-place helper production logic. Later source edits changed endpoint serialization and test-only cancellation instrumentation, not the helper. Each size ran in a separate process for 5 rounds under `/usr/bin/time -l`; the empty-test max RSS baseline was 5,865,472 B.

| Input | p50 | p95/p99 | Allocation in transform | Max RSS |
| --- | ---: | ---: | ---: | ---: |
| 1 KiB | 1 us | 36 us | 0 ops / 0 B | 5,931,008 B |
| 100 KiB | 158 us | 229 us | 0 ops / 0 B | 6,356,992 B |
| 1 MiB | 1.641 ms | 1.817 ms | 0 ops / 0 B | 9,781,248 B |
| 5 MiB | 9.030 ms | 9.804 ms | 0 ops / 0 B | 28,016,640 B |
| 50 MiB | 83.414 ms | 84.126 ms | 0 ops / 0 B | 111,165,440 B |

The probe keeps the reusable fixture plus one per-round owned input, so 50 MiB process RSS includes about 100 MiB of test-owned body storage. Pointer/capacity equality and the counting allocator independently show that the transform adds no body-sized allocation.

## Abnormal, burst and cancellation matrix

All 50 MiB modes ran 5 rounds: malformed trailing input remained exact with p95 9.822 ms and one fixed 40 B serde error allocation; disabled remained exact with p95 0.251 ms and zero allocation; already-compact remained exact with p95 81.780 ms and zero allocation.

The release burst probe ran 5 MiB at concurrency 8 for 5 rounds: 40/40 completed in 69 ms, max RSS 75,530,240 B. It also ran 50 MiB at concurrency 4 for 5 rounds: 20/20 completed in 491 ms, max RSS 268,681,216 B. Both probes performed 5/5 small recovery transforms after the burst.

The transform is a synchronous CPU function and Tokio abort is not preemptive inside its poll. The isolated current-source debug probe observed 5/5 tasks complete before abort could take effect: 5 MiB abort-wait p95 141.855 ms and 50 MiB p95 1.339 s, followed by 5/5 recovery. These debug latency values characterize cancellation semantics, not release performance; the release 50 MiB transform p95 above is the production-scale timing reference.

## Provider integration

The raw TCP fake upstream captured 80/80 real API sends after all endpoint changes: IDE/CLI, API key/profile, compression off/on, stream/non-stream, 5 rounds per cell. Path, content-type, Content-Length, SHA-256 and retained raw bytes matched. Fixtures contained duplicate keys, exponent spelling, a large integer and escapes, so any second `Value` round trip would have failed the byte comparison.

## Remaining release evidence

Bind the focused matrix to the frozen tag binary and add raw MCP wire-byte capture in the full protocol matrix. The API capture does not by itself prove MCP transport bytes, although static inspection confirms MCP calls the same helper. If final load gates show event-loop lag from concurrent near-limit synchronous bodies, move large transforms to a bounded blocking executor without changing lexical semantics.
