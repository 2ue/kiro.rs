import ipaddress
import pathlib
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import analyze_pcap_flows as analyzer  # noqa: E402


def ethernet(payload: bytes, ether_type: int) -> bytes:
    return b"\x00" * 12 + struct.pack("!H", ether_type) + payload


def ipv4_udp(source: str, destination: str, source_port: int, destination_port: int, payload: bytes) -> bytes:
    source_bytes = ipaddress.IPv4Address(source).packed
    destination_bytes = ipaddress.IPv4Address(destination).packed
    udp = struct.pack(
        "!HHHH", source_port, destination_port, 8 + len(payload), 0
    ) + payload
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
        source_bytes,
        destination_bytes,
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


class AnalyzePcapFlowsTests(unittest.TestCase):
    def test_reports_udp_flow_without_calling_it_a_tcp_session(self):
        with tempfile.TemporaryDirectory() as temp:
            path = pathlib.Path(temp) / "traffic.pcap"
            write_pcap(
                path,
                [
                    ipv4_udp(
                        "10.0.0.2",
                        "203.0.113.4",
                        51000,
                        443,
                        b"client",
                    ),
                    ipv4_udp(
                        "203.0.113.4",
                        "10.0.0.2",
                        443,
                        51000,
                        b"server",
                    ),
                ],
            )
            result = analyzer.summarize(path)
        self.assertEqual(result["pcapRecords"], 2)
        self.assertEqual(result["connectionTupleCount"], 1)
        self.assertEqual(result["tcpConnectionTupleCount"], 0)
        self.assertEqual(result["udpFlowCount"], 1)
        flow = result["connections"][0]
        self.assertEqual(flow["protocol"], "udp")
        self.assertEqual(flow["udpPayloadBytes"], len(b"clientserver"))
        self.assertFalse(flow["closureObserved"])


if __name__ == "__main__":
    unittest.main()
