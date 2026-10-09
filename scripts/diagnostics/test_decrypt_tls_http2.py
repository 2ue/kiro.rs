import csv
import io
import json
import pathlib
import struct
import sys
import tempfile
import unittest
import zlib
from subprocess import CompletedProcess
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import decrypt_tls_http2 as decrypt  # noqa: E402


def encode_string_header(name: str, value: str) -> bytes:
    name_bytes = name.encode()
    value_bytes = value.encode()
    return (
        bytes([len(name_bytes)])
        + name_bytes
        + b"\x07"
        + struct.pack("!H", len(value_bytes))
        + value_bytes
    )


def encode_eventstream_frame(event_type: str, payload: dict[str, object]) -> bytes:
    headers = encode_string_header(":message-type", "event") + encode_string_header(
        ":event-type", event_type
    )
    payload_bytes = json.dumps(payload, separators=(",", ":")).encode()
    total_length = 12 + len(headers) + len(payload_bytes) + 4
    prelude = struct.pack("!II", total_length, len(headers))
    prelude += struct.pack("!I", zlib.crc32(prelude) & 0xFFFFFFFF)
    message = prelude + headers + payload_bytes
    return message + struct.pack("!I", zlib.crc32(message) & 0xFFFFFFFF)


def tshark_row(**values: str) -> list[str]:
    return [values.get(field, "") for field in decrypt.TSHARK_FIELDS]


def tshark_output(rows: list[list[str]]) -> str:
    stream = io.StringIO()
    writer = csv.writer(stream, delimiter="\t", quotechar='"', lineterminator="\n")
    writer.writerows(rows)
    return stream.getvalue()


class DecryptTlsHttp2Tests(unittest.TestCase):
    def test_decodes_valid_aws_eventstream_payload_and_headers(self):
        raw = encode_eventstream_frame(
            "assistantResponseEvent",
            {"content": "answer text", "messageStatus": "COMPLETE"},
        )
        frames, error = decrypt.decode_aws_eventstream(raw)
        self.assertIsNone(error)
        self.assertEqual(len(frames), 1)
        self.assertEqual(frames[0]["headers"][":event-type"], "assistantResponseEvent")
        self.assertEqual(frames[0]["payload"]["content"], "answer text")

    def test_reports_invalid_frame_without_calling_it_a_success(self):
        raw = bytearray(
            encode_eventstream_frame("assistantResponseEvent", {"content": "x"})
        )
        raw[8] ^= 1
        frames, error = decrypt.decode_aws_eventstream(bytes(raw))
        self.assertEqual(frames, [])
        self.assertIn("Prelude CRC", error)

    def test_exports_request_system_prompt_and_decoded_upstream_events(self):
        request = json.dumps(
            {
                "system": "private system prompt",
                "messages": [
                    {"role": "user", "content": "private user message"},
                    {
                        "role": "assistant",
                        "content": [
                            {
                                "type": "tool_use",
                                "id": "toolu_private",
                                "name": "example_tool",
                                "input": {"query": "private tool input"},
                            }
                        ],
                    },
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "tool_result",
                                "tool_use_id": "toolu_private",
                                "content": "private tool result",
                            }
                        ],
                    },
                ],
                "tools": [
                    {
                        "name": "example_tool",
                        "description": "private tool schema",
                        "input_schema": {"type": "object"},
                    }
                ],
            },
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode()
        response = (
            encode_eventstream_frame(
                "reasoningContentEvent",
                {"reasoningText": "private reasoning trace"},
            )
            + encode_eventstream_frame(
                "toolUseEvent",
                {
                    "name": "example_tool",
                    "input": {"query": "private upstream tool input"},
                },
            )
            + encode_eventstream_frame(
                "assistantResponseEvent",
                {"content": "private model output", "messageStatus": "COMPLETE"},
            )
        )
        rows = [
            tshark_row(
                **{
                    "frame.time_epoch": "1790294400.1",
                    "frame.number": "1",
                    "ip.src": "10.0.0.2",
                    "tcp.srcport": "45000",
                    "ip.dst": "203.0.113.4",
                    "tcp.dstport": "443",
                    "tcp.stream": "0",
                    "http2.type": "1",
                    "http2.streamid": "1",
                    "http2.headers.method": "POST",
                    "http2.headers.path": "/invoke",
                    "http2.headers.authority": "upstream.example",
                    "http2.headers.content_type": "application/json",
                    "http2.header.name": decrypt.TSHARK_AGGREGATOR.join(
                        [":method", ":path", ":authority", "content-type", "x-custom-proto"]
                    ),
                    "http2.header.value": decrypt.TSHARK_AGGREGATOR.join(
                        ["POST", "/invoke", "upstream.example", "application/json", "variant-a"]
                    ),
                }
            ),
            tshark_row(
                **{
                    "frame.time_epoch": "1790294400.1",
                    "frame.number": "1",
                    "ip.src": "10.0.0.2",
                    "tcp.srcport": "45000",
                    "ip.dst": "203.0.113.4",
                    "tcp.dstport": "443",
                    "tcp.stream": "0",
                    "http2.type": "0",
                    "http2.streamid": "1",
                    "http2.data.data": request.hex(":"),
                    "http2.flags.end_stream": "True",
                }
            ),
            tshark_row(
                **{
                    "frame.time_epoch": "1790294401.2",
                    "frame.number": "2",
                    "ip.src": "203.0.113.4",
                    "tcp.srcport": "443",
                    "ip.dst": "10.0.0.2",
                    "tcp.dstport": "45000",
                    "tcp.stream": "0",
                    "http2.type": "1",
                    "http2.streamid": "1",
                    "http2.headers.content_type": "application/vnd.amazon.eventstream",
                    "http2.headers.status": "200",
                    "http2.header.name": decrypt.TSHARK_AGGREGATOR.join(
                        [":status", "content-type", "x-upstream-build"]
                    ),
                    "http2.header.value": decrypt.TSHARK_AGGREGATOR.join(
                        ["200", "application/vnd.amazon.eventstream", "build-7"]
                    ),
                }
            ),
            tshark_row(
                **{
                    "frame.time_epoch": "1790294401.2",
                    "frame.number": "2",
                    "ip.src": "203.0.113.4",
                    "tcp.srcport": "443",
                    "ip.dst": "10.0.0.2",
                    "tcp.dstport": "45000",
                    "tcp.stream": "0",
                    "http2.type": "0",
                    "http2.streamid": "1",
                    "http2.data.data": response.hex(":"),
                    "http2.flags.end_stream": "True",
                }
            ),
        ]

        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            pcap = root / "traffic.pcap"
            keylog = root / "tls-keylog.log"
            pcap.write_bytes(b"pcap placeholder")
            keylog.write_text("CLIENT_RANDOM test secret\n", encoding="utf-8")
            with patch.object(
                decrypt,
                "run_tshark",
                return_value=CompletedProcess(
                    args=["tshark"],
                    returncode=0,
                    stdout=tshark_output(rows),
                    stderr="",
                ),
            ) as run_tshark:
                report = decrypt.write_report(
                    pcap,
                    keylog,
                    root,
                    "/usr/bin/tshark",
                    timeout=10,
                    max_bytes=1024 * 1024,
                    max_tshark_output_bytes=1024 * 1024,
                )
            run_tshark.assert_called_once()
            readable = (root / "https-readable.txt").read_text(encoding="utf-8")
            self.assertEqual(report["status"], "decrypted")
            self.assertEqual(report["http2_streams"], 1)
            self.assertFalse(report["readable_truncated"])
            self.assertEqual(report["plaintext_scope"], "full_http2_bodies_when_decrypted")
            self.assertIn("private system prompt", readable)
            self.assertIn("private user message", readable)
            self.assertIn("private tool schema", readable)
            self.assertIn("private tool input", readable)
            self.assertIn("private tool result", readable)
            self.assertIn("private reasoning trace", readable)
            self.assertIn("private upstream tool input", readable)
            self.assertIn("private model output", readable)
            self.assertIn("assistantResponseEvent", readable)
            self.assertIn("reasoningContentEvent", readable)
            self.assertIn("toolUseEvent", readable)
            self.assertIn("x-custom-proto: variant-a", readable)
            self.assertIn("x-upstream-build: build-7", readable)
            self.assertIn("可读文本上限", readable)
            self.assertIn("请求 END_STREAM：是", readable)
            self.assertIn("响应 END_STREAM：是", readable)

    def test_end_stream_is_reported_separately_by_direction(self):
        rows = decrypt.parse_tshark_rows(
            tshark_output(
                [
                    tshark_row(
                        **{
                            "frame.time_epoch": "1790294400.1",
                            "frame.number": "1",
                            "ip.src": "10.0.0.2",
                            "tcp.srcport": "45000",
                            "ip.dst": "203.0.113.4",
                            "tcp.dstport": "443",
                            "tcp.stream": "0",
                            "http2.type": "1",
                            "http2.streamid": "1",
                            "http2.headers.method": "POST",
                        }
                    ),
                    tshark_row(
                        **{
                            "frame.time_epoch": "1790294400.2",
                            "frame.number": "2",
                            "ip.src": "10.0.0.2",
                            "tcp.srcport": "45000",
                            "ip.dst": "203.0.113.4",
                            "tcp.dstport": "443",
                            "tcp.stream": "0",
                            "http2.type": "0",
                            "http2.streamid": "1",
                            "http2.data.data": b"request".hex(":"),
                            "http2.flags.end_stream": "True",
                        }
                    ),
                    tshark_row(
                        **{
                            "frame.time_epoch": "1790294401.2",
                            "frame.number": "3",
                            "ip.src": "203.0.113.4",
                            "tcp.srcport": "443",
                            "ip.dst": "10.0.0.2",
                            "tcp.dstport": "45000",
                            "tcp.stream": "0",
                            "http2.type": "1",
                            "http2.streamid": "1",
                            "http2.headers.status": "200",
                        }
                    ),
                    tshark_row(
                        **{
                            "frame.time_epoch": "1790294402.2",
                            "frame.number": "4",
                            "ip.src": "203.0.113.4",
                            "tcp.srcport": "443",
                            "ip.dst": "10.0.0.2",
                            "tcp.dstport": "45000",
                            "tcp.stream": "0",
                            "http2.type": "0",
                            "http2.streamid": "1",
                            "http2.data.data": b"still-streaming".hex(":"),
                        }
                    ),
                ]
            )
        )
        streams = decrypt.assemble_streams(decrypt.expand_tshark_rows(rows))
        self.assertEqual(len(streams), 1)
        self.assertTrue(streams[0]["request_end_stream_seen"])
        self.assertFalse(streams[0]["response_end_stream_seen"])
        rendered = decrypt.render_stream(streams[0])
        self.assertIn("请求 END_STREAM：是", rendered)
        self.assertIn("响应 END_STREAM：否", rendered)
        self.assertIn("响应可能仍在进行", rendered)

    def test_collects_goaway_and_reset_stream_frames(self):
        rows = decrypt.parse_tshark_rows(
            tshark_output(
                [
                    tshark_row(
                        **{
                            "frame.time_epoch": "1790294400.1",
                            "frame.number": "9",
                            "tcp.stream": "3",
                            "ip.src": "203.0.113.4",
                            "tcp.srcport": "443",
                            "ip.dst": "10.0.0.2",
                            "tcp.dstport": "45000",
                            "http2.type": "7",
                            "http2.streamid": "0",
                            "http2.goaway.last_stream_id": "5",
                            "http2.goaway.error": "0",
                        }
                    ),
                    tshark_row(
                        **{
                            "frame.time_epoch": "1790294401.1",
                            "frame.number": "10",
                            "tcp.stream": "3",
                            "ip.src": "203.0.113.4",
                            "tcp.srcport": "443",
                            "ip.dst": "10.0.0.2",
                            "tcp.dstport": "45000",
                            "http2.type": "3",
                            "http2.streamid": "5",
                            "http2.rst_stream.error": "8",
                        }
                    ),
                ]
            )
        )
        events = decrypt.collect_http2_control_events(rows)
        self.assertEqual([item["type"] for item in events], ["GOAWAY", "RST_STREAM"])
        self.assertEqual(events[0]["goaway.last_stream_id"], "5")
        self.assertEqual(events[1]["rst_stream.error"], "8")

    def test_expands_multiple_http2_data_frames_in_one_packet(self):
        rows = decrypt.parse_tshark_rows(
            tshark_output(
                [
                    tshark_row(
                        **{
                            "tcp.stream": "4",
                            "http2.type": decrypt.TSHARK_AGGREGATOR.join(["0", "0"]),
                            "http2.streamid": decrypt.TSHARK_AGGREGATOR.join(["1", "3"]),
                            "http2.data.data": decrypt.TSHARK_AGGREGATOR.join(
                                ["41:42", "43:44"]
                            ),
                        }
                    )
                ]
            )
        )
        expanded = decrypt.expand_tshark_rows(rows)
        self.assertEqual(len(expanded), 2)
        self.assertEqual(
            [(row["http2.streamid"], row["http2.data.data"]) for row in expanded],
            [("1", "41:42"), ("3", "43:44")],
        )

    def test_missing_keylog_creates_plain_text_explanation(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            pcap = root / "traffic.pcap"
            pcap.write_bytes(b"pcap placeholder")
            report = decrypt.write_report(
                pcap,
                None,
                root,
                "/usr/bin/tshark",
                timeout=10,
                max_bytes=1024,
                max_tshark_output_bytes=1024,
            )
            text = (root / "https-readable.txt").read_text(encoding="utf-8")
            self.assertFalse(report["tls_decryption"])
            self.assertEqual(report["status"], "unavailable")
            self.assertIn("TLS 会话密钥", report["reason"])
            self.assertIn("仍是密文", text)

    def test_readable_limit_marks_plaintext_report_truncated(self):
        request = json.dumps(
            {"system": "private system prompt", "messages": [{"content": "x" * 256}]},
            separators=(",", ":"),
        ).encode()
        rows = [
            tshark_row(
                **{
                    "frame.time_epoch": "1790294400.1",
                    "frame.number": "1",
                    "ip.src": "10.0.0.2",
                    "tcp.srcport": "45000",
                    "ip.dst": "203.0.113.4",
                    "tcp.dstport": "443",
                    "tcp.stream": "0",
                    "http2.type": "1",
                    "http2.streamid": "1",
                    "http2.headers.method": "POST",
                }
            ),
            tshark_row(
                **{
                    "frame.time_epoch": "1790294400.2",
                    "frame.number": "2",
                    "ip.src": "10.0.0.2",
                    "tcp.srcport": "45000",
                    "ip.dst": "203.0.113.4",
                    "tcp.dstport": "443",
                    "tcp.stream": "0",
                    "http2.type": "0",
                    "http2.streamid": "1",
                    "http2.data.data": request.hex(":"),
                    "http2.flags.end_stream": "True",
                }
            ),
        ]
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            pcap = root / "traffic.pcap"
            keylog = root / "tls-keylog.log"
            pcap.write_bytes(b"pcap placeholder")
            keylog.write_text("CLIENT_RANDOM test secret\n", encoding="utf-8")
            with patch.object(
                decrypt,
                "run_tshark",
                return_value=CompletedProcess(
                    args=["tshark"],
                    returncode=0,
                    stdout=tshark_output(rows),
                    stderr="",
                ),
            ):
                report = decrypt.write_report(
                    pcap,
                    keylog,
                    root,
                    "/usr/bin/tshark",
                    timeout=10,
                    max_bytes=64,
                    max_tshark_output_bytes=1024 * 1024,
                )
            self.assertEqual(report["status"], "truncated")
            self.assertTrue(report["readable_truncated"])
            self.assertIn("可读文本达到", report["reason"])

    def test_tshark_output_limit_stops_subprocess(self):
        class FakeProcess:
            def __init__(self, *args, **kwargs):
                self.stdout = io.BytesIO(b"x" * 64)
                self.killed = False

            def poll(self):
                return 0

            def wait(self):
                return 0

            def kill(self):
                self.killed = True

        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            pcap = root / "capture.pcap"
            keylog = root / "keylog"
            pcap.write_bytes(b"")
            keylog.write_text("key\n", encoding="utf-8")
            with patch.object(decrypt.subprocess, "Popen", FakeProcess):
                with self.assertRaisesRegex(RuntimeError, "输出超过 8 字节上限"):
                    decrypt.run_tshark(
                        "tshark",
                        pcap,
                        keylog,
                        timeout=2,
                        max_output_bytes=8,
                    )


if __name__ == "__main__":
    unittest.main()
