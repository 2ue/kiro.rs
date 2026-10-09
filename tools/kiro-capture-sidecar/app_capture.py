"""kiro.rs 进程内 LLM 明文采集的客户端与可读报告生成。

kiro.rs 通过 Admin API 暴露采集控制：

- ``GET  /api/admin/diagnostics/capture``
- ``POST /api/admin/diagnostics/capture/start``
- ``POST /api/admin/diagnostics/capture/stop``
- ``GET  /api/admin/diagnostics/capture/{id}/download``

采集结果是 ``events.jsonl``：每行一个事件，``type`` 取值见 manifest.json 的
``eventTypes``。本模块把事件按 requestId 归并，生成可直接阅读的对话报告。
"""

from __future__ import annotations

import json
import pathlib
import shutil
import urllib.error
import urllib.request
import zipfile
from collections import OrderedDict
from typing import Iterable

CAPTURE_API = "/api/admin/diagnostics/capture"


class AdminCaptureError(RuntimeError):
    pass


class AdminCaptureClient:
    def __init__(self, base_url: str, token: str, timeout: float = 30.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout = timeout

    def _request(self, method: str, path: str, body: object | None = None):
        data = None
        headers = {"x-api-key": self.token}
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["content-type"] = "application/json"
        request = urllib.request.Request(
            self.base_url + path, data=data, headers=headers, method=method
        )
        try:
            return urllib.request.urlopen(request, timeout=self.timeout)
        except urllib.error.HTTPError as exc:
            detail = exc.read(64 * 1024).decode("utf-8", errors="replace")
            raise AdminCaptureError(f"{method} {path} -> HTTP {exc.code}: {detail}") from exc
        except (OSError, urllib.error.URLError) as exc:
            raise AdminCaptureError(f"{method} {path} failed: {exc!r}") from exc

    def _json(self, method: str, path: str, body: object | None = None) -> dict:
        with self._request(method, path, body) as response:
            return json.loads(response.read(4 * 1024 * 1024).decode("utf-8"))

    def overview(self) -> dict:
        return self._json("GET", CAPTURE_API)

    def start(
        self,
        max_duration_secs: int,
        max_bytes: int,
        max_events: int | None = None,
        include_secrets: bool = False,
    ) -> dict:
        body: dict[str, object] = {
            "maxDurationSecs": int(max_duration_secs),
            "maxBytes": int(max_bytes),
            "includeSecrets": bool(include_secrets),
        }
        if max_events:
            body["maxEvents"] = int(max_events)
        return self._json("POST", f"{CAPTURE_API}/start", body)

    def stop(self) -> dict:
        return self._json("POST", f"{CAPTURE_API}/stop", {})

    def download(self, session_id: str, destination: pathlib.Path) -> int:
        with self._request("GET", f"{CAPTURE_API}/{session_id}/download") as response:
            with destination.open("wb") as output:
                shutil.copyfileobj(response, output, 1024 * 1024)
        destination.chmod(0o600)
        return destination.stat().st_size


def extract_archive(archive: pathlib.Path, run_dir: pathlib.Path) -> pathlib.Path | None:
    """解出 events.jsonl / manifest.json，返回 events 路径。"""
    with zipfile.ZipFile(archive) as zf:
        names = set(zf.namelist())
        if "events.jsonl" not in names:
            return None
        events_path = run_dir / "llm-events.jsonl"
        with zf.open("events.jsonl") as source, events_path.open("wb") as target:
            shutil.copyfileobj(source, target, 1024 * 1024)
        events_path.chmod(0o600)
        if "manifest.json" in names:
            (run_dir / "llm-capture-manifest.json").write_bytes(zf.read("manifest.json"))
    return events_path


def iter_events(path: pathlib.Path) -> Iterable[dict]:
    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except ValueError:
                continue


# ---------------------------------------------------------------------------
# 可读报告
# ---------------------------------------------------------------------------


def _dump(value: object) -> str:
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=False)


def _one_line(value: object) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def _body_value(body: object) -> object:
    """capture::body_json 的反向展开。"""
    if not isinstance(body, dict):
        return body
    encoding = body.get("encoding")
    if encoding == "json":
        return body.get("json")
    if encoding == "text":
        return body.get("text")
    if encoding == "base64":
        return f"<base64 {body.get('bytes')} bytes>"
    return body


def _indent(text: str, prefix: str = "    ") -> str:
    return "\n".join(prefix + line if line else line for line in text.splitlines())


def _content_blocks_text(content: object) -> str:
    """渲染 Anthropic content（字符串或 block 数组）。"""
    if isinstance(content, str):
        return content
    if not isinstance(content, list):
        return _dump(content)
    parts: list[str] = []
    for block in content:
        if not isinstance(block, dict):
            parts.append(_dump(block))
            continue
        kind = block.get("type")
        if kind == "text":
            parts.append(str(block.get("text", "")))
        elif kind == "thinking":
            parts.append("[thinking]\n" + str(block.get("thinking", "")))
        elif kind == "redacted_thinking":
            parts.append("[redacted_thinking]")
        elif kind == "tool_use":
            parts.append(
                f"[tool_use id={block.get('id')} name={block.get('name')}]\n"
                + _dump(block.get("input"))
            )
        elif kind == "tool_result":
            header = f"[tool_result tool_use_id={block.get('tool_use_id')}"
            if block.get("is_error"):
                header += " is_error=true"
            parts.append(header + "]\n" + _content_blocks_text(block.get("content")))
        elif kind in {"image", "document"}:
            source = block.get("source") if isinstance(block.get("source"), dict) else {}
            data = source.get("data")
            size = len(data) if isinstance(data, str) else 0
            parts.append(
                f"[{kind} media_type={source.get('media_type')} source={source.get('type')} base64_chars={size}]"
            )
        else:
            parts.append(_dump(block))
    return "\n".join(parts)


def _render_client_request(lines: list[str], data: dict) -> None:
    body = _body_value(data.get("rawBody"))
    effective = data.get("effectiveBody")
    lines.append(
        f"客户端请求  route={data.get('route')} endpoint={data.get('endpoint')} "
        f"model={data.get('requestedModel')} upstreamModel={data.get('upstreamModel')} "
        f"stream={data.get('stream')}"
    )
    if data.get("conversationId"):
        lines.append(f"  conversationId={data.get('conversationId')}")
    lines.append("  请求头：")
    lines.append(_indent(_dump(data.get("headers")), "    "))
    if not isinstance(body, dict):
        lines.append("  请求体：")
        lines.append(_indent(str(body), "    "))
        return
    params = {
        key: value
        for key, value in body.items()
        if key not in {"system", "tools", "messages"}
    }
    lines.append("  参数：" + _one_line(params))
    system = body.get("system")
    if system:
        lines.append("  ── 系统提示词 ──")
        lines.append(_indent(_content_blocks_text(system)))
    tools = body.get("tools")
    if isinstance(tools, list) and tools:
        lines.append(f"  ── 工具定义（{len(tools)} 个）──")
        for tool in tools:
            if not isinstance(tool, dict):
                continue
            lines.append(f"    • {tool.get('name') or tool.get('type')}")
            if tool.get("description"):
                lines.append(_indent(str(tool.get("description")), "      "))
            schema = tool.get("input_schema")
            if schema is not None:
                lines.append(_indent("input_schema: " + _dump(schema), "      "))
    messages = body.get("messages")
    if isinstance(messages, list):
        lines.append(f"  ── 消息（{len(messages)} 条）──")
        for index, message in enumerate(messages, 1):
            if not isinstance(message, dict):
                continue
            lines.append(f"    [{index}] {message.get('role')}")
            lines.append(_indent(_content_blocks_text(message.get("content")), "        "))
    if effective is not None:
        lines.append("  （服务端补全后的有效请求体与原始请求体不同，完整内容见 llm-events.jsonl 的 effectiveBody）")


def _frame_payload(data: dict) -> object:
    return _body_value(data.get("payload"))


def _assemble_upstream(frames: list[dict]) -> dict[str, object]:
    """把上游 EventStream 帧拼成文本、推理与工具调用。"""
    text: list[str] = []
    reasoning: list[str] = []
    tools: "OrderedDict[str, dict]" = OrderedDict()
    others: list[str] = []
    for frame in frames:
        event_type = frame.get("eventType")
        payload = _frame_payload(frame)
        if isinstance(payload, dict) and isinstance(payload.get(event_type), dict):
            payload = payload[event_type]
        if not isinstance(payload, dict):
            continue
        if event_type == "assistantResponseEvent":
            text.append(str(payload.get("content", "")))
        elif event_type == "reasoningContentEvent":
            reasoning.append(str(payload.get("text") or payload.get("content") or ""))
        elif event_type == "toolUseEvent":
            key = str(payload.get("toolUseId") or len(tools))
            entry = tools.setdefault(key, {"name": payload.get("name"), "input": []})
            if payload.get("input") is not None:
                value = payload.get("input")
                entry["input"].append(value if isinstance(value, str) else _one_line(value))
            if payload.get("stop"):
                entry["stop"] = True
        elif event_type not in {"metadataEvent", "meteringEvent", "contextUsageEvent"}:
            others.append(f"{event_type}: {_one_line(payload)}")
    return {"text": "".join(text), "reasoning": "".join(reasoning), "tools": tools, "others": others}


def _assemble_sse(chunks: list[str]) -> dict[str, object]:
    """解析返回客户端的 SSE，拼出最终回答。"""
    blocks: "OrderedDict[int, dict]" = OrderedDict()
    events: list[str] = []
    stop_reason = None
    usage = None
    buffer = "".join(chunks)
    for raw_event in buffer.split("\n\n"):
        data_lines = [
            line[5:].lstrip() for line in raw_event.splitlines() if line.startswith("data:")
        ]
        if not data_lines:
            continue
        try:
            payload = json.loads("\n".join(data_lines))
        except ValueError:
            continue
        if not isinstance(payload, dict):
            continue
        kind = payload.get("type")
        events.append(str(kind))
        if kind == "content_block_start":
            block = payload.get("content_block") or {}
            blocks[int(payload.get("index", len(blocks)))] = {
                "type": block.get("type"),
                "name": block.get("name"),
                "id": block.get("id"),
                "text": [],
            }
        elif kind == "content_block_delta":
            index = int(payload.get("index", 0))
            entry = blocks.setdefault(index, {"type": "unknown", "text": []})
            delta = payload.get("delta") or {}
            for key in ("text", "thinking", "partial_json"):
                if key in delta:
                    entry["text"].append(str(delta[key]))
        elif kind == "message_delta":
            stop_reason = (payload.get("delta") or {}).get("stop_reason", stop_reason)
            usage = payload.get("usage", usage)
    return {"blocks": blocks, "events": events, "stop_reason": stop_reason, "usage": usage}


def render_readable(events_path: pathlib.Path, max_bytes: int | None = None) -> str:
    all_events = list(iter_events(events_path))
    ingress_to_request: dict[str, str] = {}
    for event in all_events:
        if event.get("type") != "http_egress":
            continue
        data = event.get("data") if isinstance(event.get("data"), dict) else {}
        ingress_id = data.get("ingressId")
        request_id = event.get("requestId") or data.get("requestId")
        if ingress_id and request_id:
            ingress_to_request[str(ingress_id)] = str(request_id)

    requests: "OrderedDict[str, list[dict]]" = OrderedDict()
    orphan: list[dict] = []
    type_counts: dict[str, int] = {}
    for event in all_events:
        kind = str(event.get("type"))
        type_counts[kind] = type_counts.get(kind, 0) + 1
        request_id = event.get("requestId")
        if not request_id:
            data = event.get("data") if isinstance(event.get("data"), dict) else {}
            ingress_id = data.get("ingressId")
            request_id = ingress_to_request.get(str(ingress_id)) if ingress_id else None
        if request_id:
            requests.setdefault(str(request_id), []).append(event)
        else:
            orphan.append(event)

    lines: list[str] = [
        "LLM 明文采集报告（kiro.rs 进程内采集）",
        "======================================",
        f"请求数：{len(requests)}",
        "事件统计：" + "，".join(f"{key}={value}" for key, value in sorted(type_counts.items())),
        "",
        "每个请求依次列出：客户端原始请求（请求头、系统提示词、工具定义、全部消息）→",
        "发往上游的每次尝试（URL、请求头、Kiro 协议请求体）→ 上游响应头与 EventStream 帧 →",
        "返回客户端的最终回答。完整原始数据见 llm-events.jsonl。",
        "",
    ]
    for request_id, events in requests.items():
        events.sort(key=lambda item: item.get("seq") or 0)
        first_ts = events[0].get("ts")
        last_ts = events[-1].get("ts")
        lines.append("#" * 72)
        lines.append(f"请求 {request_id}    {first_ts} → {last_ts}")
        lines.append("#" * 72)
        frames: list[dict] = []
        sse_chunks: list[str] = []
        external_chunks: list[dict] = []
        for event in events:
            kind = event.get("type")
            data = event.get("data") if isinstance(event.get("data"), dict) else {}
            if kind == "http_ingress":
                lines.append("")
                lines.append(
                    f"HTTP 入口  {data.get('method')} {data.get('path')}"
                    + (f"?{data.get('query')}" if data.get("query") else "")
                    + f" ingressId={data.get('ingressId')}"
                )
                lines.append("  请求头：")
                lines.append(_indent(_dump(data.get("headers")), "    "))
                if data.get("bodyTruncated"):
                    lines.append(f"  请求体已截断，大小={data.get('bodyBytes')}")
                else:
                    lines.append("  请求体：")
                    lines.append(_indent(_dump(_body_value(data.get("body"))), "    "))
            elif kind == "http_egress":
                lines.append(
                    f"HTTP 出口  status={data.get('status')} "
                    f"ingressId={data.get('ingressId')} requestId={data.get('requestId')}"
                )
                lines.append(_indent(_dump(data.get("headers")), "    "))
            elif kind == "client_request":
                _render_client_request(lines, data)
            elif kind in {"upstream_request", "external_upstream_request"}:
                lines.append("")
                lines.append(
                    f"上游请求  {kind} phase={data.get('phase', data.get('poolId'))} "
                    f"attempt={data.get('attempt')} credentialId={data.get('credentialId')}"
                )
                lines.append(f"  {data.get('method')} {data.get('url')}")
                lines.append("  请求头：")
                lines.append(_indent(_dump(data.get("headers")), "    "))
                lines.append("  请求体：")
                lines.append(_indent(_dump(_body_value(data.get("body")))))
            elif kind in {"upstream_response", "external_upstream_response"}:
                lines.append(
                    f"上游响应  status={data.get('status')} {data.get('httpVersion')} "
                    f"headerWait={data.get('headerWaitMs')}ms"
                )
                lines.append(_indent(_dump(data.get("headers")), "    "))
            elif kind == "upstream_error_body":
                lines.append(f"上游错误响应体  status={data.get('status')}")
                lines.append(_indent(_dump(_body_value(data.get("body")))))
            elif kind == "upstream_frame":
                frames.append(data)
            elif kind == "external_upstream_chunk":
                external_chunks.append(data)
            elif kind in {"external_upstream_body", "mcp_response_body"}:
                lines.append(f"{kind}  status={data.get('status')}:")
                lines.append(_indent(_dump(_body_value(data.get("body")))))
            elif kind in {"upstream_body", "upstream_decode_error", "upstream_stream_json_error"}:
                lines.append(f"{kind}：")
                lines.append(_indent(_dump(data)))
            elif kind == "client_sse":
                sse_chunks.append(str(data.get("text", "")))
            elif kind == "client_response":
                lines.append("")
                lines.append(f"返回客户端（{data.get('kind')}） status={data.get('status')}")
                body = data.get("body")
                body = _body_value(body) if isinstance(body, dict) and "encoding" in body else body
                if isinstance(body, dict) and isinstance(body.get("content"), list):
                    lines.append(_indent(_content_blocks_text(body.get("content"))))
                    rest = {key: value for key, value in body.items() if key != "content"}
                    lines.append("  " + _one_line(rest))
                else:
                    lines.append(_indent(_dump(body)))
            elif kind == "client_stream_end":
                lines.append(
                    f"客户端流结束  outcome={data.get('outcome')} chunks={data.get('chunks')} "
                    f"bytes={data.get('bytes')} elapsed={data.get('elapsedMs')}ms"
                )
            elif kind == "upstream_stream_end":
                lines.append(
                    f"上游流结束  reason={data.get('reason')} attempt={data.get('attempt')} "
                    f"frames={data.get('framesDecoded')} pending={data.get('pendingBytes')} "
                    f"downstreamCommitted={data.get('downstreamCommitted')}"
                )
                if data.get("detail"):
                    lines.append(_indent(str(data.get("detail")), "    "))
            elif kind == "count_tokens_result":
                lines.append(
                    f"count_tokens  model={data.get('model')} inputTokens={data.get('inputTokens')} "
                    f"calculation={data.get('calculation')}"
                )
            elif kind in {"count_tokens_upstream_request", "count_tokens_upstream_response"}:
                lines.append(f"{kind}：")
                lines.append(_indent(_dump(data)))
            elif kind == "request_summary":
                lines.append("请求汇总：")
                lines.append(_indent(_dump(data)))
        if frames:
            lines.append("")
            lines.append(f"上游 EventStream 帧（{len(frames)} 个）：")
            for index, frame in enumerate(frames, 1):
                lines.append(
                    f"  [{index}] attempt={frame.get('attempt')} {frame.get('messageType')}/"
                    f"{frame.get('eventType')} {_one_line(_frame_payload(frame))}"
                )
            assembled = _assemble_upstream(frames)
            if assembled["reasoning"]:
                lines.append("  ── 上游推理内容（拼接）──")
                lines.append(_indent(str(assembled["reasoning"])))
            if assembled["text"]:
                lines.append("  ── 上游回答文本（拼接）──")
                lines.append(_indent(str(assembled["text"])))
            for tool_id, tool in assembled["tools"].items():
                lines.append(f"  ── 上游工具调用 {tool.get('name')} ({tool_id}) ──")
                lines.append(_indent("".join(tool.get("input") or [])))
            for other in assembled["others"]:
                lines.append(f"  其他事件 {other}")
        if external_chunks:
            external_chunks.sort(key=lambda item: item.get("index") or 0)
            raw = "".join(
                str(chunk.get("text", ""))
                if chunk.get("text") is not None
                else f"<base64 {chunk.get('bytes')} bytes> {chunk.get('base64', '')}"
                for chunk in external_chunks
            )
            lines.append("")
            lines.append(f"外部号池上游原始 SSE（{len(external_chunks)} 个分块）：")
            lines.append(_indent(raw))
        if sse_chunks:
            sse = _assemble_sse(sse_chunks)
            lines.append("")
            lines.append(
                f"返回客户端（SSE） stop_reason={sse['stop_reason']} usage={_one_line(sse['usage'])}"
            )
            for index, block in sse["blocks"].items():
                header = f"  [block {index}] {block.get('type')}"
                if block.get("name"):
                    header += f" name={block.get('name')} id={block.get('id')}"
                lines.append(header)
                lines.append(_indent("".join(block.get("text") or []), "      "))
            lines.append("  SSE 事件序列：" + " ".join(sse["events"]))
        lines.append("")
    if orphan:
        lines.append("未关联请求 ID 的事件：")
        for event in orphan:
            lines.append("  " + _one_line(event))
    text = "\n".join(lines) + "\n"
    if max_bytes is not None and len(text.encode("utf-8")) > max_bytes:
        encoded = text.encode("utf-8")[:max_bytes]
        text = encoded.decode("utf-8", errors="ignore") + (
            f"\n\n[已截断：可读报告超过 {max_bytes} 字节上限；完整内容见 llm-events.jsonl]\n"
        )
    return text
