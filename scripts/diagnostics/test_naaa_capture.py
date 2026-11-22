#!/usr/bin/env python3
"""Validate the Claude Code probe capture without printing prompt or response bodies."""

from __future__ import annotations

import argparse
import collections
import json
import os
import re
import sys
import uuid
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


SESSION_HEADER = "x-claude-code-session-id"
ROUTE = "/dfcache/naaa/v1/messages"
INTEGER_PROMPT = re.compile(r"(?:给出|选择|出|有)\s*(\d+)\s*个\s*1\s*到\s*355")
INTEGER_PROMPT_ASCII = re.compile(r"\b(\d+)\s+(?:integers|numbers)\b", re.IGNORECASE)
INTEGER_RE = re.compile(r"(?<![\w.])\d+(?![\w.])")


class CaptureError(RuntimeError):
    pass


def header(row: dict[str, Any], name: str) -> str | None:
    wanted = name.lower()
    for key, value in row.get("requestHeaders", []):
        if str(key).lower() == wanted:
            return str(value)
    return None


def walk_text(value: Any) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return " ".join(walk_text(item) for item in value)
    if isinstance(value, dict):
        return " ".join(walk_text(item) for item in value.values())
    return ""


def request_text(request: dict[str, Any]) -> str:
    return walk_text(request.get("messages", []))


def response_text(response: dict[str, Any]) -> str:
    body = response.get("body")
    if isinstance(body, dict):
        return walk_text(body.get("content", []))
    chunks: list[str] = []
    for event in response.get("events", []):
        data = event.get("data", {})
        delta = data.get("delta", {}) if isinstance(data, dict) else {}
        if delta.get("type") == "text_delta":
            chunks.append(str(delta.get("text", "")))
    return "".join(chunks)


def all_content_blocks(request: dict[str, Any]) -> list[dict[str, Any]]:
    blocks: list[dict[str, Any]] = []
    for message in request.get("messages", []):
        content = message.get("content")
        if isinstance(content, list):
            blocks.extend(block for block in content if isinstance(block, dict))
    return blocks


def tool_ids(request: dict[str, Any]) -> tuple[list[str], list[str]]:
    uses: list[str] = []
    results: list[str] = []
    for block in all_content_blocks(request):
        block_type = block.get("type")
        if block_type == "tool_use" and isinstance(block.get("id"), str):
            uses.append(block["id"])
        if block_type == "tool_result" and isinstance(block.get("tool_use_id"), str):
            results.append(block["tool_use_id"])
    return uses, results


def response_event_names(response: dict[str, Any]) -> list[str]:
    return [str(event.get("event")) for event in response.get("events", [])]


def parse_sse_response(status: int, headers: Any, payload: bytes) -> dict[str, Any]:
    """Convert an Anthropic SSE response into the capture response shape."""
    events: list[dict[str, Any]] = []
    event_name: str | None = None
    data_lines: list[str] = []

    def flush_event() -> None:
        nonlocal event_name, data_lines
        if event_name is None or not data_lines:
            event_name = None
            data_lines = []
            return
        data = "\n".join(data_lines)
        if data != "[DONE]":
            try:
                value = json.loads(data)
            except json.JSONDecodeError:
                value = {"raw": data}
            events.append({"event": event_name, "data": value})
        event_name = None
        data_lines = []

    for line in payload.decode("utf-8", errors="replace").splitlines():
        if not line:
            flush_event()
        elif line.startswith("event:"):
            event_name = line[6:].strip()
        elif line.startswith("data:"):
            data_lines.append(line[5:].lstrip())
    flush_event()
    return {
        "status": status,
        "headers": dict(headers.items()) if headers is not None else {},
        "events": events,
    }


def parse_live_response(status: int, headers: Any, payload: bytes) -> dict[str, Any]:
    content_type = str(headers.get("content-type", "")) if headers is not None else ""
    if "text/event-stream" in content_type or b"event:" in payload[:256]:
        return parse_sse_response(status, headers, payload)
    try:
        body: Any = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        body = {"type": "error", "raw": payload.decode("utf-8", errors="replace")}
    return {
        "status": status,
        "headers": dict(headers.items()) if headers is not None else {},
        "body": body,
    }


def request_bytes(request: dict[str, Any]) -> bytes:
    return json.dumps(request, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def replay_live_request(
    request: dict[str, Any],
    endpoint: str,
    api_key: str,
    session_id: str | None,
    timeout: float,
) -> tuple[dict[str, Any], int]:
    payload = request_bytes(request)
    headers = {
        "accept": "text/event-stream" if request.get("stream") is True else "application/json",
        "content-type": "application/json",
        "anthropic-version": "2023-06-01",
        "x-api-key": api_key,
        "user-agent": "naaa-capture-live-replay/1",
    }
    if session_id:
        headers[SESSION_HEADER] = session_id
    http_request = Request(endpoint, data=payload, headers=headers, method="POST")
    try:
        with urlopen(http_request, timeout=timeout) as response:
            status = int(response.status)
            response_headers = response.headers
            response_payload = response.read()
    except HTTPError as exc:
        status = int(exc.code)
        response_headers = exc.headers
        response_payload = exc.read()
    except (OSError, URLError) as exc:
        return {
            "status": 0,
            "headers": {},
            "body": {
                "type": "error",
                "error": {"type": "transport_error", "message": str(exc)},
            },
        }, len(payload)
    return parse_live_response(status, response_headers, response_payload), len(payload)


def integer_request_count(text: str) -> int | None:
    match = INTEGER_PROMPT.search(text) or INTEGER_PROMPT_ASCII.search(text)
    return int(match.group(1)) if match else None


def integer_response_values(text: str) -> list[int]:
    return [int(value) for value in INTEGER_RE.findall(text)]


def validate_row(row: dict[str, Any], errors: list[str]) -> None:
    serial = row.get("serial", "?")
    if row.get("method") != "POST" or row.get("uri") != ROUTE:
        errors.append(f"#{serial}: unexpected request route")
    if row.get("bodyComplete") is not True:
        errors.append(f"#{serial}: request body is incomplete")
    if row.get("contentLength") != row.get("requestBodyBytes"):
        errors.append(f"#{serial}: Content-Length does not match body bytes")

    response = row.get("response") or {}
    if response.get("status") != 200:
        errors.append(f"#{serial}: response status is {response.get('status')!r}")
    request = row.get("request") or {}
    # Claude Code omits `stream` on the four non-stream arithmetic probes.
    if request.get("stream") is not True:
        body = response.get("body")
        if not isinstance(body, dict) or body.get("type") != "message":
            errors.append(f"#{serial}: non-stream response is not an Anthropic message")
        return

    names = response_event_names(response)
    if not names or names[0] != "message_start" or names[-1] != "message_stop":
        errors.append(f"#{serial}: stream does not start/end with message_start/message_stop")
    if "message_delta" not in names:
        errors.append(f"#{serial}: stream has no final message_delta")
    if names.count("message_start") != 1 or names.count("message_stop") != 1:
        errors.append(f"#{serial}: stream has duplicate terminal events")

    start = next(
        (event.get("data", {}) for event in response.get("events", []) if event.get("event") == "message_start"),
        {},
    )
    usage = start.get("message", {}).get("usage", {}) if isinstance(start, dict) else {}
    if not isinstance(usage.get("input_tokens"), int) or usage["input_tokens"] <= 0:
        errors.append(f"#{serial}: message_start input_tokens is not positive")
    delta_events = [
        event.get("data", {})
        for event in response.get("events", [])
        if event.get("event") == "message_delta"
    ]
    final_usage = delta_events[-1].get("usage", {}) if delta_events else {}
    if not isinstance(final_usage.get("output_tokens"), int) or final_usage["output_tokens"] < 0:
        errors.append(f"#{serial}: final output_tokens is missing")


def validate_session(session_id: str, rows: list[dict[str, Any]], errors: list[str]) -> dict[str, Any]:
    if len(rows) != 23:
        errors.append(f"session {session_id}: expected 23 requests, got {len(rows)}")

    probes = {
        "logic": any("逻辑网格题" in request_text(row["request"]) for row in rows),
        "antml": any("<antml:" in request_text(row["request"]) for row in rows),
        "web_search": any("web_search" in request_text(row["request"]) for row in rows),
        "cutoff": any("training data cutoff" in request_text(row["request"]) for row in rows),
        "code_execution": any("HELLO_CHECK" in request_text(row["request"]) for row in rows),
        "arithmetic": any("1+1" in request_text(row["request"]) for row in rows),
        "identity": any("Who exactly are you?" in request_text(row["request"]) for row in rows),
    }
    missing = [name for name, present in probes.items() if not present]
    if missing:
        errors.append(f"session {session_id}: missing probes {','.join(missing)}")

    integer_counts = [
        integer_request_count(request_text(row["request"]))
        for row in rows
    ]
    integer_counts = [count for count in integer_counts if count is not None]
    if len(integer_counts) != 3:
        errors.append(f"session {session_id}: expected 3 integer probes, got {len(integer_counts)}")

    documents = [
        block
        for row in rows
        for block in all_content_blocks(row["request"])
        if block.get("type") == "document"
    ]
    images = [
        block
        for row in rows
        for block in all_content_blocks(row["request"])
        if block.get("type") == "image"
    ]
    if len(documents) != 1:
        errors.append(f"session {session_id}: expected one document block, got {len(documents)}")
    if len(images) != 1:
        errors.append(f"session {session_id}: expected one image block, got {len(images)}")
    if documents and documents[0].get("source", {}).get("media_type") != "application/pdf":
        errors.append(f"session {session_id}: document is not application/pdf")
    if images and images[0].get("source", {}).get("type") not in {"base64", "url"}:
        errors.append(f"session {session_id}: image source is not base64/url")

    continuations = [row for row in rows if len(row["request"].get("messages", [])) > 1]
    expected_history = list(range(3, 22, 2))
    actual_history = [len(row["request"].get("messages", [])) for row in continuations]
    if actual_history != expected_history:
        errors.append(f"session {session_id}: history lengths {actual_history!r} != {expected_history!r}")

    tool_rounds = []
    for row in continuations:
        uses, results = tool_ids(row["request"])
        if not results or not set(results).issubset(set(uses)):
            errors.append(f"#{row.get('serial')}: tool_result is not paired with an earlier tool_use")
        if len(uses) != len(results):
            errors.append(f"#{row.get('serial')}: tool_use/tool_result counts differ")
        tool_rounds.append((len(uses), len(results)))
    if tool_rounds != [(index, index) for index in range(1, 11)]:
        errors.append(f"session {session_id}: tool round counts {tool_rounds!r}")

    return {
        "requests": len(rows),
        "integer_counts": integer_counts,
        "continuation_history": actual_history,
        "tool_rounds": tool_rounds,
        "non_stream_requests": sum(row["request"].get("stream") is not True for row in rows),
    }


def load_rows(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        raise CaptureError(f"cannot read {path}: {exc}") from exc
    for line_number, line in enumerate(lines, start=1):
        try:
            value = json.loads(line)
        except json.JSONDecodeError as exc:
            raise CaptureError(f"line {line_number}: invalid JSON: {exc}") from exc
        if not isinstance(value, dict):
            raise CaptureError(f"line {line_number}: JSON value is not an object")
        rows.append(value)
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", type=Path, help="JSONL capture exported by the naaa route")
    parser.add_argument(
        "--strict-number-counts",
        action="store_true",
        help="fail when a numeric probe response count/range differs from its prompt",
    )
    parser.add_argument(
        "--live",
        action="store_true",
        help="replay request bodies to a live Anthropic-compatible endpoint",
    )
    parser.add_argument(
        "--live-endpoint",
        default="http://127.0.0.1:19023/cc/v1/messages",
        help="live endpoint used with --live",
    )
    parser.add_argument(
        "--api-key-env",
        default="ANTHROPIC_API_KEY",
        help="environment variable containing the live endpoint request key",
    )
    parser.add_argument(
        "--live-timeout",
        type=float,
        default=300.0,
        help="per-request live timeout in seconds",
    )
    parser.add_argument(
        "--live-model",
        help="with --live, override only the request model while keeping the original prompt",
    )
    parser.add_argument(
        "--live-serial",
        type=int,
        action="append",
        help="with --live, replay only this capture serial (repeatable)",
    )
    args = parser.parse_args()

    try:
        rows = load_rows(args.capture)
    except CaptureError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 2

    errors: list[str] = []
    if len(rows) != 92:
        errors.append(f"expected 92 requests, got {len(rows)}")
    sessions: dict[str, list[dict[str, Any]]] = collections.defaultdict(list)
    for row in rows:
        session_id = header(row, SESSION_HEADER)
        if not session_id:
            errors.append(f"#{row.get('serial', '?')}: missing {SESSION_HEADER}")
            continue
        sessions[session_id].append(row)
        if not args.live:
            validate_row(row, errors)

    if len(sessions) != 4:
        errors.append(f"expected 4 sessions, got {len(sessions)}")

    summaries: dict[str, dict[str, Any]] = {}
    for session_id, session_rows in sessions.items():
        summaries[session_id] = validate_session(session_id, session_rows, errors)

    evaluated_rows = rows
    if args.live:
        api_key = os.environ.get(args.api_key_env)
        if not api_key:
            errors.append(f"--live requires a non-empty ${args.api_key_env}")
            evaluated_rows = []
        else:
            selected_serials = set(args.live_serial or [])
            session_ids = {session_id: str(uuid.uuid4()) for session_id in sessions}
            evaluated_rows = []
            for row in rows:
                serial = int(row.get("serial", 0))
                if selected_serials and serial not in selected_serials:
                    continue
                request = row.get("request")
                if not isinstance(request, dict):
                    errors.append(f"#{serial}: request body is not an object")
                    continue
                live_request = dict(request)
                if args.live_model:
                    live_request["model"] = args.live_model
                captured_session = header(row, SESSION_HEADER)
                response, body_bytes = replay_live_request(
                    live_request,
                    args.live_endpoint,
                    api_key,
                    session_ids.get(captured_session or ""),
                    args.live_timeout,
                )
                live_row = dict(row)
                live_row["requestBodyBytes"] = body_bytes
                live_row["contentLength"] = body_bytes
                live_row["bodyComplete"] = True
                live_row["request"] = live_request
                live_row["response"] = response
                evaluated_rows.append(live_row)
                validate_row(live_row, errors)
                print(
                    f"live=#{serial} status={response.get('status')} "
                    f"events={len(response.get('events', []))}",
                    file=sys.stderr,
                )
            if selected_serials:
                seen_serials = {int(row.get("serial", 0)) for row in evaluated_rows}
                errors.extend(
                    f"live serial not found: {serial}"
                    for serial in sorted(selected_serials - seen_serials)
                )
            if not evaluated_rows and not errors:
                errors.append("--live selected no requests")

    number_mismatches: list[tuple[int, int, int, int]] = []
    for row in evaluated_rows:
        expected = integer_request_count(request_text(row["request"]))
        if expected is None:
            continue
        values = integer_response_values(response_text(row["response"]))
        out_of_range = [value for value in values if not 1 <= value <= 355]
        if len(values) != expected or out_of_range:
            number_mismatches.append((row["serial"], expected, len(values), len(out_of_range)))

    print(f"capture={args.capture} requests={len(rows)} sessions={len(sessions)}")
    for session_id, summary in summaries.items():
        print(
            f"session={session_id} requests={summary['requests']} "
            f"integer_counts={summary['integer_counts']} "
            f"history={summary['continuation_history']} "
            f"non_stream={summary['non_stream_requests']}"
        )
    if number_mismatches:
        print(f"number_probe_mismatches={number_mismatches}")
        if args.strict_number_counts:
            errors.extend(
                f"#{serial}: expected {expected} integers, got {actual} ({out_of_range} out of range)"
                for serial, expected, actual, out_of_range in number_mismatches
            )

    if errors:
        print("FAIL:")
        for error in errors:
            print(f"- {error}")
        return 1
    print("PASS: naaa capture protocol contract")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
