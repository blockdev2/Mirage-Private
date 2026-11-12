# Native capture probe

Windows-only manual experiment: creates a synthetic Win32 window, captures
only its owned HWND using WGC, and copies frames into an independently owned
D3D11 texture. It does not enumerate or capture user windows, use the network,
capture audio or change the desktop application. An explicit diagnostic option
encodes its final owned texture to a new private MP4 file.

This is a separate Cargo workspace with pinned `windows-capture` 2.0.1 and
its own lockfile. Existing application dependencies remain separate.

From the repository root:

```powershell
cargo build --locked --manifest-path recon/probe-capture/Cargo.toml --target-dir .audit-local/native-capture-target
python scripts/check_native_capture.py --binary .audit-local/native-capture-target/debug/mirage-native-capture-probe.exe
```

The probe briefly displays its own animated green window without requesting
focus, resizes it once, captures at least forty frames, and closes it. The
manual wrapper enforces a thirty-second process deadline and stops only its
newly launched process. No image is written. The normal capture loop has an
eight-second deadline. Driver calls and shutdown are additionally bounded by
the wrapper's process deadline.

One final staging readback checks the center pixel after the capture thread
has stopped and the original frame pool has closed. This proves that the
owned texture remains readable in that experiment; it is not zero-copy video
streaming. The callback does not retain pooled frames or call `Frame::buffer()`.
The callback records CPU copy-submission cost, not GPU completion or encoder
latency. Window decoration/DPI can make captured dimensions differ from the
fixture's requested logical client size.

## Verification

```powershell
cargo test --locked --manifest-path recon/probe-capture/Cargo.toml --target-dir .audit-local/native-capture-target
cargo test --locked --manifest-path recon/probe-capture/Cargo.toml --target-dir .audit-local/native-capture-target -- --include-ignored
cargo fmt --manifest-path recon/probe-capture/Cargo.toml -- --check
cargo clippy --locked --manifest-path recon/probe-capture/Cargo.toml --target-dir .audit-local/native-capture-target --all-targets --no-deps -- -D warnings
cargo audit --file recon/probe-capture/Cargo.lock
```

Three pure bounds/color tests run normally. The fourth creates an owned
window and is ignored by default because it requires an interactive Windows
desktop; it was explicitly run in this experiment. None captures a user window.

## Observed failure and correction

Initial WGC creation failed with `ItemConvertFailed`, HRESULT `0x80070057`.
The first fixture used `WS_EX_TOOLWINDOW`, which the library's eligibility
check rejects. An interactive test reproduced that rejection before correction.
Removing that flag passed eligibility but the `WS_EX_NOACTIVATE` fixture still
failed native item creation on the tested system. Keeping the GUI message queue
active during startup alone did not resolve it. A standard `WS_EX_APPWINDOW`
fixture, still shown with `SW_SHOWNOACTIVATE`, succeeded. This is an observed
fixture-specific result, not a universal guarantee about every window style.

The probe now preserves the conversion HRESULT. Startup runs outside the GUI
thread while the fixture queue continues to pump. The native creation result
is authoritative; an eligibility predicate alone is not proof of capturability.
The [libwebrtc WGC tests](https://chromium.googlesource.com/external/webrtc/+/HEAD/modules/desktop_capture/win/wgc_capturer_win_unittest.cc)
also cover unsupported tool-window style and resizing. No upstream code was copied.

## Result and limits

A passing run received forty frames, allocated two owned textures across one
resize, validated the expected BGRA color with one final readback, and exited
zero. H.264/NVENC/AMF/QSV, adapter selection, capture loss/restart, user-selected
windows, monitor capture and official-viewer video are not validated by this
capture-only result. See [native architecture](../../docs/native-streaming-architecture.md).

## GPU-input H.264 diagnostic

```powershell
python scripts/check_native_encoding.py --binary .audit-local/native-capture-target/debug/mirage-native-capture-probe.exe --private-dir .audit-local/native-encoding
python -m unittest discover -s scripts -p test_native_encoding.py
```

Requires installed FFprobe and FFmpeg; this script downloads nothing. It creates
a temporary directory under the supplied private directory, starts only its own
probe with a thirty-second deadline, verifies the file independently, checks
overwrite rejection, then removes its temporary synthetic output. Diagnostic
stderr and output paths are not printed. Failed direct runs may leave incomplete
files; keep those private and use a new path for each attempt.

The native option is `--encode <new-private-output.mp4>`. Existing paths are
rejected before capture and MF file creation also uses exclusive creation.
After WGC stops, D3D11 video processing scales the final BGRA resource to
640x360 NV12 with explicit BT.709 color settings. The MF sink writer receives
DXGI surface buffers and twenty timestamped samples at 20 FPS, targeting H.264
at 800 kbit/s. The same immutable final texture is repeated. The writer's device
manager uses the capture device; context multithread protection is enabled while
MF resources are alive and its previous mode is restored afterwards.

Observed result: FFprobe counted twenty H.264 frames at 640x360 and 20/1 FPS;
FFmpeg decoded the expected green center pixel. A repeated output-path attempt
was rejected without changing the file. This is a file/resource proof, **not a
live capture/encode throughput benchmark**. No additional explicit CPU frame
readback is used for encoding, but internal encoder copies are unknown.

Encoder identification (2026-11-12 follow-up): after `BeginWriting`, the probe
walks the writer chain with `IMFSinkWriterEx::GetTransformForStream` and
classifies the first `MFT_CATEGORY_VIDEO_ENCODER` transform. Hardware MFTs
publish `MFT_ENUM_HARDWARE_URL_Attribute`; its presence yields
`encoder_kind: "hardware"`, absence `"software"`, an inaccessible attribute
store `"unknown"`, and no encoder `"missing"` (rejected by the validator).
Vendor and adapter names are intentionally not reported. On the development
workstation three consecutive independently verified runs reported
`hardware`. This establishes that MF selected a hardware encoder MFT for this
configuration; it does not identify which adapter executed it on a multi-GPU
machine, and other machines may legitimately fall back to software.
Elementary access units, RTP, official viewers, live texture recycling,
bitrate adaptation, GPU completion timing and other adapters remain untested.

Failures found and corrected:

- Shader-resource-only BGRA textures failed `CreateVideoProcessorInputView`
  with `0x80070057`. Adding a render-target binding satisfies the documented
  [processor-input resource requirements](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11videodevice-createvideoprocessorinputview).
- An explicit byte-stream `Close` after synchronous writer finalization and
  destruction returned `0x80070057` although FFprobe decoded a valid file. The
  sink manages the stream lifecycle; the probe now finalizes and releases its
  references without that redundant close. Finalize failures remain fatal.
- Missing MF color attributes produced a decoded center of BGRA (81,201,32),
  outside the fixture tolerance. Propagating BT.709 primaries, transfer, matrix
  and limited range into both media types passed the same independent pixel
  check. The validator retains the incorrect color as a regression case.
- Clippy rejected an unnecessary return and an unused previous-mode value;
  both were corrected without suppressing diagnostics.

References: [MF GPU device manager](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-sink-writer-d3d-manager),
[hardware transform permission](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-readwrite-enable-hardware-transforms),
[color propagation](https://learn.microsoft.com/en-us/windows/win32/medfound/extended-color-information),
[synchronous finalization](https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsinkwriter-finalize).

## Direct MFT multi-codec stream diagnostic (`--stream`)

Added 2026-11-12. `--stream <new-private-directory>` captures the owned
synthetic window as before, converts it to NV12 on the GPU, and then, for each
of **H.264, HEVC and AV1**, drives the encoder `IMFTransform` directly instead
of using the MP4 sink writer. The output is raw access units (Annex B for
H.264/HEVC, low-overhead OBUs for AV1) — the form an RTP sender consumes.
`--codecs` prints capability discovery only.

How the encoder is driven:

- Candidates come from `MFTEnumEx` (hardware first, then software), NV12 in,
  codec out. Async hardware MFTs are unlocked (`MF_TRANSFORM_ASYNC_UNLOCK`) and
  driven through `METransformNeedInput/HaveOutput/DrainComplete` events, polled
  with `MF_EVENT_FLAG_NO_WAIT` under a 15 s deadline so a stuck driver cannot
  hang the probe. Sync MFTs use the `ProcessInput`/`ProcessOutput` loop with
  `MF_E_NOTACCEPTING` back-pressure. Stream changes renegotiate the output type.
- The capture device's `IMFDXGIDeviceManager` is handed to hardware encoders,
  so the NV12 texture is consumed without CPU readback.
- Parameter sets missing from the first unit would be taken from
  `MF_MT_MPEG_SEQUENCE_HEADER` (reported as `parameter_sets_from_media_type`).

Verification, all without FFmpeg:

- `bitstream.rs` (pure Rust, unit-tested): Annex B splitting, emulation-prevention
  removal, Exp-Golomb, H.264 and HEVC SPS dimensions including conformance
  cropping, AV1 OBU parsing, sequence-header dimensions and KEY_FRAME detection.
- `rtp.rs` (pure Rust, unit-tested): RFC 6184 (single NAL + FU-A), RFC 7798
  (single NAL + FU) and the AV1 RTP payload format (OBU elements without
  `obu_size`, Z/Y fragmentation, N on a new coded video sequence). Every run
  depacketizes and requires a byte-exact round trip against a canonical form.
- An OS decoder MFT decodes the units back to NV12 and the centre sample is
  compared with the BT.709 limited-range value of the fixture colour
  (expected Y≈135, U≈100, V≈62).
- `scripts/check_native_stream.py` re-parses the written elementary files with
  a separate Python implementation and cross-checks lengths, keyframes and
  parameter sets against the probe report.

Observed on the development workstation (three consecutive runs, identical):

| Codec | Encoder | Units | Bitstream dims | Keyframes | RTP (MTU 1200) | OS decode |
| --- | --- | --- | --- | --- | --- | --- |
| H.264 | hardware | 20 | 640x360 | 1 | 43 packets, lossless | centre YUV (135,100,62), match |
| HEVC | hardware | 20 | 640x360 | 1 | 45 packets, lossless | no OS decoder installed |
| AV1 | hardware | 20 | 640x360 | 1 | 24 packets, lossless | no OS decoder installed |

Findings that matter for the product:

- **Adapter affinity.** On a multi-GPU machine, hardware encoder MFTs belonging
  to other adapters rejected the capture device manager with `E_FAIL`
  (`skipped_candidates`); the probe fell through to the encoder on the capture
  adapter. Production code should select it directly via `MFTEnum2` with
  `MFT_ENUM_ADAPTER_LUID` of the capture device instead of trial and error.
- **Decode capability is the real constraint, not encode.** This machine
  encodes HEVC and AV1 in hardware but has no Media Foundation decoder for
  either (they ship as optional Store codec extensions). A receiver like this
  could only watch H.264. Codec choice must therefore be negotiated from the
  *viewer's* decoders, with H.264 as the universal baseline; HEVC/AV1 are
  opportunistic upgrades. Hardware decoder MFT counts were 0 for all codecs,
  which is normal: MF exposes DXVA hardware decode through the software decoder
  MFT, so a count of 0 does not mean decode is CPU-only.
- The official TeamSpeak viewer's accepted codecs are still unknown; the only
  measured interop is the earlier Chromium VP8 test. Confirm before choosing a
  default.

Limits: one repeated still frame (no motion, no rate control or keyframe
interval tested), 640x360@20, file output rather than live RTP sockets, HEVC/AV1
pixel content not decoded on this machine, latency and encode-time percentiles
not measured.

## Live RTP over UDP (`--rtp-send` / `--rtp-recv`)

Added 2026-11-12. Two separate processes exchange real RTP over loopback UDP.

- `--rtp-recv <port> <h264|hevc|av1> <frames> <new-dir>` binds `127.0.0.1`,
  prints a ready line, reassembles access units as packets arrive, inspects
  each with the Rust bitstream parser, then decodes the stream through an OS
  decoder (when one exists) and measures every decoded frame.
- `--rtp-send <127.0.0.1:port> <codec> <frames> [drop-packet-index]` captures
  the owned fixture, then encodes a **moving pattern** (a 32x32 block travels
  across the top band each frame; each frame gets its own NV12 texture) paced
  in real time at 20 FPS. Each unit is packetized and sent the moment the
  encoder releases it. Non-loopback destinations are refused. The optional
  index deliberately drops one packet (sequence still advances) to test
  recovery.
- The encoder is asked for a 20-frame GOP through `ICodecAPI`
  (`CODECAPI_AVEncMPVGOPSize`) so late joiners and loss recovery get periodic
  keyframes; the sender reports whether the encoder accepted it.
- `Reassembler` (pure Rust, unit-tested) locks the SSRC, detects gaps and
  sequence wrap, discards incomplete units, and after any loss withholds output
  until the next keyframe — where a production receiver would send a keyframe
  request (RTCP PLI), which is not implemented yet.
- `scripts/check_native_rtp.py` orchestrates both processes per codec and
  validates what the receiver observed; `--loss-test` adds a drop-packet-50 run.

Observed on the development workstation (60 frames each; repeated, identical):

| Codec | Encoder | GOP accepted | Packets | Loss | Keyframes at | Median / p95 inter-arrival | Media rate | OS decode |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| H.264 | hardware | yes | 134 | 0 | 0, 20, 40 | 50 / ~56 ms | ~115 kbit/s | 60/60 frames, colour match, motion in all 60 |
| HEVC | hardware | yes | 137 | 0 | 0, 20, 40 | 50 / ~57 ms | ~118 kbit/s | no OS decoder |
| AV1 | hardware | yes | 72 | 0 | 0, 20, 40 | 50 / ~54 ms | ~101 kbit/s | no OS decoder |

Loss-recovery run (packet 50 dropped), identical for all three codecs: 1 packet
lost, 1 damaged unit discarded, 19 units withheld, 40 delivered, playback
resumed exactly at the next keyframe; for H.264 all 40 decoded frames matched
the fixture colour (no corruption after recovery).

What this does and does not show: the native capture → GPU convert → hardware
encode → RTP → UDP → reassembly → decode chain works end to end in real time
for one process pair on one machine. The low bitrate reflects the simple
synthetic content; the 800 kbit/s target is not enforced (no CBR mode set).
Not covered: real network loss/jitter/reordering, RTCP (PLI/NACK, receiver
reports, congestion control), SRTP/DTLS encryption, cross-machine NAT, live
re-capture per frame (the pattern is drawn on the captured texture), and the
official TeamSpeak viewer. With a 1 s GOP and no PLI, worst-case recovery
after loss is about one second.

## Choosing the encoder: GPU, integrated GPU or CPU

Added 2026-11-12. `--rtp-send` accepts:

- `--backend auto|hardware|software` (`gpu`/`cpu` are aliases). `hardware`
  never falls back to the CPU; `software` uses only CPU encoders; `auto` tries
  hardware first and falls back to the CPU encoder.
- `--adapter auto|high-performance|minimum-power|<index>` (`dgpu`/`igpu`
  aliases). The two named choices follow **Windows' own GPU preference**
  (`IDXGIFactory6::EnumAdapterByGpuPreference`), the same ordering Windows uses
  for "High performance" / "Power saving" app settings; indices come from
  `--devices`. `auto` prefers the capture GPU (zero copy), then the rest by
  performance.

`--devices` lists every adapter with its kind (discrete / integrated / software
render), Windows' preference ranks, which one captures, and hardware encoders
**per codec per adapter**, plus CPU encoders per codec. Encoders are matched to
adapters with `MFTEnum2` + `MFT_ENUM_ADAPTER_LUID`; note the LUID must be passed
as an 8-byte blob — passed as a UINT64 the filter is silently ignored and every
adapter appears to own every encoder (this was observed and fixed).

Policy (pure, unit-tested in `selection.rs`): an explicit GPU choice names
exactly one GPU. If that GPU has no encoder for the codec, the request is
refused (or goes to the CPU under `--backend auto`) — it is **never moved to a
different GPU**. The first live matrix caught exactly that bug (AV1 requested on
an integrated GPU without AV1 silently ran on the discrete GPU); the planner and
a regression test were fixed.

Data path per choice (capture always happens on the default adapter):

| Encoder location | Delivery | Copies per frame |
| --- | --- | --- |
| Capture GPU | `zero_copy_gpu` | none (NV12 texture shared with the encoder) |
| Another GPU | `cross_adapter_copy` | GPU → system memory → other GPU |
| CPU | `system_memory` | GPU → system memory |

Encoders are also asked for low-latency mode (`CODECAPI_AVLowLatencyMode`), no
B-frames (`CODECAPI_AVEncMPVDefaultBPictureCount = 0`) and a 20-frame GOP; the
sender reports which hints each encoder accepted.

`scripts/check_native_rtp.py --matrix` runs backend/adapter × codec live over
UDP and verifies the encoder ran where requested. Observed on the development
workstation (one discrete GPU, one integrated GPU, 60 frames each):

| Codec | Discrete GPU (zero copy) | Integrated GPU (cross-adapter) | CPU |
| --- | --- | --- | --- |
| H.264 | encode p50 ≈2 ms / p95 ≈3.5 ms | copy ≈1 ms, encode p50 ≈3.5 ms | copy ≈0.7 ms, encode p50 ≈0.6 ms |
| HEVC | ≈2 / ≈3.6 ms | copy ≈1 ms, encode ≈3 ms | refused: no CPU encoder |
| AV1 | ≈2.2 / ≈3.7 ms | refused: no AV1 encoder on that GPU | refused: no CPU encoder |

All streamed runs delivered 60/60 frames without loss; H.264 decoded with the
correct colour on every placement. Before the low-latency hints, HEVC on the
integrated GPU buffered 2–3 frames (encode p50 ≈100 ms, p95 ≈151 ms); with them
it dropped to ≈3 ms. The discrete GPU's encoders declined the explicit B-frame
setting but already ran at low latency. CPU timings reflect a tiny 640x360
synthetic frame and are not representative of real screen content.

Product guidance: default to `auto` (zero copy on the capture GPU). Offer the
integrated GPU as a "save the gaming GPU" option — it costs one readback and
upload per frame (≈1 ms at 640x360, growing with resolution). The CPU is a
compatibility fallback, H.264 only on stock Windows. Capturing *on* the chosen
GPU (instead of copying) needs the capture device to be created on that adapter
and is a follow-up.
