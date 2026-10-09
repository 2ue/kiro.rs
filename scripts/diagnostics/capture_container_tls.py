#!/usr/bin/env python3
"""Bounded capture of selected IPv4/IPv6 TCP and UDP traffic in a Docker netns.

The capture preserves complete frames up to snaplen. This includes encrypted
TLS/QUIC payloads, so later offline protocol analysis has the packet bytes it
needs while the capture remains bounded by duration and artifact size.
"""

from __future__ import annotations

import argparse
import ipaddress
import os
import signal
import socket
import struct
import sys
import time
from pathlib import Path

from packet_protocols import ethernet_transport


PCAP_GLOBAL = struct.Struct("<IHHIIII")
PCAP_PACKET = struct.Struct("<IIII")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--interface", required=True)
    parser.add_argument(
        "--source-ip",
        required=True,
        help="IP address to retain; use 0.0.0.0 or :: to disable the local-address filter.",
    )
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--duration", type=float, default=900.0)
    parser.add_argument(
        "--port",
        action="append",
        type=int,
        dest="ports",
        default=None,
        help="Retain TCP/UDP packets involving this port; repeat for multiple ports (default: 443).",
    )
    parser.add_argument(
        "--snaplen",
        type=int,
        default=65535,
        help="Maximum captured bytes per packet; 65535 retains complete normal Ethernet frames.",
    )
    parser.add_argument(
        "--destination-ip",
        action="append",
        default=[],
        help="Only retain packets exchanged with one of these IP addresses.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if not 64 <= args.snaplen <= 262144:
        raise SystemExit("--snaplen must be between 64 and 262144")
    source_filter = ipaddress.ip_address(args.source_ip)
    port_filters = args.ports or [443]
    if any(not (1 <= port <= 65535) for port in port_filters):
        raise SystemExit("--port must be between 1 and 65535")
    destination_ips = {str(ipaddress.ip_address(value)) for value in args.destination_ip}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    captured = 0
    bytes_written = 0
    started = time.time()
    deadline = started + max(0.1, args.duration)
    stop_requested = False

    def request_stop(_signum: int, _frame: object) -> None:
        nonlocal stop_requested
        stop_requested = True

    signal.signal(signal.SIGINT, request_stop)
    signal.signal(signal.SIGTERM, request_stop)
    try:
        # Keep the file observable while the bounded capture is still running.
        with os.fdopen(fd, "wb", buffering=0) as output:
            output.write(PCAP_GLOBAL.pack(0xA1B2C3D4, 2, 4, 0, 0, args.snaplen, 1))
            packet_socket = socket.socket(
                socket.AF_PACKET, socket.SOCK_RAW, socket.ntohs(0x0003)
            )
            packet_socket.bind((args.interface, 0))
            packet_socket.settimeout(1.0)
            try:
                while not stop_requested and time.time() < deadline:
                    try:
                        packet, _ = packet_socket.recvfrom(262144)
                    except TimeoutError:
                        continue
                    transport = ethernet_transport(packet)
                    if transport is None:
                        continue
                    source = transport.source
                    destination = transport.destination
                    if destination_ips:
                        local_is_unspecified = source_filter.is_unspecified
                        local_to_kiro = (
                            (local_is_unspecified or source == args.source_ip)
                            and destination in destination_ips
                        )
                        kiro_to_local = (
                            (local_is_unspecified or destination == args.source_ip)
                            and source in destination_ips
                        )
                        if not (local_to_kiro or kiro_to_local):
                            continue
                    elif source_filter.is_unspecified:
                        pass
                    elif source != args.source_ip and destination != args.source_ip:
                        continue
                    if not any(
                        port
                        in (transport.source_port, transport.destination_port)
                        for port in port_filters
                    ):
                        continue
                    now = time.time()
                    seconds = int(now)
                    micros = int((now - seconds) * 1_000_000)
                    captured_packet = packet[: args.snaplen]
                    output.write(
                        PCAP_PACKET.pack(
                            seconds,
                            micros,
                            len(captured_packet),
                            len(packet),
                        )
                    )
                    output.write(captured_packet)
                    captured += 1
                    bytes_written += len(captured_packet)
            finally:
                packet_socket.close()
    except PermissionError as exc:
        print(f"capture requires CAP_NET_RAW/root: {exc}", file=sys.stderr)
        return 2
    elapsed = time.time() - started
    print(
        f"captured_packets={captured} captured_bytes={bytes_written} "
        f"elapsed_secs={elapsed:.1f} stop_requested={stop_requested} output={args.output}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
