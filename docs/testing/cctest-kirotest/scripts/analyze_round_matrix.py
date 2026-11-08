#!/usr/bin/env python3
"""为一轮抓包生成脱敏的请求级协议矩阵。

只输出请求 ID、事件类型计数和布尔状态，不复制请求正文、header、凭证或对话文本。
"""

from __future__ import annotations

import collections
import json
import pathlib
import sys
from typing import Any


def parse_sse(text: str) -> list[tuple[str, dict[str, Any]]]:
    events: list[tuple[str, dict[str, Any]]] = []
    for chunk in text.split("\n\n"):
        event_type = None
        payload = None
        for line in chunk.splitlines():
            if line.startswith("event: "):
                event_type = line[7:]
            elif line.startswith("data: "):
                try:
                    payload = json.loads(line[6:])
                except json.JSONDecodeError:
                    payload = None
        if event_type and isinstance(payload, dict):
            events.append((event_type, payload))
    return events


def classify(*, stream: bool, blocks: list[dict[str, Any]], thinking: bool, history_thinking: bool) -> str:
    if not stream:
        return "non_stream"
    if any(block.get("type") == "server_tool_use" for block in blocks):
        return "server_tool"
    if thinking and any(block.get("type") == "tool_use" for block in blocks):
        return "thinking_tool"
    if thinking:
        return "thinking_text"
    if any(block.get("type") == "tool_use" for block in blocks):
        return "tool_continuation" if history_thinking else "tool"
    return "plain_stream"


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"用法：{pathlib.Path(sys.argv[0]).name} EVENTS.JSONL")
    path = pathlib.Path(sys.argv[1])
    events = [json.loads(line) for line in path.open(encoding="utf-8")]
    clients = {
        event["requestId"]: event
        for event in events
        if event.get("type") == "client_request"
    }
    summaries = {
        event.get("data", {}).get("id"): event.get("data", {})
        for event in events
        if event.get("type") == "request_summary"
    }
    client_sse: dict[str, list[str]] = collections.defaultdict(list)
    upstream: dict[str, list[dict[str, Any]]] = collections.defaultdict(list)
    for event in events:
        request_id = event.get("requestId")
        if event.get("type") == "client_sse" and request_id:
            client_sse[request_id].append(event.get("data", {}).get("text", ""))
        if event.get("type") in {
            "upstream_frame",
            "upstream_request",
            "upstream_response",
            "upstream_stream_end",
        } and request_id:
            upstream[request_id].append(event)

    matrix: list[dict[str, Any]] = []
    for request_id, request_event in clients.items():
        body = request_event.get("data", {}).get("rawBody", {}).get("json") or {}
        sse = [item for text in client_sse[request_id] for item in parse_sse(text)]
        sse_counts = collections.Counter(event_type for event_type, _ in sse)
        blocks: list[dict[str, Any]] = []
        thinking_delta_count = 0
        signature_delta_count = 0
        estimated_shapes = collections.Counter()
        noop_keepalive = False
        has_thinking = False
        has_tool = False
        has_server_tool = False
        has_websearch = False
        for event_type, payload in sse:
            if event_type == "content_block_start":
                block = payload.get("content_block") or {}
                block_type = block.get("type")
                block_name = block.get("name")
                blocks.append({"type": block_type, "name": block_name})
                has_thinking |= block_type == "thinking"
                has_tool |= block_type == "tool_use"
                has_server_tool |= block_type == "server_tool_use"
                has_websearch |= block_type == "server_tool_use" and block_name == "web_search"
            elif event_type == "content_block_delta":
                delta = payload.get("delta") or {}
                delta_type = delta.get("type")
                if delta_type == "thinking_delta":
                    thinking_delta_count += 1
                    if "estimated_tokens" not in delta:
                        estimated_shapes["missing"] += 1
                    elif delta["estimated_tokens"] is None:
                        estimated_shapes["null"] += 1
                    else:
                        estimated_shapes["number"] += 1
                    noop_keepalive |= delta.get("thinking", "") == ""
                elif delta_type == "signature_delta":
                    signature_delta_count += 1

        history_thinking = any(
            isinstance(message.get("content"), list)
            and any(
                isinstance(block, dict) and block.get("type") == "thinking"
                for block in message["content"]
            )
            for message in body.get("messages", [])
        )
        status_codes = [
            event.get("data", {}).get("status")
            for event in upstream[request_id]
            if event.get("type") == "upstream_response"
        ]
        upstream_frame_types = collections.Counter(
            event.get("data", {}).get("eventType", "unknown")
            for event in upstream[request_id]
            if event.get("type") == "upstream_frame"
        )
        summary = summaries.get(request_id, {})
        trace = summary.get("latencyTrace") or {}
        client_lifecycle_complete = all(
            sse_counts.get(event_type, 0) == 1
            for event_type in ("message_start", "message_delta", "message_stop")
        )
        matrix.append(
            {
                "request_id": request_id,
                "request_class": classify(
                    stream=bool(body.get("stream")),
                    blocks=blocks,
                    thinking=has_thinking,
                    history_thinking=history_thinking,
                ),
                "stream": bool(body.get("stream")),
                "thinking": has_thinking,
                "signature_delta": signature_delta_count > 0,
                "thinking_delta_count": thinking_delta_count,
                "estimated_tokens_shape": dict(estimated_shapes),
                "noop_keepalive": noop_keepalive,
                "history_thinking": history_thinking,
                "tool_use": has_tool,
                "server_tool_use": has_server_tool,
                "websearch": has_websearch,
                "blocks": blocks,
                "upstream_statuses": status_codes,
                "upstream_request_count": sum(
                    event.get("type") == "upstream_request"
                    for event in upstream[request_id]
                ),
                "upstream_frame_types": dict(upstream_frame_types),
                "upstream_stream_end": any(
                    event.get("type") == "upstream_stream_end"
                    for event in upstream[request_id]
                ),
                "upstream_eof_without_completed": trace.get("upstreamEofWithoutCompleted"),
                "saw_upstream_completed": trace.get("sawUpstreamCompleted"),
                "stop_reason_source": trace.get("stopReasonSource"),
                "terminal_reason": trace.get("terminalReason"),
                "downstream_stop_reason": summary.get("downstreamStopReason"),
                "route_subtype": summary.get("routeSubtype"),
                "client_lifecycle_complete": client_lifecycle_complete,
                "client_sse_counts": dict(sse_counts),
            }
        )

    print(json.dumps({"capture": str(path), "requests": matrix}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
