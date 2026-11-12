"""Manual Windows integration check; captures only the probe's owned test window.

No desktop, microphone, network or user-window input is supported. Stops only
the newly launched probe, enforcing a thirty-second process deadline. No raw
frame or native diagnostic text is printed.
"""
import argparse
import json
from pathlib import Path
import subprocess
import os


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this interactive native check requires Windows")
    try:
        result = subprocess.run([str(args.binary.resolve())], stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                timeout=30, creationflags=subprocess.CREATE_NO_WINDOW)
    except subprocess.TimeoutExpired:
        print('{"passed":false,"category":"process_deadline_exceeded"}')
        return 1
    summary = None
    for line in result.stdout.decode("utf-8", errors="replace").splitlines():
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict) and "captured_frames" in value:
            summary = value
    passed = (result.returncode == 0 and summary is not None
              and summary.get("captured_frames", 0) >= 40
              and summary.get("owned_texture_allocations", 0) >= 2
              and summary.get("fixture_pixel_matches") is True
              and summary.get("resize_observed") is True
              and summary.get("validation_readbacks") == 1
              and summary.get("user_windows_captured") is False
              and summary.get("network_used") is False)
    print(json.dumps({"passed": passed, "exit_code": result.returncode, "summary": summary}))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
