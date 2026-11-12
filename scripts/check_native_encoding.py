"""Independent verification of the owned-window native H.264 diagnostic."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def validate_stream(stream):
    return (stream.get("codec_name") == "h264" and stream.get("width") == 640
            and stream.get("height") == 360
            and stream.get("nb_read_frames") == "20"
            and stream.get("avg_frame_rate") == "20/1")


def validate_pixel(pixel):
    return (len(pixel) == 4 and all(abs(a - b) <= 20
            for a, b in zip(pixel[:3], (80, 180, 20))) and pixel[3] >= 240)


def run(command, timeout=30):
    """Keep external paths and raw diagnostics out of public output."""
    result = subprocess.run(command, capture_output=True, timeout=timeout,
                            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
    if result.returncode:
        raise RuntimeError("diagnostic subprocess failed")
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--private-dir", type=Path, required=True)
    parser.add_argument("--ffprobe", default="ffprobe")
    parser.add_argument("--ffmpeg", default="ffmpeg")
    args = parser.parse_args()
    args.private_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="h264-", dir=args.private_dir) as directory:
        output = Path(directory) / "fixture.mp4"
        native = json.loads(run([str(args.binary.resolve()), "--encode", str(output.resolve())]))
        if not (native.get("fixture_pixel_matches") and native.get("encoded_video")
                and native.get("user_windows_captured") is False
                and native.get("network_used") is False
                and native.get("encoding", {}).get("encoder_kind")
                in ("hardware", "software", "unknown")):
            raise RuntimeError("native capture/encoding report failed validation")
        info = json.loads(run([args.ffprobe, "-v", "error", "-count_frames",
                               "-select_streams", "v:0", "-show_entries",
                               "stream=codec_name,width,height,nb_read_frames,avg_frame_rate",
                               "-of", "json", str(output)]))
        streams = info.get("streams", [])
        if len(streams) != 1 or not validate_stream(streams[0]):
            raise RuntimeError("encoded stream failed independent validation")
        # Decode the complete frame, then inspect its center. Never write raw
        # pixels, encoder metadata or native stderr to the shared report.
        pixels = run([args.ffmpeg, "-v", "error", "-i", str(output), "-frames:v", "1",
                      "-f", "rawvideo", "-pix_fmt", "bgra", "pipe:1"])
        offset = (180 * 640 + 320) * 4
        if len(pixels) != 640 * 360 * 4 or not validate_pixel(pixels[offset:offset + 4]):
            raise RuntimeError("decoded fixture pixel mismatch")
        digest = hashlib.sha256(output.read_bytes()).digest()
        repeated = subprocess.run([str(args.binary.resolve()), "--encode", str(output.resolve())],
                                  capture_output=True, timeout=10,
                                  creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        if repeated.returncode == 0 or hashlib.sha256(output.read_bytes()).digest() != digest:
            raise RuntimeError("existing output was not protected")
        print(json.dumps({"independent_decode_passed": True, **streams[0],
                          "fixture_pixel_matches": True, "overwrite_rejected": True,
                          "encoding": native["encoding"]}))


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired):
        print(json.dumps({"independent_decode_passed": False,
                          "error": "Native encoding validation failed; private diagnostics require local review."}))
        raise SystemExit(1)
