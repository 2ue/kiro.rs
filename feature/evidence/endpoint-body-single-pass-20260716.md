# Endpoint body single-pass focused evidence - 2026-07-16

Status: focused semantic/provider pass on a dirty tree; post-allocation-fix final release binding pending.

## Red evidence

`cargo test cli_rewrite_noop_is_byte_identical_for_five_rounds -- --nocapture` failed on round 0 before the fix. The old output changed formatted `{"z":1.0,"a":1e+02}` content into key-sorted output with `100.0`, and converted `\u00e9` into a literal character.

The escaped-key red fixtures then showed that raw substring prefilters missed valid semantic JSON keys: `orig\u0069n`, `additionalModelRequest\u0046ields`, and `output\u005fconfig`. CLI origin/thinking cleanup and IDE thinking injection were skipped.

The pre-optimization 50 MiB release mutation probe found about 200 MiB cumulative allocation, 150 MiB internal peak live bytes and a retained output capacity near 100 MiB. This identified growth in `serde_json::to_string`, not a second semantic parse pass.

## Green evidence

Endpoint focused tests passed 37/37 after the no-op, escaped-key, single-pass and preallocated-serializer fixes; two isolated perf probes remained ignored. Relevant cases:

- CLI no-op exact bytes: 5/5;
- IDE API-key no-profile/no-thinking no-op exact bytes: 5/5;
- CLI normalized origin/profile and IDE existing thinking/profile exact bytes: 5/5 each;
- CLI combined origin + unsupported thinking removal + profile injection: 5/5 full Value diff;
- IDE combined adaptive thinking + profile injection: 5/5 full Value diff;
- semantic escaped keys, mixed upper/lower hex and multiple escapes: 5/5;
- target text in values, strings and nested schema keys remains exact: 5/5;
- malformed and Value-recursion-limit inputs remain exact, followed by recovery: 5/5;
- existing URL, Host, token type, region, profile, model field and thinking tests: pass.

The production endpoint methods perform at most one `Value` parse and one serialization per body. Ordinary no-op requests use raw marker fast paths. A raw miss with backslashes uses an allocation-free semantic object-key scanner; only a real mutation serializes. Invalid/scalar inputs return unchanged. Serialization preallocates from the original body plus bounded thinking/profile growth.

## Provider wire evidence

The raw TCP fake upstream passed 80/80 API sends after the allocation fix: IDE/CLI, API key/profile, compression off/on, stream/non-stream, 5 rounds per cell. Every path, content-type, Content-Length, SHA-256 and raw body matched expected bytes. Provider focused runtime was 1.79 seconds excluding compilation.

## Five-size performance evidence

Release binary `375f682c19462aae922d6fe0a7b9c947bb293f10e5745c30ebc7bfd2937e4bec` contains the escaped-key detector but predates the preallocated serializer. It is valid final evidence for unchanged no-op paths and red characterization for mutation only.

| Route/mode | Size | p50 | p95/p99 | Per-round allocation | Max RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| CLI escaped no-op | 1 KiB | 1 us | 763 us | 1 op / 1,024 B | 6,782,976 B |
| CLI escaped no-op | 100 KiB | 67 us | 85 us | 1 op / 102,400 B | 6,209,536 B |
| CLI escaped no-op | 1 MiB | 683 us | 905 us | 1 op / 1,048,576 B | 8,093,696 B |
| CLI escaped no-op | 5 MiB | 4.721 ms | 7.344 ms | 1 op / 5,242,880 B | 32,473,088 B |
| CLI escaped no-op | 50 MiB | 41.392 ms | 45.087 ms | 1 op / 52,428,800 B | 111,017,984 B |
| IDE escaped no-op | 1 KiB | 1 us | 992 us | 1 op / 1,024 B | 6,651,904 B |
| IDE escaped no-op | 100 KiB | 60 us | 78 us | 1 op / 102,400 B | 6,291,456 B |
| IDE escaped no-op | 1 MiB | 613 us | 753 us | 1 op / 1,048,576 B | 8,323,072 B |
| IDE escaped no-op | 5 MiB | 3.812 ms | 3.941 ms | 1 op / 5,242,880 B | 32,358,400 B |
| IDE escaped no-op | 50 MiB | 40.860 ms | 44.638 ms | 1 op / 52,428,800 B | 111,181,824 B |

The single allocation is the required owned return value; no `Value` tree is built when no declared field changes.

Pre-fix release 50 MiB mutation red baselines were: CLI p95 163.429 ms, 19 ops, 209,717,815 B cumulative allocation, 157,288,872 B internal peak live, 104,857,584 B output capacity, max RSS 268,517,376 B; IDE p95 112.861 ms, 21 ops, 209,718,286 B cumulative allocation, 157,289,031 B peak live, 104,857,694 B output capacity, max RSS 164,249,600 B.

Current post-fix debug 50 MiB, five rounds each:

| Route | Ops | Cumulative allocation | Internal peak live | Output capacity | Max RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| CLI mutation | 17 | 104,860,111 B | 104,860,088 B | 52,428,800 B | 166,658,048 B |
| IDE mutation | 18 | 104,860,289 B | 104,860,265 B | 52,428,928 B | 164,151,296 B |

The fix halves cumulative allocation and output capacity and reduces internal peak live by roughly one third. Debug latency is intentionally omitted as a release comparison. `cargo check --tests` completed with 0 errors and 0 warnings.

## Remaining evidence

Rebuild the frozen candidate once and rerun post-fix release mutation p50/p95/p99 and RSS; do not reuse the pre-fix release mutation numbers as green evidence. The full protocol release matrix must also provide MCP wire capture and concurrent near-limit event-loop/RSS recovery. Current semantic, allocation and API wire gates are green.
