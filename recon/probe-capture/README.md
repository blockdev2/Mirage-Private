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
