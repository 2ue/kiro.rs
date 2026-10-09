#!/usr/bin/env python3
"""Export readable HTTP/2 bodies and AWS EventStream frames from a TLS pcap.

TLS decryption is performed by Wireshark's tshark using an existing NSS-format
TLS key log. This script never changes the target process or its traffic.
"""

from __future__ import annotations

import argparse
import base64
import csv
import datetime as dt
import gzip
import io
import json
import pathlib
import shutil
import struct
import subprocess
import tempfile
import threading
import time
import zlib
from typing import Any


TSHARK_AGGREGATOR = "\x1f"


TSHARK_FIELDS = (
    "frame.time_epoch",
    "frame.number",
    "ip.src",
    "ipv6.src",
    "tcp.srcport",
    "ip.dst",
    "ipv6.dst",
    "tcp.dstport",
    "tcp.stream",
    "http2.type",
    "http2.streamid",
    "http2.flags",
    "http2.headers.method",
    "http2.headers.path",
    "http2.headers.authority",
    "http2.headers.scheme",
    "http2.headers.authorization",
    "http2.headers.accept",
    "http2.headers.content_type",
    "http2.headers.content_encoding",
    "http2.headers.content_length",
    "http2.headers.user_agent",
    "http2.headers.server",
    "http2.headers.status",
    "http2.data.data",
    "http2.flags.end_stream",
    "http2.rst_stream.error",
    "http2.goaway.last_stream_id",
    "http2.goaway.error",
    "http2.goaway.addata",
    "http2.settings.id",
    "http2.settings.max_concurrent_streams",
    "http2.settings.initial_window_size",
    "http2.window_update.window_size_increment",
    "http2.header.name",
    "http2.header.value",
)

HTTP2_CONTROL_TYPES = {
    "2": "PRIORITY",
    "3": "RST_STREAM",
    "4": "SETTINGS",
    "5": "PUSH_PROMISE",
    "6": "PING",
    "7": "GOAWAY",
    "8": "WINDOW_UPDATE",
    "9": "CONTINUATION",
    "10": "ALTSVC",
    "11": "BLOCKED",
}


def utc_timestamp(value: str) -> str:
    try:
        timestamp = float(value)
        return (
            dt.datetime.fromtimestamp(timestamp, dt.timezone.utc)
            .isoformat(timespec="milliseconds")
            .replace("+00:00", "Z")
        )
    except (ValueError, OverflowError, OSError):
        return value or "时间未知"


def decode_hex_bytes(value: str) -> bytes:
    normalized = value.strip().replace(":", "").replace(" ", "")
    if not normalized:
        return b""
    try:
        return bytes.fromhex(normalized)
    except ValueError:
        return b""


def parse_tshark_rows(output: str) -> list[dict[str, str]]:
    rows: list[dict[str, str]] = []
    for values in csv.reader(output.splitlines(), delimiter="\t", quotechar='"'):
        if not values or all(not value for value in values):
            continue
        if len(values) < len(TSHARK_FIELDS):
            values.extend([""] * (len(TSHARK_FIELDS) - len(values)))
        rows.append(dict(zip(TSHARK_FIELDS, values, strict=False)))
    return rows


def aws_eventstream_header_values(headers: bytes) -> dict[str, str]:
    values: dict[str, str] = {}
    offset = 0
    type_names = {
        0: "true",
        1: "false",
        2: "byte",
        3: "short",
        4: "int",
        5: "long",
        6: "bytes",
        7: "string",
        8: "timestamp",
        9: "uuid",
    }
    fixed_sizes = {2: 1, 3: 2, 4: 4, 5: 8, 8: 8, 9: 16}
    while offset < len(headers):
        if offset + 1 > len(headers):
            break
        name_length = headers[offset]
        offset += 1
        if name_length == 0 or offset + name_length + 1 > len(headers):
            break
        name = headers[offset : offset + name_length].decode("utf-8", "replace")
        offset += name_length
        value_type = headers[offset]
        offset += 1
        if value_type in (0, 1):
            value = type_names[value_type]
        elif value_type in fixed_sizes:
            size = fixed_sizes[value_type]
            if offset + size > len(headers):
                break
            raw = headers[offset : offset + size]
            offset += size
            value = raw.hex() if value_type == 9 else str(int.from_bytes(raw, "big", signed=True))
        elif value_type in (6, 7):
            if offset + 2 > len(headers):
                break
            size = struct.unpack_from("!H", headers, offset)[0]
            offset += 2
            if offset + size > len(headers):
                break
            raw = headers[offset : offset + size]
            offset += size
            value = raw.decode("utf-8", "replace") if value_type == 7 else raw.hex()
        else:
            # Unknown header value encoding means the remainder cannot be
            # interpreted safely. Preserve the frame payload elsewhere.
            break
        values[name] = value
    return values


def decode_aws_eventstream(body: bytes) -> tuple[list[dict[str, Any]], str | None]:
    frames: list[dict[str, Any]] = []
    offset = 0
    while offset < len(body):
        remaining = len(body) - offset
        if remaining < 16:
            return frames, f"末尾还剩 {remaining} 字节，不足一个 EventStream 帧"
        total_length, headers_length, prelude_crc = struct.unpack_from("!III", body, offset)
        if total_length < 16 or total_length > remaining:
            return frames, (
                f"帧长度异常：声明 {total_length} 字节，当前位置剩余 {remaining} 字节"
            )
        frame_end = offset + total_length
        message_crc = struct.unpack_from("!I", body, frame_end - 4)[0]
        actual_prelude_crc = zlib.crc32(body[offset : offset + 8]) & 0xFFFFFFFF
        actual_message_crc = zlib.crc32(body[offset : frame_end - 4]) & 0xFFFFFFFF
        if prelude_crc != actual_prelude_crc:
            return frames, f"第 {len(frames) + 1} 帧 Prelude CRC 不匹配"
        if message_crc != actual_message_crc:
            return frames, f"第 {len(frames) + 1} 帧 Message CRC 不匹配"
        headers_start = offset + 12
        headers_end = headers_start + headers_length
        if headers_end > frame_end - 4:
            return frames, f"第 {len(frames) + 1} 帧头部长度越界"
        payload = body[headers_end : frame_end - 4]
        headers = aws_eventstream_header_values(body[headers_start:headers_end])
        try:
            payload_value: Any = json.loads(payload)
            payload_text = json.dumps(payload_value, ensure_ascii=False, indent=2)
        except (UnicodeDecodeError, json.JSONDecodeError):
            payload_text = payload.decode("utf-8", "replace")
            payload_value = None
        frames.append(
            {
                "index": len(frames) + 1,
                "bytes": total_length,
                "headers": headers,
                "payload_bytes": len(payload),
                "payload": payload_value,
                "payload_text": payload_text,
            }
        )
        offset = frame_end
    return frames, None


def render_body(body: bytes) -> str:
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError:
        return (
            "[正文不是 UTF-8；以下为 Base64 原文]\n"
            + base64.b64encode(body).decode("ascii")
        )
    try:
        value = json.loads(text)
    except json.JSONDecodeError:
        return text
    return json.dumps(value, ensure_ascii=False, indent=2)


def decode_http_body(body: bytes, content_encoding: str) -> tuple[bytes, str | None]:
    encoding = content_encoding.strip().lower()
    if not encoding or encoding == "identity":
        return body, None
    max_decoded_bytes = 64 * 1024 * 1024
    try:
        if encoding == "gzip":
            with gzip.GzipFile(fileobj=io.BytesIO(body)) as compressed:
                decoded = compressed.read(max_decoded_bytes + 1)
        elif encoding == "deflate":
            decompressor = zlib.decompressobj()
            decoded = decompressor.decompress(body, max_decoded_bytes + 1)
            if decompressor.unconsumed_tail:
                return body, "解压后正文超过 64 MiB 上限，保留压缩原文"
        else:
            return body, f"暂不支持解压 Content-Encoding: {content_encoding}"
    except (OSError, EOFError, zlib.error) as exc:
        return body, f"HTTP 正文解压失败：{exc}"
    if len(decoded) > max_decoded_bytes:
        return body, "解压后正文超过 64 MiB 上限，保留压缩原文"
    return decoded, None


def run_tshark(
    tshark: str,
    pcap: pathlib.Path,
    keylog: pathlib.Path,
    timeout: float,
    max_output_bytes: int,
) -> subprocess.CompletedProcess[str]:
    command = [
        tshark,
        "-r",
        str(pcap),
        "-2",
        "-o",
        f"tls.keylog_file:{keylog}",
        "-Y",
        "http2",
        "-T",
        "fields",
        "-E",
        "separator=/t",
        "-E",
        "quote=d",
        "-E",
        f"aggregator={TSHARK_AGGREGATOR}",
        "-E",
        "occurrence=a",
    ]
    for field in TSHARK_FIELDS:
        command.extend(["-e", field])
    with tempfile.TemporaryFile() as stdout_file, tempfile.TemporaryFile() as stderr_file:
        process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=stderr_file,
        )
        output_state = {"bytes": 0, "limited": False}

        def copy_bounded_stdout() -> None:
            assert process.stdout is not None
            while True:
                chunk = process.stdout.read(64 * 1024)
                if not chunk:
                    return
                remaining = max_output_bytes - output_state["bytes"]
                if len(chunk) > remaining:
                    if remaining > 0:
                        stdout_file.write(chunk[:remaining])
                    output_state["bytes"] = max_output_bytes
                    output_state["limited"] = True
                    process.kill()
                    return
                stdout_file.write(chunk)
                output_state["bytes"] += len(chunk)

        stdout_thread = threading.Thread(
            target=copy_bounded_stdout,
            name="tshark-output-reader",
            daemon=True,
        )
        stdout_thread.start()
        deadline = time.monotonic() + timeout
        timed_out = False
        while process.poll() is None and not output_state["limited"]:
            if time.monotonic() >= deadline:
                timed_out = True
                process.kill()
                break
            time.sleep(0.05)
        returncode = process.wait()
        stdout_thread.join(timeout=5)
        if stdout_thread.is_alive() and process.stdout is not None:
            process.stdout.close()
            stdout_thread.join(timeout=1)
        if output_state["limited"]:
            raise RuntimeError(
                f"tshark 输出超过 {max_output_bytes} 字节上限，已终止解密以保护采集器资源"
            )
        if timed_out:
            raise subprocess.TimeoutExpired(command, timeout)
        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = stdout_file.read().decode("utf-8", "replace")
        stderr = stderr_file.read().decode("utf-8", "replace")
        return subprocess.CompletedProcess(
            args=command,
            returncode=returncode,
            stdout=stdout,
            stderr=stderr,
        )


def assemble_streams(rows: list[dict[str, str]]) -> list[dict[str, Any]]:
    streams: dict[tuple[str, str], dict[str, Any]] = {}
    for row in rows:
        tcp_stream = row.get("tcp.stream", "")
        http2_stream = row.get("http2.streamid", "")
        if not tcp_stream or not http2_stream:
            continue
        key = (tcp_stream, http2_stream)
        item = streams.setdefault(
            key,
            {
                "tcp_stream": tcp_stream,
                "http2_stream": http2_stream,
                "first_at": row.get("frame.time_epoch", ""),
                "last_at": row.get("frame.time_epoch", ""),
                "client": None,
                "request_headers": {},
                "response_headers": {},
                "request_all_headers": [],
                "response_all_headers": [],
                "request_chunks": [],
                "response_chunks": [],
                "packet_numbers": set(),
                "request_first_at": None,
                "request_last_at": None,
                "response_first_at": None,
                "response_last_at": None,
                "request_end_stream_seen": False,
                "response_end_stream_seen": False,
                "request_end_stream_at": None,
                "response_end_stream_at": None,
            },
        )
        if row.get("frame.number"):
            item["packet_numbers"].add(row["frame.number"])
        item["last_at"] = row.get("frame.time_epoch", item["last_at"])
        source_ip = row.get("ip.src") or row.get("ipv6.src") or "?"
        destination_ip = row.get("ip.dst") or row.get("ipv6.dst") or "?"
        source = f"{source_ip}:{row.get('tcp.srcport') or '?'}"
        destination = f"{destination_ip}:{row.get('tcp.dstport') or '?'}"

        method = row.get("http2.headers.method", "")
        if method:
            item["client"] = source
            item["request_headers"].update({
                "method": method,
                "path": row.get("http2.headers.path", ""),
                "authority": row.get("http2.headers.authority", ""),
                "scheme": row.get("http2.headers.scheme", ""),
                "authorization": row.get("http2.headers.authorization", ""),
                "accept": row.get("http2.headers.accept", ""),
                "content_type": row.get("http2.headers.content_type", ""),
                "content_encoding": row.get("http2.headers.content_encoding", ""),
                "content_length": row.get("http2.headers.content_length", ""),
                "user_agent": row.get("http2.headers.user_agent", ""),
            })
        status = row.get("http2.headers.status", "")
        if status:
            item["response_headers"].update({
                "status": status,
                "content_type": row.get("http2.headers.content_type", ""),
                "content_encoding": row.get("http2.headers.content_encoding", ""),
                "content_length": row.get("http2.headers.content_length", ""),
                "server": row.get("http2.headers.server", ""),
            })

        if item["client"] is None:
            source_port = int(row.get("tcp.srcport") or 0)
            destination_port = int(row.get("tcp.dstport") or 0)
            if source_port == 443 and destination_port != 443:
                item["client"] = destination
            elif destination_port == 443 and source_port != 443:
                item["client"] = source
        direction = (
            "request"
            if item["client"] is not None and source == item["client"]
            else "response"
        )
        direction_at = row.get("frame.time_epoch", "")
        first_at_key = f"{direction}_first_at"
        last_at_key = f"{direction}_last_at"
        if item[first_at_key] is None:
            item[first_at_key] = direction_at
        item[last_at_key] = direction_at

        raw_names = split_field_values(row.get("http2.header.name", ""))
        raw_values = split_field_values(row.get("http2.header.value", ""))
        if raw_names and len(raw_names) == len(raw_values):
            item[f"{direction}_all_headers"].extend(
                zip(raw_names, raw_values, strict=True)
            )

        data = decode_hex_bytes(row.get("http2.data.data", ""))
        if data:
            if source == item["client"]:
                item["request_chunks"].append(data)
            else:
                item["response_chunks"].append(data)
        end_stream = row.get("http2.flags.end_stream", "").lower() in {
            "true",
            "1",
        }
        if end_stream:
            end_stream_key = f"{direction}_end_stream_seen"
            end_stream_at_key = f"{direction}_end_stream_at"
            item[end_stream_key] = True
            item[end_stream_at_key] = direction_at

    result = list(streams.values())
    result.sort(key=lambda item: (int(item["tcp_stream"]), int(item["http2_stream"])))
    for item in result:
        item["request_body"] = b"".join(item.pop("request_chunks"))
        item["response_body"] = b"".join(item.pop("response_chunks"))
        item["packet_count"] = len(item.pop("packet_numbers"))
        item["request_all_headers"] = [
            {"name": name, "value": value} for name, value in item["request_all_headers"]
        ]
        item["response_all_headers"] = [
            {"name": name, "value": value} for name, value in item["response_all_headers"]
        ]
    return result


def expand_tshark_rows(rows: list[dict[str, str]]) -> list[dict[str, str]]:
    """Split multiple HTTP/2 frames coalesced into one captured TCP packet."""

    expanded: list[dict[str, str]] = []
    packet_fields = (
        "frame.time_epoch",
        "frame.number",
        "ip.src",
        "ipv6.src",
        "tcp.srcport",
        "ip.dst",
        "ipv6.dst",
        "tcp.dstport",
        "tcp.stream",
    )
    header_fields = (
        "http2.headers.method",
        "http2.headers.path",
        "http2.headers.authority",
        "http2.headers.scheme",
        "http2.headers.authorization",
        "http2.headers.accept",
        "http2.headers.content_type",
        "http2.headers.content_encoding",
        "http2.headers.content_length",
        "http2.headers.user_agent",
        "http2.headers.server",
        "http2.headers.status",
    )
    for row in rows:
        common = {field: row.get(field, "") for field in packet_fields}
        frame_types = split_field_values(row.get("http2.type", ""))
        stream_ids = split_field_values(row.get("http2.streamid", ""))
        end_flags = split_field_values(row.get("http2.flags.end_stream", ""))

        # HEADERS frames carry request method/path or response status. Within
        # a packet direction, these values are ordered with their stream IDs.
        header_ids = [
            stream_id
            for frame_type, stream_id in zip(frame_types, stream_ids, strict=False)
            if frame_type == "1" and stream_id
        ]
        field_values = {
            field: split_field_values(row.get(field, "")) for field in header_fields
        }
        raw_names = split_field_values(row.get("http2.header.name", ""))
        raw_values = split_field_values(row.get("http2.header.value", ""))
        header_blocks = decoded_header_blocks(raw_names, raw_values, header_ids)
        methods = field_values["http2.headers.method"]
        statuses = field_values["http2.headers.status"]
        header_frame_indexes = [
            frame_index
            for frame_index, (frame_type, stream_id) in enumerate(
                zip(frame_types, stream_ids, strict=False)
            )
            if frame_type == "1" and stream_id
        ]
        for index, header_id in enumerate(header_ids):
            method = methods[index] if index < len(methods) else ""
            status = statuses[index] if index < len(statuses) else ""
            raw_headers = header_blocks.get(header_id, [])
            if not method and not status and not raw_headers:
                continue
            header_row = dict(common)
            header_row.update(
                {
                    "http2.type": "1",
                    "http2.streamid": header_id,
                    "http2.data.data": "",
                    "http2.flags.end_stream": (
                        end_flags[header_frame_indexes[index]]
                        if index < len(header_frame_indexes)
                        and header_frame_indexes[index] < len(end_flags)
                        else ""
                    ),
                    "http2.header.name": TSHARK_AGGREGATOR.join(
                        name for name, _ in raw_headers
                    ),
                    "http2.header.value": TSHARK_AGGREGATOR.join(
                        value for _, value in raw_headers
                    ),
                }
            )
            for field, values in field_values.items():
                if len(values) == len(header_ids):
                    header_row[field] = values[index]
                elif len(header_ids) == 1 and values:
                    header_row[field] = values[0]
                else:
                    header_row[field] = ""
            header_row["_decoded_headers"] = raw_headers
            expanded.append(header_row)

        # DATA frames carry the body bytes. Filtering happens after field
        # extraction, so use HTTP/2 frame type to align each DATA payload with
        # its stream ID even when several streams share one TCP packet.
        data_values = split_field_values(row.get("http2.data.data", ""))
        data_frame_indexes = [
            index
            for index, frame_type in enumerate(frame_types)
            if frame_type == "0" and index < len(stream_ids)
        ]
        for data_index, frame_index in enumerate(data_frame_indexes):
            if data_index >= len(data_values):
                break
            data_row = dict(common)
            data_row.update(
                {
                    "http2.type": "0",
                    "http2.streamid": stream_ids[frame_index],
                    "http2.data.data": data_values[data_index],
                    "http2.flags.end_stream": (
                        end_flags[frame_index]
                        if frame_index < len(end_flags)
                        else ""
                    ),
                }
            )
            expanded.append(data_row)
    return expanded


def split_field_values(value: str) -> list[str]:
    return value.split(TSHARK_AGGREGATOR) if value else []


def collect_http2_control_events(rows: list[dict[str, str]]) -> list[dict[str, str]]:
    """Retain frame-level termination and flow-control evidence."""
    events: list[dict[str, str]] = []
    control_fields = (
        "http2.rst_stream.error",
        "http2.goaway.last_stream_id",
        "http2.goaway.error",
        "http2.goaway.addata",
        "http2.settings.id",
        "http2.settings.max_concurrent_streams",
        "http2.settings.initial_window_size",
        "http2.window_update.window_size_increment",
    )
    for row in rows:
        frame_types = split_field_values(row.get("http2.type", ""))
        if not frame_types:
            continue
        stream_ids = split_field_values(row.get("http2.streamid", ""))
        flags = split_field_values(row.get("http2.flags", ""))
        values_by_field = {
            field: split_field_values(row.get(field, "")) for field in control_fields
        }
        source_ip = row.get("ip.src") or row.get("ipv6.src") or "?"
        destination_ip = row.get("ip.dst") or row.get("ipv6.dst") or "?"
        source = f"{source_ip}:{row.get('tcp.srcport') or '?'}"
        destination = f"{destination_ip}:{row.get('tcp.dstport') or '?'}"
        for index, frame_type in enumerate(frame_types):
            if frame_type not in HTTP2_CONTROL_TYPES:
                continue
            event: dict[str, str] = {
                "time": utc_timestamp(row.get("frame.time_epoch", "")),
                "frame": row.get("frame.number", ""),
                "tcp_stream": row.get("tcp.stream", ""),
                "type": HTTP2_CONTROL_TYPES[frame_type],
                "stream_id": stream_ids[index] if index < len(stream_ids) else "0",
                "source": source,
                "destination": destination,
                "flags": flags[index] if index < len(flags) else "",
            }
            for field, values in values_by_field.items():
                if index < len(values):
                    event[field.removeprefix("http2.")] = values[index]
            events.append(event)
    return events


def decoded_header_blocks(
    names: list[str], values: list[str], stream_ids: list[str]
) -> dict[str, list[tuple[str, str]]]:
    """Map decoded HPACK headers to streams when field ordering is unambiguous."""
    if not names or len(names) != len(values) or not stream_ids:
        return {}
    pairs = list(zip(names, values, strict=True))
    blocks: list[list[tuple[str, str]]] = []
    current: list[tuple[str, str]] = []
    current_has_pseudo = False
    for pair in pairs:
        is_pseudo = pair[0].startswith(":")
        if is_pseudo and current and current_has_pseudo:
            blocks.append(current)
            current = []
            current_has_pseudo = False
        current.append(pair)
        current_has_pseudo |= is_pseudo
    if current:
        blocks.append(current)
    if len(stream_ids) == 1:
        return {stream_ids[0]: pairs}
    if len(blocks) != len(stream_ids):
        return {}
    return {
        stream_id: block
        for stream_id, block in zip(stream_ids, blocks, strict=True)
    }


def render_stream(item: dict[str, Any]) -> str:
    request_body, request_decode_error = decode_http_body(
        item["request_body"],
        item["request_headers"].get("content_encoding", ""),
    )
    response_body, response_decode_error = decode_http_body(
        item["response_body"],
        item["response_headers"].get("content_encoding", ""),
    )
    lines = [
        f"HTTP/2 会话：TCP {item['tcp_stream']} / Stream {item['http2_stream']}",
        f"时间：{utc_timestamp(item['first_at'])} 至 {utc_timestamp(item['last_at'])}",
        f"数据包数：{item['packet_count']}",
        (
            "请求 END_STREAM："
            f"{'是' if item['request_end_stream_seen'] else '否'}"
            + (
                f"（{utc_timestamp(item['request_end_stream_at'])}）"
                if item["request_end_stream_at"]
                else ""
            )
        ),
        (
            "响应 END_STREAM："
            f"{'是' if item['response_end_stream_seen'] else '否'}"
            + (
                f"（{utc_timestamp(item['response_end_stream_at'])}）"
                if item["response_end_stream_at"]
                else ""
            )
        ),
        (
            "请求首末帧："
            f"{utc_timestamp(item['request_first_at'] or '')} 至 "
            f"{utc_timestamp(item['request_last_at'] or '')}"
            if item["request_first_at"]
            else "请求首末帧：未识别"
        ),
        (
            "响应首末帧："
            f"{utc_timestamp(item['response_first_at'] or '')} 至 "
            f"{utc_timestamp(item['response_last_at'] or '')}"
            if item["response_first_at"]
            else "响应首末帧：未识别"
        ),
    ]
    if not item["request_end_stream_seen"] and item["response_end_stream_seen"]:
        lines.extend(
            [
                "判断提示：只观察到响应方向 END_STREAM，没有观察到请求方向 END_STREAM。",
                "这只是抓包证据中的方向性差异；需结合 trailers、RST_STREAM 和采集边界复核。",
            ]
        )
    elif item["request_end_stream_seen"] and not item["response_end_stream_seen"]:
        lines.extend(
            [
                "判断提示：只观察到请求方向 END_STREAM，响应方向 END_STREAM 未观察到。",
                "响应可能仍在进行，也可能超出采集窗口或未被完整解码；单凭 pcap 不可断言上游未结束。",
            ]
        )
    elif not item["response_end_stream_seen"]:
        lines.append(
            "判断提示：采集证据中未观察到响应方向 END_STREAM；需结合采集边界、RST_STREAM 和上游事件帧判断。"
        )
    lines.extend(
        [
            "",
            "请求头摘要：",
            f"  方法：{item['request_headers'].get('method') or '未识别'}",
            f"  地址：{item['request_headers'].get('authority') or '未识别'}{item['request_headers'].get('path') or ''}",
            f"  Scheme：{item['request_headers'].get('scheme') or '未提供'}",
            f"  Content-Type：{item['request_headers'].get('content_type') or '未提供'}",
            f"  Content-Encoding：{item['request_headers'].get('content_encoding') or '未提供'}",
            f"  Content-Length：{item['request_headers'].get('content_length') or '未提供'}",
            f"  Accept：{item['request_headers'].get('accept') or '未提供'}",
            f"  User-Agent：{item['request_headers'].get('user_agent') or '未提供'}",
            f"  Authorization：{item['request_headers'].get('authorization') or '未提供'}",
            "  解码后的全部请求头：",
        ]
    )
    if item["request_all_headers"]:
        lines.extend(
            f"    {header['name']}: {header['value']}"
            for header in item["request_all_headers"]
        )
    else:
        lines.append("    [未取得完整 HPACK 解码头列表]")
    lines.extend(
        [
            f"  请求正文：{len(item['request_body'])} 字节",
            f"  解压后请求正文：{len(request_body)} 字节",
            "",
            "请求正文：",
            render_body(request_body) if request_body else "[未观察到 HTTP/2 请求正文]",
            "",
            "响应头摘要：",
            f"  HTTP 状态：{item['response_headers'].get('status') or '未识别'}",
            f"  Content-Type：{item['response_headers'].get('content_type') or '未提供'}",
            f"  Content-Encoding：{item['response_headers'].get('content_encoding') or '未提供'}",
            f"  Content-Length：{item['response_headers'].get('content_length') or '未提供'}",
            f"  Server：{item['response_headers'].get('server') or '未提供'}",
            "  解码后的全部响应头：",
        ]
    )
    if item["response_all_headers"]:
        lines.extend(
            f"    {header['name']}: {header['value']}"
            for header in item["response_all_headers"]
        )
    else:
        lines.append("    [未取得完整 HPACK 解码头列表]")
    lines.extend(
        [
            f"  响应正文：{len(item['response_body'])} 字节",
            f"  解压后响应正文：{len(response_body)} 字节",
            "",
            "响应正文：",
        ]
    )
    if request_decode_error:
        lines.append(f"[请求正文说明] {request_decode_error}")
    if response_decode_error:
        lines.append(f"[响应正文说明] {response_decode_error}")

    frames, error = decode_aws_eventstream(response_body)
    if frames:
        lines.append(f"已解码 AWS EventStream 帧：{len(frames)}")
        for frame in frames:
            headers = frame["headers"]
            event_type = headers.get(":event-type", headers.get(":message-type", "未标注事件类型"))
            lines.extend(
                [
                    "",
                    f"--- 帧 {frame['index']}：{event_type}（{frame['bytes']} 字节）---",
                    f"帧头：{json.dumps(headers, ensure_ascii=False, sort_keys=True)}",
                    f"Payload：{frame['payload_bytes']} 字节",
                    frame["payload_text"],
                ]
            )
        if error:
            lines.extend(["", f"[EventStream 解码未完成] {error}"])
    elif response_body:
        lines.append(render_body(response_body))
        if error:
            lines.extend(["", f"[响应正文不是完整 AWS EventStream] {error}"])
    else:
        lines.append("[未观察到 HTTP/2 响应正文]")
    return "\n".join(lines)


def write_report(
    pcap: pathlib.Path,
    keylog: pathlib.Path | None,
    output: pathlib.Path,
    tshark: str | None,
    timeout: float,
    max_bytes: int,
    max_tshark_output_bytes: int,
) -> dict[str, Any]:
    output.mkdir(parents=True, exist_ok=True)
    readable_path = output / "https-readable.txt"
    manifest: dict[str, Any] = {
        "pcap": pcap.name,
        "keylog": keylog.name if keylog else None,
        "tshark": tshark,
        "tls_decryption": False,
        "plaintext_scope": "full_http2_bodies_when_decrypted",
        "max_readable_bytes": max_bytes,
        "readable_truncated": False,
        "http2_streams": 0,
        "decrypted_payload_bytes": 0,
        "status": "unavailable",
        "reason": None,
        "errors": [],
    }
    if not pcap.is_file():
        manifest["reason"] = "没有 traffic.pcap 文件"
    elif keylog is None or not keylog.is_file() or keylog.stat().st_size == 0:
        manifest["reason"] = "没有可用的 TLS 会话密钥；HTTPS 正文仍是密文"
    elif not tshark:
        manifest["reason"] = "系统没有安装 tshark，无法离线解密 TLS"
    else:
        try:
            result = run_tshark(
                tshark, pcap, keylog, timeout, max_tshark_output_bytes
            )
            if result.returncode != 0:
                manifest["reason"] = "tshark 解密或 HTTP/2 解析失败"
                manifest["errors"].append(result.stderr[-8000:])
            else:
                packet_rows = parse_tshark_rows(result.stdout)
                streams = assemble_streams(expand_tshark_rows(packet_rows))
                control_events = collect_http2_control_events(packet_rows)
                manifest["http2_streams"] = len(streams)
                manifest["http2_control_events"] = len(control_events)
                manifest["http2_control_event_counts"] = {
                    event_type: sum(
                        event["type"] == event_type for event in control_events
                    )
                    for event_type in sorted(set(HTTP2_CONTROL_TYPES.values()))
                    if any(event["type"] == event_type for event in control_events)
                }
                manifest["decrypted_payload_bytes"] = sum(
                    len(item["request_body"]) + len(item["response_body"])
                    for item in streams
                )
                has_decrypted_body = manifest["decrypted_payload_bytes"] > 0
                manifest["tls_decryption"] = has_decrypted_body
                manifest["status"] = (
                    "decrypted"
                    if has_decrypted_body
                    else ("no_http2_body" if streams else "no_http2")
                )
                manifest["reason"] = (
                    None
                    if has_decrypted_body
                    else (
                        "TLS keylog 可读取，但采集包中没有完整 HTTP/2 正文"
                        if streams
                        else "TLS keylog 可读取，但没有解析出 HTTP/2；协议可能不是 HTTP/2，或 keylog 与本次 pcap 不匹配"
                    )
                )
                rendered = [
                    (
                        "HTTPS 明文内容（由 pcap + TLS keylog 离线解密）"
                        if has_decrypted_body
                        else "TLS/HTTP/2 可读性检查结果"
                    ),
                    "=" * 58,
                    f"pcap：{pcap.name}",
                    f"HTTP/2 会话数：{len(streams)}",
                    f"HTTP/2 控制帧：{len(control_events)}",
                    f"正文总量：{manifest['decrypted_payload_bytes']} 字节",
                    f"可读文本上限：{max_bytes} 字节",
                    "",
                    "重要：本文件可能包含完整 system prompt、用户对话、工具定义、工具输入输出、thinking/reasoning、模型输出及认证头相关的应用数据。请按敏感数据保存。",
                    "",
                ]
                if control_events:
                    rendered.extend(
                        [
                            "HTTP/2 控制帧（RST_STREAM、GOAWAY、SETTINGS、WINDOW_UPDATE 等）：",
                            "-" * 58,
                        ]
                    )
                    for event in control_events:
                        details = ", ".join(
                            f"{key}={value}"
                            for key, value in event.items()
                            if key
                            not in {
                                "time",
                                "frame",
                                "tcp_stream",
                                "type",
                                "stream_id",
                                "source",
                                "destination",
                            }
                            and value
                        )
                        rendered.append(
                            f"{event['time']} 帧#{event['frame']} TCP#{event['tcp_stream']} "
                            f"{event['source']} -> {event['destination']} "
                            f"{event['type']} stream={event['stream_id']}"
                            + (f"（{details}）" if details else "")
                        )
                    rendered.append("")
                used = sum(len(line.encode("utf-8")) + 1 for line in rendered)
                for index, item in enumerate(streams, 1):
                    block = f"\n\n===== 上游请求 {index} =====\n{render_stream(item)}"
                    block_bytes = len(block.encode("utf-8"))
                    if used + block_bytes > max_bytes:
                        manifest["status"] = "truncated"
                        manifest["readable_truncated"] = True
                        manifest["reason"] = (
                            f"可读文本达到 {max_bytes} 字节上限，后续 HTTP/2 会话未写入"
                        )
                        manifest["errors"].append(
                            f"在第 {index} 个 HTTP/2 会话处达到可读文本大小上限"
                        )
                        break
                    rendered.append(block)
                    used += block_bytes
                if len(streams) == 0:
                    rendered.append("没有解析出 HTTP/2 会话。")
                elif not has_decrypted_body:
                    rendered.append("解析到了 HTTP/2 会话，但没有提取到请求或响应正文。")
                readable_path.write_text("\n".join(rendered) + "\n", encoding="utf-8")
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
            manifest["reason"] = f"运行 tshark 失败：{exc}"
            manifest["errors"].append(repr(exc))

    if not readable_path.exists():
        readable_path.write_text(
            "\n".join(
                [
                    "HTTPS 明文内容不可用",
                    "====================",
                    f"原因：{manifest['reason'] or '未知原因'}",
                    "",
                    "traffic.pcap 仍然保留原始 TLS 证据，但没有有效解密结果时，其中的 HTTPS 正文仍是密文。",
                    "要自动解密，需要目标进程为这些 TLS 会话提供会话密钥，并且采集机安装 tshark。",
                    "",
                ]
            ),
            encoding="utf-8",
        )
    (output / "tls-decryption.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pcap", required=True, type=pathlib.Path)
    parser.add_argument("--keylog", type=pathlib.Path)
    parser.add_argument("--output-dir", required=True, type=pathlib.Path)
    parser.add_argument("--tshark", default=shutil.which("tshark"))
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--max-bytes", type=int, default=64 * 1024 * 1024)
    parser.add_argument(
        "--max-tshark-output-bytes",
        type=int,
        default=192 * 1024 * 1024,
    )
    args = parser.parse_args()
    if args.timeout <= 0 or args.max_bytes <= 0 or args.max_tshark_output_bytes <= 0:
        parser.error("timeout and byte limits must be positive")
    report = write_report(
        args.pcap,
        args.keylog,
        args.output_dir,
        args.tshark,
        args.timeout,
        args.max_bytes,
        args.max_tshark_output_bytes,
    )
    print(json.dumps(report, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
