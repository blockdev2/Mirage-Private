#!/usr/bin/env python3
"""Checks that the bridge's reported ping matches the real round trip.

Run it against a server on this machine (loopback, real round trip well under
1 ms). Before the session loop woke every 10 ms the bridge reported 6-90 ms
here, because a ping reply sat in the socket until the next wake-up. The check
fails when the median reported ping exceeds --max-median-ms.

Use only a server you control. Audio stays off; profiles live under .audit-local.
"""
import argparse
import statistics
import sys
import time
from pathlib import Path

from check_channel_chat import Peer


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="ts6-bridge executable")
    parser.add_argument("--server", required=True, help="host:port of a loopback test server")
    parser.add_argument("--seconds", type=int, default=20)
    parser.add_argument("--max-median-ms", type=float, default=5.0)
    args = parser.parse_args()
    children = []
    try:
        peer = Peer(args.binary, "ping-latency", children)
        peer.send({"op": "connect", "address": args.server, "nickname": "Mirage-ping"})
        peer.until(lambda event: event.get("ev") == "state")
        samples = []
        end = time.monotonic() + args.seconds
        while time.monotonic() < end:
            event = peer.until(lambda e: e.get("ev") == "audio" and e.get("ping_ms") is not None)
            samples.append(event["ping_ms"])
    finally:
        for child in children:
            child.close()
    median = statistics.median(samples) if samples else float("inf")
    print(f"samples={len(samples)} min={min(samples, default=0):.1f} "
          f"median={median:.1f} max={max(samples, default=0):.1f} ms")
    if median > args.max_median_ms:
        print(f"FAIL: median ping {median:.1f} ms exceeds {args.max_median_ms} ms on a loopback server")
        sys.exit(1)
    print("PASS: reported ping matches the loopback round trip")


if __name__ == "__main__":
    main()
