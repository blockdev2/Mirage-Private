"""Independent check of the native multi-codec stream diagnostic (no FFmpeg).

Runs the probe's ``--stream`` mode, then re-parses every elementary stream it
wrote with this file's own Annex B / AV1 OBU reader. The Rust probe's parser
and this one are separate implementations; both must agree.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

EXTENSIONS = {"h264": "h264", "hevc": "h265", "av1": "obu"}


def annexb_nals(data):
    nals, starts, i = [], [], 0
    while i + 3 <= len(data):
        if data[i] == 0 and data[i + 1] == 0 and data[i + 2] == 1:
            starts.append((i, i + 3))
            i += 3
        else:
            i += 1
    for k, (_, begin) in enumerate(starts):
        end = starts[k + 1][0] if k + 1 < len(starts) else len(data)
        while end > begin and data[end - 1] == 0:
            end -= 1
        if end > begin:
            nals.append(data[begin:end])
    return nals


def leb128(data, pos):
    value = 0
    for i in range(8):
        if pos + i >= len(data):
            break
        value |= (data[pos + i] & 0x7F) << (7 * i)
        if not data[pos + i] & 0x80:
            return value, i + 1
    raise ValueError("invalid leb128")


def av1_obu_types(data):
    types, pos = [], 0
    while pos < len(data):
        header = data[pos]
        if header & 0x80 or not header & 0x02:
            raise ValueError("forbidden bit or unsized OBU in low-overhead stream")
        header_len = 2 if header & 0x04 else 1
        size, used = leb128(data, pos + header_len)
        start = pos + header_len + used
        pos = start + size
        if pos > len(data):
            raise ValueError("truncated OBU")
        kind = (header >> 3) & 0x0F
        # show_existing_frame == 0 and frame_type == KEY_FRAME on frame OBUs.
        key = kind in (3, 6) and size > 0 and data[start] & 0xE0 == 0
        types.append((kind, key))
    return types


def summarize(codec, data):
    """Return (keyframes, has_parameter_sets, units) for a whole stream."""
    if codec == "h264":
        kinds = [nal[0] & 0x1F for nal in annexb_nals(data)]
        return kinds.count(5), 7 in kinds and 8 in kinds, len(kinds)
    if codec == "hevc":
        kinds = [(nal[0] >> 1) & 0x3F for nal in annexb_nals(data)]
        irap = sum(16 <= kind <= 23 for kind in kinds)
        return irap, {32, 33, 34} <= set(kinds), len(kinds)
    obus = av1_obu_types(data)
    return sum(key for _, key in obus), any(kind == 1 for kind, _ in obus), len(obus)


def validate_codec(name, report, stream_path):
    if not report.get("encoded"):
        return {"codec": name, "encoded": False}
    data = stream_path.read_bytes()
    if len(data) != report["bytes"]:
        raise RuntimeError(f"{name}: file length differs from probe report")
    keys, parameter_sets, units = summarize(name, data)
    rtp = report["rtp"]
    ok = (report["bitstream_dims"] == [640, 360] and report["first_unit_keyframe"]
          and parameter_sets and keys >= 1 and units >= report["access_units"]
          and rtp["roundtrip_lossless"] and rtp["max_packet"] <= rtp["mtu"]
          and report["os_decode"]["status"] in ("decoded", "no_os_decoder")
          and (report["os_decode"]["status"] != "decoded"
               or report["os_decode"]["fixture_matches"]))
    if not ok:
        raise RuntimeError(f"{name}: stream failed independent validation")
    return {"codec": name, "encoded": True, "encoder_kind": report["encoder_kind"],
            "independent_units": units, "independent_keys": keys,
            "os_decode": report["os_decode"]["status"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--private-dir", type=Path, required=True)
    parser.add_argument("--require", default="h264",
                        help="comma-separated codecs that must encode (default h264)")
    args = parser.parse_args()
    args.private_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="stream-", dir=args.private_dir) as parent:
        out = Path(parent) / "run"
        result = subprocess.run([str(args.binary.resolve()), "--stream", str(out)],
                                capture_output=True, timeout=120,
                                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        if result.returncode:
            raise RuntimeError("probe failed")
        codecs = json.loads(result.stdout)["streaming"]["codecs"]
        summary = [validate_codec(name, codecs[name], out / f"fixture.{ext}")
                   for name, ext in EXTENSIONS.items()]
        missing = [c for c in args.require.split(",") if c and not codecs.get(c, {}).get("encoded")]
        if missing:
            raise RuntimeError(f"required codecs did not encode: {missing}")
        print(json.dumps({"independent_stream_check_passed": True, "codecs": summary}))


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"independent_stream_check_passed": False, "error": str(error)}))
        raise SystemExit(1)
