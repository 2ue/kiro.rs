#!/usr/bin/env python3
"""Summarize bidirectional IPv4/IPv6 TCP and UDP endpoint tuples in a classic pcap.

Transport payload sizes come from the IP/UDP/TCP length fields, even when
snaplen truncates a packet. They include retransmissions and encrypted
TLS/QUIC bytes; one TCP tuple may contain many HTTP requests. This tool cannot
infer request completion.
"""

from __future__ import annotations

import argparse
import collections
import datetime
import json
import struct
from pathlib import Path

from packet_protocols import ethernet_transport


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, int(round(fraction * (len(ordered) - 1))))
    return ordered[index]


def iso_utc(timestamp: float) -> str:
    return datetime.datetime.fromtimestamp(timestamp, datetime.timezone.utc).isoformat(
        timespec="milliseconds"
    )


def endpoint_text(endpoint: tuple[str, int]) -> str:
    return f"{endpoint[0]}:{endpoint[1]}"


def new_direction() -> dict[str, object]:
    return {
        "packets": 0,
        "tcpPayloadBytes": 0,
        "udpPayloadBytes": 0,
        "capturedTcpPayloadBytes": 0,
        "capturedUdpPayloadBytes": 0,
        "snaplenTruncatedPackets": 0,
        "synPackets": 0,
        "finPackets": 0,
        "rstPackets": 0,
        "firstPayload": None,
        "lastPayload": None,
        "payloadGaps": [],
    }


def new_connection() -> dict[str, object]:
    return {
        "first": None,
        "last": None,
        "lastPacket": None,
        "packets": 0,
        "gaps": [],
        "directions": collections.defaultdict(new_direction),
    }


def summarize(path: Path) -> dict[str, object]:
    connections: dict[
        tuple[str, tuple[str, int], tuple[str, int]], dict[str, object]
    ] = collections.defaultdict(new_connection)
    capture_first = None
    capture_last = None
    records = 0
    skipped_unparsed_packets = 0

    with path.open("rb") as capture:
        global_header = capture.read(24)
        if len(global_header) != 24:
            raise ValueError("short pcap global header")
        formats = {
            b"\xd4\xc3\xb2\xa1": ("<", 1_000_000),
            b"\xa1\xb2\xc3\xd4": (">", 1_000_000),
            b"\x4d\x3c\xb2\xa1": ("<", 1_000_000_000),
            b"\xa1\xb2\x3c\x4d": (">", 1_000_000_000),
        }
        magic = global_header[:4]
        if magic not in formats:
            raise ValueError(f"unsupported pcap magic: {magic.hex()}")
        endian, timestamp_units = formats[magic]
        _, _, _, _, snaplen, link_type = struct.unpack_from(
            endian + "HHIIII", global_header, 4
        )
        if link_type != 1:
            raise ValueError(f"unsupported pcap link type: {link_type} (expected Ethernet)")

        while True:
            record_header = capture.read(16)
            if not record_header:
                break
            if len(record_header) != 16:
                raise ValueError(f"truncated pcap record header after {records} records")
            seconds, fraction, captured_len, original_len = struct.unpack(
                endian + "IIII", record_header
            )
            if captured_len > snaplen or captured_len > original_len:
                raise ValueError(f"invalid pcap record length at record {records + 1}")
            packet = capture.read(captured_len)
            if len(packet) != captured_len:
                raise ValueError(f"truncated pcap packet at record {records + 1}")
            records += 1
            timestamp = seconds + fraction / timestamp_units
            capture_first = timestamp if capture_first is None else min(capture_first, timestamp)
            capture_last = timestamp if capture_last is None else max(capture_last, timestamp)

            transport = ethernet_transport(packet)
            if transport is None:
                skipped_unparsed_packets += 1
                continue
            source_endpoint = (transport.source, transport.source_port)
            destination_endpoint = (transport.destination, transport.destination_port)
            key = (
                transport.protocol,
                *sorted((source_endpoint, destination_endpoint)),
            )
            flags = transport.tcp_flags
            payload_bytes = transport.payload_bytes
            captured_payload_bytes = transport.captured_payload_bytes
            connection = connections[key]
            if connection["first"] is None:
                connection["first"] = timestamp
            if connection["lastPacket"] is not None:
                connection["gaps"].append(max(0.0, timestamp - connection["lastPacket"]))
            connection["lastPacket"] = timestamp
            connection["last"] = timestamp
            connection["packets"] += 1

            direction = connection["directions"][source_endpoint]
            direction["packets"] += 1
            if transport.protocol == "tcp":
                direction["tcpPayloadBytes"] += payload_bytes
                direction["capturedTcpPayloadBytes"] += captured_payload_bytes
            else:
                direction["udpPayloadBytes"] += payload_bytes
                direction["capturedUdpPayloadBytes"] += captured_payload_bytes
            direction["snaplenTruncatedPackets"] += int(captured_len < original_len)
            if transport.protocol == "tcp":
                direction["synPackets"] += int(bool(flags & 0x02))
                direction["finPackets"] += int(bool(flags & 0x01))
                direction["rstPackets"] += int(bool(flags & 0x04))
            if payload_bytes:
                if direction["firstPayload"] is None:
                    direction["firstPayload"] = timestamp
                if direction["lastPayload"] is not None:
                    direction["payloadGaps"].append(
                        max(0.0, timestamp - direction["lastPayload"])
                    )
                direction["lastPayload"] = timestamp

    output_connections = []
    for (protocol, left, right), connection in connections.items():
        directions = {}
        for source, destination in ((left, right), (right, left)):
            direction = connection["directions"].get(source, new_direction())
            gaps = direction["payloadGaps"]
            directions[f"{endpoint_text(source)}->{endpoint_text(destination)}"] = {
                "packets": direction["packets"],
                "protocol": protocol,
                "tcpPayloadBytes": direction["tcpPayloadBytes"],
                "udpPayloadBytes": direction["udpPayloadBytes"],
                "capturedTcpPayloadBytes": direction["capturedTcpPayloadBytes"],
                "capturedUdpPayloadBytes": direction["capturedUdpPayloadBytes"],
                "snaplenTruncatedPackets": direction["snaplenTruncatedPackets"],
                "synPackets": direction["synPackets"],
                "finPackets": direction["finPackets"],
                "rstPackets": direction["rstPackets"],
                "firstPayloadUtc": iso_utc(direction["firstPayload"])
                if direction["firstPayload"] is not None
                else None,
                "lastPayloadUtc": iso_utc(direction["lastPayload"])
                if direction["lastPayload"] is not None
                else None,
                "maxPayloadGapSec": round(max(gaps), 3) if gaps else 0.0,
            }
        direct = list(connection["directions"].values())
        syn_seen = protocol == "tcp" and any(
            direction["synPackets"] for direction in direct
        )
        rst_seen = protocol == "tcp" and any(
            direction["rstPackets"] for direction in direct
        )
        both_fin = protocol == "tcp" and len(direct) == 2 and all(
            direction["finPackets"] for direction in direct
        )
        gaps = connection["gaps"]
        output_connections.append(
            {
                "endpointA": endpoint_text(left),
                "endpointB": endpoint_text(right),
                "protocol": protocol,
                "firstSeenUtc": iso_utc(connection["first"]),
                "lastSeenUtc": iso_utc(connection["last"]),
                "durationSec": round(connection["last"] - connection["first"], 3),
                "packets": connection["packets"],
                "tcpPayloadBytes": sum(d["tcpPayloadBytes"] for d in direct),
                "udpPayloadBytes": sum(d["udpPayloadBytes"] for d in direct),
                "capturedTcpPayloadBytes": sum(
                    d["capturedTcpPayloadBytes"] for d in direct
                ),
                "capturedUdpPayloadBytes": sum(
                    d["capturedUdpPayloadBytes"] for d in direct
                ),
                "maxPacketGapSec": round(max(gaps), 3) if gaps else 0.0,
                "p50PacketGapSec": round(percentile(gaps, 0.50), 3),
                "p95PacketGapSec": round(percentile(gaps, 0.95), 3),
                "synObserved": syn_seen,
                "closureObserved": rst_seen or both_fin,
                "possibleStartBeforeCapture": not syn_seen,
                "possibleContinuationAfterCapture": not (rst_seen or both_fin),
                "atCaptureStart": capture_first is not None
                and connection["first"] - capture_first <= 1.0,
                "atCaptureEnd": capture_last is not None
                and capture_last - connection["last"] <= 1.0,
                "directions": directions,
            }
        )
    output_connections.sort(
        key=lambda item: item["tcpPayloadBytes"] + item["udpPayloadBytes"],
        reverse=True,
    )
    tcp_count = sum(item["protocol"] == "tcp" for item in output_connections)
    udp_count = sum(item["protocol"] == "udp" for item in output_connections)
    return {
        "pcap": str(path),
        "captureFirstPacketUtc": iso_utc(capture_first) if capture_first is not None else None,
        "captureLastPacketUtc": iso_utc(capture_last) if capture_last is not None else None,
        "pcapRecords": records,
        "skippedUnparsedPackets": skipped_unparsed_packets,
        "connectionTupleCount": len(output_connections),
        "tcpConnectionTupleCount": tcp_count,
        "udpFlowCount": udp_count,
        "note": "Bidirectional TCP/UDP endpoint tuples, not HTTP requests. Payload sizes come from protocol length fields and may include retransmissions. Missing SYN/FIN/RST does not prove an application request remained open; UDP has no transport close marker.",
        "connections": output_connections,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("pcap", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = summarize(args.pcap)
    rendered = json.dumps(result, ensure_ascii=True, separators=(",", ":"))
    if args.output:
        args.output.write_text(rendered + "\n", encoding="utf-8")
    else:
        print(rendered)


if __name__ == "__main__":
    main()
