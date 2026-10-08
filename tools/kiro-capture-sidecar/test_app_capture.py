import io
import json
import pathlib
import sys
import tempfile
import threading
import unittest
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import app_capture
import server


def sample_events() -> list[dict]:
    request_body = {
        "model": "claude-sonnet-5",
        "max_tokens": 64,
        "stream": True,
        "system": [{"type": "text", "text": "SYSTEM_PROMPT_MARKER"}],
        "tools": [{"name": "read_file", "description": "read a file", "input_schema": {"type": "object"}}],
        "messages": [
            {"role": "user", "content": "USER_MARKER"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "read_file", "input": {"p": "a"}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "TOOL_RESULT_MARKER"}]},
        ],
    }
    sse = (
        'event: message_start\ndata: {"type":"message_start","message":{}}\n\n'
        'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n'
        'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}\n\n'
        'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"world"}}\n\n'
        'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}\n\n'
    )
    rid = "req_01test"
    return [
        {"seq": 1, "ts": "t1", "type": "client_request", "requestId": rid,
         "data": {"route": "local", "endpoint": "/v1/messages", "requestedModel": "claude-sonnet-5",
                  "headers": {"x-api-key": "[redacted] (len=6)"},
                  "rawBody": {"encoding": "json", "bytes": 1, "json": request_body}}},
        {"seq": 2, "ts": "t2", "type": "upstream_request", "requestId": rid,
         "data": {"phase": "api", "attempt": 1, "credentialId": 7, "method": "POST",
                  "url": "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
                  "headers": {"authorization": "Bearer [redacted] (len=40)"},
                  "body": {"encoding": "json", "bytes": 1, "json": {"conversationState": {"currentMessage": {"userInputMessage": {"content": "KIRO_BODY_MARKER"}}}}}}},
        {"seq": 3, "ts": "t3", "type": "upstream_response", "requestId": rid,
         "data": {"status": 200, "httpVersion": "HTTP/2.0", "headerWaitMs": 12, "headers": {}}},
        {"seq": 4, "ts": "t4", "type": "upstream_frame", "requestId": rid,
         "data": {"attempt": 1, "messageType": "event", "eventType": "reasoningContentEvent",
                  "payload": {"encoding": "json", "bytes": 1, "json": {"text": "THINKING_MARKER"}}}},
        {"seq": 5, "ts": "t5", "type": "upstream_frame", "requestId": rid,
         "data": {"attempt": 1, "messageType": "event", "eventType": "assistantResponseEvent",
                  "payload": {"encoding": "json", "bytes": 1, "json": {"content": "Hello world"}}}},
        {"seq": 6, "ts": "t6", "type": "client_sse", "requestId": rid, "data": {"index": 1, "text": sse}},
        {"seq": 7, "ts": "t7", "type": "client_stream_end", "requestId": rid,
         "data": {"outcome": "completed", "chunks": 1, "bytes": len(sse), "elapsedMs": 5}},
    ]


def sample_archive() -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as zf:
        zf.writestr("manifest.json", json.dumps({"sessionId": "capture-x"}))
        zf.writestr("events.jsonl", "\n".join(json.dumps(e) for e in sample_events()) + "\n")
    return buffer.getvalue()


class FakeAdmin(BaseHTTPRequestHandler):
    calls: list[str] = []

    def _send(self, status: int, body: bytes, content_type: str = "application/json") -> None:
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _authorized(self) -> bool:
        if self.headers.get("x-api-key") != "admin-secret":
            self._send(401, b"{}")
            return False
        return True

    def do_POST(self):
        if not self._authorized():
            return
        length = int(self.headers.get("content-length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        FakeAdmin.calls.append(f"POST {self.path} {json.dumps(body, sort_keys=True)}")
        if self.path.endswith("/start"):
            self._send(200, json.dumps({"state": "running", "sessionId": "capture-x"}).encode())
        elif self.path.endswith("/stop"):
            self._send(200, json.dumps({"state": "stopped", "sessionId": "capture-x", "droppedEvents": 0}).encode())
        else:
            self._send(404, b"{}")

    def do_GET(self):
        if not self._authorized():
            return
        FakeAdmin.calls.append(f"GET {self.path}")
        if self.path.endswith("/capture-x/download"):
            self._send(200, sample_archive(), "application/zip")
        else:
            self._send(404, b"{}")

    def log_message(self, *args):
        pass


class RenderTests(unittest.TestCase):
    def test_readable_report_contains_every_layer(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "events.jsonl"
            path.write_text("\n".join(json.dumps(e) for e in sample_events()), encoding="utf-8")
            text = app_capture.render_readable(path)
        for marker in (
            "SYSTEM_PROMPT_MARKER",
            "read_file",
            "USER_MARKER",
            "TOOL_RESULT_MARKER",
            "KIRO_BODY_MARKER",
            "THINKING_MARKER",
            "reasoningContentEvent",
            "上游回答文本",
            "Hello world",
            "stop_reason=end_turn",
            "outcome=completed",
        ):
            self.assertIn(marker, text)

    def test_readable_report_respects_byte_limit(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "events.jsonl"
            path.write_text("\n".join(json.dumps(e) for e in sample_events()), encoding="utf-8")
            text = app_capture.render_readable(path, max_bytes=200)
        self.assertIn("已截断", text)

    def test_readable_report_renders_ingress_count_tokens_external_and_summary_events(self):
        rid = "req_rejected_capture"
        events = [
            {
                "seq": 1,
                "ts": "t1",
                "type": "http_ingress",
                "requestId": None,
                "data": {
                    "ingressId": "ingress-1",
                    "method": "POST",
                    "path": "/v1/messages/count_tokens",
                    "query": "x=1",
                    "headers": {"x-api-key": "[redacted] (len=3)"},
                    "body": {"encoding": "json", "json": {"model": "m"}},
                },
            },
            {
                "seq": 2,
                "ts": "t2",
                "type": "http_egress",
                "requestId": rid,
                "data": {
                    "ingressId": "ingress-1",
                    "requestId": rid,
                    "status": 401,
                    "headers": {},
                },
            },
            {
                "seq": 3,
                "ts": "t3",
                "type": "count_tokens_result",
                "requestId": rid,
                "data": {"model": "m", "inputTokens": 12, "calculation": "local"},
            },
            {
                "seq": 4,
                "ts": "t4",
                "type": "external_upstream_chunk",
                "requestId": rid,
                "data": {"index": 1, "text": "data: one\n\n"},
            },
            {
                "seq": 5,
                "ts": "t5",
                "type": "mcp_response_body",
                "requestId": rid,
                "data": {"status": 200, "body": {"encoding": "text", "text": "MCP_MARKER"}},
            },
            {
                "seq": 6,
                "ts": "t6",
                "type": "upstream_stream_end",
                "requestId": rid,
                "data": {"reason": "completed", "attempt": 1, "framesDecoded": 2},
            },
            {
                "seq": 7,
                "ts": "t7",
                "type": "request_summary",
                "requestId": rid,
                "data": {"status": "success", "outputTokens": 2},
            },
        ]
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "events.jsonl"
            path.write_text("\n".join(json.dumps(e) for e in events), encoding="utf-8")
            text = app_capture.render_readable(path)
        for marker in (
            "HTTP 入口",
            "status=401",
            "count_tokens",
            "inputTokens=12",
            "外部号池上游原始 SSE",
            "data: one",
            "MCP_MARKER",
            "上游流结束",
            "请求汇总",
        ):
            self.assertIn(marker, text)

    def test_readable_report_keeps_headers_for_truncated_ingress(self):
        events = [
            {
                "seq": 1,
                "ts": "t1",
                "type": "http_ingress",
                "requestId": None,
                "data": {
                    "ingressId": "ingress-rejected",
                    "method": "POST",
                    "path": "/v1/messages",
                    "headers": {
                        "x-api-key": "[redacted] (len=16)",
                        "x-e2e-marker": "TRUNCATED_HEADER_MARKER",
                    },
                    "bodyTruncated": True,
                    "bodyBytes": 0,
                    "bodyReadError": "request body was not read",
                },
            },
            {
                "seq": 2,
                "ts": "t2",
                "type": "http_egress",
                "requestId": "req_rejected",
                "data": {
                    "ingressId": "ingress-rejected",
                    "requestId": "req_rejected",
                    "status": 401,
                    "headers": {},
                },
            },
        ]
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "events.jsonl"
            path.write_text("\n".join(json.dumps(e) for e in events), encoding="utf-8")
            text = app_capture.render_readable(path)

        self.assertIn("请求头", text)
        self.assertIn("TRUNCATED_HEADER_MARKER", text)
        self.assertIn("请求体已截断，大小=0", text)


class NoPcapCollectorTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        FakeAdmin.calls = []
        self.admin = ThreadingHTTPServer(("127.0.0.1", 0), FakeAdmin)
        threading.Thread(target=self.admin.serve_forever, daemon=True).start()
        token_file = pathlib.Path(self.tmp.name) / "admin-key"
        token_file.write_text("admin-secret\n", encoding="utf-8")
        self.args = SimpleNamespace(
            output=str(pathlib.Path(self.tmp.name) / "out"),
            container="test-container",
            target_port=59137,
            probe_host="127.0.0.1",
            interface="lo0",
            source_ip="127.0.0.1",
            destination_ip=[],
            duration=0.3,
            max_bytes=1000000,
            max_readable_bytes=1000000,
            python="python3",
            tls_keylog_file=None,
            admin_url=f"http://127.0.0.1:{self.admin.server_address[1]}",
            admin_token_file=str(token_file),
            include_external_pools=False,
            runtime_snapshot_interval=0,
            usage_record_limit=200,
            diagnostic_log_path="/app/logs/tool-format-debug",
            diagnostic_log_max_files=64,
            diagnostic_log_max_bytes=64 * 1024 * 1024,
            app_capture="auto",
            app_capture_include_secrets=False,
            no_pcap=True,
        )

    def tearDown(self):
        self.admin.shutdown()
        self.admin.server_close()
        self.tmp.cleanup()

    def test_no_pcap_run_collects_kiro_capture_into_artifact(self):
        collector = server.Collector(self.args)
        with patch.object(collector, "snapshot_state"), patch.object(collector, "logs"), patch.object(
            collector, "collect_diagnostic_logs", return_value={"files": []}
        ), patch.object(collector, "append_runtime_timeline"), patch.object(
            collector, "capture", side_effect=AssertionError("pcap must not run")
        ):
            self.assertTrue(collector.start())
            collector.thread.join(timeout=10)
        self.assertEqual(collector.status["state"], "completed", collector.status)
        artifact = collector.status["last_artifact"]
        self.assertTrue(artifact, collector.status)
        with zipfile.ZipFile(collector.root / artifact) as zf:
            names = set(zf.namelist())
            self.assertIn("llm-events.jsonl", names)
            self.assertIn("llm-readable.txt", names)
            readable = zf.read("llm-readable.txt").decode("utf-8")
            manifest = json.loads(zf.read("manifest.json"))
        self.assertIn("SYSTEM_PROMPT_MARKER", readable)
        self.assertEqual(manifest["app_capture"]["event_count"], len(sample_events()))
        self.assertTrue(manifest["capabilities"]["app_capture"])
        combined = (collector.root / artifact.replace(".zip", "-readable.txt")).read_text(encoding="utf-8")
        standalone = (collector.root / artifact.replace(".zip", "-events.jsonl")).read_text(encoding="utf-8")
        self.assertEqual(len(standalone.splitlines()), len(sample_events()))
        self.assertTrue(collector.artifact_page()["items"][0]["events_available"])
        self.assertIn("USER_MARKER", combined)
        start_call = next(call for call in FakeAdmin.calls if call.startswith("POST") and "/start" in call)
        self.assertIn('"includeSecrets": false', start_call)
        self.assertTrue(any(call.endswith("/capture-x/download") for call in FakeAdmin.calls))

    def test_stop_request_ends_no_pcap_window_early(self):
        self.args.duration = 30
        collector = server.Collector(self.args)
        with patch.object(collector, "snapshot_state"), patch.object(collector, "logs"), patch.object(
            collector, "collect_diagnostic_logs", return_value={"files": []}
        ), patch.object(collector, "append_runtime_timeline"):
            collector.start()
            threading.Timer(0.3, collector.stop).start()
            collector.thread.join(timeout=10)
        self.assertFalse(collector.thread.is_alive())
        self.assertEqual(collector.status["state"], "completed")


if __name__ == "__main__":
    unittest.main()
