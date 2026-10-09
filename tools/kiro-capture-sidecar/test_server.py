import json
import ipaddress
import pathlib
import struct
import sys
import tempfile
import threading
import unittest
import urllib.request
from http.server import ThreadingHTTPServer
from types import SimpleNamespace
from urllib.parse import urlparse
from unittest.mock import Mock, patch

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import server


def ethernet(payload: bytes, ether_type: int) -> bytes:
    return b"\x00" * 12 + struct.pack("!H", ether_type) + payload


def ipv4_udp(source: str, destination: str, source_port: int, destination_port: int, payload: bytes) -> bytes:
    udp = struct.pack("!HHHH", source_port, destination_port, 8 + len(payload), 0) + payload
    packet = struct.pack(
        "!BBHHHBBH4s4s",
        0x45,
        0,
        20 + len(udp),
        1,
        0,
        64,
        17,
        0,
        ipaddress.IPv4Address(source).packed,
        ipaddress.IPv4Address(destination).packed,
    ) + udp
    return ethernet(packet, 0x0800)


def write_pcap(path: pathlib.Path, packets: list[bytes]) -> None:
    with path.open("wb") as output:
        output.write(struct.pack("<IHHIIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, 1))
        for index, packet in enumerate(packets, 1):
            output.write(
                struct.pack("<IIII", 1_700_000_000 + index, 0, len(packet), len(packet))
            )
            output.write(packet)


class SidecarTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        args = SimpleNamespace(
            output=self.tmp.name,
            container="test-container",
            target_port=59137,
            probe_host="127.0.0.1",
            interface="lo0",
            source_ip="127.0.0.1",
            destination_ip=[],
            duration=0.1,
            max_bytes=1000000,
            max_readable_bytes=1000000,
            python="python3",
            tls_keylog_file=None,
            admin_url=None,
            admin_token_file=None,
            include_external_pools=False,
            runtime_snapshot_interval=15,
            usage_record_limit=200,
            diagnostic_log_path="/app/logs/tool-format-debug",
            diagnostic_log_max_files=64,
            diagnostic_log_max_bytes=64 * 1024 * 1024,
        )
        self.collector = server.Collector(args)
        server.Handler.collector = self.collector
        server.Handler.token = "secret"
        self.http = ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        self.thread = threading.Thread(target=self.http.serve_forever, daemon=True)
        self.thread.start()
        self.base = f"http://127.0.0.1:{self.http.server_port}"

    def tearDown(self):
        self.http.shutdown()
        self.http.server_close()
        self.tmp.cleanup()

    def request(self, path, method="GET", token="secret"):
        req = urllib.request.Request(self.base + path, method=method)
        if token:
            req.add_header("Authorization", f"Bearer {token}")
        return urllib.request.urlopen(req)

    def test_requires_token(self):
        with self.assertRaises(urllib.error.HTTPError) as ctx:
            self.request("/api/status", token=None)
        self.assertEqual(ctx.exception.code, 401)

    def test_control_page_is_reachable_before_token_entry(self):
        response = self.request("/", token=None)
        page = response.read().decode("utf-8")
        self.assertIn("控制 token", page)
        self.assertIn("连接", page)

    def test_status_and_artifact_download(self):
        response = self.request("/api/status")
        status = json.load(response)
        self.assertEqual(status["target_port"], 59137)
        self.assertEqual(status["state"], "idle")
        self.assertIsNone(status["paused_at"])
        self.assertIn("progress", status)
        self.assertIn("events", status)
        self.assertIn("capture_capabilities", status)
        self.assertFalse(status["capture_capabilities"]["tls_decryption"])
        self.assertTrue(
            any(
                "https-readable.txt" in item["file"]
                for item in status["data_collection"]
            )
        )
        artifact = pathlib.Path(self.tmp.name) / "run.zip"
        artifact.write_bytes(b"zip")
        response = self.request("/api/artifacts/run.zip")
        self.assertEqual(response.read(), b"zip")

    def test_idle_lifecycle_controls_are_noops(self):
        for path, key in (("/api/pause", "paused"), ("/api/resume", "resumed"), ("/api/stop", "stop_requested")):
            response = self.request(path, method="POST")
            self.assertFalse(json.load(response)[key])

    def test_control_page_exposes_lifecycle_controls(self):
        response = self.request("/")
        page = response.read().decode("utf-8")
        self.assertIn("开始采集", page)
        self.assertIn("暂停采集", page)
        self.assertIn("继续采集", page)
        self.assertIn("停止并生成采集包", page)
        self.assertIn("实时采集数据", page)
        self.assertIn("实时采集日志", page)
        self.assertIn("terminal", page)
        self.assertNotIn("progressPercent", page)
        self.assertNotIn("progressFill", page)
        self.assertIn("captureDuration", page)
        self.assertIn("TLS 解密材料", page)
        self.assertIn("纯文本", page)
        self.assertIn("上一页", page)
        self.assertIn("TCP 会话 / UDP 流", page)

    def test_progress_counts_udp_flows_without_tcp_session(self):
        path = pathlib.Path(self.tmp.name) / "traffic.pcap"
        write_pcap(
            path,
            [
                ipv4_udp("10.0.0.2", "203.0.113.4", 51000, 443, b"client"),
                ipv4_udp("203.0.113.4", "10.0.0.2", 443, 51000, b"server"),
            ],
        )
        progress = server.PcapProgress(path, "2026-09-25T00:00:00Z", 60).finish()
        self.assertEqual(progress["pcap_records"], 2)
        self.assertEqual(progress["tcp_packets"], 0)
        self.assertEqual(progress["session_count"], 0)
        self.assertEqual(progress["udp_packets"], 2)
        self.assertEqual(progress["udp_flow_count"], 1)
        self.assertEqual(progress["udp_payload_bytes"], len(b"clientserver"))
        self.assertTrue(progress["data_seen"])

    def test_pause_resume_signal_only_capture_process_group(self):
        self.collector.thread = Mock()
        self.collector.thread.is_alive.return_value = True
        self.collector.capture_process = Mock(pid=12345)
        self.collector.status["state"] = "running"
        with patch("server.os.killpg") as killpg:
            self.assertTrue(self.collector.pause())
            self.assertEqual(self.collector.status["state"], "paused")
            self.assertTrue(self.collector.resume())
            self.assertEqual(self.collector.status["state"], "running")
            self.assertEqual(killpg.call_count, 2)
            self.assertEqual(killpg.call_args_list[0].args[0], 12345)
            self.assertEqual(killpg.call_args_list[1].args[0], 12345)

    def test_resume_during_capture_start_clears_pending_pause(self):
        self.collector.thread = Mock()
        self.collector.thread.is_alive.return_value = True
        self.collector.status["state"] = "paused"
        self.collector.status["paused_at"] = "2026-09-25T00:00:00Z"
        self.collector.pause_requested = True
        self.assertTrue(self.collector.resume())
        self.assertFalse(self.collector.pause_requested)
        self.assertEqual(self.collector.status["state"], "running")

    def test_artifact_path_traversal_rejected(self):
        with self.assertRaises(urllib.error.HTTPError) as ctx:
            self.request("/api/artifacts/../secret.zip")
        self.assertEqual(ctx.exception.code, 400)

    def test_artifact_page_returns_metadata(self):
        for name in ("old.zip", "new.zip"):
            path = pathlib.Path(self.tmp.name) / name
            path.write_bytes(name.encode())
        response = self.request("/api/status?page=1&page_size=1")
        status = json.load(response)
        self.assertEqual(len(status["artifacts"]), 1)
        self.assertEqual(status["artifacts_total"], 2)
        self.assertIn("created_at", status["artifacts"][0])
        self.assertIn("size_bytes", status["artifacts"][0])

    def test_artifact_metadata_reads_manifest_times_and_progress(self):
        artifact = pathlib.Path(self.tmp.name) / "capture.zip"
        manifest = {
            "started_at": "2026-09-25T00:01:02Z",
            "finished_at": "2026-09-25T00:01:12Z",
            "capture_progress": {
                "elapsed_seconds": 10.2,
                "pcap_records": 21,
                "session_count": 3,
            },
        }
        import zipfile

        with zipfile.ZipFile(artifact, "w") as archive:
            archive.writestr("manifest.json", json.dumps(manifest))
        response = self.request("/api/artifacts?page=1&page_size=6")
        page = json.load(response)
        item = page["items"][0]
        self.assertEqual(item["started_at"], "2026-09-25T00:01:02Z")
        self.assertEqual(item["finished_at"], "2026-09-25T00:01:12Z")
        self.assertEqual(item["duration_seconds"], 10.2)
        self.assertEqual(item["pcap_records"], 21)
        self.assertEqual(item["session_count"], 3)

    def test_plain_text_report_is_available_from_artifact_list_and_endpoint(self):
        archive_path = pathlib.Path(self.tmp.name) / "capture.zip"
        archive_path.write_bytes(b"zip")
        report_path = pathlib.Path(self.tmp.name) / "capture-readable.txt"
        report_path.write_text("上游请求正文与运行状态", encoding="utf-8")
        page = self.collector.artifact_page()
        self.assertTrue(page["items"][0]["readable_available"])
        response = self.request("/api/readable/capture.zip")
        self.assertEqual(response.headers["Content-Type"], "text/plain; charset=utf-8")
        self.assertEqual(response.read().decode("utf-8"), "上游请求正文与运行状态")

    def test_events_jsonl_is_available_from_artifact_list_and_endpoint(self):
        archive_path = pathlib.Path(self.tmp.name) / "capture.zip"
        archive_path.write_bytes(b"zip")
        events_path = pathlib.Path(self.tmp.name) / "capture-events.jsonl"
        events_path.write_text('{"type":"client_request"}\n', encoding="utf-8")
        page = self.collector.artifact_page()
        self.assertTrue(page["items"][0]["events_available"])
        response = self.request("/api/events/capture.zip")
        self.assertEqual(response.headers["Content-Type"], "application/x-ndjson; charset=utf-8")
        self.assertEqual(response.read().decode("utf-8"), '{"type":"client_request"}\n')

    def test_events_jsonl_path_traversal_rejected(self):
        with self.assertRaises(urllib.error.HTTPError) as error:
            self.request("/api/events/../secret.zip")
        self.assertEqual(error.exception.code, 400)

    def test_readable_report_path_traversal_rejected(self):
        with self.assertRaises(urllib.error.HTTPError) as ctx:
            self.request("/api/readable/../secret.zip")
        self.assertEqual(ctx.exception.code, 400)

    def test_no_data_does_not_create_artifact(self):
        progress = server.empty_progress()
        progress["phase"] = "finished"
        with patch.object(
            self.collector,
            "snapshot_state",
        ), patch.object(
            self.collector,
            "logs",
        ), patch.object(
            self.collector,
            "collect_diagnostic_logs",
            return_value={"files": []},
        ), patch.object(
            self.collector,
            "capture",
            return_value={
                "returncode": 0,
                "stderr": "",
                "progress": progress,
                "tls_keylog": {"available": False},
            },
        ):
            self.collector.start()
            self.collector.thread.join(timeout=2)
        self.assertEqual(self.collector.status["state"], "completed")
        self.assertIsNone(self.collector.status["last_artifact"])
        self.assertEqual(self.collector.artifacts(), [])
        self.assertIn("未捕获", self.collector.status["last_message"])

    def test_readable_text_files_explain_capture_without_special_software(self):
        run_dir = pathlib.Path(self.tmp.name) / "readable"
        run_dir.mkdir()
        server.write_json(
            run_dir / "pcap-analysis.json",
            {
                "pcapRecords": 3,
                "connectionTupleCount": 1,
                "captureFirstPacketUtc": "2026-09-25T00:00:00.000Z",
                "captureLastPacketUtc": "2026-09-25T00:00:02.000Z",
                "connections": [
                    {
                        "endpointA": "10.0.0.2:50000",
                        "endpointB": "203.0.113.10:443",
                        "firstSeenUtc": "2026-09-25T00:00:00.000Z",
                        "lastSeenUtc": "2026-09-25T00:00:02.000Z",
                        "durationSec": 2.0,
                        "packets": 3,
                        "tcpPayloadBytes": 2048,
                        "maxPacketGapSec": 2.0,
                        "p95PacketGapSec": 2.0,
                        "closureObserved": False,
                        "possibleStartBeforeCapture": False,
                        "possibleContinuationAfterCapture": True,
                    }
                ],
            },
        )
        server.write_json(
            run_dir / "state-start.json",
            {
                "collected_at": "2026-09-25T00:00:00Z",
                "socket_probe": {"ok": True},
                "admin_state": {
                    "endpoints": {
                        "credential_summary": {
                            "body": {
                                "globalInFlightRequests": 2,
                                "queuedRequests": 1,
                                "available": 3,
                                "globalMaxConcurrentRequests": 10,
                            }
                        },
                        "local_credential_runtime": {"body": {"items": [{"id": 1}]}},
                    }
                },
            },
        )
        server.write_json(
            run_dir / "state-end.json",
            {
                "collected_at": "2026-09-25T00:00:02Z",
                "socket_probe": {"ok": True},
                "admin_state": {
                    "endpoints": {
                        "credential_summary": {
                            "body": {
                                "globalInFlightRequests": 0,
                                "queuedRequests": 0,
                                "available": 4,
                                "globalMaxConcurrentRequests": 10,
                            }
                        },
                        "local_credential_runtime": {"body": {"items": []}},
                    }
                },
            },
        )
        (run_dir / "runtime-timeline.jsonl").write_text(
            json.dumps(
                {
                    "collected_at": "2026-09-25T00:00:01Z",
                    "reason": "periodic",
                    "pcap": {
                        "elapsed_seconds": 1.0,
                        "pcap_records": 2,
                        "pcap_bytes": 2048,
                        "session_count": 1,
                        "active_session_count": 1,
                        "udp_flow_count": 0,
                    },
                    "admin_state": {
                        "endpoints": {
                            "credential_summary": {
                                "body": {
                                    "globalInFlightRequests": 2,
                                    "queuedRequests": 1,
                                    "available": 3,
                                    "globalMaxConcurrentRequests": 10,
                                }
                            },
                            "local_credential_runtime": {
                                "body": {"items": [{"id": 1}, {"id": 2}]}
                            },
                        }
                    },
                },
                ensure_ascii=False,
            )
            + "\n",
            encoding="utf-8",
        )
        self.collector.status["started_at"] = "2026-09-25T00:00:00Z"
        self.collector.status["progress"] = {
            **server.empty_progress(),
            "phase": "finished",
            "elapsed_seconds": 2.0,
            "pcap_bytes": 4096,
            "pcap_records": 3,
            "tcp_packets": 3,
            "tcp_payload_bytes": 2048,
            "session_count": 1,
            "active_session_count": 1,
            "tls_keylog_available": False,
        }
        self.collector.add_event("测试事件")
        self.collector.write_readable_files(
            run_dir,
            {"started_at": "2026-09-25T00:00:00Z", "finished_at": "2026-09-25T00:00:02Z"},
            self.collector.status["progress"],
        )
        self.assertIn("测试事件", (run_dir / "capture-events.txt").read_text())
        self.assertIn("TCP 会话：1", (run_dir / "tcp-flows.txt").read_text())
        self.assertIn("全局并发：2，排队：1", (run_dir / "runtime-state.txt").read_text())
        upstream = (run_dir / "upstream-readable.txt").read_text()
        self.assertIn("当前没有 TLS 会话密钥", upstream)
        self.assertIn("https-readable.txt", (run_dir / "capture-summary.txt").read_text())
        self.assertIn("文本编辑器", (run_dir / "README.txt").read_text())
        combined = run_dir / "capture-readable.txt"
        self.assertTrue(combined.is_file())
        self.assertIn("运行状态可读摘要", combined.read_text(encoding="utf-8"))
        self.assertIn("运行状态时间线", combined.read_text(encoding="utf-8"))
        self.assertIn("本地账号摘要：globalInFlight=2", (run_dir / "runtime-timeline.txt").read_text(encoding="utf-8"))
        self.assertEqual(combined.stat().st_mode & 0o777, 0o600)

    def test_decrypt_capture_writes_clear_missing_keylog_reason(self):
        run_dir = pathlib.Path(self.tmp.name) / "decrypt-no-keylog"
        run_dir.mkdir()
        (run_dir / "traffic.pcap").write_bytes(b"pcap placeholder")
        report = self.collector.decrypt_capture(run_dir)
        text = (run_dir / "https-readable.txt").read_text(encoding="utf-8")
        self.assertEqual(report["status"], "unavailable")
        self.assertIn("TLS 会话密钥", report["reason"])
        self.assertIn("仍是密文", text)
        self.assertTrue((run_dir / "tls-decryption-command.json").is_file())

    def test_decrypt_capture_uses_configured_readable_limit(self):
        run_dir = pathlib.Path(self.tmp.name) / "decrypt-limit"
        run_dir.mkdir()
        (run_dir / "traffic.pcap").write_bytes(b"pcap placeholder")
        self.collector.args.max_bytes = 9000
        self.collector.args.max_readable_bytes = 7000

        captured_command = {}

        def fake_run(command, timeout=0):
            captured_command["command"] = command
            server.write_json(
                run_dir / "tls-decryption.json",
                {
                    "status": "unavailable",
                    "tls_decryption": False,
                    "http2_streams": 0,
                    "reason": "test",
                },
            )
            (run_dir / "https-readable.txt").write_text("test\n", encoding="utf-8")
            return {"returncode": 0, "stdout": "", "stderr": "", "elapsed_ms": 1}

        with patch("server.run_readonly", fake_run):
            self.collector.decrypt_capture(run_dir)

        command = captured_command["command"]
        self.assertEqual(command[command.index("--max-bytes") + 1], "7000")
        self.assertEqual(
            command[command.index("--max-tshark-output-bytes") + 1],
            "9000",
        )

    def test_admin_state_excludes_external_pools_by_default(self):
        token_file = pathlib.Path(self.tmp.name) / "admin-token"
        token_file.write_text("secret-admin", encoding="utf-8")
        self.collector.args.admin_url = "http://127.0.0.1:59137"
        self.collector.args.admin_token_file = str(token_file)
        self.collector.args.include_external_pools = False
        requested_paths = []

        class FakeResponse:
            status = 200

            def __init__(self, body):
                self.body = body

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self, _limit):
                return json.dumps(self.body).encode("utf-8")

        def fake_urlopen(request, timeout=0):
            requested_paths.append(urlparse(request.full_url).path)
            if request.full_url.endswith("/api/admin/config/runtime"):
                return FakeResponse(
                    {
                        "config": {
                            "credentialMaxConcurrentRequests": 10,
                            "externalPools": [{"name": "out-of-scope"}],
                        }
                    }
                )
            return FakeResponse({"items": [], "globalInFlightRequests": 1})

        with patch("urllib.request.urlopen", fake_urlopen):
            state = self.collector.admin_state(timeout=0.1)

        self.assertNotIn("/api/admin/external-pools/status", requested_paths)
        runtime = state["endpoints"]["runtime_config"]["body"]
        self.assertEqual(
            runtime["config"]["externalPools"],
            "<excluded: external pool out of scope>",
        )
        self.assertFalse(state["scope"]["external_pools_included"])

    def test_admin_state_collects_bounded_local_usage_records(self):
        token_file = pathlib.Path(self.tmp.name) / "admin-token"
        token_file.write_text("secret-admin", encoding="utf-8")
        self.collector.args.admin_url = "http://127.0.0.1:59137"
        self.collector.args.admin_token_file = str(token_file)
        self.collector.args.include_external_pools = False
        requested_urls = []

        class FakeResponse:
            status = 200

            def __init__(self, body):
                self.body = body

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self, _limit):
                return json.dumps(self.body).encode("utf-8")

        def fake_urlopen(request, timeout=0):
            requested_urls.append(request.full_url)
            path = urlparse(request.full_url).path
            if path.endswith("/usage-records"):
                return FakeResponse(
                    {
                        "records": [
                            {
                                "id": "req-local",
                                "routeKind": "local_credential",
                                "credentialId": 7,
                            },
                            {
                                "id": "req-external",
                                "routeKind": "external_pool",
                                "externalPool": {"name": "out-of-scope"},
                            },
                        ]
                    }
                )
            return FakeResponse({"items": [], "globalInFlightRequests": 1})

        with patch("urllib.request.urlopen", fake_urlopen):
            state = self.collector.admin_state(
                timeout=0.1,
                since="2026-09-25T00:00:00Z",
                until="2026-09-25T00:01:00Z",
            )

        usage_urls = [
            url
            for url in requested_urls
            if urlparse(url).path.endswith("/usage-records")
        ]
        self.assertEqual(len(usage_urls), 1)
        self.assertIn("routeKind=local_credential", usage_urls[0])
        self.assertIn("limit=200", usage_urls[0])
        body = state["endpoints"]["usage_records"]["body"]
        self.assertEqual(len(body["records"]), 1)
        self.assertEqual(body["records"][0]["id"], "req-local")

    def test_collects_local_tool_format_debug_jsonl_from_logs_mount(self):
        host_logs = pathlib.Path(self.tmp.name) / "host-logs"
        debug_dir = host_logs / "tool-format-debug"
        debug_dir.mkdir(parents=True)
        source = debug_dir / "tool-format-2026-09-27-0000-000.jsonl"
        source.write_text(
            json.dumps({"requestId": "req-local", "routeKind": "local_credential"})
            + "\n"
            + json.dumps(
                {
                    "requestId": "req-external",
                    "routeKind": "external_pool",
                    "externalPool": {"name": "out-of-scope"},
                }
            )
            + "\n",
            encoding="utf-8",
        )
        run_dir = pathlib.Path(self.tmp.name) / "diagnostic-run"
        run_dir.mkdir()
        inspect = {
            "returncode": 0,
            "stdout": json.dumps(
                [{"Destination": "/app/logs", "Source": str(host_logs)}]
            ),
            "stderr": "",
        }
        with patch("server.run_readonly", return_value=inspect):
            result = self.collector.collect_diagnostic_logs(
                run_dir, "2026-09-26T00:00:00Z"
            )
        self.assertEqual(len(result["files"]), 1)
        copied = (run_dir / "tool-format-debug" / source.name).read_text()
        self.assertIn("req-local", copied)
        self.assertNotIn("req-external", copied)
        self.assertEqual(result["skipped_external_lines"], 1)
        self.assertTrue((run_dir / "diagnostic-log-index.json").is_file())

    def test_usage_window_text_writes_local_usage_snapshot(self):
        run_dir = pathlib.Path(self.tmp.name) / "usage-run"
        run_dir.mkdir()
        server.write_json(
            run_dir / "state-end.json",
            {
                "admin_state": {
                    "endpoints": {
                        "usage_records": {
                            "body": {
                                "records": [
                                    {
                                        "id": "req-local",
                                        "endpoint": "/cc/v1/messages",
                                        "status": "success",
                                        "credentialId": 7,
                                        "durationMs": 1234,
                                    }
                                ]
                            }
                        }
                    }
                }
            },
        )
        text = self.collector.usage_window_text(run_dir)
        self.assertIn("req-local", text)
        self.assertIn("1234", text)
        self.assertTrue((run_dir / "usage-records-local.json").is_file())

    def test_admin_state_can_include_external_pools_when_explicit(self):
        token_file = pathlib.Path(self.tmp.name) / "admin-token"
        token_file.write_text("secret-admin", encoding="utf-8")
        self.collector.args.admin_url = "http://127.0.0.1:59137"
        self.collector.args.admin_token_file = str(token_file)
        self.collector.args.include_external_pools = True
        requested_paths = []

        class FakeResponse:
            status = 200

            def __init__(self, body):
                self.body = body

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self, _limit):
                return json.dumps(self.body).encode("utf-8")

        def fake_urlopen(request, timeout=0):
            requested_paths.append(urlparse(request.full_url).path)
            return FakeResponse({"ok": True})

        with patch("urllib.request.urlopen", fake_urlopen):
            state = self.collector.admin_state(timeout=0.1)

        self.assertIn("/api/admin/external-pools/status", requested_paths)
        self.assertTrue(state["scope"]["external_pools_included"])


if __name__ == "__main__":
    unittest.main()
