#!/usr/bin/env python3
"""Live chat attachment and emoji regression through two isolated bridges.

Peer A sends emoji shortcodes, a PNG, an animated GIF and a plain file to a
test channel; peer B must see each attachment in its chat event, fetch it over
the server's file transfer and get byte-identical content back. Negative cases
(oversized, missing, relative path) must be refused before any upload.

Default TeamSpeak servers give the guest group download but no upload power;
uploads then fail with `upload_permission` and this check exits 2 (unverified).
Use only a server you are authorized to test on, in a dedicated test channel.
Audio is never enabled. Profiles and fixtures stay under ignored .audit-local.
"""
import argparse
import json
from pathlib import Path
import struct
import time
import zlib

from check_channel_chat import ROOT, Peer

FIXTURES = ROOT / ".audit-local/chat-attachments"


def png_1x1(rgb):
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
    raw = b"\x00" + bytes(rgb)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def gif_two_frames():
    """A 1x1 GIF89a with two frames (green, then blue) that loops forever."""
    header = b"GIF89a" + struct.pack("<HH", 1, 1) + bytes([0xF1, 0, 0])
    palette = bytes([0, 180, 80, 0, 0, 255, 0, 0, 0, 255, 255, 255])
    loop = b"\x21\xFF\x0BNETSCAPE2.0\x03\x01\x00\x00\x00"
    frames = b""
    for index in (0, 1):
        frames += b"\x21\xF9\x04\x00\x0A\x00\x00\x00"  # 100 ms delay
        frames += b"\x2C" + struct.pack("<HHHH", 0, 0, 1, 1) + b"\x00"
        frames += b"\x02\x02" + (b"\x44\x01" if index == 0 else b"\x4C\x01") + b"\x00"
    return header + palette + loop + frames + b"\x3B"


KINDS = ("image", "gif", "file")
REFUSALS = ("refused_oversized", "refused_missing", "refused_relative", "refused_stale_session")


def verdict(results):
    """0 when everything passed, 2 when the server denies uploads (unverified), else 1."""
    refusals = all(results.get(key) for key in REFUSALS)
    if (results.get("emoji_received") and refusals
            and all(results.get(kind, {}).get("bytes_identical") for kind in KINDS)):
        return 0
    if refusals and all(results.get(kind, {}).get("send_error_code") == "upload_permission"
                        for kind in KINDS):
        # Default servers give guests download but no upload power.
        print("UNVERIFIED: the server denies uploads to this identity; the success path "
              "needs i_ft_file_upload_power on a test server")
        return 2
    return 1


def wait_result(peer, request_id, timeout=40):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        event = peer.until(lambda e: e.get("ev") == "chat_result")
        if event["request_id"] == request_id:
            return event
    raise RuntimeError("no chat_result")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="ts6-bridge executable")
    parser.add_argument("--server", required=True, help="host:port of an authorized test server")
    parser.add_argument("--channel", help="dedicated test channel (default: the server's default channel)")
    args = parser.parse_args()
    FIXTURES.mkdir(parents=True, exist_ok=True)
    stamp = str(int(time.time()))
    files = {
        "image": (FIXTURES / f"mirage-photo-{stamp}.png", png_1x1((0, 180, 80))),
        "gif": (FIXTURES / f"mirage-anim-{stamp}.gif", gif_two_frames()),
        "file": (FIXTURES / f"relatório ação {stamp}.txt", "Mirage attachment test ✓\n".encode()),
    }
    for path, data in files.values():
        path.write_bytes(data)
    big = FIXTURES / f"too-big-{stamp}.bin"
    big.write_bytes(b"\0" * (5_000_001))

    children = []
    results = {}
    try:
        a = Peer(args.binary, "attach-a", children)
        b = Peer(args.binary, "attach-b", children)
        for peer, nick in ((a, "Mirage-attach-A"), (b, "Mirage-attach-B")):
            command = {"op": "connect", "address": args.server, "nickname": nick}
            if args.channel:
                command["channel"] = args.channel
            peer.send(command)
            snapshot = peer.until(lambda e: e.get("ev") == "state")["snapshot"]
            own = next(c for c in snapshot["clients"] if c["is_self"])
            name = next(c["name"] for c in snapshot["channels"] if c["id"] == own["channel_id"])
            if args.channel and name != args.channel:
                raise RuntimeError("not in the test channel")
            peer.channel = str(own["channel_id"])

        # Emoji: shortcodes expand on send; Unicode survives the round trip.
        text = f"Mirage emoji test {stamp} :fire: :thumbsup: 😄 ação"
        a.send({"op": "chat", "session_id": a.session_id, "channel": a.channel,
                "text": text, "request_id": "emoji"})
        results["emoji_send_accepted"] = wait_result(a, "emoji")["error"] is None
        got = b.until(lambda e: e.get("ev") == "chat" and stamp in e.get("text", "")
                      and "emoji" in e.get("text", ""))
        results["emoji_received"] = got["text"] == f"Mirage emoji test {stamp} 🔥 👍 😄 ação"

        for kind, (path, data) in files.items():
            request = f"attach-{kind}"
            a.send({"op": "chat_attach", "session_id": a.session_id, "channel": a.channel,
                    "path": str(path.resolve()), "request_id": request})
            result = wait_result(a, request)
            entry = {"send_error_code": result.get("code")}
            if result["error"] is None:
                wanted = "picture" if kind in ("image", "gif") else "file"
                event = b.until(lambda e: e.get("ev") == "chat" and any(
                    att.get("kind") == wanted and stamp in att.get("name", "")
                    for att in e.get("attachments", [])))
                attachment = next(att for att in event["attachments"]
                                  if att.get("kind") == wanted and stamp in att.get("name", ""))
                entry["receiver_saw_attachment"] = True
                entry["message_text"] = event["text"][:160]
                b.send({"op": "chat_fetch", "channel": attachment["channel"],
                        "path": attachment["path"]})
                fetched = b.until(lambda e: e.get("ev") == "chat_file"
                                  and e.get("path") == attachment["path"])
                entry["fetched"] = fetched["file"] is not None
                entry["kind"] = fetched.get("kind")
                if fetched["file"]:
                    entry["bytes_identical"] = Path(fetched["file"]).read_bytes() == data
                    # A second fetch must come from the cache immediately.
                    b.send({"op": "chat_fetch", "channel": attachment["channel"],
                            "path": attachment["path"]})
                    again = b.until(lambda e: e.get("ev") == "chat_file"
                                    and e.get("path") == attachment["path"])
                    entry["cached_second_fetch"] = again["file"] == fetched["file"]
            results[kind] = entry

        for label, path in (("oversized", big), ("missing", FIXTURES / "nope.png"),
                            ("relative", Path("relative.png"))):
            a.send({"op": "chat_attach", "session_id": a.session_id, "channel": a.channel,
                    "path": str(path.resolve()) if label != "relative" else "relative.png",
                    "request_id": label})
            results[f"refused_{label}"] = wait_result(a, label).get("code") == "invalid"
        a.send({"op": "chat_attach", "session_id": "stale", "channel": a.channel,
                "path": str(files["image"][0].resolve()), "request_id": "stale"})
        results["refused_stale_session"] = wait_result(a, "stale").get("code") == "stale_session"
    finally:
        for peer in children:
            peer.close()
        big.unlink(missing_ok=True)
    print(json.dumps(results, ensure_ascii=False, indent=1))
    raise SystemExit(verdict(results))

if __name__ == "__main__":
    main()
