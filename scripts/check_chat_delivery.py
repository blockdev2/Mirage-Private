#!/usr/bin/env python3
"""Manual two-client chat regression against a trusted loopback TS6 server.

Uses isolated ignored identities, no microphone, and no administrative interface.
Requires a running server on 127.0.0.1:9987. Never prints raw peer output.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--port", type=int, default=9987)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error("port must be between 1 and 65535")
    address = f"127.0.0.1:{args.port}"
    private = ROOT / ".audit-local/chat-delivery"
    private.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["ALLOW_PRIVATE_TARGETS"] = "1"
    lines = queue.Queue()
    children = []

    def launch(role, command):
        process = subprocess.Popen(
            [str(args.binary.resolve()), *command, "--name", "chat-" + role,
             "--identity", str(private / (role + ".json"))],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
        )
        children.append(process)
        return process

    def wait_for(pattern, seconds=35):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                line = lines.get(timeout=max(.01, deadline - time.monotonic()))
            except queue.Empty:
                break
            match = re.search(pattern, line)
            if match:
                return match
        raise RuntimeError("expected receiver event did not arrive; raw output withheld")

    try:
        receiver = launch("receiver", ["probe", address, "120"])

        def read():
            for line in iter(receiver.stdout.readline, b""):
                lines.put(line.decode("utf-8", errors="replace"))

        threading.Thread(target=read, daemon=True).start()
        receiver_id = wait_for(r'"own_client_id": (\d+)').group(1)
        version = wait_for(r'"version": "(6\.[0-9A-Za-z .:\[\]-]+)"').group(1)
        wait_for(r"--- events for")
        results = {"server_version": version}
        for label, target in [("channel", "channel"), ("direct", "@" + receiver_id)]:
            fixture = "fixture-" + label + "-" + str(time.monotonic_ns()) + " | / \\"
            sender = launch("sender-" + label,
                            ["chat", address, target, fixture])
            output, _ = sender.communicate(timeout=35)
            if sender.returncode != 0:
                raise RuntimeError("sender failed; raw output withheld")
            wait_for(r"\[chat\] chat-sender-" + label + r" \(\d+\): " + re.escape(fixture), 8)
            results[label + "_received"] = True
            results[label + "_sender_echo"] = b"delivered (echo received)" in output
        print(json.dumps(results))
    finally:
        for child in children:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
            if child.stdout:
                child.stdout.close()


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.TimeoutExpired):
        print("FAIL: chat integration did not complete; private process output withheld")
        raise SystemExit(1) from None
