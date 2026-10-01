# kiro.rs capture sidecar

This directory is an independent diagnostic service. It is not part of the Rust
binary and is not included by the production `Dockerfile`. The sidecar only
reads Docker state and captures packets; it never restarts, replaces, or
reconfigures the target container.

## Plaintext LLM capture (recommended)

kiro.rs records the full LLM exchange in-process. The sidecar controls it
through the Admin API, so no pcap, TLS key log, or tshark is needed:

```sh
python3 tools/kiro-capture-sidecar/server.py \
  --bind 127.0.0.1 --port 19137 \
  --container kiro-rs-2ue-59137-app \
  --admin-url http://127.0.0.1:59137 \
  --admin-token-file /path/to/admin-key \
  --no-pcap \
  --duration 900 --max-bytes 1073741824
```

`Start capture` calls `POST /api/admin/diagnostics/capture/start`, and
`Stop capture` (or the end of `--duration`) calls `.../stop`. The sidecar then
downloads the session ZIP and writes these files into the artifact:

- `llm-events.jsonl`: raw events, one JSON object per line, linked by `requestId`.
- `llm-readable.txt`: a per-request report of the exchange. It covers the
  client request headers, the system prompt, tool definitions, and every
  message (including tool_use and tool_result). For each upstream attempt it
  shows the URL, headers, and Kiro request body, then the upstream response
  headers and every AWS EventStream frame. It also shows the reassembled
  upstream text, reasoning, and tool calls, followed by the SSE or JSON answer
  returned to the client and how the stream ended (completed or client dropped).

Event types: `client_request`, `upstream_request`, `upstream_response`,
`upstream_error_body`, `upstream_frame`, `upstream_body`,
`upstream_decode_error`, `upstream_stream_json_error`, `client_sse`,
`client_response`, `client_stream_end`, `external_upstream_request`,
`external_upstream_response`. Local Kiro, external-pool, and WebSearch/MCP
routes are all covered.

Authorization, x-api-key, Cookie, and similar header values are redacted to
`[redacted] (len=N)` by default. Use `--app-capture-include-secrets` to keep
them. Message content is never redacted.

Capture is idle until started. While idle, the request path only pays one
atomic load. While running, events go through a bounded channel to a writer
thread under `logs/llm-capture/` (override with `KIRO_CAPTURE_DIR`). When the
channel is full, or the byte or event budget is reached, events are dropped and
counted (`droppedEvents`) instead of blocking requests. The service stops a
session on its own after `maxDurationSecs`, even if the sidecar dies.

Known gaps:

- Requests rejected before model resolution and body conversion produce only
  their `client_response` error, not a `client_request`.
- `count_tokens` is not captured.

Drop `--no-pcap` to also run the packet capture described below.

## Run (packet capture)

```sh
python3 tools/kiro-capture-sidecar/server.py \
  --bind 127.0.0.1 --port 19137 \
  --container kiro-rs-2ue-59137-app \
  --target-port 59137 \
  --network-namespace container \
  --upstream-port 443 \
  --max-readable-bytes 536870912
```

Open `http://127.0.0.1:19137/`. Set a bearer token with
`--token-file /path/to/token` before exposing the control port beyond localhost.
For queue and in-flight evidence from the kiro.rs process, configure its
read-only Admin API explicitly with `--admin-url http://127.0.0.1:59137`
and `--admin-token-file /path/to/admin-key`. The sidecar reads only credential
summary/runtime, runtime config, bounded local `usage-records`, and
usage-writer state at capture start, during capture, and at capture end.
External-pool status and external-pool fields are excluded by default so a
local credential investigation does not mix account pools; add
`--include-external-pools` only for an explicit cross-pool investigation. The
Admin key is read from disk and is never copied into the artifact.
The capture is bounded by duration and maximum artifact bytes. The completed
capture list provides both the evidence ZIP and a standalone
`capture-readable.txt` download, so the main report can be opened without
extracting the archive or installing an analysis tool. It combines
`README.txt`, `capture-summary.txt`, `capture-events.txt`, `tcp-flows.txt`,
`runtime-state.txt`, `runtime-timeline.txt`, `usage-window.txt`,
`https-readable.txt`, and `upstream-readable.txt`.

The ZIP also keeps raw evidence such as `manifest.json`, state snapshots,
container logs, bounded `usage-records-local.json`, local
`tool-format-debug/` JSONL diagnostics, command results, and a classic
`traffic.pcap` when packet capture is available. In container namespace mode the pcap captures the target
container's outbound TCP/TLS and UDP/QUIC connections to the configured
upstream port, not the published local port 59137. `traffic.pcap` is a binary
source-of-truth file and is not the primary operator view.

If `--tls-keylog-file` points to an NSS-format key log that covers the captured
TLS sessions and `tshark` is installed on the collector, the sidecar runs
`scripts/diagnostics/decrypt_tls_http2.py` after capture. It writes the
decrypted HTTP/2 request bodies, response bodies, AWS EventStream frame
headers, event names, JSON payloads, decoded HTTP/2 headers, directional
`END_STREAM` observations, and `RST_STREAM`/`GOAWAY` control frames to
`https-readable.txt`. The report keeps request and response `END_STREAM`
separate: one direction ending does not mean the other direction ended.
Absence of an end marker is reported as an observation, not by itself proof
that the upstream failed to finish; capture-window boundaries and stream reset
frames must also be considered. The file can contain complete system prompts,
user messages, tool schemas, tool inputs, tool results, thinking/reasoning
payloads, model output, and authorization headers. Increase
`--max-readable-bytes` when a long capture must preserve every decrypted HTTP/2
stream in `https-readable.txt`; if the limit is reached, `tls-decryption.json`
and the readable report say the output was truncated.
`tls-decryption.json` records whether decryption succeeded, how many HTTP/2
streams and control frames were parsed, and whether a size limit was reached.

Capture ZIPs and their standalone readable reports are created with owner-only
file permissions on the collector host. The content itself remains unredacted
to preserve protocol evidence.

When Admin snapshots are configured, `runtime-timeline.jsonl` and
`runtime-timeline.txt` preserve a bounded timeline of pcap counters, socket
probes, local credential queue/in-flight snapshots, and local usage records
during the capture window. `usage-records-local.json` is the end-of-window
structured view and `usage-window.txt` is its readable summary. The collector
also copies only newly modified local `tool-format-debug/*.jsonl` files from
the target's `/app/logs` mount, bounded by file count and byte limits; lines
identified as external-pool records are excluded. Use that timeline together
with `usage_records` and decrypted HTTP/2
frames to distinguish queue wait, lease occupancy, upstream response body,
EOF/completion, client disconnect, and release evidence. Without Admin
credentials the timeline still records pcap progress and the port probe, but it
cannot prove request-level lease state.

The sidecar cannot create TLS session secrets for an already-running process.
Without a key log covering the captured handshake/session, the HTTPS body
remains encrypted; `https-readable.txt` will state that directly. To produce a
key log, the target runtime must already have its TLS key logging configured
and the sidecar must be able to read the resulting file. The sidecar consumes
that file and does not edit or restart the target container. `tshark` is an
optional system package and is not installed automatically by the Python
collector.

## Full plaintext capture

To capture every recoverable application-level detail, run the sidecar with:

```sh
python3 tools/kiro-capture-sidecar/server.py \
  --bind 0.0.0.0 --port 19137 \
  --container kiro-rs-2ue-59137-app \
  --target-port 59137 \
  --network-namespace container \
  --upstream-port 443 \
  --tls-keylog-file /path/shared/with/kiro/tls-keylog.log \
  --tshark /usr/bin/tshark \
  --usage-record-limit 200 \
  --diagnostic-log-path /app/logs/tool-format-debug \
  --max-readable-bytes 1073741824
```

This can preserve system prompts, user messages, tool schemas, tool inputs,
tool results, thinking/reasoning payloads, model output, response status,
HTTP/2 headers, AWS EventStream frame headers/payloads, `END_STREAM`,
`RST_STREAM`, and `GOAWAY` observations. It still cannot decrypt HTTPS sessions
whose TLS secrets were not written to the key log, and it cannot recover
connections that started before the key log/capture window. If
`tls-decryption.json.status` is `truncated`, raise `--max-readable-bytes` and
capture a new window.

The sidecar starts in `idle` and does not capture until `Start capture` is
pressed. While a capture is running, `Pause capture` freezes only the sidecar's
capture process, `Resume capture` continues it, and `Stop capture` finalizes the
current artifact.

## Deployment boundary

Run this as a separate process/container/project with its own volume and port.
Do not add it to the `kiro-rs` image or replace the `59137` service. A Docker
deployment needs read-only access to `/var/run/docker.sock`, host networking (or
an equivalent capture interface), and `CAP_NET_RAW` for pcap. If those are not
available, the sidecar still collects Docker/log/state evidence and records the
pcap failure in its manifest.
