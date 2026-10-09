"""Small Ethernet parser shared by the bounded capture and pcap analyzers."""

from __future__ import annotations

import ipaddress
import struct
from dataclasses import dataclass


@dataclass(frozen=True)
class TransportPacket:
    source: str
    destination: str
    source_port: int
    destination_port: int
    protocol: str
    tcp_flags: int
    payload_bytes: int
    captured_payload_bytes: int


VLAN_TYPES = {0x8100, 0x88A8, 0x9100}
IPV6_EXTENSION_TYPES = {0, 43, 60, 135, 139, 140}


def ethernet_transport(packet: bytes) -> TransportPacket | None:
    """Parse TCP/UDP ports and payload lengths from Ethernet IPv4/IPv6 frames."""
    if len(packet) < 14:
        return None
    ether_type = struct.unpack_from("!H", packet, 12)[0]
    offset = 14
    while ether_type in VLAN_TYPES:
        if len(packet) < offset + 4:
            return None
        ether_type = struct.unpack_from("!H", packet, offset + 2)[0]
        offset += 4

    if ether_type == 0x0800:
        parsed_ip = _ipv4(packet, offset)
    elif ether_type == 0x86DD:
        parsed_ip = _ipv6(packet, offset)
    else:
        return None
    if parsed_ip is None:
        return None

    source, destination, protocol_number, transport_offset, ip_payload_bytes = parsed_ip
    if protocol_number not in (6, 17):
        return None
    if len(packet) < transport_offset + (20 if protocol_number == 6 else 8):
        return None
    source_port, destination_port = struct.unpack_from("!HH", packet, transport_offset)
    if protocol_number == 6:
        header_bytes = (packet[transport_offset + 12] >> 4) * 4
        if header_bytes < 20 or ip_payload_bytes < header_bytes:
            return None
        if len(packet) < transport_offset + header_bytes:
            return None
        payload_bytes = ip_payload_bytes - header_bytes
        captured_payload_bytes = min(
            payload_bytes,
            max(0, len(packet) - transport_offset - header_bytes),
        )
        flags = packet[transport_offset + 13]
        protocol = "tcp"
    else:
        udp_length = struct.unpack_from("!H", packet, transport_offset + 4)[0]
        if udp_length < 8:
            return None
        udp_payload_bytes = min(max(0, udp_length - 8), max(0, ip_payload_bytes - 8))
        payload_bytes = udp_payload_bytes
        captured_payload_bytes = min(
            udp_payload_bytes,
            max(0, len(packet) - transport_offset - 8),
        )
        flags = 0
        protocol = "udp"

    return TransportPacket(
        source=source,
        destination=destination,
        source_port=source_port,
        destination_port=destination_port,
        protocol=protocol,
        tcp_flags=flags,
        payload_bytes=payload_bytes,
        captured_payload_bytes=captured_payload_bytes,
    )


def _ipv4(
    packet: bytes, offset: int
) -> tuple[str, str, int, int, int] | None:
    if len(packet) < offset + 20:
        return None
    version_ihl = packet[offset]
    if version_ihl >> 4 != 4:
        return None
    header_bytes = (version_ihl & 0x0F) * 4
    if header_bytes < 20 or len(packet) < offset + header_bytes:
        return None
    total_bytes = struct.unpack_from("!H", packet, offset + 2)[0]
    if total_bytes < header_bytes:
        return None
    fragment = struct.unpack_from("!H", packet, offset + 6)[0]
    if fragment & 0x1FFF:
        # Non-initial fragments do not contain transport ports.
        return None
    source = str(ipaddress.IPv4Address(packet[offset + 12 : offset + 16]))
    destination = str(ipaddress.IPv4Address(packet[offset + 16 : offset + 20]))
    protocol = packet[offset + 9]
    transport_offset = offset + header_bytes
    ip_payload_bytes = total_bytes - header_bytes
    return source, destination, protocol, transport_offset, ip_payload_bytes


def _ipv6(
    packet: bytes, offset: int
) -> tuple[str, str, int, int, int] | None:
    if len(packet) < offset + 40 or packet[offset] >> 4 != 6:
        return None
    payload_bytes = struct.unpack_from("!H", packet, offset + 4)[0]
    next_header = packet[offset + 6]
    source = str(ipaddress.IPv6Address(packet[offset + 8 : offset + 24]))
    destination = str(ipaddress.IPv6Address(packet[offset + 24 : offset + 40]))
    transport_offset = offset + 40
    consumed = 0

    while next_header in IPV6_EXTENSION_TYPES or next_header in (44, 51):
        if transport_offset + 2 > len(packet) or consumed + 2 > payload_bytes:
            return None
        extension_type = next_header
        next_header = packet[transport_offset]
        length_byte = packet[transport_offset + 1]
        if extension_type == 44:
            extension_bytes = 8
            if transport_offset + extension_bytes > len(packet):
                return None
            fragment = struct.unpack_from("!H", packet, transport_offset + 2)[0]
            if fragment & 0xFFF8:
                return None
        elif extension_type == 51:
            extension_bytes = (length_byte + 2) * 4
        else:
            extension_bytes = (length_byte + 1) * 8
        if (
            extension_bytes < 8
            or transport_offset + extension_bytes > len(packet)
            or consumed + extension_bytes > payload_bytes
        ):
            return None
        transport_offset += extension_bytes
        consumed += extension_bytes

    remaining_payload = payload_bytes - consumed
    return source, destination, next_header, transport_offset, remaining_payload
