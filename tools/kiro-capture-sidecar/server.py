#!/usr/bin/env python3
"""Independent, bounded evidence collector for one kiro.rs deployment.

The service is deliberately stdlib-only so it can run beside an existing
kiro.rs process without rebuilding or importing the application. It performs
read-only commands, captures only the configured target port, and writes each
run to a private temporary directory before atomically publishing an artifact.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zipfile
from collections import deque
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, unquote, urlparse

DIAGNOSTICS_DIRECTORY = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "diagnostics"
if str(DIAGNOSTICS_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(DIAGNOSTICS_DIRECTORY))
from packet_protocols import ethernet_transport

SIDECAR_DIRECTORY = pathlib.Path(__file__).resolve().parent
if str(SIDECAR_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SIDECAR_DIRECTORY))
import app_capture


DEFAULT_CONTAINER = "kiro-rs-2ue-59137-app"
DEFAULT_TARGET_PORT = 59137
DEFAULT_MAX_DURATION = 900
DEFAULT_MAX_BYTES = 512 * 1024 * 1024
DEFAULT_MAX_READABLE_BYTES = 512 * 1024 * 1024
DEFAULT_ARTIFACT_PAGE_SIZE = 6
DEFAULT_RUNTIME_SNAPSHOT_INTERVAL = 15.0
DEFAULT_USAGE_RECORD_LIMIT = 200
DEFAULT_DIAGNOSTIC_LOG_MAX_FILES = 64
DEFAULT_DIAGNOSTIC_LOG_MAX_BYTES = 64 * 1024 * 1024
DEFAULT_DIAGNOSTIC_LOG_PATH = "/app/logs/tool-format-debug"
CAPTURE_POLL_INTERVAL = 0.5
EVENT_LOG_LIMIT = 500
PCAP_GLOBAL_HEADER_SIZE = 24
PCAP_RECORD_HEADER_SIZE = 16
PCAP_FORMATS = {
    b"\xd4\xc3\xb2\xa1": ("<", 1_000_000),
    b"\xa1\xb2\xc3\xd4": (">", 1_000_000),
    b"\x4d\x3c\xb2\xa1": ("<", 1_000_000_000),
    b"\xa1\xb2\x3c\x4d": (">", 1_000_000_000),
}
SENSITIVE_KEY_PARTS = (
    "password",
    "passwd",
    "token",
    "secret",
    "api_key",
    "apikey",
    "authorization",
    "postgres_url",
    "redis_url",
)

DATA_COLLECTION = [
    {
        "name": "上游 TCP/TLS 与 UDP/QUIC 网络数据包",
        "file": "traffic.pcap",
        "description": "原始二进制底层证据，覆盖配置的上游端口上的 TCP/TLS 与 UDP/QUIC；日常先看同包内的中文流量摘要。",
    },
    {
        "name": "TLS 会话密钥",
        "file": "tls-keylog.log",
        "description": "仅在 59137 启用 TLS key log 后出现；用于独立离线解密 HTTPS。该文件含会话密钥。",
    },
    {
        "name": "HTTPS 可读请求和响应",
        "file": "https-readable.txt、tls-decryption.json",
        "description": "存在有效 TLS keylog 和 tshark 时，自动还原 HTTP/2 正文并解析 AWS EventStream；否则说明缺少的条件。",
    },
    {
        "name": "TCP 会话与 UDP 流分析",
        "file": "pcap-analysis.json",
        "description": "按传输协议和四元组统计方向、数据量、时间间隔、TCP FIN/RST；UDP 没有传输层关闭标志。",
    },
    {
        "name": "容器运行状态",
        "file": "state-start.json、state-end.json",
        "description": "采集开始和结束时的 Docker inspect、资源统计、进程列表与端口探测。",
    },
    {
        "name": "kiro.rs 只读运行快照",
        "file": "state-start.json、state-end.json、runtime-timeline.jsonl、runtime-timeline.txt",
        "description": "已配置 Admin API 时采集本地凭据摘要、运行时和运行时配置；外部池状态默认不采集，敏感字段会隐藏。",
    },
    {
        "name": "服务容器日志",
        "file": "container.log",
        "description": "采集时间窗口内的带时间戳 Docker 容器日志。",
    },
    {
        "name": "本地请求级 usage",
        "file": "state-start.json、state-end.json、runtime-timeline.jsonl、usage-window.txt",
        "description": "只读采集 routeKind=local_credential 的 usage 记录和 writer 状态；外部池字段默认排除。",
    },
    {
        "name": "tool-format-debug 诊断 JSONL",
        "file": "tool-format-debug/、diagnostic-log-index.json",
        "description": "从目标容器的 /app/logs/tool-format-debug 挂载卷限量复制本地诊断记录；不会读取或保存外部池记录。",
    },
    {
        "name": "采集清单与命令记录",
        "file": "manifest.json、*-command.json",
        "description": "采集时间、边界、能力、命令返回码和失败原因。",
    },
    {
        "name": "中文说明与可读摘要",
        "file": "capture-readable.txt、README.txt、capture-summary.txt、capture-events.txt、tcp-flows.txt、https-readable.txt、upstream-readable.txt",
        "description": "可从列表直接下载 capture-readable.txt，无需先解压 ZIP；有解密条件时包含 HTTPS 明文对话。",
    },
]


def is_sensitive_key(key: str) -> bool:
    normalized = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", key.replace("-", "_")).lower()
    return any(part in normalized for part in SENSITIVE_KEY_PARTS)


def redact_value(value: object, key: str | None = None) -> object:
    if key is not None and is_sensitive_key(key):
        if isinstance(value, list):
            return ["<redacted>" for _ in value]
        return "<redacted>"
    if isinstance(value, dict):
        return {str(k): redact_value(v, str(k)) for k, v in value.items()}
    if isinstance(value, list):
        redacted: list[object] = []
        for item in value:
            if isinstance(item, str) and "=" in item:
                env_key, _, _ = item.partition("=")
                if is_sensitive_key(env_key):
                    redacted.append(f"{env_key}=<redacted>")
                    continue
            redacted.append(redact_value(item))
        return redacted
    return value


def exclude_external_pool_values(value: object) -> object:
    """Remove external-pool account details from local-only evidence."""
    if isinstance(value, dict):
        result: dict[str, object] = {}
        for raw_key, raw_value in value.items():
            key = str(raw_key)
            normalized = re.sub(
                r"([a-z0-9])([A-Z])", r"\1_\2", key.replace("-", "_")
            ).lower()
            if "external_pool" in normalized or "externalpool" in normalized:
                result[key] = "<excluded: external pool out of scope>"
            else:
                result[key] = exclude_external_pool_values(raw_value)
        return result
    if isinstance(value, list):
        return [exclude_external_pool_values(item) for item in value]
    return value


def is_local_usage_record(value: object) -> bool:
    if not isinstance(value, dict):
        return False
    route_kind = value.get("routeKind", value.get("route_kind"))
    if route_kind is not None and str(route_kind) not in {
        "local_credential",
        "local-credential",
        "local",
    }:
        return False
    for raw_key in value:
        normalized = re.sub(
            r"([a-z0-9])([A-Z])", r"\1_\2", str(raw_key).replace("-", "_")
        ).lower()
        if "external_pool" in normalized or "externalpool" in normalized:
            return False
    return True


def keep_local_usage_records(value: object) -> object:
    """Drop external usage rows before redaction so they are not analyzed."""
    if isinstance(value, list):
        return [item for item in value if is_local_usage_record(item)]
    if not isinstance(value, dict):
        return value
    result = dict(value)
    for key in ("records", "items", "data"):
        rows = result.get(key)
        if isinstance(rows, list):
            result[key] = [
                item for item in rows if is_local_usage_record(item)
            ]
    return result


def redact_admin_body(
    endpoint_name: str,
    body: object,
    include_external_pools: bool,
) -> object:
    redacted = redact_value(body)
    if include_external_pools:
        return redacted
    return exclude_external_pool_values(redacted)


def redact_command_result(result: dict[str, object]) -> dict[str, object]:
    sanitized = dict(result)
    stdout = sanitized.get("stdout")
    if isinstance(stdout, str):
        try:
            parsed = json.loads(stdout)
        except ValueError:
            pass
        else:
            sanitized["stdout"] = json.dumps(
                redact_value(parsed), ensure_ascii=False, indent=2
            )
    return sanitized


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


def strip_ansi(value: str) -> str:
    """Remove terminal color/control sequences from a readable log copy."""
    return ANSI_ESCAPE_RE.sub("", value)


def format_bytes(value: int | float) -> str:
    size = max(0.0, float(value))
    units = ("B", "KiB", "MiB", "GiB", "TiB")
    unit = units[0]
    for candidate in units:
        unit = candidate
        if size < 1024 or candidate == units[-1]:
            break
        size /= 1024
    return f"{size:.1f} {unit}" if unit != "B" else f"{int(size)} B"


def run_readonly(command: list[str], timeout: float = 8.0) -> dict[str, object]:
    """Run a command and preserve failures as evidence instead of raising."""
    started = time.monotonic()
    try:
        result = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return {
            "command": command,
            "returncode": result.returncode,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
        }
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {
            "command": command,
            "returncode": None,
            "stdout": "",
            "stderr": repr(exc),
            "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
        }


def write_json(path: pathlib.Path, value: object) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def read_json(path: pathlib.Path) -> object | None:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def empty_progress() -> dict[str, object]:
    return {
        "phase": "idle",
        "started_at": None,
        "finished_at": None,
        "updated_at": None,
        "elapsed_seconds": 0.0,
        "pcap_bytes": 0,
        "pcap_records": 0,
        "tcp_packets": 0,
        "udp_packets": 0,
        "tcp_payload_bytes": 0,
        "udp_payload_bytes": 0,
        "session_count": 0,
        "udp_flow_count": 0,
        "closed_session_count": 0,
        "active_session_count": 0,
        "syn_packets": 0,
        "fin_packets": 0,
        "rst_packets": 0,
        "capture_rate_bytes_per_second": 0.0,
        "first_packet_at": None,
        "last_packet_at": None,
        "data_seen": False,
        "no_data_reason": None,
    }


def packet_endpoints(
    packet: bytes,
) -> tuple[tuple[str, int], tuple[str, int], int, str] | None:
    """Return transport endpoints, TCP flags/payload size, and protocol."""
    parsed = ethernet_transport(packet)
    if parsed is None:
        return None
    packed = parsed.tcp_flags | (parsed.payload_bytes << 8)
    return (
        (parsed.source, parsed.source_port),
        (parsed.destination, parsed.destination_port),
        packed,
        parsed.protocol,
    )


class PcapProgress:
    """Incrementally summarize a pcap while the capture helper appends records."""

    def __init__(
        self, path: pathlib.Path, started_at: str, duration_seconds: float
    ) -> None:
        self.path = path
        self.started_at = started_at
        self.duration_seconds = max(0.1, duration_seconds)
        self.offset = 0
        self.endian = "<"
        self.timestamp_units = 1_000_000
        self.sessions: dict[
            tuple[str, tuple[str, int], tuple[str, int]], dict[str, object]
        ] = {}
        self.udp_flows: set[
            tuple[str, tuple[str, int], tuple[str, int]]
        ] = set()
        self.progress = empty_progress()
        self.progress.update(
            {
                "phase": "capturing",
                "started_at": started_at,
            }
        )

    def _reset(self) -> None:
        self.offset = 0
        self.sessions.clear()
        self.udp_flows.clear()
        self.progress = empty_progress()
        self.progress.update(
            {
                "phase": "capturing",
                "started_at": self.started_at,
            }
        )

    def _read_records(self) -> None:
        if not self.path.exists():
            return
        try:
            size = self.path.stat().st_size
            if size < self.offset:
                self._reset()
            with self.path.open("rb") as capture:
                capture.seek(self.offset)
                if self.offset == 0:
                    header = capture.read(PCAP_GLOBAL_HEADER_SIZE)
                    if len(header) < PCAP_GLOBAL_HEADER_SIZE:
                        return
                    format_info = PCAP_FORMATS.get(header[:4])
                    if format_info is None:
                        return
                    self.endian, self.timestamp_units = format_info
                    self.offset = PCAP_GLOBAL_HEADER_SIZE
                while True:
                    record_header = capture.read(PCAP_RECORD_HEADER_SIZE)
                    if len(record_header) < PCAP_RECORD_HEADER_SIZE:
                        break
                    seconds, fraction, captured_len, original_len = struct.unpack(
                        self.endian + "IIII", record_header
                    )
                    if captured_len > 262144 or captured_len > original_len:
                        break
                    packet = capture.read(captured_len)
                    if len(packet) < captured_len:
                        break
                    self.offset += PCAP_RECORD_HEADER_SIZE + captured_len
                    timestamp = seconds + fraction / self.timestamp_units
                    timestamp_text = dt.datetime.fromtimestamp(
                        timestamp, dt.timezone.utc
                    ).isoformat(timespec="milliseconds").replace("+00:00", "Z")
                    self.progress["pcap_records"] = int(self.progress["pcap_records"]) + 1
                    self.progress["pcap_bytes"] = self.offset
                    self.progress["first_packet_at"] = (
                        self.progress["first_packet_at"] or timestamp_text
                    )
                    self.progress["last_packet_at"] = timestamp_text
                    parsed = packet_endpoints(packet)
                    if parsed is None:
                        continue
                    source, destination, packed_flags, protocol = parsed
                    flags = packed_flags & 0xFF
                    payload_bytes = packed_flags >> 8
                    packet_counter = f"{protocol}_packets"
                    payload_counter = f"{protocol}_payload_bytes"
                    self.progress[packet_counter] = (
                        int(self.progress[packet_counter]) + 1
                    )
                    self.progress[payload_counter] = (
                        int(self.progress[payload_counter]) + payload_bytes
                    )
                    if protocol == "udp":
                        key = ("udp", *sorted((source, destination)))
                        self.udp_flows.add(key)
                        continue
                    self.progress["syn_packets"] = (
                        int(self.progress["syn_packets"]) + int(bool(flags & 0x02))
                    )
                    self.progress["fin_packets"] = (
                        int(self.progress["fin_packets"]) + int(bool(flags & 0x01))
                    )
                    self.progress["rst_packets"] = (
                        int(self.progress["rst_packets"]) + int(bool(flags & 0x04))
                    )
                    key = ("tcp", *sorted((source, destination)))
                    session = self.sessions.setdefault(
                        key,
                        {"fin_directions": set(), "rst": False},
                    )
                    if flags & 0x01:
                        session["fin_directions"].add(source)
                    if flags & 0x04:
                        session["rst"] = True
            self.progress["session_count"] = len(self.sessions)
            self.progress["udp_flow_count"] = len(self.udp_flows)
            closed = 0
            for session in self.sessions.values():
                if session["rst"] or len(session["fin_directions"]) >= 2:
                    closed += 1
            self.progress["closed_session_count"] = closed
            self.progress["active_session_count"] = max(0, len(self.sessions) - closed)
            self.progress["data_seen"] = bool(self.progress["pcap_records"])
        except (OSError, ValueError, struct.error):
            return

    def poll(self) -> dict[str, object]:
        self._read_records()
        now = time.monotonic()
        started_monotonic = getattr(self, "started_monotonic", now)
        self.started_monotonic = started_monotonic
        elapsed = max(0.0, now - started_monotonic)
        self.progress["elapsed_seconds"] = round(elapsed, 1)
        self.progress["capture_rate_bytes_per_second"] = round(
            int(self.progress["pcap_bytes"]) / elapsed if elapsed else 0.0,
            1,
        )
        self.progress["updated_at"] = utc_now()
        return dict(self.progress)

    def finish(self, phase: str = "finished") -> dict[str, object]:
        self._read_records()
        self.progress["phase"] = phase
        self.progress["finished_at"] = utc_now()
        return self.poll()


class Collector:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.root = pathlib.Path(args.output).resolve()
        self.root.mkdir(parents=True, exist_ok=True)
        self.lock = threading.Lock()
        self.thread: threading.Thread | None = None
        self.capture_process: subprocess.Popen[str] | None = None
        self.pause_requested = False
        self.stop_requested = False
        self.stop_event = threading.Event()
        self.events: deque[dict[str, object]] = deque(maxlen=EVENT_LOG_LIMIT)
        self.event_seq = 0
        self._last_event_records = 0
        self._last_event_bytes = 0
        self._last_event_sessions = 0
        self._last_event_active_sessions = 0
        self._last_event_keylog_bytes = 0
        self._last_event_at = 0.0
        self.status: dict[str, object] = {
            "state": "idle",
            "target_container": args.container,
            "target_port": args.target_port,
            "capture_limits": {
                "duration_seconds": args.duration,
                "max_artifact_bytes": args.max_bytes,
            },
            "last_artifact": None,
            "last_error": None,
            "last_message": None,
            "last_capture": empty_progress(),
            "paused_at": None,
            "stop_requested": False,
            "events": [],
        }

    def tshark_executable(self) -> str | None:
        configured = getattr(self.args, "tshark", None)
        return shutil.which(configured) if configured else shutil.which("tshark")

    def _append_event_locked(
        self,
        message: str,
        level: str = "info",
        details: dict[str, object] | None = None,
    ) -> None:
        self.event_seq += 1
        event: dict[str, object] = {
            "seq": self.event_seq,
            "at": utc_now(),
            "level": level,
            "message": message,
        }
        if details:
            event["details"] = details
        self.events.append(event)
        self.status["events"] = list(self.events)

    def add_event(
        self,
        message: str,
        level: str = "info",
        details: dict[str, object] | None = None,
    ) -> None:
        with self.lock:
            self._append_event_locked(message, level, details)

    def event_snapshot(self) -> list[dict[str, object]]:
        with self.lock:
            return list(self.events)

    def _record_progress_events_locked(
        self, progress: dict[str, object]
    ) -> None:
        phase = str(progress.get("phase") or "")
        previous = self.status.get("progress")
        previous_phase = previous.get("phase") if isinstance(previous, dict) else None
        if phase and phase != previous_phase:
            self._append_event_locked(f"采集阶段：{phase}")

        records = int(progress.get("pcap_records") or 0)
        bytes_seen = int(progress.get("pcap_bytes") or 0)
        sessions = int(progress.get("session_count") or 0)
        active_sessions = int(progress.get("active_session_count") or 0)
        keylog_bytes = int(progress.get("tls_keylog_bytes") or 0)
        now = time.monotonic()
        records_changed = records > self._last_event_records
        bytes_changed = bytes_seen > self._last_event_bytes
        sessions_changed = (
            sessions != self._last_event_sessions
            or active_sessions != self._last_event_active_sessions
        )
        if records_changed and (
            records <= 1
            or records % 10 == 0
            or now - self._last_event_at >= 2.0
        ):
            self._append_event_locked(
                f"已读取 {records} 个数据包，累计 {format_bytes(bytes_seen)}",
                details={
                    "pcap_records": records,
                    "pcap_bytes": bytes_seen,
                    "tcp_packets": int(progress.get("tcp_packets") or 0),
                },
            )
            self._last_event_at = now
        if sessions_changed:
            self._append_event_locked(
                f"TCP 会话 {sessions} 个，活跃 {active_sessions} 个",
                details={
                    "session_count": sessions,
                    "active_session_count": active_sessions,
                    "closed_session_count": int(progress.get("closed_session_count") or 0),
                },
            )
        if keylog_bytes > self._last_event_keylog_bytes:
            self._append_event_locked(
                f"TLS 会话密钥文件新增 {format_bytes(keylog_bytes - self._last_event_keylog_bytes)}",
                details={"tls_keylog_bytes": keylog_bytes},
            )
        self._last_event_records = max(self._last_event_records, records)
        self._last_event_bytes = max(self._last_event_bytes, bytes_seen)
        self._last_event_sessions = sessions
        self._last_event_active_sessions = active_sessions
        self._last_event_keylog_bytes = max(self._last_event_keylog_bytes, keylog_bytes)

    def update_progress(self, progress: dict[str, object]) -> None:
        with self.lock:
            self._record_progress_events_locked(progress)
            self.status["progress"] = dict(progress)
            self.status["progress_updated_at"] = utc_now()

    def current_progress(self) -> dict[str, object]:
        with self.lock:
            progress = self.status.get("progress")
            if isinstance(progress, dict):
                return dict(progress)
            return empty_progress()

    def snapshot_state(self, run_dir: pathlib.Path, phase: str) -> None:
        self.add_event(f"采集{phase}状态快照开始")
        inspect_result = redact_command_result(
            run_readonly(["docker", "inspect", self.args.container])
        )
        snapshot = {
            "collected_at": utc_now(),
            "phase": phase,
            "target_container": self.args.container,
            "target_port": self.args.target_port,
            "docker_inspect": inspect_result,
            "docker_stats": run_readonly(
                ["docker", "stats", "--no-stream", "--no-trunc", self.args.container]
            ),
            "docker_top": run_readonly(["docker", "top", self.args.container]),
            "socket_probe": self.socket_probe(),
        }
        if self.args.admin_url:
            snapshot["admin_state"] = self.admin_state(
                since=self.status.get("started_at"),
                until=utc_now() if phase == "end" else None,
            )
        write_json(
            run_dir / f"state-{phase}.json",
            snapshot,
        )
        self.add_event(f"采集{phase}状态快照完成")

    def admin_state(
        self,
        timeout: float = 3.0,
        since: str | None = None,
        until: str | None = None,
    ) -> dict[str, object]:
        """Use only read endpoints; avoid triggering account probes or cleanup."""
        import urllib.error
        import urllib.request

        base = self.args.admin_url.rstrip("/")
        token = pathlib.Path(self.args.admin_token_file).read_text(encoding="utf-8").strip()
        include_external_pools = bool(
            getattr(self.args, "include_external_pools", False)
        )
        result: dict[str, object] = {
            "base_url": base,
            "checked_at": utc_now(),
            "scope": {
                "local_credentials": True,
                "external_pools_included": include_external_pools,
            },
            "endpoints": {},
        }
        endpoints = {
            "credential_summary": "/api/admin/credentials/summary",
            "local_credential_runtime": "/api/admin/credentials/runtime",
            "runtime_config": "/api/admin/config/runtime",
            "usage_writer_stats": "/api/admin/usage-writer-stats",
        }
        if include_external_pools:
            endpoints["external_pool_status"] = "/api/admin/external-pools/status"
        for name, path in endpoints.items():
            # kiro.rs Admin middleware accepts x-api-key (and Bearer for normal
            # HTTP callers); use the deployment's canonical header here so the
            # snapshot remains read-only and works with the 0.0.169 image.
            request = urllib.request.Request(base + path, headers={"x-api-key": token})
            started = time.monotonic()
            try:
                with urllib.request.urlopen(request, timeout=timeout) as response:
                    payload = response.read(2 * 1024 * 1024)
                    decoded = json.loads(payload.decode("utf-8"))
                    result["endpoints"][name] = {
                        "status": response.status,
                        "body": redact_admin_body(
                            name,
                            decoded,
                            include_external_pools,
                        ),
                        "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
                    }
            except (OSError, ValueError, urllib.error.URLError) as exc:
                result["endpoints"][name] = {
                    "error": repr(exc),
                    "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
                }

        usage_limit = int(
            getattr(self.args, "usage_record_limit", DEFAULT_USAGE_RECORD_LIMIT)
            or DEFAULT_USAGE_RECORD_LIMIT
        )
        usage_limit = max(1, min(1000, usage_limit))
        usage_query = {
            "routeKind": "local_credential",
            "limit": str(usage_limit),
        }
        if since:
            usage_query["since"] = since
        if until:
            usage_query["until"] = until
        usage_path = f"/api/admin/usage-records?{urlencode(usage_query)}"
        request = urllib.request.Request(
            base + usage_path,
            headers={"x-api-key": token},
        )
        started = time.monotonic()
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                payload = response.read(4 * 1024 * 1024)
                decoded = json.loads(payload.decode("utf-8"))
                result["endpoints"]["usage_records"] = {
                    "status": response.status,
                    "query": usage_query,
                    "body": redact_admin_body(
                        "usage_records",
                        keep_local_usage_records(decoded),
                        include_external_pools,
                    ),
                    "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
                }
        except (OSError, ValueError, urllib.error.URLError) as exc:
            result["endpoints"]["usage_records"] = {
                "query": usage_query,
                "error": repr(exc),
                "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
            }
        return result

    def runtime_snapshot_interval(self) -> float:
        value = getattr(
            self.args,
            "runtime_snapshot_interval",
            DEFAULT_RUNTIME_SNAPSHOT_INTERVAL,
        )
        try:
            interval = float(value)
        except (TypeError, ValueError):
            return DEFAULT_RUNTIME_SNAPSHOT_INTERVAL
        if interval <= 0:
            return 0.0
        return max(2.0, min(300.0, interval))

    def append_runtime_timeline(
        self,
        run_dir: pathlib.Path,
        progress: dict[str, object],
        reason: str,
    ) -> None:
        """Persist a lightweight request-window timeline during capture."""
        entry: dict[str, object] = {
            "collected_at": utc_now(),
            "reason": reason,
            "pcap": {
                "phase": progress.get("phase"),
                "elapsed_seconds": progress.get("elapsed_seconds"),
                "pcap_records": progress.get("pcap_records"),
                "pcap_bytes": progress.get("pcap_bytes"),
                "tcp_packets": progress.get("tcp_packets"),
                "udp_packets": progress.get("udp_packets"),
                "session_count": progress.get("session_count"),
                "active_session_count": progress.get("active_session_count"),
                "udp_flow_count": progress.get("udp_flow_count"),
                "first_packet_at": progress.get("first_packet_at"),
                "last_packet_at": progress.get("last_packet_at"),
            },
            "socket_probe": self.socket_probe(),
        }
        if self.args.admin_url:
            entry["admin_state"] = self.admin_state(
                timeout=1.5,
                since=self.status.get("started_at"),
                until=utc_now(),
            )
        with (run_dir / "runtime-timeline.jsonl").open(
            "a",
            encoding="utf-8",
        ) as timeline:
            timeline.write(json.dumps(entry, ensure_ascii=False, separators=(",", ":")) + "\n")

    def runtime_timeline_text(self, run_dir: pathlib.Path) -> str:
        path = run_dir / "runtime-timeline.jsonl"
        lines = [
            "运行状态时间线",
            "================",
            "来源：采集期间 sidecar 周期性读取 pcap 进度、端口探测和只读 Admin 本地账号状态。",
            "外部池状态默认不采集；如果没有配置 Admin URL/token，则只包含 pcap 和端口探测。",
            "",
        ]
        if not path.is_file():
            lines.append("没有 runtime-timeline.jsonl。")
            return "\n".join(lines) + "\n"
        count = 0
        try:
            with path.open(encoding="utf-8") as source:
                for raw_line in source:
                    raw_line = raw_line.strip()
                    if not raw_line:
                        continue
                    count += 1
                    try:
                        entry = json.loads(raw_line)
                    except ValueError:
                        lines.append(f"[{count}] 无法解析时间线记录")
                        continue
                    pcap = entry.get("pcap")
                    pcap = pcap if isinstance(pcap, dict) else {}
                    admin = entry.get("admin_state")
                    endpoints = admin.get("endpoints", {}) if isinstance(admin, dict) else {}
                    summary_endpoint = endpoints.get("credential_summary")
                    summary_body = (
                        summary_endpoint.get("body")
                        if isinstance(summary_endpoint, dict)
                        else None
                    )
                    local_endpoint = endpoints.get("local_credential_runtime")
                    local_body = (
                        local_endpoint.get("body")
                        if isinstance(local_endpoint, dict)
                        else None
                    )
                    lines.append(
                        "[{}] {} reason={} elapsed={}s packets={} bytes={} tcpSessions={} active={} udpFlows={}".format(
                            count,
                            entry.get("collected_at", "未知"),
                            entry.get("reason", "sample"),
                            pcap.get("elapsed_seconds", 0),
                            pcap.get("pcap_records", 0),
                            format_bytes(int(pcap.get("pcap_bytes") or 0)),
                            pcap.get("session_count", 0),
                            pcap.get("active_session_count", 0),
                            pcap.get("udp_flow_count", 0),
                        )
                    )
                    if isinstance(summary_body, dict):
                        lines.append(
                            "    本地账号摘要：globalInFlight={} queued={} available={} maxConcurrent={}".format(
                                summary_body.get("globalInFlightRequests", "未知"),
                                summary_body.get("queuedRequests", "未知"),
                                summary_body.get("available", "未知"),
                                summary_body.get("globalMaxConcurrentRequests", "未知"),
                            )
                        )
                    if isinstance(local_body, dict):
                        items = local_body.get("items")
                        lines.append(
                            "    本地账号运行时条目：{}".format(
                                len(items) if isinstance(items, list) else "未知"
                            )
                        )
        except OSError as exc:
            lines.append(f"读取 runtime-timeline.jsonl 失败：{exc!r}")
        if count == 0:
            lines.append("没有采集到运行状态时间线记录。")
        return "\n".join(lines) + "\n"

    def usage_window_text(self, run_dir: pathlib.Path) -> str:
        """Render a bounded local-only usage view beside raw snapshots."""
        snapshot = read_json(run_dir / "state-end.json")
        admin = snapshot.get("admin_state") if isinstance(snapshot, dict) else None
        endpoints = admin.get("endpoints", {}) if isinstance(admin, dict) else {}
        endpoint = endpoints.get("usage_records")
        body = endpoint.get("body") if isinstance(endpoint, dict) else None
        if isinstance(body, dict):
            records = body.get("records")
            if not isinstance(records, list):
                records = body.get("items")
        elif isinstance(body, list):
            records = body
        else:
            records = []
        records = records if isinstance(records, list) else []
        write_json(
            run_dir / "usage-records-local.json",
            body
            if body is not None
            else {
                "records": [],
                "reason": "state-end 未提供 usage-records",
            },
        )
        lines = [
            "本地 Kiro usage 时间窗口",
            "========================",
            "来源：只读 Admin /api/admin/usage-records，查询固定为 routeKind=local_credential。",
            "外部池记录和 externalPool 字段已按采集范围排除。",
            f"记录数：{len(records)}",
            "",
        ]
        if not records:
            lines.append("没有可用的本地 usage 记录；请结合 runtime-timeline.jsonl 和 pcap 复核。")
            return "\n".join(lines) + "\n"
        for index, record in enumerate(records, 1):
            if not isinstance(record, dict):
                lines.append(f"[{index}] 非对象记录")
                continue
            request_id = (
                record.get("id")
                or record.get("requestId")
                or record.get("request_id")
                or "未知"
            )
            lines.extend(
                [
                    f"[{index}] request={request_id}",
                    f"    createdAt={record.get('createdAt', record.get('created_at', '未知'))}",
                    f"    endpoint={record.get('endpoint', '未知')} model={record.get('model', '未知')}",
                    f"    status={record.get('status', '未知')} stream={record.get('stream', '未知')}",
                    f"    credentialId={record.get('credentialId', record.get('credential_id', '未知'))}",
                    f"    durationMs={record.get('durationMs', record.get('duration_ms', '未知'))}",
                    f"    firstTokenLatencyMs={record.get('firstTokenLatencyMs', record.get('first_token_latency_ms', '未知'))}",
                    "",
                ]
            )
        return "\n".join(lines) + "\n"

    def socket_probe(self) -> dict[str, object]:
        started = time.monotonic()
        try:
            with socket.create_connection((self.args.probe_host, self.args.target_port), timeout=2):
                return {"ok": True, "elapsed_ms": round((time.monotonic() - started) * 1000, 1)}
        except OSError as exc:
            return {"ok": False, "error": repr(exc), "elapsed_ms": round((time.monotonic() - started) * 1000, 1)}

    def logs(self, run_dir: pathlib.Path, since: str) -> None:
        self.add_event("正在读取目标容器日志")
        result = run_readonly(
            ["docker", "logs", "--timestamps", "--since", since, self.args.container],
            timeout=20,
        )
        readable_log = strip_ansi(
            str(result.get("stdout", "")) + str(result.get("stderr", ""))
        )
        (run_dir / "container.log").write_text(readable_log, encoding="utf-8", errors="replace")
        write_json(run_dir / "container-log-command.json", result)
        if result.get("returncode") == 0:
            self.add_event("目标容器日志已保存")
        else:
            self.add_event("目标容器日志读取失败", "error", {"returncode": result.get("returncode")})

    @staticmethod
    def _contains_external_pool_key(value: object) -> bool:
        if isinstance(value, dict):
            for raw_key, raw_value in value.items():
                key = re.sub(
                    r"([a-z0-9])([A-Z])", r"\1_\2", str(raw_key).replace("-", "_")
                ).lower()
                if "external_pool" in key or "externalpool" in key:
                    return True
                if Collector._contains_external_pool_key(raw_value):
                    return True
        elif isinstance(value, list):
            return any(Collector._contains_external_pool_key(item) for item in value)
        return False

    @staticmethod
    def _diagnostic_record_is_local(raw_line: bytes) -> bool:
        """Keep local tool-format records without guessing from free text."""
        try:
            value = json.loads(raw_line.decode("utf-8"))
        except (UnicodeDecodeError, ValueError):
            # The recorder writes JSONL. Preserve malformed lines as evidence
            # only when they do not visibly identify an external pool.
            return b"externalPool" not in raw_line and b"external_pool" not in raw_line
        if not isinstance(value, dict):
            return False
        route_kind = value.get("routeKind", value.get("route_kind"))
        if route_kind is not None and str(route_kind) not in {
            "local_credential",
            "local-credential",
            "local",
        }:
            return False
        return not Collector._contains_external_pool_key(value)

    def _diagnostic_log_source(self) -> pathlib.Path | None:
        """Find the host path mounted as the target's /app/logs directory."""
        result = run_readonly(
            ["docker", "inspect", "-f", "{{json .Mounts}}", self.args.container],
            timeout=8,
        )
        if result.get("returncode") != 0:
            return None
        try:
            mounts = json.loads(str(result.get("stdout") or ""))
        except ValueError:
            return None
        if not isinstance(mounts, list):
            return None
        destination = pathlib.PurePosixPath(
            str(getattr(self.args, "diagnostic_log_path", DEFAULT_DIAGNOSTIC_LOG_PATH))
        )
        for mount in mounts:
            if not isinstance(mount, dict):
                continue
            mount_destination = pathlib.PurePosixPath(str(mount.get("Destination") or ""))
            source = mount.get("Source")
            if (
                not source
                or not destination.is_relative_to("/app/logs")
                or mount_destination != pathlib.PurePosixPath("/app/logs")
            ):
                continue
            return pathlib.Path(str(source)) / destination.relative_to("/app/logs")
        # The common path above is intentionally conservative. If the
        # configured diagnostic path is not under /app/logs, do not guess.
        return None

    def _copy_diagnostic_jsonl(
        self,
        source: pathlib.Path,
        destination: pathlib.Path,
        max_bytes: int,
    ) -> dict[str, object]:
        total_bytes = source.stat().st_size
        copied_bytes = 0
        included_lines = 0
        excluded_lines = 0
        truncated = total_bytes > max_bytes
        destination.parent.mkdir(parents=True, exist_ok=True)
        with source.open("rb") as input_file, destination.open("wb") as output_file:
            if truncated:
                input_file.seek(max(0, total_bytes - max_bytes))
                input_file.readline()
                output_file.write(
                    f"# 已从文件尾部读取，原始大小 {total_bytes} 字节，单文件上限 {max_bytes} 字节\n".encode(
                        "utf-8"
                    )
                )
            for raw_line in input_file:
                if not self._diagnostic_record_is_local(raw_line):
                    excluded_lines += 1
                    continue
                if copied_bytes + len(raw_line) > max_bytes:
                    truncated = True
                    break
                output_file.write(raw_line)
                copied_bytes += len(raw_line)
                included_lines += 1
        destination.chmod(0o600)
        return {
            "source": str(source),
            "artifact": str(destination.relative_to(destination.parents[1])),
            "original_bytes": total_bytes,
            "copied_bytes": copied_bytes,
            "included_lines": included_lines,
            "excluded_external_lines": excluded_lines,
            "truncated": truncated,
        }

    def collect_diagnostic_logs(self, run_dir: pathlib.Path, since: str) -> dict[str, object]:
        """Copy bounded local tool-format JSONL from the target's read-only volume."""
        configured_path = pathlib.PurePosixPath(
            str(getattr(self.args, "diagnostic_log_path", DEFAULT_DIAGNOSTIC_LOG_PATH))
        )
        result: dict[str, object] = {
            "container_path": str(configured_path),
            "max_files": int(
                getattr(self.args, "diagnostic_log_max_files", DEFAULT_DIAGNOSTIC_LOG_MAX_FILES)
            ),
            "max_bytes": int(
                getattr(self.args, "diagnostic_log_max_bytes", DEFAULT_DIAGNOSTIC_LOG_MAX_BYTES)
            ),
            "files": [],
            "total_original_bytes": 0,
            "total_copied_bytes": 0,
            "skipped_external_lines": 0,
            "reason": None,
        }
        source_dir = self._diagnostic_log_source()
        if source_dir is None:
            result["reason"] = "未找到目标容器 /app/logs 的宿主机挂载路径"
            write_json(run_dir / "diagnostic-log-index.json", result)
            self.add_event("未找到 tool-format-debug 挂载路径", "warning")
            return result
        result["host_path"] = str(source_dir)
        if not source_dir.is_dir():
            result["reason"] = "tool-format-debug 目录不存在"
            write_json(run_dir / "diagnostic-log-index.json", result)
            self.add_event("目标容器没有 tool-format-debug 目录", "info")
            return result
        try:
            since_epoch = dt.datetime.fromisoformat(
                since.replace("Z", "+00:00")
            ).timestamp()
        except (TypeError, ValueError, OverflowError):
            since_epoch = 0.0
        max_files = max(1, min(256, int(result["max_files"])))
        max_bytes = max(1024, min(512 * 1024 * 1024, int(result["max_bytes"])))
        candidates = []
        for path in source_dir.glob("*.jsonl"):
            try:
                stat = path.stat()
            except OSError:
                continue
            if stat.st_mtime + 2 < since_epoch:
                continue
            candidates.append((stat.st_mtime, path))
        candidates.sort(reverse=True, key=lambda item: item[0])
        target_dir = run_dir / "tool-format-debug"
        remaining = max_bytes
        for _, source in candidates[:max_files]:
            if remaining <= 0:
                break
            try:
                per_file_limit = min(8 * 1024 * 1024, remaining)
                copied = self._copy_diagnostic_jsonl(
                    source,
                    target_dir / source.name,
                    per_file_limit,
                )
            except OSError as exc:
                result.setdefault("errors", []).append(
                    {"source": str(source), "error": repr(exc)}
                )
                continue
            result["files"].append(copied)
            result["total_original_bytes"] += int(copied["original_bytes"])
            result["total_copied_bytes"] += int(copied["copied_bytes"])
            result["skipped_external_lines"] += int(
                copied["excluded_external_lines"]
            )
            remaining -= int(copied["copied_bytes"])
        if not result["files"]:
            result["reason"] = "采集窗口内没有新的 tool-format-debug JSONL"
        write_json(run_dir / "diagnostic-log-index.json", result)
        self.add_event(
            "tool-format-debug 诊断记录已读取",
            details={
                "files": len(result["files"]),
                "bytes": result["total_copied_bytes"],
                "external_lines_skipped": result["skipped_external_lines"],
            },
        )
        return result

    def capture(self, run_dir: pathlib.Path) -> dict[str, object]:
        output = run_dir / "traffic.pcap"
        self.add_event(
            "启动旁路抓包进程，仅过滤目标容器到配置的上游端口流量",
            details={"target_port": self.args.target_port, "upstream_ports": self.args.upstream_port},
        )
        keylog_path = self.prepare_keylog(run_dir)
        progress_tracker = PcapProgress(
            output,
            str(self.status.get("started_at") or utc_now()),
            float(self.args.duration),
        )
        initial_progress = self.progress_with_keylog(progress_tracker.poll(), keylog_path)
        self.update_progress(initial_progress)
        self.append_runtime_timeline(run_dir, initial_progress, "capture_start")
        next_runtime_snapshot = time.monotonic() + self.runtime_snapshot_interval()
        capture_command = [
            self.args.python,
            str(pathlib.Path(__file__).parent.parent.parent / "scripts/diagnostics/capture_container_tls.py"),
            "--interface",
            self.args.interface,
            "--source-ip",
            self.args.source_ip,
            "--output",
            str(output),
            "--duration",
            str(self.args.duration),
        ]
        for port in self.args.upstream_port:
            capture_command += ["--port", str(port)]
        namespace_mode = getattr(self.args, "network_namespace", "host")
        if namespace_mode == "container" and self.args.interface == "any":
            capture_command[capture_command.index("--interface") + 1] = "eth0"
        command = capture_command
        if namespace_mode == "container":
            pid_result = run_readonly(["docker", "inspect", "-f", "{{.State.Pid}}", self.args.container])
            pid_text = str(pid_result.get("stdout", "")).strip()
            if pid_result.get("returncode") != 0 or not pid_text.isdigit() or int(pid_text) <= 0:
                write_json(run_dir / "network-namespace-command.json", pid_result)
                return {
                    "command": ["nsenter", "-t", pid_text, "-n", *capture_command],
                    "returncode": None,
                    "stdout": "",
                    "stderr": "target container PID unavailable for network namespace capture",
                    "elapsed_ms": 0.0,
                }
            command = ["nsenter", "-t", pid_text, "-n", *capture_command]
            write_json(
                run_dir / "network-namespace-command.json",
                {"container": self.args.container, "pid": int(pid_text), "command": command},
            )
        if self.args.destination_ip:
            for value in self.args.destination_ip:
                command += ["--destination-ip", value]
        started = time.monotonic()
        try:
            process = subprocess.Popen(
                command,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            with self.lock:
                self.capture_process = process
                pause_requested = self.pause_requested
                stop_requested = self.stop_requested
            self.add_event("抓包进程已启动", details={"pid": process.pid})
            if stop_requested:
                self.signal_capture_group(signal.SIGTERM)
            elif pause_requested:
                self.signal_capture_group(signal.SIGSTOP)
            stdout = ""
            stderr = ""
            while True:
                try:
                    stdout, stderr = process.communicate(timeout=CAPTURE_POLL_INTERVAL)
                    break
                except subprocess.TimeoutExpired:
                    progress = self.progress_with_keylog(
                        progress_tracker.poll(),
                        keylog_path,
                    )
                    self.update_progress(progress)
                    snapshot_interval = self.runtime_snapshot_interval()
                    if snapshot_interval and time.monotonic() >= next_runtime_snapshot:
                        self.append_runtime_timeline(
                            run_dir,
                            progress,
                            "periodic",
                        )
                        next_runtime_snapshot = time.monotonic() + snapshot_interval
            final_progress = self.progress_with_keylog(
                progress_tracker.finish(),
                keylog_path,
            )
            self.update_progress(final_progress)
            self.append_runtime_timeline(run_dir, final_progress, "capture_finish")
            result = {
                "command": command,
                "returncode": process.returncode,
                "stdout": stdout,
                "stderr": stderr,
                "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
            }
            self.add_event(
                "抓包进程已退出",
                "info" if process.returncode == 0 else "error",
                {"returncode": process.returncode, "elapsed_ms": result["elapsed_ms"]},
            )
        except subprocess.TimeoutExpired:
            self.signal_capture_group(signal.SIGTERM, process)
            stdout, stderr = process.communicate(timeout=5)
            final_progress = self.progress_with_keylog(
                progress_tracker.finish(),
                keylog_path,
            )
            self.update_progress(final_progress)
            self.append_runtime_timeline(run_dir, final_progress, "capture_timeout")
            result = {
                "command": command,
                "returncode": process.returncode,
                "stdout": stdout,
                "stderr": stderr + " capture process exceeded timeout",
                "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
            }
        except OSError as exc:
            final_progress = self.progress_with_keylog(
                progress_tracker.finish("failed"),
                keylog_path,
            )
            self.update_progress(final_progress)
            self.append_runtime_timeline(run_dir, final_progress, "capture_failed")
            result = {
                "command": command,
                "returncode": None,
                "stdout": "",
                "stderr": repr(exc),
                "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
            }
            self.add_event("抓包进程启动失败", "error", {"error": repr(exc)})
        finally:
            with self.lock:
                self.capture_process = None
        write_json(run_dir / "pcap-command.json", result)
        if output.exists() and output.stat().st_size > self.args.max_bytes:
            output.unlink(missing_ok=True)
            result["stderr"] = str(result.get("stderr", "")) + " artifact exceeded max bytes and was removed"
            result["returncode"] = None
            self.add_event("抓包文件超过大小上限，已删除", "error")
        result["progress"] = self.progress_with_keylog(
            progress_tracker.finish(), keylog_path
        )
        if keylog_path is not None and keylog_path.exists():
            keylog_copy = run_dir / "tls-keylog.log"
            shutil.copyfile(keylog_path, keylog_copy)
            keylog_copy.chmod(0o600)
            result["tls_keylog"] = {
                "path": str(keylog_path),
                "artifact": keylog_copy.name,
                "bytes": keylog_copy.stat().st_size,
                "available": keylog_copy.stat().st_size > 0,
            }
            self.add_event(
                "TLS 会话密钥文件状态已读取",
                details={"bytes": result["tls_keylog"]["bytes"]},
            )
        else:
            result["tls_keylog"] = {
                "available": False,
                "reason": "59137 未配置可访问的 TLS key log 文件。",
            }
            self.add_event("未发现 TLS 会话密钥文件；采集包中的 TLS 负载保持加密", "warning")
        return result

    def decrypt_capture(self, run_dir: pathlib.Path) -> dict[str, object]:
        """Try offline TLS/HTTP2 decryption without touching the target service."""
        script = (
            pathlib.Path(__file__).parent.parent.parent
            / "scripts/diagnostics/decrypt_tls_http2.py"
        )
        command = [
            self.args.python,
            str(script),
            "--pcap",
            str(run_dir / "traffic.pcap"),
            "--output-dir",
            str(run_dir),
            "--max-bytes",
            str(self.args.max_readable_bytes),
            "--max-tshark-output-bytes",
            str(max(self.args.max_readable_bytes, self.args.max_bytes)),
        ]
        keylog = run_dir / "tls-keylog.log"
        if keylog.is_file() and keylog.stat().st_size > 0:
            command.extend(["--keylog", str(keylog)])
        tshark = self.tshark_executable()
        if tshark:
            command.extend(["--tshark", tshark])
        self.add_event(
            "正在尝试离线解密 HTTPS 并整理 HTTP/2 对话",
            details={
                "tls_keylog_available": keylog.is_file() and keylog.stat().st_size > 0,
                "tshark": tshark or self.tshark_executable(),
            },
        )
        result = run_readonly(command, timeout=180)
        write_json(run_dir / "tls-decryption-command.json", result)
        report = read_json(run_dir / "tls-decryption.json")
        if isinstance(report, dict):
            status = str(report.get("status") or "unavailable")
            if status == "decrypted":
                self.add_event(
                    "HTTPS 明文已解密并写入 https-readable.txt",
                    details={
                        "http2_streams": report.get("http2_streams", 0),
                        "decrypted_payload_bytes": report.get(
                            "decrypted_payload_bytes", 0
                        ),
                    },
                )
            elif status == "truncated":
                self.add_event("HTTPS 明文已解密，但可读文本达到大小上限", "warning")
            else:
                self.add_event(
                    f"HTTPS 正文无法解密：{report.get('reason') or status}",
                    "warning",
                )
            return report
        reason = (
            str(result.get("stderr") or result.get("stdout") or "解密脚本未生成结果")
        )[-4000:]
        self.add_event("HTTPS 解密辅助程序未能生成报告", "error", {"detail": reason})
        fallback = {
            "status": "failed",
            "tls_decryption": False,
            "http2_streams": 0,
            "reason": reason,
            "errors": [reason],
        }
        (run_dir / "https-readable.txt").write_text(
            "HTTPS 明文内容不可用\n====================\n"
            f"原因：{reason}\n",
            encoding="utf-8",
        )
        write_json(run_dir / "tls-decryption.json", fallback)
        return fallback

    def write_readable_files(
        self,
        run_dir: pathlib.Path,
        manifest: dict[str, object],
        progress: dict[str, object] | None,
    ) -> None:
        """Add operator-facing text without replacing the raw evidence files."""
        current = progress or empty_progress()
        events = self.event_snapshot()
        event_lines = []
        for event in events:
            event_lines.append(
                f"{event.get('at', '--')} [{event.get('level', 'info')}] "
                f"{event.get('message', '')}"
            )
        (run_dir / "capture-events.txt").write_text(
            "\n".join(event_lines) + ("\n" if event_lines else ""),
            encoding="utf-8",
        )
        summary = [
            "Kiro 独立采集摘要",
            "==================",
            f"采集开始：{manifest.get('started_at') or '未知'}",
            f"采集结束：{manifest.get('finished_at') or '未知'}",
            f"目标容器：{self.args.container}",
            f"目标服务端口：{self.args.target_port}",
            f"采集阶段：{current.get('phase') or '未知'}",
            f"已用时长：{current.get('elapsed_seconds', 0)} 秒",
            f"pcap 文件：{format_bytes(int(current.get('pcap_bytes') or 0))}",
            f"pcap 记录：{current.get('pcap_records', 0)}",
            f"TCP 数据包：{current.get('tcp_packets', 0)}，负载：{format_bytes(int(current.get('tcp_payload_bytes') or 0))}",
            f"UDP 数据包：{current.get('udp_packets', 0)}，负载：{format_bytes(int(current.get('udp_payload_bytes') or 0))}",
            f"TCP 会话：{current.get('session_count', 0)}（活跃 {current.get('active_session_count', 0)}），UDP 流：{current.get('udp_flow_count', 0)}",
            f"FIN：{current.get('fin_packets', 0)}，RST：{current.get('rst_packets', 0)}",
            f"首个数据包：{current.get('first_packet_at') or '无'}",
            f"最后数据包：{current.get('last_packet_at') or '无'}",
            f"TLS 会话密钥：{'可用' if current.get('tls_keylog_available') else '不可用（TLS 负载仍是密文）'}",
            "",
            "如何阅读：",
            "- capture-events.txt：按时间排列的采集器实时事件，可直接用文本编辑器打开。",
            "- capture-events.json：同一事件的结构化版本，适合脚本分析。",
            "- capture-summary.txt：本文件，汇总本次采集事实。",
            "- https-readable.txt：有可用 TLS keylog 和 tshark 时，展示解密后的 HTTP/2 请求正文及上游 EventStream；否则解释缺少条件。",
            "- tls-decryption.json：解密结果、会话数量、正文大小和失败原因。",
            "- max-readable-bytes：控制 https-readable.txt 的明文输出上限；完整长会话需要调大该值。",
            "- pcap-analysis.json：TCP 会话和 UDP 流的间隔分析，可用文本编辑器打开；它不是 HTTP 请求列表。",
            "- traffic.pcap：原始二进制底层证据，日常分析不用打开。",
            "- tls-keylog.log：仅在采集器配置了可读取的 TLS keylog 后出现；文件内是会话密钥。",
            "- container.log：去除颜色控制符后的容器日志。",
            "- state-start.json/state-end.json：采集开始和结束时的容器与只读运行状态快照。",
            "- runtime-timeline.jsonl/runtime-timeline.txt：采集期间的 pcap 进度、端口探测和只读本地账号 Admin 时间线。",
            "- usage-records-local.json/usage-window.txt：采集窗口内本地账号 usage 记录的结构化和可读摘要。",
            "- diagnostic-log-index.json：tool-format-debug 只读复制的文件清单、大小和外部池过滤计数。",
            "- tool-format-debug/：限量复制的本地 tool-format JSONL 诊断记录。",
            "",
            "边界：",
            "pcap 只证明采集到的 TCP/TLS 或 UDP/QUIC 传输层字节、时间间隔和 TCP FIN/RST；没有 TLS 会话密钥时不能从中读取上游 EventStream 文本。",
            "缺少 SYN/FIN/RST 不等于应用请求一定没有结束；需要结合 TLS 解密、容器日志和应用侧事件记录判断。",
        ]
        (run_dir / "capture-summary.txt").write_text(
            "\n".join(summary) + "\n", encoding="utf-8"
        )
        analysis = read_json(run_dir / "pcap-analysis.json")
        flow_lines = [
            "传输流列表（从 pcap 事实生成；不是 HTTP 请求列表）",
            "==================================================",
        ]
        if isinstance(analysis, dict):
            flow_lines.extend(
                [
                    f"数据包记录：{analysis.get('pcapRecords', 0)}",
                    f"传输流：{analysis.get('connectionTupleCount', 0)}（TCP {analysis.get('tcpConnectionTupleCount', 0)}，UDP {analysis.get('udpFlowCount', 0)}）；TCP 会话：{analysis.get('tcpConnectionTupleCount', analysis.get('connectionTupleCount', 0))}",
                    f"抓包首包：{analysis.get('captureFirstPacketUtc') or '无'}",
                    f"抓包末包：{analysis.get('captureLastPacketUtc') or '无'}",
                    "",
                ]
            )
            connections = analysis.get("connections")
            if isinstance(connections, list) and connections:
                for index, connection in enumerate(connections, 1):
                    flow_lines.extend(
                        [
                            f"[{index}] {connection.get('protocol', 'tcp').upper()} {connection.get('endpointA')} <-> {connection.get('endpointB')}",
                            f"    首包：{connection.get('firstSeenUtc')}，末包：{connection.get('lastSeenUtc')}",
                            f"    持续：{connection.get('durationSec', 0)} 秒，数据包：{connection.get('packets', 0)}，TCP 负载：{format_bytes(int(connection.get('tcpPayloadBytes') or 0))}，UDP 负载：{format_bytes(int(connection.get('udpPayloadBytes') or 0))}",
                            f"    最大包间隔：{connection.get('maxPacketGapSec', 0)} 秒，P95 包间隔：{connection.get('p95PacketGapSec', 0)} 秒",
                            f"    看到连接关闭：{'是' if connection.get('closureObserved') else '否'}；可能在采集前已开始：{'是' if connection.get('possibleStartBeforeCapture') else '否'}；可能在采集后仍继续：{'是' if connection.get('possibleContinuationAfterCapture') else '否'}",
                            "",
                        ]
                    )
            else:
                flow_lines.append("没有解析到 TCP 会话或 UDP 流。")
        else:
            flow_lines.append("没有可用的 pcap-analysis.json。")
        flow_lines.extend(
            [
                "说明：没有 FIN/RST 只表示抓包窗口内未观察到 TCP 关闭；UDP 没有传输层关闭标志，均不足以证明上游应用没有结束。",
                "要判断 EventStream 是否有结束标识，还需要 TLS 明文或 kiro.rs 应用侧协议事件记录。",
            ]
        )
        (run_dir / "tcp-flows.txt").write_text("\n".join(flow_lines) + "\n", encoding="utf-8")

        keylog_available = bool(current.get("tls_keylog_available"))
        decryption = read_json(run_dir / "tls-decryption.json")
        decryption = decryption if isinstance(decryption, dict) else {}
        decryption_status = str(decryption.get("status") or "unavailable")
        upstream_lines = [
            "上游协议可读性",
            "================",
            f"TLS 会话密钥：{'已采集' if keylog_available else '未采集'}",
            f"离线解密状态：{decryption_status}",
            f"HTTP/2 会话：{decryption.get('http2_streams', 0)}",
            f"已解密正文：{format_bytes(int(decryption.get('decrypted_payload_bytes') or 0))}",
            "",
        ]
        if decryption_status in {"decrypted", "truncated"}:
            upstream_lines.extend(
                [
                    "独立采集器已使用 tshark 和 TLS keylog 离线解密本次 pcap。",
                    "请直接打开 https-readable.txt 查看每个 HTTP/2 请求正文、响应状态、EventStream 事件类型和 payload。",
                    "若状态为 truncated，部分会话因可读文本大小上限没有写入；上限和详情见 tls-decryption.json。",
                    "明文可能包含 system prompt、用户对话、工具定义、工具输入输出、thinking/reasoning、模型回复和认证头。",
                ]
            )
        else:
            reason = str(
                decryption.get("reason")
                or "当前采集未具备 HTTPS 明文解密条件。"
            )
            upstream_lines.extend(
                [
                    f"本次未能生成 HTTPS 明文：{reason}",
                    "请打开 https-readable.txt 查看同一原因及下一步所需条件。",
                    "当前没有 TLS 会话密钥时，不能从旁路 pcap 还原 HTTPS 正文。",
                    "现在能直接确认的内容只有 TCP/UDP 流、包数、字节数、时间间隔以及 TCP FIN/RST。",
                    "要从独立旁路包还原 HTTPS 正文，需要 TLS keylog 覆盖本次连接，并在采集机安装 tshark。",
                ]
            )
        (run_dir / "upstream-readable.txt").write_text("\n".join(upstream_lines) + "\n", encoding="utf-8")
        (run_dir / "runtime-timeline.txt").write_text(
            self.runtime_timeline_text(run_dir),
            encoding="utf-8",
        )
        (run_dir / "usage-window.txt").write_text(
            self.usage_window_text(run_dir),
            encoding="utf-8",
        )

        state_lines = [
            "运行状态可读摘要",
            "================",
            "以下内容来自采集开始和结束时的只读 Admin/Docker 快照。",
            "",
        ]
        for phase in ("start", "end"):
            snapshot = read_json(run_dir / f"state-{phase}.json")
            state_lines.append(f"[{phase}]")
            if not isinstance(snapshot, dict):
                state_lines.append("快照不可用")
                state_lines.append("")
                continue
            state_lines.append(f"采集时间：{snapshot.get('collected_at', '未知')}")
            socket_probe = snapshot.get("socket_probe")
            state_lines.append(
                f"端口探测：{socket_probe.get('ok') if isinstance(socket_probe, dict) else '未知'}"
            )
            admin = snapshot.get("admin_state")
            endpoints = admin.get("endpoints", {}) if isinstance(admin, dict) else {}
            summary_endpoint = endpoints.get("credential_summary")
            summary_body = summary_endpoint.get("body") if isinstance(summary_endpoint, dict) else None
            if isinstance(summary_body, dict):
                state_lines.append(
                    "全局并发：{}，排队：{}，可用本地账号：{}，最大并发：{}".format(
                        summary_body.get("globalInFlightRequests", "未知"),
                        summary_body.get("queuedRequests", "未知"),
                        summary_body.get("available", "未知"),
                        summary_body.get("globalMaxConcurrentRequests", "未知"),
                    )
                )
            local_endpoint = endpoints.get("local_credential_runtime")
            local_body = local_endpoint.get("body") if isinstance(local_endpoint, dict) else None
            if isinstance(local_body, dict):
                items = local_body.get("items")
                state_lines.append(f"本地账号运行时条目：{len(items) if isinstance(items, list) else '未知'}")
            state_lines.append("")
        (run_dir / "runtime-state.txt").write_text("\n".join(state_lines), encoding="utf-8")
        readme = """本采集包文件说明
==================

这是 kiro.rs 59137 的独立旁路采集证据。采集器不会替换或重启 59137。

直接用文本编辑器先看：
0. llm-readable.txt：kiro.rs 进程内明文采集的对话报告（系统提示词、工具定义、用户消息、
   发往上游的 Kiro 请求、上游 EventStream 帧、返回客户端的回答）。原始事件见 llm-events.jsonl。
1. capture-summary.txt：本次采集的可读摘要。
2. capture-events.txt：采集过程实时事件的文本版。
3. container.log：目标容器日志。
4. tcp-flows.txt：每条 TCP 连接的时间、字节、间隔、FIN/RST。
5. runtime-state.txt：采集前后并发、排队、本地账号和容器状态摘要。
6. runtime-timeline.txt：采集期间的 pcap 进度、端口探测和只读本地账号 Admin 时间线。
7. usage-window.txt：采集窗口内本地 usage 记录的可读摘要。
8. diagnostic-log-index.json：tool-format-debug 只读复制清单。
9. https-readable.txt：具备 TLS keylog 和 tshark 时，展示 HTTPS 请求正文、上游 EventStream 事件和 payload。
10. upstream-readable.txt：本次 HTTPS 解密结果及缺失条件。

原始证据（通常不直接打开）：
- traffic.pcap 是二进制原始网络包；日常先看 tcp-flows.txt 和 upstream-readable.txt。
- tls-keylog.log 存在且采集机安装 tshark 时，采集器会自动解密 pcap 并写入 https-readable.txt。
- https-readable.txt 中包含完整对话正文时，可能包含 system prompt、用户内容、工具 schema、工具输入输出、thinking/reasoning、模型输出和认证头。
- tls-decryption.json 会说明解密状态、解析到的 HTTP/2 会话数和任何截断情况。
- 如果没有 tls-keylog.log，traffic.pcap 中的应用负载保持密文，这是采集能力限制，不是文件损坏。
- 如果 `tls-decryption.json.status` 为 `truncated`，需要提高 `--max-readable-bytes` 后重新采集，才能让长会话完整写入可读文本。
- runtime-timeline.jsonl 是逐条 JSON 时间线，可与 usage_records、Redis lease、HTTP/2 frame 时间对齐。
- 外部池状态默认不采集；需要跨池诊断时必须显式启动参数开启，避免污染本地账号分析。

结构化证据：
- manifest.json：采集范围、时间、能力和错误。
- capture-events.json：实时事件的 JSON 版。
- state-start.json/state-end.json：容器、资源、进程、Admin 只读快照。
- usage-records-local.json：本地账号 usage 记录的结构化快照。
- tool-format-debug/：本地 tool-format-debug JSONL 的限量副本。
- *-command.json：执行过的只读命令、返回码和输出。
"""
        (run_dir / "README.txt").write_text(readme, encoding="utf-8")
        readable_parts = (
            "README.txt",
            "llm-readable.txt",
            "capture-summary.txt",
            "runtime-state.txt",
            "runtime-timeline.txt",
            "usage-window.txt",
            "tcp-flows.txt",
            "upstream-readable.txt",
            "https-readable.txt",
            "capture-events.txt",
        )
        combined = []
        for name in readable_parts:
            path = run_dir / name
            if path.is_file():
                combined.extend(
                    [
                        "",
                        "=" * 72,
                        f"文件：{name}",
                        "=" * 72,
                        path.read_text(encoding="utf-8", errors="replace"),
                    ]
                )
        combined_path = run_dir / "capture-readable.txt"
        combined_path.write_text("\n".join(combined).lstrip() + "\n", encoding="utf-8")
        combined_path.chmod(0o600)

    def prepare_keylog(self, run_dir: pathlib.Path) -> pathlib.Path | None:
        configured = getattr(self.args, "tls_keylog_file", None)
        if not configured:
            return None
        path = pathlib.Path(configured).expanduser()
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            existing_bytes = path.stat().st_size if path.exists() else 0
            write_json(
                run_dir / "tls-keylog-command.json",
                {
                    "path": str(path),
                    "action": "observe-without-truncation",
                    "existing_bytes": existing_bytes,
                    "ok": True,
                },
            )
            return path
        except OSError as exc:
            write_json(
                run_dir / "tls-keylog-command.json",
                {
                    "path": str(path),
                    "action": "truncate-before-capture",
                    "ok": False,
                    "error": repr(exc),
                },
            )
            return None

    @staticmethod
    def progress_with_keylog(
        progress: dict[str, object], keylog_path: pathlib.Path | None
    ) -> dict[str, object]:
        result = dict(progress)
        keylog_bytes = 0
        if keylog_path is not None:
            try:
                keylog_bytes = keylog_path.stat().st_size
            except OSError:
                keylog_bytes = 0
        result["tls_keylog_available"] = keylog_bytes > 0
        result["tls_keylog_bytes"] = keylog_bytes
        return result

    def app_capture_enabled(self) -> bool:
        mode = getattr(self.args, "app_capture", "auto")
        if mode == "off":
            return False
        return bool(self.args.admin_url and self.args.admin_token_file)

    def app_capture_client(self) -> app_capture.AdminCaptureClient:
        token = pathlib.Path(self.args.admin_token_file).read_text(encoding="utf-8").strip()
        return app_capture.AdminCaptureClient(self.args.admin_url, token)

    def start_app_capture(self) -> dict[str, object]:
        """请求 kiro.rs 开始进程内明文采集；失败只记录，不中断旁路采集。"""
        if not self.app_capture_enabled():
            return {"enabled": False, "reason": "未配置 --admin-url/--admin-token-file 或已关闭 --app-capture"}
        client = self.app_capture_client()
        # 服务端时长留出余量，正常由 sidecar 主动停止；sidecar 异常退出时由服务端到期自停。
        limits = {
            "max_duration_secs": int(self.args.duration) + 120,
            "max_bytes": int(self.args.max_bytes),
            "include_secrets": bool(getattr(self.args, "app_capture_include_secrets", False)),
        }
        try:
            status = client.start(**limits)
        except app_capture.AdminCaptureError as exc:
            self.add_event("kiro.rs 进程内明文采集启动失败", "error", {"error": str(exc)})
            return {"enabled": True, "started": False, "error": str(exc)}
        self.add_event(
            "kiro.rs 进程内明文采集已启动",
            details={"session_id": status.get("sessionId")},
        )
        return {"enabled": True, "started": True, "session_id": status.get("sessionId"), "status": status}

    def finish_app_capture(
        self, run_dir: pathlib.Path, session: dict[str, object]
    ) -> dict[str, object]:
        if not session.get("started"):
            return session
        client = self.app_capture_client()
        session_id = str(session.get("session_id") or "")
        result: dict[str, object] = dict(session)
        try:
            stopped = client.stop()
        except app_capture.AdminCaptureError as exc:
            # 可能已被服务端到期停止；按会话 ID 继续尝试下载。
            stopped = {"error": str(exc)}
        result["stopped"] = stopped
        try:
            archive = run_dir / "llm-capture.zip"
            result["archive_bytes"] = client.download(session_id, archive)
            events_path = app_capture.extract_archive(archive, run_dir)
            archive.unlink(missing_ok=True)
        except (app_capture.AdminCaptureError, OSError, zipfile.BadZipFile) as exc:
            self.add_event("kiro.rs 明文采集结果下载失败", "error", {"error": str(exc)})
            result["error"] = str(exc)
            return result
        event_count = 0
        if events_path is not None:
            with events_path.open("rb") as handle:
                event_count = sum(1 for _ in handle)
            (run_dir / "llm-readable.txt").write_text(
                app_capture.render_readable(events_path, self.args.max_readable_bytes),
                encoding="utf-8",
            )
        result["event_count"] = event_count
        result["dropped_events"] = stopped.get("droppedEvents") if isinstance(stopped, dict) else None
        self.add_event(
            "kiro.rs 明文采集结果已写入 llm-readable.txt",
            "info" if event_count else "warning",
            {"events": event_count, "dropped": result["dropped_events"]},
        )
        return result

    def wait_window(self, run_dir: pathlib.Path) -> dict[str, object]:
        """--no-pcap 模式：不抓包，只等待采集窗口结束或收到停止请求。"""
        started = time.monotonic()
        progress = empty_progress()
        progress.update({"phase": "running", "started_at": utc_now()})
        self.update_progress(progress)
        self.append_runtime_timeline(run_dir, progress, "capture_start")
        next_snapshot = time.monotonic() + self.runtime_snapshot_interval()
        while time.monotonic() - started < float(self.args.duration):
            with self.lock:
                if self.stop_requested:
                    break
            time.sleep(CAPTURE_POLL_INTERVAL)
            progress["elapsed_seconds"] = round(time.monotonic() - started, 1)
            progress["updated_at"] = utc_now()
            self.update_progress(dict(progress))
            interval = self.runtime_snapshot_interval()
            if interval and time.monotonic() >= next_snapshot:
                self.append_runtime_timeline(run_dir, progress, "periodic")
                next_snapshot = time.monotonic() + interval
        progress.update({"phase": "finished", "finished_at": utc_now()})
        self.update_progress(dict(progress))
        self.append_runtime_timeline(run_dir, progress, "capture_finish")
        return {
            "command": None,
            "returncode": 0,
            "stdout": "",
            "stderr": "",
            "skipped": "pcap disabled by --no-pcap",
            "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
            "progress": progress,
            "tls_keylog": {"available": False, "reason": "--no-pcap 模式未抓包"},
        }

    def run(self) -> None:
        started_at = utc_now()
        self.add_event("采集任务已启动")
        run_dir = pathlib.Path(tempfile.mkdtemp(prefix="capture-", dir=self.root))
        manifest: dict[str, object] = {
            "schema": 1,
            "started_at": started_at,
            "target": {"container": self.args.container, "port": self.args.target_port},
            "limits": {"duration_seconds": self.args.duration, "max_artifact_bytes": self.args.max_bytes},
            "capabilities": {
                "payload_decryption": False,
                "tls_keylog": False,
                "tshark": bool(self.tshark_executable()),
                "pcap": False,
                "usage_records": bool(self.args.admin_url),
                "tool_format_debug": False,
            },
            "errors": [],
        }
        try:
            self.snapshot_state(run_dir, "start")
            app_session = self.start_app_capture()
            if getattr(self.args, "no_pcap", False):
                pcap_result = self.wait_window(run_dir)
            else:
                pcap_result = self.capture(run_dir)
            app_result = self.finish_app_capture(run_dir, app_session)
            manifest["app_capture"] = app_result
            progress = pcap_result.get("progress")
            tls_keylog = pcap_result.get("tls_keylog")
            if isinstance(progress, dict):
                progress["tls_keylog_available"] = bool(
                    isinstance(tls_keylog, dict) and tls_keylog.get("available")
                )
                progress["tls_keylog_bytes"] = (
                    tls_keylog.get("bytes")
                    if isinstance(tls_keylog, dict) and tls_keylog.get("available")
                    else 0
                )
            if isinstance(progress, dict):
                manifest["capture_progress"] = progress
            manifest["capabilities"] = {
                "payload_decryption": False,
                "tls_keylog": False,
                "tshark": bool(
                    self.tshark_executable()
                ),
                "pcap": pcap_result.get("returncode") == 0,
                "usage_records": bool(self.args.admin_url),
                "tool_format_debug": False,
                "app_capture": bool(app_result.get("event_count")),
            }
            if isinstance(tls_keylog, dict) and tls_keylog.get("available"):
                manifest["capabilities"]["tls_keylog"] = True
            else:
                manifest["capabilities"]["tls_keylog_reason"] = (
                    tls_keylog.get("reason")
                    if isinstance(tls_keylog, dict)
                    else "TLS key log unavailable"
                )
            if pcap_result.get("returncode") != 0:
                manifest["errors"].append({"step": "pcap", "detail": pcap_result})
                self.add_event("抓包阶段返回失败", "error")
            pcap_seen = bool(isinstance(progress, dict) and progress.get("data_seen"))
            data_seen = pcap_seen or bool(app_result.get("event_count"))
            if pcap_seen and (run_dir / "traffic.pcap").exists():
                analyzer = pathlib.Path(__file__).parent.parent.parent / "scripts/diagnostics/analyze_pcap_flows.py"
                analysis = run_readonly([self.args.python, str(analyzer), str(run_dir / "traffic.pcap")], timeout=15)
                write_json(run_dir / "pcap-analysis-command.json", analysis)
                if analysis.get("returncode") == 0:
                    (run_dir / "pcap-analysis.json").write_text(str(analysis.get("stdout", "")), encoding="utf-8")
                    self.add_event("TCP 会话与 UDP 流分析已生成")
                else:
                    manifest["errors"].append({"step": "pcap_analysis", "detail": analysis})
                    self.add_event("TCP 会话与 UDP 流分析失败", "error")
                decryption = self.decrypt_capture(run_dir)
                manifest["tls_decryption"] = decryption
                manifest["capabilities"]["payload_decryption"] = bool(
                    decryption.get("tls_decryption")
                )
                manifest["capabilities"]["http2_streams"] = int(
                    decryption.get("http2_streams") or 0
                )
                if decryption.get("status") in {"failed", "error"}:
                    manifest["errors"].append(
                        {"step": "tls_decryption", "detail": decryption}
                    )
            self.snapshot_state(run_dir, "end")
            self.logs(run_dir, started_at)
            diagnostic_logs = self.collect_diagnostic_logs(run_dir, started_at)
            manifest["diagnostic_logs"] = diagnostic_logs
            manifest["capabilities"]["tool_format_debug"] = bool(
                diagnostic_logs.get("files")
            )
            manifest["finished_at"] = utc_now()
            if isinstance(progress, dict):
                progress["finished_at"] = manifest["finished_at"]
            if not data_seen:
                manifest["result"] = "no_data"
                manifest["errors"].append(
                    {
                        "step": "capture",
                        "detail": "no packets matched the configured capture filters and no kiro.rs capture events were recorded",
                    }
                )
                self.add_event("采集窗口内没有数据包也没有 kiro.rs 明文事件，未生成采集包", "warning")
            else:
                self.add_event(
                    "采集数据已整理，正在生成采集包",
                    details={"artifact": f"{run_dir.name}.zip"},
                )
            self.write_readable_files(run_dir, manifest, progress if isinstance(progress, dict) else None)
            write_json(run_dir / "manifest.json", manifest)
            write_json(run_dir / "capture-events.json", self.event_snapshot())
            artifact = None
            if data_seen:
                artifact = self.root / f"{run_dir.name}.zip"
                with zipfile.ZipFile(artifact, "w", zipfile.ZIP_DEFLATED) as archive:
                    for path in sorted(run_dir.rglob("*")):
                        if path.is_file():
                            archive.write(path, path.relative_to(run_dir))
                artifact.chmod(0o600)
                readable_artifact = self.root / f"{run_dir.name}-readable.txt"
                shutil.copyfile(run_dir / "capture-readable.txt", readable_artifact)
                readable_artifact.chmod(0o600)
                events_source = run_dir / "llm-events.jsonl"
                if events_source.is_file():
                    events_artifact = self.root / f"{run_dir.name}-events.jsonl"
                    shutil.copyfile(events_source, events_artifact)
                    events_artifact.chmod(0o600)
            else:
                self.add_event("采集任务完成，但没有可发布的采集包", "warning")
            shutil.rmtree(run_dir, ignore_errors=True)
            with self.lock:
                capture_error = None
                if pcap_result.get("returncode") != 0:
                    capture_error = str(pcap_result.get("stderr") or "抓包进程返回失败")
                self.status.update(
                    {
                        "state": "completed",
                        "last_artifact": artifact.name if artifact else self.status.get("last_artifact"),
                        "last_error": capture_error,
                        "last_message": (
                            None if artifact else (
                                "本次采集未捕获到符合过滤条件的网络数据，未生成采集包。"
                                if not capture_error
                                else "抓包过程失败，未生成采集包。"
                            )
                        ),
                        "last_capture": progress or empty_progress(),
                        "progress": progress or empty_progress(),
                        "progress_updated_at": utc_now(),
                        "paused_at": None,
                    }
                )
        except Exception as exc:  # evidence collection must report failure and remain controllable
            manifest["finished_at"] = utc_now()
            manifest["errors"].append({"step": "collector", "detail": repr(exc)})
            self.add_event("采集任务异常结束", "error", {"error": repr(exc)})
            write_json(run_dir / "manifest.json", manifest)
            write_json(run_dir / "capture-events.json", self.event_snapshot())
            with self.lock:
                self.status.update(
                    {
                        "state": "failed",
                        "last_error": repr(exc),
                        "last_message": "采集过程失败，未生成采集包。",
                        "last_capture": self.current_progress(),
                        "progress_updated_at": utc_now(),
                    }
                )
        finally:
            with self.lock:
                if self.status.get("state") in {"running", "paused", "stopping"}:
                    self.status["state"] = "completed"
                    self.status["paused_at"] = None

    def start(self) -> bool:
        with self.lock:
            if self.thread and self.thread.is_alive():
                return False
            self.events.clear()
            self.event_seq = 0
            self._last_event_records = 0
            self._last_event_bytes = 0
            self._last_event_sessions = 0
            self._last_event_active_sessions = 0
            self._last_event_keylog_bytes = 0
            self._last_event_at = 0.0
            self.stop_event.clear()
            self.pause_requested = False
            self.stop_requested = False
            self.status.update(
                {
                    "state": "running",
                    "last_error": None,
                    "last_message": None,
                    "last_capture": empty_progress(),
                    "progress": empty_progress(),
                    "started_at": utc_now(),
                    "paused_at": None,
                    "stop_requested": False,
                    "progress_updated_at": utc_now(),
                }
            )
            self.thread = threading.Thread(target=self.run, name="capture", daemon=True)
            self.thread.start()
            self._append_event_locked("已接受开始采集请求")
            return True

    def pause(self) -> bool:
        with self.lock:
            active = (
                bool(self.thread and self.thread.is_alive())
                and self.status.get("state") in {"starting", "running"}
            )
            if not active:
                return False
            self.pause_requested = True
            process = self.capture_process
            if process is not None and not self.signal_capture_group(signal.SIGSTOP, process):
                return False
            self.status.update({"state": "paused", "paused_at": utc_now()})
            self._append_event_locked("已暂停抓包进程")
            return True

    def resume(self) -> bool:
        with self.lock:
            active = (
                bool(self.thread and self.thread.is_alive())
                and self.status.get("state") == "paused"
            )
            if not active:
                return False
            self.pause_requested = False
            process = self.capture_process
            if process is not None and not self.signal_capture_group(signal.SIGCONT, process):
                return False
            self.status.update({"state": "running", "paused_at": None})
            self._append_event_locked("已继续抓包进程")
            return True

    def stop(self) -> bool:
        with self.lock:
            active = bool(self.thread and self.thread.is_alive())
            if active:
                self.stop_requested = True
                self.pause_requested = False
                self.status.update({"state": "stopping", "stop_requested": True})
                process = self.capture_process
                if process is not None:
                    self.signal_capture_group(signal.SIGCONT, process)
                    self.signal_capture_group(signal.SIGTERM, process)
                self._append_event_locked("已请求停止采集并生成采集包")
            return active

    def signal_capture_group(
        self,
        signum: signal.Signals,
        process: subprocess.Popen[str] | None = None,
    ) -> bool:
        target = process or self.capture_process
        if target is None:
            return False
        try:
            os.killpg(target.pid, signum)
            return True
        except (OSError, ProcessLookupError):
            return False

    def artifacts(self) -> list[str]:
        return sorted(
            self.root.glob("*.zip"),
            key=lambda path: path.stat().st_mtime,
            reverse=True,
        )

    def artifact_metadata(self, path: pathlib.Path) -> dict[str, object]:
        stat = path.stat()
        created_at = dt.datetime.fromtimestamp(
            stat.st_mtime, dt.timezone.utc
        ).isoformat(timespec="seconds").replace("+00:00", "Z")
        metadata: dict[str, object] = {
            "name": path.name,
            "created_at": created_at,
            "started_at": None,
            "finished_at": created_at,
            "size_bytes": stat.st_size,
            "duration_seconds": None,
            "pcap_records": None,
            "session_count": None,
            "readable_available": path.with_name(
                f"{path.stem}-readable.txt"
            ).is_file(),
            "events_available": path.with_name(f"{path.stem}-events.jsonl").is_file(),
        }
        # The ZIP manifest is authoritative for collection times. Fall back to
        # filesystem mtime for older artifacts or a partially written archive.
        try:
            with zipfile.ZipFile(path) as archive:
                manifest = json.loads(archive.read("manifest.json").decode("utf-8"))
            started_at = manifest.get("started_at")
            finished_at = manifest.get("finished_at")
            progress = manifest.get("capture_progress")
            if isinstance(started_at, str):
                metadata["started_at"] = started_at
                metadata["created_at"] = started_at
            if isinstance(finished_at, str):
                metadata["finished_at"] = finished_at
            if isinstance(progress, dict):
                metadata["duration_seconds"] = progress.get("elapsed_seconds")
                metadata["pcap_records"] = progress.get("pcap_records")
                metadata["session_count"] = progress.get("session_count")
        except (OSError, KeyError, ValueError, zipfile.BadZipFile):
            pass
        return metadata

    def artifact_page(self, page: int = 1, page_size: int = DEFAULT_ARTIFACT_PAGE_SIZE) -> dict[str, object]:
        page = max(1, page)
        page_size = max(1, min(50, page_size))
        paths = self.artifacts()
        total = len(paths)
        total_pages = max(1, (total + page_size - 1) // page_size)
        page = min(page, total_pages)
        start = (page - 1) * page_size
        items = [self.artifact_metadata(path) for path in paths[start : start + page_size]]
        return {
            "items": items,
            "artifacts": items,
            "page": page,
            "page_size": page_size,
            "total": total,
            "total_pages": total_pages,
        }


class Handler(BaseHTTPRequestHandler):
    collector: Collector
    token: str | None

    def auth_ok(self) -> bool:
        if not self.token:
            return True
        return self.headers.get("Authorization", "") == f"Bearer {self.token}"

    def send_json(self, value: object, status: int = 200) -> None:
        body = json.dumps(value, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        # Keep the control page reachable so an operator can enter the bearer
        # token in the page. API calls and artifact downloads remain protected.
        if self.path.startswith("/api/") and not self.auth_ok():
            self.send_json({"error": "unauthorized"}, HTTPStatus.UNAUTHORIZED)
            return
        if urlparse(self.path).path == "/api/status":
            query = parse_qs(urlparse(self.path).query)
            try:
                page = int(query.get("page", ["1"])[0])
                page_size = int(query.get("page_size", [str(DEFAULT_ARTIFACT_PAGE_SIZE)])[0])
            except ValueError:
                self.send_json({"error": "page and page_size must be integers"}, HTTPStatus.BAD_REQUEST)
                return
            with self.collector.lock:
                value = dict(self.collector.status)
            value["progress"] = value.get("progress") or value.get("last_capture") or empty_progress()
            artifact_page = self.collector.artifact_page(page, page_size)
            value["artifacts"] = artifact_page["items"]
            value["artifacts_page"] = artifact_page["page"]
            value["artifacts_page_size"] = artifact_page["page_size"]
            value["artifacts_total"] = artifact_page["total"]
            value["artifacts_total_pages"] = artifact_page["total_pages"]
            value["data_collection"] = DATA_COLLECTION
            value["capture_capabilities"] = {
                "pcap": True,
                "encrypted_tls_payload": True,
                "tls_keylog_available": bool(
                    isinstance(value.get("progress"), dict)
                    and value["progress"].get("tls_keylog_available")
                ),
                "tshark_available": bool(self.collector.tshark_executable()),
            }
            value["capture_capabilities"]["tls_decryption"] = bool(
                value["capture_capabilities"]["tls_keylog_available"]
                and value["capture_capabilities"]["tshark_available"]
            )
            value["capture_capabilities"]["tls_decryption_reason"] = (
                "TLS keylog 和 tshark 均可用；采集停止后会尝试离线解密，并在采集包中生成 https-readable.txt。"
                if value["capture_capabilities"]["tls_decryption"]
                else "完整 HTTPS 正文还原需要 TLS keylog 覆盖本次连接，并安装 tshark；否则采集包保留密文和缺失条件说明。"
            )
            self.send_json(value)
            return
        if self.path.startswith("/api/artifacts") and not self.path.startswith("/api/artifacts/"):
            query = parse_qs(urlparse(self.path).query)
            try:
                page = int(query.get("page", ["1"])[0])
                page_size = int(query.get("page_size", [str(DEFAULT_ARTIFACT_PAGE_SIZE)])[0])
            except ValueError:
                self.send_json({"error": "page and page_size must be integers"}, HTTPStatus.BAD_REQUEST)
                return
            self.send_json(self.collector.artifact_page(page, page_size))
            return
        if self.path.startswith("/api/artifacts/"):
            name = pathlib.PurePosixPath(unquote(urlparse(self.path).path)).name
            if name != urlparse(self.path).path.removeprefix("/api/artifacts/") or not name.endswith(".zip"):
                self.send_json({"error": "invalid artifact"}, HTTPStatus.BAD_REQUEST)
                return
            path = self.collector.root / name
            if not path.is_file():
                self.send_json({"error": "not found"}, HTTPStatus.NOT_FOUND)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/zip")
            self.send_header("Content-Length", str(path.stat().st_size))
            self.end_headers()
            with path.open("rb") as source:
                shutil.copyfileobj(source, self.wfile)
            return
        if self.path.startswith("/api/events/"):
            name = unquote(urlparse(self.path).path).removeprefix("/api/events/")
            if pathlib.PurePosixPath(name).name != name or not name.endswith(".zip"):
                self.send_json({"error": "invalid artifact"}, HTTPStatus.BAD_REQUEST)
                return
            events_path = self.collector.root / f"{pathlib.Path(name).stem}-events.jsonl"
            if not events_path.is_file():
                self.send_json({"error": "events jsonl not found"}, HTTPStatus.NOT_FOUND)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/x-ndjson; charset=utf-8")
            self.send_header(
                "Content-Disposition",
                f'attachment; filename="{pathlib.Path(name).stem}-events.jsonl"',
            )
            self.send_header("Content-Length", str(events_path.stat().st_size))
            self.end_headers()
            with events_path.open("rb") as source:
                shutil.copyfileobj(source, self.wfile)
            return
        if self.path.startswith("/api/readable/"):
            name = unquote(urlparse(self.path).path).removeprefix("/api/readable/")
            if pathlib.PurePosixPath(name).name != name or not name.endswith(".zip"):
                self.send_json({"error": "invalid artifact"}, HTTPStatus.BAD_REQUEST)
                return
            readable_path = self.collector.root / f"{pathlib.Path(name).stem}-readable.txt"
            if not readable_path.is_file():
                self.send_json({"error": "readable report not found"}, HTTPStatus.NOT_FOUND)
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header(
                "Content-Disposition",
                'attachment; filename="capture-readable.txt"',
            )
            self.send_header("Content-Length", str(readable_path.stat().st_size))
            self.end_headers()
            with readable_path.open("rb") as source:
                shutil.copyfileobj(source, self.wfile)
            return
        body = """<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Kiro 流量采集控制台</title>
  <style>
    :root {
      color-scheme: light;
      --bg: #f3f6fa;
      --panel: #ffffff;
      --line: #dbe3ec;
      --text: #17212b;
      --muted: #647384;
      --blue: #2563eb;
      --blue-soft: #eaf1ff;
      --green: #087f6c;
      --green-soft: #e6f6f2;
      --amber: #a15c00;
      --amber-soft: #fff4df;
      --red: #b42335;
      --red-soft: #fff0f2;
      --shadow: 0 12px 34px rgba(31, 48, 67, 0.08);
    }

    * {
      box-sizing: border-box;
    }

    body {
      margin: 0;
      background: var(--bg);
      color: var(--text);
      font: 14px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI",
        "PingFang SC", "Microsoft YaHei", sans-serif;
    }

    .page {
      width: min(1120px, calc(100% - 32px));
      margin: 0 auto;
      padding: 36px 0 52px;
    }

    .header {
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      gap: 24px;
      margin-bottom: 24px;
    }

    .kicker {
      color: var(--blue);
      font-size: 12px;
      font-weight: 700;
      letter-spacing: 0.08em;
    }

    h1 {
      margin: 6px 0 8px;
      font-size: 30px;
      letter-spacing: -0.03em;
      line-height: 1.2;
    }

    .subtitle {
      margin: 0;
      color: var(--muted);
    }

    .port {
      flex: 0 0 auto;
      padding: 8px 12px;
      border: 1px solid #cbdafa;
      border-radius: 999px;
      background: var(--blue-soft);
      color: #1e4da8;
      font-size: 12px;
      font-weight: 700;
    }

    .panel {
      border: 1px solid var(--line);
      border-radius: 14px;
      background: var(--panel);
      box-shadow: var(--shadow);
    }

    .token-panel {
      margin-bottom: 18px;
      padding: 20px;
    }

    .token-title {
      margin: 0 0 12px;
      font-size: 15px;
      font-weight: 750;
    }

    .token-row {
      display: flex;
      align-items: center;
      gap: 10px;
    }

    .token-input {
      min-width: 0;
      flex: 1;
      padding: 11px 13px;
      border: 1px solid #c7d1dd;
      border-radius: 9px;
      background: #fbfcfe;
      color: var(--text);
      outline: none;
    }

    .token-input:focus {
      border-color: var(--blue);
      box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.12);
    }

    .token-help {
      margin: 10px 0 0;
      color: var(--muted);
      font-size: 12px;
    }

    code {
      padding: 2px 5px;
      border-radius: 4px;
      background: #eef2f7;
      color: #334155;
      font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
      font-size: 12px;
    }

    .grid {
      display: grid;
      grid-template-columns: 1.2fr 0.8fr;
      gap: 18px;
    }

    .card {
      padding: 22px;
    }

    .card-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
      margin-bottom: 18px;
    }

    .card-title {
      font-size: 16px;
      font-weight: 750;
    }

    .card-meta {
      color: var(--muted);
      font-size: 12px;
    }

    .state-line {
      display: flex;
      align-items: center;
      gap: 10px;
      margin-bottom: 18px;
    }

    .badge {
      padding: 6px 10px;
      border-radius: 999px;
      font-size: 12px;
      font-weight: 750;
    }

    .badge.idle,
    .badge.completed {
      background: #edf1f5;
      color: #526171;
    }

    .badge.running {
      background: var(--green-soft);
      color: var(--green);
    }

    .badge.paused {
      background: var(--amber-soft);
      color: var(--amber);
    }

    .badge.stopping,
    .badge.failed {
      background: var(--red-soft);
      color: var(--red);
    }

    .state-description {
      color: var(--muted);
    }

    .terminal-panel {
      margin-bottom: 18px;
      overflow: hidden;
      border: 1px solid #263445;
      border-radius: 10px;
      background: #101820;
    }

    .terminal-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
      padding: 10px 14px;
      border-bottom: 1px solid #263445;
      color: #b9c8d8;
      font-size: 12px;
    }

    .terminal-header strong {
      color: #f3f7fb;
      font-weight: 650;
    }

    .terminal-live {
      color: #7ee2b8;
      font-variant-numeric: tabular-nums;
    }

    .terminal {
      height: 260px;
      overflow: auto;
      margin: 0;
      padding: 13px 14px 15px;
      color: #d3e3f2;
      font: 12px/1.65 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
      white-space: pre-wrap;
      word-break: break-word;
    }

    .terminal-line {
      min-height: 1.65em;
    }

    .terminal-line .event-time {
      color: #7890a8;
    }

    .terminal-line.warning .event-message {
      color: #f5cc7a;
    }

    .terminal-line.error .event-message {
      color: #ff9d9d;
    }

    .terminal-line.info .event-message {
      color: #d3e3f2;
    }

    .facts {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 10px;
      margin-bottom: 18px;
    }

    .fact {
      min-width: 0;
      padding: 12px;
      border: 1px solid #e1e7ef;
      border-radius: 10px;
      background: #f8fafc;
    }

    .fact-label {
      color: var(--muted);
      font-size: 11px;
    }

    .fact-value {
      margin-top: 4px;
      overflow-wrap: anywhere;
      font-size: 13px;
    }

    .actions {
      display: flex;
      flex-wrap: wrap;
      gap: 9px;
    }

    .button {
      padding: 10px 14px;
      border: 1px solid #c6d1df;
      border-radius: 9px;
      background: #ffffff;
      color: #263646;
      cursor: pointer;
      font: inherit;
      font-weight: 700;
      transition: border-color 0.15s, background 0.15s, transform 0.15s;
    }

    .button:hover:not(:disabled) {
      border-color: #8ea5c1;
      background: #f5f8fc;
      transform: translateY(-1px);
    }

    .button:disabled {
      cursor: not-allowed;
      opacity: 0.42;
    }

    .button.primary {
      border-color: var(--blue);
      background: var(--blue);
      color: #ffffff;
    }

    .button.primary:hover:not(:disabled) {
      background: #1d4ed8;
    }

    .button.warn {
      border-color: #e7bd74;
      background: var(--amber-soft);
      color: var(--amber);
    }

    .button.danger {
      border-color: #e7a8b2;
      background: var(--red-soft);
      color: var(--red);
    }

    .message {
      min-height: 22px;
      margin-top: 14px;
      color: var(--muted);
      font-size: 12px;
    }

    .message.error {
      color: var(--red);
    }

    .artifacts {
      display: flex;
      flex-direction: column;
      gap: 9px;
    }

    .artifact {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
      padding: 11px 12px;
      border: 1px solid #e1e7ef;
      border-radius: 10px;
      background: #f8fafc;
    }

    .artifact-name {
      overflow: hidden;
      color: #3a4b5d;
      font-size: 12px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .download {
      flex: 0 0 auto;
      color: var(--blue);
      font-size: 12px;
      font-weight: 750;
      text-decoration: none;
    }

    .artifact-downloads {
      display: flex;
      flex: 0 0 auto;
      align-items: center;
      gap: 10px;
    }

    .empty,
    .note {
      color: var(--muted);
      font-size: 12px;
    }

    .pager {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 10px;
      margin-top: 14px;
    }

    .note {
      margin-top: 18px;
    }

    .note strong {
      color: #3c4d5f;
    }

    @media (max-width: 760px) {
      .page {
        width: min(100% - 22px, 620px);
        padding-top: 22px;
      }

      .header {
        display: block;
      }

      .port {
        display: inline-block;
        margin-top: 14px;
      }

      .grid {
        grid-template-columns: 1fr;
      }

      .token-row {
        align-items: stretch;
        flex-direction: column;
      }

      .facts {
        grid-template-columns: 1fr;
      }
    }
  </style>
</head>
<body>
  <main class="page">
    <header class="header">
      <div>
        <div class="kicker">独立诊断服务</div>
        <h1>Kiro 流量采集控制台</h1>
        <p class="subtitle">只控制旁路采集器，不会重启或修改 59137 主服务。</p>
      </div>
      <div class="port">公网端口 19137</div>
    </header>

    <section class="panel token-panel">
      <h2 class="token-title">连接控制台</h2>
      <div class="token-row">
        <input
          id="token"
          class="token-input"
          type="password"
          autocomplete="off"
          placeholder="粘贴控制 token"
        >
        <button class="button primary" onclick="connect()">连接</button>
      </div>
      <p class="token-help">
        token 不在聊天窗口中显示。SSH 登录服务器后执行：
        <code>cat /var/lib/kiro-capture-sidecar/control-token</code>
        页面本身可以直接打开，控制和下载操作需要 token。
      </p>
    </section>

    <div class="grid">
      <section class="panel card">
        <div class="card-header">
          <div class="card-title">采集状态</div>
          <div id="updated" class="card-meta">尚未连接</div>
        </div>

        <div class="state-line">
          <span id="badge" class="badge idle">空闲</span>
          <span id="stateText" class="state-description">默认关闭采集</span>
        </div>

        <div class="terminal-panel" aria-live="polite">
          <div class="terminal-header">
            <strong>实时采集日志</strong>
            <span id="terminalLive" class="terminal-live">等待连接</span>
          </div>
          <div id="terminal" class="terminal" role="log" aria-live="polite">
            <div class="terminal-line info"><span class="event-time">--:--:--</span> <span class="event-message">等待采集器事件...</span></div>
          </div>
        </div>

        <div class="facts">
          <div class="fact">
            <div class="fact-label">目标容器</div>
            <div id="target" class="fact-value">kiro-rs-2ue-59137-app</div>
          </div>
          <div class="fact">
            <div class="fact-label">目标服务端口</div>
            <div id="service" class="fact-value">59137</div>
          </div>
          <div class="fact">
            <div class="fact-label">最近采集包</div>
            <div id="last" class="fact-value">暂无</div>
          </div>
          <div class="fact">
            <div class="fact-label">最近错误</div>
            <div id="error" class="fact-value">暂无</div>
          </div>
          <div class="fact">
            <div class="fact-label">实时采集数据</div>
            <div id="captureBytes" class="fact-value">0 B</div>
          </div>
          <div class="fact">
            <div class="fact-label">采集包 / TCP / UDP 包</div>
            <div id="capturePackets" class="fact-value">0 / 0</div>
          </div>
          <div class="fact">
            <div class="fact-label">TCP 会话 / UDP 流</div>
            <div id="captureSessions" class="fact-value">0（活跃 0）</div>
          </div>
          <div class="fact">
            <div class="fact-label">TLS 解密材料</div>
            <div id="tlsCapability" class="fact-value">未采集</div>
          </div>
          <div class="fact">
            <div class="fact-label">已采集时长</div>
            <div id="captureDuration" class="fact-value">0 秒</div>
          </div>
          <div class="fact">
            <div class="fact-label">首包 / 末包时间</div>
            <div id="packetTimes" class="fact-value">暂无 / 暂无</div>
          </div>
        </div>

        <div class="actions">
          <button id="start" class="button primary" onclick="action('/api/start', 'POST')">
            开始采集
          </button>
          <button id="pause" class="button warn" onclick="action('/api/pause', 'POST')" disabled>
            暂停采集
          </button>
          <button id="resume" class="button" onclick="action('/api/resume', 'POST')" disabled>
            继续采集
          </button>
          <button id="stop" class="button danger" onclick="action('/api/stop', 'POST')" disabled>
            停止并生成采集包
          </button>
          <button class="button" onclick="refresh()">刷新状态</button>
        </div>

        <div id="message" class="message">请输入 token 后连接控制台。</div>
      </section>

      <section class="panel card">
        <div class="card-header">
          <div class="card-title">已完成的采集包</div>
          <div class="card-meta">ZIP 文件</div>
        </div>

        <div id="artifacts" class="artifacts">
          <div class="empty">连接后显示采集包。</div>
        </div>

        <div id="artifactPager" class="pager" hidden>
          <button id="previousPage" class="button" onclick="changePage(-1)">上一页</button>
          <span id="pageLabel" class="card-meta"></span>
          <button id="nextPage" class="button" onclick="changePage(1)">下一页</button>
        </div>

        <div class="note">
          <strong>采集内容：</strong>
          <span id="collectionNote">上游 TCP/TLS 与 UDP/QUIC 流量、传输流分析、容器状态、只读 Admin 快照和容器日志。</span>
        </div>
      </section>
    </div>
  </main>

  <script>
    const tokenKey = "kiroCaptureToken";
    const element = (id) => document.getElementById(id);
    const getToken = () =>
      element("token").value.trim() || sessionStorage.getItem(tokenKey) || "";
    const stateLabels = {
      idle: "默认关闭采集",
      running: "正在采集",
      paused: "已暂停采集",
      stopping: "正在停止并生成采集包",
      completed: "上一次采集已完成",
      failed: "采集失败",
    };
    const stateNames = {
      idle: "空闲",
      running: "采集中",
      paused: "已暂停",
      stopping: "正在停止",
      completed: "已完成",
      failed: "失败",
    };

    function showMessage(text, isError = false) {
      const message = element("message");
      message.textContent = text;
      message.className = isError ? "message error" : "message";
    }

    function connect() {
      const value = element("token").value.trim();
      if (!value) {
        showMessage("请输入控制 token。", true);
        return;
      }
      sessionStorage.setItem(tokenKey, value);
      refresh();
    }

    async function callApi(path, method = "GET") {
      const response = await fetch(path, {
        method,
        headers: { Authorization: `Bearer ${getToken()}` },
      });
      let payload = {};
      try {
        payload = await response.json();
      } catch (_) {
        payload = {};
      }
      if (!response.ok) {
        if (response.status === 401) {
          throw new Error("token 无效或未填写。");
        }
        throw new Error(JSON.stringify(payload));
      }
      return payload;
    }

    function renderArtifacts(names) {
      const container = element("artifacts");
      container.replaceChildren();
      if (!names || !names.length) {
        container.innerHTML = '<div class="empty">暂无已完成采集包。</div>';
        return;
      }

      for (const item of names) {
        const name = typeof item === "string" ? item : item.name;
        const row = document.createElement("div");
        row.className = "artifact";

        const details = document.createElement("div");
        details.className = "artifact-name";
        const label = document.createElement("div");
        label.textContent = name;
        const metadata = document.createElement("div");
        metadata.className = "card-meta";
        const started = item.started_at || item.created_at;
        const finished = item.finished_at || item.created_at;
        const duration = item.duration_seconds == null
          ? "时长未知"
          : `时长 ${formatElapsed(item.duration_seconds)}`;
        metadata.textContent = `${formatBytes(item.size_bytes || 0)} · 开始 ${formatDate(started)} · 完成 ${formatDate(finished)} · ${duration}`;
        details.append(label, metadata);

        const link = document.createElement("a");
        link.className = "download";
        link.href = `/api/artifacts/${encodeURIComponent(name)}`;
        link.textContent = "下载";
        link.onclick = (event) => {
          event.preventDefault();
          downloadArtifact(name);
        };

        const downloads = document.createElement("div");
        downloads.className = "artifact-downloads";
        downloads.append(link);
        if (item.readable_available) {
          const readable = document.createElement("button");
          readable.className = "download";
          readable.type = "button";
          readable.textContent = "纯文本";
          readable.onclick = () => downloadReadable(name);
          downloads.append(readable);
        }
        if (item.events_available) {
          const events = document.createElement("button");
          events.className = "download";
          events.type = "button";
          events.textContent = "JSONL";
          events.onclick = () => downloadEvents(name);
          downloads.append(events);
        }
        row.append(details, downloads);
        container.append(row);
      }
    }

    function formatBytes(value) {
      if (!value) return "0 B";
      const units = ["B", "KiB", "MiB", "GiB"];
      let size = Number(value);
      let index = 0;
      while (size >= 1024 && index < units.length - 1) {
        size /= 1024;
        index += 1;
      }
      return `${size.toFixed(index ? 1 : 0)} ${units[index]}`;
    }

    function formatDate(value) {
      return value ? new Date(value).toLocaleString() : "时间未知";
    }

    function formatElapsed(value) {
      const seconds = Math.max(0, Number(value) || 0);
      if (seconds < 60) return `${seconds.toFixed(1)} 秒`;
      const minutes = Math.floor(seconds / 60);
      const remainder = Math.round(seconds % 60);
      return `${minutes} 分 ${remainder} 秒`;
    }

    function formatEventTime(value) {
      if (!value) return "--:--:--";
      const date = new Date(value);
      return Number.isNaN(date.getTime()) ? "--:--:--" : date.toLocaleTimeString();
    }

    function renderEvents(events) {
      const terminal = element("terminal");
      const wasNearBottom = terminal.scrollHeight - terminal.scrollTop - terminal.clientHeight < 28;
      terminal.replaceChildren();
      const rows = events && events.length ? events : [{ at: null, level: "info", message: "等待采集器事件..." }];
      for (const event of rows) {
        const line = document.createElement("div");
        line.className = `terminal-line ${event.level || "info"}`;
        const time = document.createElement("span");
        time.className = "event-time";
        time.textContent = formatEventTime(event.at);
        const message = document.createElement("span");
        message.className = "event-message";
        message.textContent = event.message || "";
        line.append(time, document.createTextNode(" "), message);
        terminal.append(line);
      }
      if (wasNearBottom || rows.length <= 1) terminal.scrollTop = terminal.scrollHeight;
      element("terminalLive").textContent = events && events.length
        ? `最近 ${formatEventTime(events[events.length - 1].at)}`
        : "等待事件";
    }

    let artifactPage = 1;
    const artifactPageSize = 6;

    function changePage(delta) {
      artifactPage = Math.max(1, artifactPage + delta);
      refresh();
    }

    function renderProgress(status) {
      const progress = status.progress || {};
      element("captureBytes").textContent = `${formatBytes(progress.pcap_bytes || 0)} · ${formatBytes(progress.capture_rate_bytes_per_second || 0)}/秒`;
      element("capturePackets").textContent = `${progress.pcap_records || 0} / ${progress.tcp_packets || 0} / ${progress.udp_packets || 0}`;
      element("captureSessions").textContent = `${progress.session_count || 0}（活跃 ${progress.active_session_count || 0}） / UDP ${progress.udp_flow_count || 0}`;
      element("captureDuration").textContent = formatElapsed(progress.elapsed_seconds);
      element("packetTimes").textContent = `${formatDate(progress.first_packet_at)} / ${formatDate(progress.last_packet_at)}`;
      element("tlsCapability").textContent = progress.tls_keylog_available
        ? `已采集（${formatBytes(progress.tls_keylog_bytes || 0)}）`
        : "未采集，TLS 仍为密文";
      const capabilities = status.capture_capabilities || {};
      element("collectionNote").textContent = capabilities.tls_decryption
        ? "采集结束后会尝试生成 https-readable.txt，查看解密后的请求正文和上游事件；原始 TLS 包与会话密钥也会保留。"
        : capabilities.tshark_available
          ? "已安装 tshark；若采集时取得覆盖本次连接的 TLS keylog，会自动生成 https-readable.txt。"
          : "完整 HTTPS 正文还原需要 TLS keylog 和 tshark；不满足时会在包内说明原因，原始 TLS 包仍会保留。";
    }

    function renderStatus(status) {
      const state = status.state || "idle";
      const badge = element("badge");
      badge.textContent = stateNames[state] || state;
      badge.className = `badge ${state}`;

      element("stateText").textContent = stateLabels[state] || state;
      element("target").textContent = status.target_container || "-";
      element("service").textContent = status.target_port || "-";
      element("last").textContent = status.last_artifact || "暂无";
      element("error").textContent = status.last_error || status.last_message || "暂无";
      element("updated").textContent =
        `更新于 ${new Date().toLocaleTimeString()}`;

      element("start").disabled =
        ["running", "paused", "stopping"].includes(state);
      element("pause").disabled = state !== "running";
      element("resume").disabled = state !== "paused";
      element("stop").disabled =
        !["running", "paused", "stopping"].includes(state);

      renderProgress(status);
      renderEvents(status.events || []);
      renderArtifacts(status.artifacts);
      const totalPages = status.artifacts_total_pages || 1;
      element("artifactPager").hidden = totalPages <= 1;
      element("pageLabel").textContent = `第 ${status.artifacts_page || artifactPage} / ${totalPages} 页，共 ${status.artifacts_total || 0} 个`;
      element("previousPage").disabled = (status.artifacts_page || artifactPage) <= 1;
      element("nextPage").disabled = (status.artifacts_page || artifactPage) >= totalPages;
    }

    async function refresh() {
      if (!getToken()) {
        showMessage("请输入控制 token 后连接控制台。");
        return;
      }

      try {
        const status = await callApi(`/api/status?page=${artifactPage}&page_size=${artifactPageSize}`);
        renderStatus(status);
        showMessage("状态已更新。");
      } catch (error) {
        showMessage(error.message, true);
      }
    }

    async function action(path, method) {
      if (!getToken()) {
        showMessage("请输入控制 token 后连接控制台。", true);
        return;
      }

      try {
        showMessage("正在执行操作...");
        const result = await callApi(path, method);
        const accepted =
          result.started !== false &&
          result.paused !== false &&
          result.resumed !== false &&
          result.stop_requested !== false;
        showMessage(
          accepted ? "操作已接受。" : "当前状态不允许执行该操作。",
          !accepted,
        );
        await refresh();
      } catch (error) {
        showMessage(error.message, true);
      }
    }

    async function downloadArtifact(name) {
      try {
        const response = await fetch(
          `/api/artifacts/${encodeURIComponent(name)}`,
          { headers: { Authorization: `Bearer ${getToken()}` } },
        );
        if (!response.ok) {
          throw new Error(response.status === 401 ? "token 无效或未填写。" : "下载失败。");
        }
        const url = URL.createObjectURL(await response.blob());
        const link = document.createElement("a");
        link.href = url;
        link.download = name;
        link.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      } catch (error) {
        showMessage(error.message, true);
      }
    }

    async function downloadEvents(name) {
      try {
        const response = await fetch(
          `/api/events/${encodeURIComponent(name)}`,
          { headers: { Authorization: `Bearer ${getToken()}` } },
        );
        if (!response.ok) {
          throw new Error(response.status === 401 ? "token 无效或未填写。" : "JSONL 下载失败。");
        }
        const url = URL.createObjectURL(await response.blob());
        const link = document.createElement("a");
        link.href = url;
        link.download = `${name.replace(/\\.zip$/i, "")}-events.jsonl`;
        link.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      } catch (error) {
        showMessage(error.message, true);
      }
    }

    async function downloadReadable(name) {
      try {
        const response = await fetch(
          `/api/readable/${encodeURIComponent(name)}`,
          { headers: { Authorization: `Bearer ${getToken()}` } },
        );
        if (!response.ok) {
          throw new Error(response.status === 401 ? "token 无效或未填写。" : "纯文本报告下载失败。");
        }
        const url = URL.createObjectURL(await response.blob());
        const link = document.createElement("a");
        link.href = url;
        link.download = `${name.replace(/\\.zip$/i, "")}-readable.txt`;
        link.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      } catch (error) {
        showMessage(error.message, true);
      }
    }

    element("token").value = sessionStorage.getItem(tokenKey) || "";
    if (getToken()) {
      refresh();
    }
    setInterval(() => {
      if (getToken()) {
        refresh();
      }
    }, 5000);
  </script>
</body>
</html>""".encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        if not self.auth_ok():
            self.send_json({"error": "unauthorized"}, HTTPStatus.UNAUTHORIZED)
            return
        if self.path == "/api/start":
            self.send_json({"started": self.collector.start()})
            return
        if self.path == "/api/pause":
            self.send_json({"paused": self.collector.pause()})
            return
        if self.path == "/api/resume":
            self.send_json({"resumed": self.collector.resume()})
            return
        if self.path == "/api/stop":
            self.send_json({"stop_requested": self.collector.stop()})
            return
        self.send_json({"error": "not found"}, HTTPStatus.NOT_FOUND)

    def log_message(self, format: str, *args: object) -> None:
        return


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=19137)
    parser.add_argument("--container", default=DEFAULT_CONTAINER)
    parser.add_argument("--target-port", type=int, default=DEFAULT_TARGET_PORT)
    parser.add_argument(
        "--upstream-port",
        type=int,
        action="append",
        default=None,
        help="Upstream TCP/UDP port(s) to capture inside the target namespace (default: 443).",
    )
    parser.add_argument("--probe-host", default="127.0.0.1")
    parser.add_argument("--interface", default="any")
    parser.add_argument("--source-ip", default="0.0.0.0")
    parser.add_argument(
        "--network-namespace",
        choices=("host", "container"),
        default="container",
        help="Capture on the host or the target container network namespace.",
    )
    parser.add_argument("--destination-ip", action="append", default=[])
    parser.add_argument("--duration", type=float, default=DEFAULT_MAX_DURATION)
    parser.add_argument("--max-bytes", type=int, default=DEFAULT_MAX_BYTES)
    parser.add_argument(
        "--max-readable-bytes",
        type=int,
        default=DEFAULT_MAX_READABLE_BYTES,
        help="Maximum bytes for https-readable.txt before truncation; raise this for full plaintext conversations.",
    )
    parser.add_argument("--output", default="./kiro-capture-artifacts")
    parser.add_argument(
        "--tls-keylog-file",
        help="Optional readable path to an NSS-format TLS key log shared with 59137; copied into completed artifacts.",
    )
    parser.add_argument(
        "--tshark",
        help="Optional tshark executable path; defaults to tshark from PATH.",
    )
    parser.add_argument("--token-file")
    parser.add_argument("--admin-url", help="Optional local kiro.rs base URL for read-only admin snapshots")
    parser.add_argument("--admin-token-file", help="Admin API key file; required with --admin-url")
    parser.add_argument(
        "--runtime-snapshot-interval",
        type=float,
        default=DEFAULT_RUNTIME_SNAPSHOT_INTERVAL,
        help="Seconds between lightweight runtime timeline samples during capture; 0 disables periodic samples.",
    )
    parser.add_argument(
        "--usage-record-limit",
        type=int,
        default=DEFAULT_USAGE_RECORD_LIMIT,
        help="Maximum local usage records fetched per Admin snapshot.",
    )
    parser.add_argument(
        "--diagnostic-log-path",
        default=DEFAULT_DIAGNOSTIC_LOG_PATH,
        help="Target-container path of the local tool-format-debug JSONL directory.",
    )
    parser.add_argument(
        "--diagnostic-log-max-files",
        type=int,
        default=DEFAULT_DIAGNOSTIC_LOG_MAX_FILES,
        help="Maximum diagnostic JSONL files copied into one artifact.",
    )
    parser.add_argument(
        "--diagnostic-log-max-bytes",
        type=int,
        default=DEFAULT_DIAGNOSTIC_LOG_MAX_BYTES,
        help="Maximum total diagnostic JSONL bytes copied into one artifact.",
    )
    parser.add_argument(
        "--include-external-pools",
        action="store_true",
        help="Include read-only external-pool Admin status. Default is local Kiro credentials only.",
    )
    parser.add_argument(
        "--app-capture",
        choices=("auto", "off"),
        default="auto",
        help="kiro.rs in-process plaintext capture via Admin API; auto enables it when --admin-url is set.",
    )
    parser.add_argument(
        "--app-capture-include-secrets",
        action="store_true",
        help="Keep Authorization/x-api-key/Cookie header values in kiro.rs capture events (redacted by default).",
    )
    parser.add_argument(
        "--no-pcap",
        action="store_true",
        help="Skip packet capture; only collect kiro.rs in-process capture, logs and Admin snapshots.",
    )
    parser.add_argument("--python", default=os.environ.get("PYTHON", "python3"))
    args = parser.parse_args()
    if (
        args.duration <= 0
        or args.max_bytes <= 0
        or args.max_readable_bytes <= 0
        or not (1 <= args.target_port <= 65535)
        or args.usage_record_limit <= 0
        or args.diagnostic_log_max_files <= 0
        or args.diagnostic_log_max_bytes <= 0
    ):
        parser.error(
            "duration, max-bytes, max-readable-bytes, target-port, usage-record-limit, "
            "diagnostic-log-max-files, and diagnostic-log-max-bytes must be positive"
        )
    args.upstream_port = args.upstream_port or [443]
    if any(not (1 <= port <= 65535) for port in args.upstream_port):
        parser.error("--upstream-port must be between 1 and 65535")
    if args.admin_url and not args.admin_token_file:
        parser.error("--admin-token-file is required with --admin-url")
    if args.no_pcap and not args.admin_url:
        parser.error("--no-pcap requires --admin-url/--admin-token-file so kiro.rs capture can run")
    return args


def main() -> int:
    args = parse_args()
    token = pathlib.Path(args.token_file).read_text(encoding="utf-8").strip() if args.token_file else None
    collector = Collector(args)
    Handler.collector = collector
    Handler.token = token
    server = ThreadingHTTPServer((args.bind, args.port), Handler)

    def request_shutdown(_signum: int, _frame: object) -> None:
        # BaseServer.shutdown waits for serve_forever to observe the flag, so
        # invoke it off the signal-handler thread. Calling it inline from the
        # main thread can deadlock systemd shutdown.
        threading.Thread(target=server.shutdown, name="sidecar-shutdown", daemon=True).start()

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, request_shutdown)
    print(f"kiro capture sidecar listening on http://{args.bind}:{args.port}", flush=True)
    try:
        server.serve_forever()
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
