#!/usr/bin/env python3
"""Manual channel-chat bridge regression against an existing trusted loopback server.

Starts two isolated bridges, never enables audio, and stops only its own children.
Requires built browser assets. Profiles remain private under ignored .audit-local.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import time

from smoke_bridge import handshake, frame, send

ROOT = Path(__file__).resolve().parent.parent


class ConnectionFailure(RuntimeError):
    """A sanitized diagnostic category, never the server-controlled error text."""

    def __init__(self, error):
        text = str(error).lower()
        if "handshake" in text and "connection failed, you are banned" in text:
            category = "handshake_banned"
        elif "handshake" in text:
            category = "handshake_failed"
        else:
            category = "connection_failed"
        super().__init__(category)


class Peer:
    def __init__(self, binary, name, children):
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            self.port = reservation.getsockname()[1]
        profile = ROOT / ".audit-local/channel-chat" / name
        profile.mkdir(parents=True, exist_ok=True)
        env = os.environ.copy()
        env["APPDATA"] = str(profile)
        env["ALLOW_PRIVATE_TARGETS"] = "1"
        env.pop("TS6_BRIDGE_PORT", None)
        env.pop("TS6_UI_DIR", None)
        self.process = subprocess.Popen(
            [str(binary.resolve()), "--port", str(self.port), "--ui", str(ROOT / "client/apps/ui/dist")],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
        )
        self.connection = self.stream = None
        children.append(self)
        deadline = time.monotonic() + 10
        while True:
            try:
                self.connection, self.stream, status = handshake(
                    self.port, f"127.0.0.1:{self.port}", f"http://127.0.0.1:{self.port}")
                assert status == 101
                break
            except OSError:
                if time.monotonic() >= deadline or self.process.poll() is not None:
                    raise RuntimeError("bridge startup failed") from None
                time.sleep(.05)
        self.history = []
        self.until(lambda event: event.get("ev") == "hello" and event.get("channel_chat") is True)

    @property
    def session_id(self):
        return next(event["session_id"] for event in reversed(self.history)
                    if event.get("ev") == "status" and event.get("session_id"))

    def send(self, command):
        send(self.connection, json.dumps(command))

    def until(self, predicate):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            self.connection.settimeout(max(.01, deadline - time.monotonic()))
            opcode, payload = frame(self.stream)
            if opcode != 1:
                raise RuntimeError("unexpected bridge frame")
            event = json.loads(payload)
            self.history.append(event)
            if (event.get("ev") == "status" and event.get("connected") is False
                    and event.get("error")):
                raise ConnectionFailure(event["error"])
            if predicate(event):
                return event
        raise RuntimeError("expected bridge event deadline exceeded")

    def close(self):
        if self.stream:
            self.stream.close()
        if self.connection:
            self.connection.close()
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--server-port", type=int, default=9987)
    args = parser.parse_args()
    if not 1 <= args.server_port <= 65535:
        parser.error("invalid server port")
    children = []
    try:
        a, b = [Peer(args.binary, name, children) for name in ("a", "b")]
        channels = []
        for peer, name in [(a, "wire-chat-a"), (b, "wire-chat-b")]:
            peer.send({"op": "connect", "address": f"127.0.0.1:{args.server_port}", "nickname": name})
            snapshot = peer.until(lambda event: event.get("ev") == "state")["snapshot"]
            assert snapshot["server"]["version"].startswith("6.")
            channels.append(str(next(client["channel_id"] for client in snapshot["clients"] if client["is_self"])))
        assert channels[0] == channels[1]
        for index, sender, receiver in [(1, a, b), (2, b, a)]:
            text = f"channel-wire-{index} <script>fixture</script> | / \\ " + chr(0x1F600)
            request_id = f"fixture-{index}"
            sender.send({"op": "chat", "session_id": sender.session_id, "channel": channels[0], "text": text, "request_id": request_id})
            result = sender.until(lambda event: event.get("ev") == "chat_result" and event["request_id"] == request_id)
            assert result["error"] is None
            if not any(event.get("ev") == "chat" and event["text"] == text for event in sender.history):
                sender.until(lambda event: event.get("ev") == "chat" and event["text"] == text)
            received = receiver.until(lambda event: event.get("ev") == "chat" and event["text"] == text)
            assert received["channel"] == channels[0] and not received["from_self"]
            assert sum(event.get("ev") == "chat" and event["text"] == text for event in sender.history) == 1
        for request_id, channel, text in [("stale", "invalid", "fixture"), ("empty", channels[0], " "), ("large", channels[0], "x" * 1025)]:
            a.send({"op": "chat", "session_id": a.session_id, "channel": channel, "text": text, "request_id": request_id})
            assert a.until(lambda event: event.get("ev") == "chat_result" and event["request_id"] == request_id)["error"]
        a.send({"op": "chat", "session_id": "stale-session", "channel": channels[0], "text": "fixture", "request_id": "stale-session"})
        assert a.until(lambda event: event.get("ev") == "chat_result" and event["request_id"] == "stale-session")["error"]
        old_session = a.session_id
        a.send({"op": "disconnect"})
        a.until(lambda event: event.get("ev") == "status" and not event["connected"])
        a.send({"op": "chat", "session_id": a.session_id, "channel": channels[0], "text": "fixture", "request_id": "offline"})
        assert a.until(lambda event: event.get("ev") == "chat_result" and event["request_id"] == "offline")["error"]
        a.send({"op": "connect", "address": f"127.0.0.1:{args.server_port}", "nickname": "wire-chat-a"})
        a.until(lambda event: event.get("ev") == "state")
        assert a.session_id != old_session
        a.send({"op": "chat", "session_id": old_session, "channel": channels[0], "text": "fixture", "request_id": "previous-session"})
        assert a.until(lambda event: event.get("ev") == "chat_result" and event["request_id"] == "previous-session")["error"]
        print(json.dumps({"channel_roundtrip": True, "sender_echo_once": True, "unicode_and_escaping": True,
                          "stale_empty_large_offline_rejected": True, "previous_session_rejected": True, "audio_enabled": False}))
    finally:
        for child in children:
            child.close()


if __name__ == "__main__":
    try:
        main()
    except ConnectionFailure as error:
        print(f"FAIL: channel-chat connection: {error}; private output withheld")
        raise SystemExit(1) from None
    except (OSError, RuntimeError, AssertionError, subprocess.TimeoutExpired):
        print("FAIL: channel-chat verification failed; private output withheld")
        raise SystemExit(1) from None
