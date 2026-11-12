"""Live loopback RTP check for the native probe: two processes, real UDP.

Starts ``--rtp-recv`` (waits for its ready line), then ``--rtp-send`` with a
moving synthetic pattern paced at 20 FPS, for each requested codec. Validates
what the *receiver* observed. Loopback only; no FFmpeg, no user windows.
"""

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile

NO_WINDOW = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0


def free_udp_port():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def validate(receiver, sender, frames):
    """Return a list of failed conditions (empty means pass)."""
    failures = []
    checks = {
        "all units received": receiver["units_received"] == frames,
        "no packet loss": receiver["lost_packets"] == 0 and receiver["dropped_units"] == 0,
        "no malformed or foreign packets": receiver["malformed_packets"] == 0
        and receiver["foreign_ssrc"] == 0,
        "starts with keyframe": receiver["first_unit_keyframe"],
        "dimensions 640x360": receiver["bitstream_dims"] == [640, 360],
        "sender and receiver agree": sender["units_sent"] == receiver["units_received"]
        and sender["packets_sent"] == receiver["packets"],
        "real-time pacing": 35 <= receiver["interarrival_ms"]["p50"] <= 65,
    }
    if sender.get("gop_accepted_by_encoder"):
        checks["periodic keyframes"] = receiver["keyframes"] >= frames // 20
    decode = receiver["os_decode"]
    if decode["status"] == "decoded":
        checks["decoded every unit"] = decode["frames"] == frames
        checks["fixture colour"] = decode["fixture_matches"]
        checks["motion visible"] = decode["distinct_motion_bands"] >= frames // 2
    elif decode["status"] != "no_os_decoder":
        checks["decode did not fail"] = False
    failures = [name for name, ok in checks.items() if not ok]
    return failures


def validate_loss(receiver, frames):
    """After one dropped packet the receiver must discard the damaged unit,
    withhold frames until the next keyframe, and decode cleanly again."""
    decode = receiver["os_decode"]
    checks = {
        "exactly one packet lost": receiver["lost_packets"] == 1,
        "damaged unit discarded": receiver["dropped_units"] >= 1,
        "frames withheld until keyframe": receiver["withheld_awaiting_keyframe"] >= 1,
        "accounting adds up": receiver["units_received"] + receiver["dropped_units"]
        + receiver["withheld_awaiting_keyframe"] == frames,
        "resumed on a keyframe": receiver["keyframes"] >= 2,
    }
    if decode["status"] == "decoded":
        checks["decoded every delivered unit"] = decode["frames"] == receiver["units_received"]
        checks["no corruption after recovery"] = decode["fixture_matches"]
    return [name for name, ok in checks.items() if not ok]


# Requested placement -> what the sender must report (None = no constraint).
EXPECTED_PLACEMENT = {
    ("hardware", "high-performance"): ("discrete", None),
    ("hardware", "minimum-power"): ("integrated", None),
    ("software", "auto"): ("cpu", "system_memory"),
}
MATRIX = [("auto", "auto"), ("hardware", "high-performance"),
          ("hardware", "minimum-power"), ("software", "auto")]


def placement_failures(tx, backend, adapter):
    """Check the encoder ran where it was asked to, never silently elsewhere."""
    expected = EXPECTED_PLACEMENT.get((backend, adapter))
    if not expected:
        return []
    kind, delivery = expected
    target = tx["target"]
    failures = []
    if target["adapter_kind"] != kind:
        failures.append(f"placed on {target['adapter_kind']}, requested {kind}")
    if delivery and target["delivery"] != delivery:
        failures.append(f"delivery {target['delivery']}, expected {delivery}")
    if target["delivery"] == "cross_adapter_copy" and target["adapter_is_capture_gpu"]:
        failures.append("cross-adapter copy reported for the capture GPU")
    return failures


def run_codec(binary, codec, frames, private_dir, drop=None, backend="auto", adapter="auto"):
    port = free_udp_port()
    with tempfile.TemporaryDirectory(prefix=f"rtp-{codec}-", dir=private_dir) as parent:
        receiver = subprocess.Popen(
            [str(binary), "--rtp-recv", str(port), codec, str(frames), str(Path(parent) / "rx")],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=NO_WINDOW, text=True)
        try:
            ready = json.loads(receiver.stdout.readline())
            if not ready.get("ready") or ready.get("port") != port:
                raise RuntimeError("receiver did not become ready")
            sender = subprocess.run(
                [str(binary), "--rtp-send", f"127.0.0.1:{port}", codec, str(frames),
                 "--backend", backend, "--adapter", adapter]
                + (["--drop", str(drop)] if drop is not None else []),
                capture_output=True, text=True, timeout=120, creationflags=NO_WINDOW)
            if sender.returncode:
                if "no encoder satisfies" in sender.stderr:
                    # A clean, explicit refusal is the correct outcome when the
                    # machine has no encoder for this codec/placement.
                    return {"codec": codec, "backend": backend, "adapter": adapter,
                            "passed": True, "status": "unavailable_refused_cleanly"}
                raise RuntimeError(f"sender failed: {sender.stderr.strip()[-300:]}")
            out, err = receiver.communicate(timeout=90)
            if receiver.returncode:
                raise RuntimeError(f"receiver failed: {err.strip()[-300:]}")
        finally:
            if receiver.poll() is None:
                receiver.kill()
                receiver.wait()
        rx = json.loads(out.strip().splitlines()[-1])
        tx = json.loads(sender.stdout.strip().splitlines()[-1])["streaming"]
    failures = validate(rx, tx, frames) if drop is None else validate_loss(rx, frames)
    failures += placement_failures(tx, backend, adapter)
    return {
        "codec": codec, "backend": backend, "adapter": adapter, "status": "streamed",
        "target": tx["target"], "copy_ms": tx["copy_ms"],
        "encode_latency_ms": tx["encode_latency_ms"], "dropped_packet": drop, "passed": not failures, "failures": failures,
        "encoder_kind": tx["encoder_kind"], "gop_accepted": tx["gop_accepted_by_encoder"],
        "low_latency_accepted": tx["low_latency_accepted"],
        "zero_b_frames_accepted": tx["zero_b_frames_accepted"],
        "units": rx["units_received"], "packets": rx["packets"], "lost": rx["lost_packets"],
        "keyframes": rx["keyframes"], "keyframe_positions": rx["keyframe_positions"],
        "media_kbps": round(rx["media_bitrate_kbps"], 1),
        "interarrival_ms": {k: round(v, 1) for k, v in rx["interarrival_ms"].items()},
        "dropped_units": rx["dropped_units"],
        "withheld_awaiting_keyframe": rx["withheld_awaiting_keyframe"],
        "decode": rx["os_decode"],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--private-dir", type=Path, required=True)
    parser.add_argument("--codecs", default="h264,hevc,av1")
    parser.add_argument("--frames", type=int, default=60)
    parser.add_argument("--backend", default="auto")
    parser.add_argument("--adapter", default="auto")
    parser.add_argument("--matrix", action="store_true",
                        help="run every backend/GPU combination for each codec")
    parser.add_argument("--loss-test", action="store_true",
                        help="also drop packet 50 of an extra run per codec and check recovery")
    args = parser.parse_args()
    if args.loss_test and args.frames < 60:
        # Packet 50 falls after the frame-20 keyframe; recovery needs the
        # frame-40 keyframe plus at least one more GOP to observe it.
        parser.error("--loss-test needs --frames >= 60")
    args.private_dir.mkdir(parents=True, exist_ok=True)
    codecs = [codec for codec in args.codecs.split(",") if codec]
    combos = MATRIX if args.matrix else [(args.backend, args.adapter)]
    results = [run_codec(args.binary.resolve(), codec, args.frames, args.private_dir,
                         backend=backend, adapter=adapter)
               for codec in codecs for backend, adapter in combos]
    if args.loss_test:
        results += [run_codec(args.binary.resolve(), codec, args.frames, args.private_dir, drop=50)
                    for codec in codecs]
    print(json.dumps({"live_rtp_check_passed": all(r["passed"] for r in results),
                      "results": results}))
    if not all(r["passed"] for r in results):
        raise SystemExit(1)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"live_rtp_check_passed": False, "error": str(error)}))
        raise SystemExit(1)
