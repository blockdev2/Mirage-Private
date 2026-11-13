#!/usr/bin/env python3
"""Manual screen-share regression against an existing trusted loopback TS6 server.

Starts two isolated bridges (never enables audio, stops only its own children):
one shares its primary monitor (`share_start`), the other watches it
(`watch_stream`). Passes when the sharer reports `live` and the viewer reaches
`playing`, so setupstream, the join request and the WebRTC signaling all went
through the real server. Both ends are this project's own code: it does not
show that the official TeamSpeak client can watch. Needs a machine with a
hardware H.264 encoder and a monitor. Profiles stay under ignored .audit-local.

    python scripts/check_share_session.py --binary <ts6-bridge.exe> --server-port 9990
"""
import argparse
import json
from pathlib import Path
import sys
import time

from check_channel_chat import Peer


def wait_for(peer, event, wanted, seconds=25):
    """True when an `event` whose state is in `wanted` shows up; prints each state."""
    deadline = time.monotonic() + seconds
    seen = 0
    while time.monotonic() < deadline:
        for item in peer.history[seen:]:
            if item.get("ev") == event:
                print(event, item.get("state"), item.get("reason"))
                if item.get("state") in wanted:
                    return True
                if item.get("state") == "failed":
                    return False
        seen = len(peer.history)
        try:
            peer.until(lambda e: e.get("ev") == event)
        except Exception:
            return False
    return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--server-port", type=int, default=9987)
    args = parser.parse_args()
    address = f"127.0.0.1:{args.server_port}"
    children = []
    try:
        sharer = Peer(args.binary, "share-a", children)
        sharer.send({"op": "connect", "address": address, "nickname": "Sharer"})
        sharer.until(lambda e: e.get("ev") == "state")
        sharer.send({"op": "share_start"})
        if not wait_for(sharer, "share_state", {"live"}):
            print("FAIL: the share never went live")
            return 1
        viewer = Peer(args.binary, "share-b", children)
        viewer.send({"op": "connect", "address": address, "nickname": "Viewer"})
        viewer.until(lambda e: e.get("ev") == "state")
        snapshot = next(e for e in reversed(viewer.history) if e.get("ev") == "state")["snapshot"]
        target = next(c for c in snapshot["clients"] if not c["is_self"])
        viewer.send({"op": "watch_stream", "client": target["id"]})
        if not wait_for(viewer, "stream_state", {"playing"}):
            print("FAIL: the viewer never reached playing")
            return 1
        sharer.send({"op": "share_stop"})
        print("PASS: shared, joined and playing through the server")
        return 0
    finally:
        for child in children:
            child.process.kill()


if __name__ == "__main__":
    sys.exit(main())
