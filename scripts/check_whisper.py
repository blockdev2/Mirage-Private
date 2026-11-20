#!/usr/bin/env python3
"""Whispering through the bridge, against a throwaway loopback server.

Three bridges join one channel: a sender transmitting a synthetic tone, a
target and a bystander. It checks that the tone reaches both while it goes to
the channel, only the target while the sender whispers to it, and the
bystander again once the whisper is off. The tone plays through this
computer's speakers for the length of the run (about 25 seconds).

    python scripts/check_whisper.py --binary <ts6-bridge.exe> --server-port 29987
"""
import argparse
from pathlib import Path
import sys
import time

from check_channel_chat import Peer
from check_direct_chat import connect

WINDOW = 4  # seconds of listening per phase


def pump(peer, seconds):
    """Reads events for `seconds` so the audio counters are current."""
    end = time.monotonic() + seconds
    peer.until(lambda event: time.monotonic() >= end)


def rx_packets(peer):
    return next(e["rx_packets"] for e in reversed(peer.history) if e.get("ev") == "audio")


def received_during(listeners, seconds):
    """How many voice packets each listener got in the next `seconds`."""
    before = [rx_packets(p) for p in listeners]
    for peer in listeners:
        pump(peer, seconds)
    return [rx_packets(p) - b for p, b in zip(listeners, before)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--server-port", type=int, required=True)
    args = parser.parse_args()
    children = []
    try:
        sender, target, bystander = (Peer(args.binary, f"wh-{n}", children) for n in ("sender", "target", "bystander"))
        snap_s = connect(sender, "Wh Sender", args.server_port)
        snap_t = connect(target, "Wh Target", args.server_port)
        snap_b = connect(bystander, "Wh Bystander", args.server_port)
        id_t, id_b = snap_t["own_client_id"], snap_b["own_client_id"]
        sender.until(lambda e: e.get("ev") == "state" and {id_t, id_b} <= {c["id"] for c in e["snapshot"]["clients"]})

        for peer in (target, bystander):
            peer.send({"op": "audio", "enabled": True, "tone": False})
        sender.send({"op": "audio", "enabled": True, "tone": True, "voice_activation": True, "vad_threshold": -80})
        for peer in (sender, target, bystander):
            pump(peer, 2)

        got = received_during([target, bystander], WINDOW)
        assert all(n > 20 for n in got), f"the channel did not receive the tone: {got}"
        print("PASS: the tone reached the target and the bystander through the channel:", got)

        sender.send({"op": "whisper", "clients": [id_t]})
        pump(sender, 1)
        got = received_during([target, bystander], WINDOW)
        assert got[0] > 20 and got[1] <= 3, f"whisper not limited to the target: {got}"
        print("PASS: while whispering only the target hears it:", got)

        sender.send({"op": "whisper", "clients": [], "channels": []})
        pump(sender, 1)
        got = received_during([target, bystander], WINDOW)
        assert all(n > 20 for n in got), f"the channel did not hear it again: {got}"
        print("PASS: with the whisper off the channel hears it again:", got)
        return 0
    finally:
        for child in children:
            child.close()


if __name__ == "__main__":
    sys.exit(main())
