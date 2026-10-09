import ipaddress
import pathlib
import struct
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from packet_protocols import ethernet_transport


def ethernet(payload: bytes, ether_type: int) -> bytes:
    return b"\x00" * 12 + struct.pack("!H", ether_type) + payload


class PacketProtocolTests(unittest.TestCase):
    def test_parses_ipv4_udp_quic_shape(self):
        source = ipaddress.IPv4Address("10.0.0.2").packed
        destination = ipaddress.IPv4Address("203.0.113.4").packed
        payload = b"quic-payload"
        udp = struct.pack("!HHHH", 51000, 443, 8 + len(payload), 0) + payload
        ipv4 = struct.pack(
            "!BBHHHBBH4s4s",
            0x45,
            0,
            20 + len(udp),
            7,
            0,
            64,
            17,
            0,
            source,
            destination,
        ) + udp
        parsed = ethernet_transport(ethernet(ipv4, 0x0800))
        self.assertIsNotNone(parsed)
        assert parsed is not None
        self.assertEqual(parsed.protocol, "udp")
        self.assertEqual((parsed.source_port, parsed.destination_port), (51000, 443))
        self.assertEqual(parsed.payload_bytes, len(payload))

    def test_parses_ipv6_tcp_with_extension_header(self):
        source = ipaddress.IPv6Address("2001:db8::2").packed
        destination = ipaddress.IPv6Address("2001:db8::4").packed
        tcp_payload = b"tls"
        tcp = (
            struct.pack("!HHIIHHHH", 51001, 443, 1, 0, 0x5002, 65535, 0, 0)
            + tcp_payload
        )
        # Hop-by-hop header: next header TCP, eight bytes total.
        hop = bytes([6, 0]) + b"\x00" * 6
        ipv6 = (
            struct.pack(
                "!IHBB16s16s",
                (6 << 28),
                len(hop) + len(tcp),
                0,
                64,
                source,
                destination,
            )
            + hop
            + tcp
        )
        parsed = ethernet_transport(ethernet(ipv6, 0x86DD))
        self.assertIsNotNone(parsed)
        assert parsed is not None
        self.assertEqual(parsed.protocol, "tcp")
        self.assertEqual((parsed.source_port, parsed.destination_port), (51001, 443))
        self.assertEqual(parsed.payload_bytes, len(tcp_payload))
        self.assertTrue(parsed.tcp_flags & 0x02)

    def test_ignores_non_initial_ipv4_fragment(self):
        source = ipaddress.IPv4Address("10.0.0.2").packed
        destination = ipaddress.IPv4Address("203.0.113.4").packed
        ipv4 = struct.pack(
            "!BBHHHBBH4s4s",
            0x45,
            0,
            20,
            9,
            1,
            64,
            6,
            0,
            source,
            destination,
        )
        self.assertIsNone(ethernet_transport(ethernet(ipv4, 0x0800)))


if __name__ == "__main__":
    unittest.main()
