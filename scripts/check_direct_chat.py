#!/usr/bin/env python3
"""Private and server-wide chat through the bridge, against a loopback server.

Needs a TS6 server you started yourself (a fresh one avoids the same-identity
cooldown). Two bridges connect; it checks that a private message reaches only
its recipient and names the sender as its peer, that a server-wide message
reaches the other client (a default guest may not send one: pass `--token-file`
with the server's first privilege key, never printed, to test it as an
administrator), that a recipient who is not on the server is refused
with `stale_client`, and that a channel message still carries its channel.

    python scripts/check_direct_chat.py --binary <ts6-bridge.exe> --server-port 29987 [--token-file key.txt]
"""
import argparse
from pathlib import Path
import sys
import time

from check_channel_chat import ConnectionFailure, Peer


def connect(peer, name, port):
    # A server keeps the previous session of an identity for ~30 s: retry.
    for attempt in range(6):
        peer.send({"op": "connect", "address": f"127.0.0.1:{port}", "nickname": name})
        try:
            return peer.until(lambda e: e.get("ev") == "state")["snapshot"]
        except ConnectionFailure as failure:
            if attempt == 5 or "handshake" not in str(failure):
                raise
            time.sleep(10)


def say(peer, request_id, text, **extra):
    peer.send({"op": "chat", "session_id": peer.session_id, "text": text, "request_id": request_id, **extra})
    return peer.until(lambda e: e.get("ev") == "chat_result" and e["request_id"] == request_id)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--server-port", type=int, required=True)
    parser.add_argument("--token-file", type=Path)
    args = parser.parse_args()
    children = []
    try:
        a, b = Peer(args.binary, "dm-a", children), Peer(args.binary, "dm-b", children)
        snap_a = connect(a, "Dm Alice", args.server_port)
        snap_b = connect(b, "Dm Bob", args.server_port)
        id_a = snap_a["own_client_id"]
        id_b = snap_b["own_client_id"]
        channel = str(next(c["channel_id"] for c in snap_a["clients"] if c["is_self"]))
        # Alice must see Bob before she can write to him.
        a.until(lambda e: e.get("ev") == "state" and any(c["id"] == id_b for c in e["snapshot"]["clients"]))

        result = say(a, "dm-1", "oi em particular", scope="private", client=id_b)
        assert result["error"] is None, result
        got = b.until(lambda e: e.get("ev") == "chat" and e["text"] == "oi em particular")
        assert got["scope"] == "private" and got["peer_id"] == id_a and got["from_id"] == id_a, got
        assert got["channel"] == "" and got["from_self"] is False, got
        print("PASS: a private message reached its recipient, naming the sender as the peer")

        result = say(b, "dm-2", "resposta", scope="private", client=id_a)
        assert result["error"] is None, result
        reply = a.until(lambda e: e.get("ev") == "chat" and e["text"] == "resposta")
        assert reply["scope"] == "private" and reply["peer_id"] == id_b, reply
        print("PASS: the reply came back the other way")

        if args.token_file:
            a.send({"op": "admin", "request_id": "key", "action": "use_token",
                    "token": args.token_file.read_text(encoding="utf-8").strip()})
            assert a.until(lambda e: e.get("ev") == "admin_result" and e["request_id"] == "key")["ok"]
        result = say(a, "dm-3", "para todos", scope="server")
        if result["error"] is None:
            everyone = b.until(lambda e: e.get("ev") == "chat" and e["text"] == "para todos")
            assert everyone["scope"] == "server" and everyone["channel"] == "", everyone
            print("PASS: a server-wide message reached the other client")
        else:
            assert not args.token_file, result
            print("INFO: the server refused a guest's server-wide message:", result["code"])

        gone = say(a, "dm-4", "ninguém", scope="private", client=65000)
        assert gone["code"] == "stale_client", gone
        itself = say(a, "dm-5", "eu mesmo", scope="private", client=id_a)
        assert itself["code"] == "stale_client", itself
        print("PASS: a missing recipient, or yourself, is refused locally:", gone["error"])

        result = say(a, "dm-6", "no canal", channel=channel)
        assert result["error"] is None, result
        room = b.until(lambda e: e.get("ev") == "chat" and e["text"] == "no canal")
        assert room["scope"] == "channel" and room["channel"] == channel, room
        print("PASS: a channel message still carries its channel")

        def bob_as_seen_by_alice(event):
            if event.get("ev") != "state":
                return None
            return next((c for c in event["snapshot"]["clients"] if c["id"] == id_b), None)

        b.send({"op": "away", "away": True, "message": "volto logo"})
        seen = a.until(lambda e: (c := bob_as_seen_by_alice(e)) is not None and c["away"])
        away_bob = bob_as_seen_by_alice(seen)
        assert away_bob["away_message"] == "volto logo", away_bob
        # Going away again without text would keep the old text: come back first.
        b.send({"op": "away", "away": False})
        a.until(lambda e: (c := bob_as_seen_by_alice(e)) is not None and not c["away"])
        b.send({"op": "away", "away": True})
        a.until(lambda e: (c := bob_as_seen_by_alice(e)) is not None and c["away"] and c["away_message"] is None)
        print("PASS: an away message reaches the others; away without text has none")
        return 0
    finally:
        for child in children:
            child.close()


if __name__ == "__main__":
    sys.exit(main())
