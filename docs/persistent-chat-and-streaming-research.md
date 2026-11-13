# Persistent chat and screen-streaming research

Date: 2026-11-12. Requested scope: persistent group/contact chat compatible with
TeamSpeak, plus screen sharing. This is backend research and a proposed
implementation sequence, not a claim that either feature is implemented.
The team's frontend owner retains UI/design work.

## Existing implementation

Channel chat and direct-client messages use the voice server's text protocol.
They do not implement the requested persistent contact/group chat service.
The bridge currently exposes only channel chat. The CLI `stream` diagnostic
announces a P2P stream and polls notifications, but has no WebRTC peer,
SDP/ICE negotiation, video encoding, screen capture or rendered receiver.

## Evidence for persistent chat

Official support describes contacts addressed by myTS username or
`username@myteamspeak.com`, invitation acceptance, and subsequent contact chat:
[contact guide](https://support.teamspeak.com/hc/en-us/articles/360002743437-How-do-I-add-a-contact-and-start-a-chat-on-TeamSpeak-6).
The [group guide](https://support.teamspeak.com/hc/en-us/articles/4523999467933-Group-Chats-How-do-I-join-a-group-chat-how-do-I-create-a-group-chat)
describes public/private groups and optional encryption for private groups.
These are product behaviors, not public authentication API specifications.

Static inspection of existing extracted official application resources found:

- `libtschat_client_lib_export.dll`: `org.matrix.login.jwt`, Matrix login/sync
  and room API route patterns, and encrypted-session implementation strings.
- `TeamSpeak.dll`: `org.matrix.login.jwt`, `notifychatlogintoken`, `__CENTRAL__`,
  and public `clientapi.myteamspeak.com` service route prefixes including
  authentication, session, messenger and tschat. Route strings alone do not
  establish methods, payloads or a supported third-party login contract.
- `html/client_ui/main.js`: central/default chat connection construction and
  decentral chat connection construction using `GetDecentralEndpoints()` and
  a returned chat domain. That function calls a native `getWellKnown` bridge;
  it is not evidence that a UDP command with that name exists.

Inspected resource SHA-256 values:

| Resource | SHA-256 |
| --- | --- |
| libtschat_client_lib_export.dll | af66934edfb7f10441a6ff0a92626317117f44919e011b29e3c4ab6dcc5799ab |
| TeamSpeak.dll | cd8f7eba5f048609bb30943fc56154756ba2e73909f2947d9fa7c29322cbdf4b |
| html/client_ui/main.js | 352683c3eb1d674c6b0fe098ec8ba132b696739a33424080c9266b751de41537 |

The executable's version metadata did not establish a precise beta version.
These hashes identify the inspected artifacts; they are not upstream integrity
verification or evidence that the installed resources are the latest release.
Only packaged application resources were inspected. No account profile,
credential store, identity content, chat database or personal history was read.
No proprietary implementation is copied into Mirage.

The pinned Go reference names `chatlogintoken` and homebase commands, but its
changelog reports command-not-found results for some of those commands over
ServerQuery on beta13.1. That is not a test of the voice-client transport or
proof that all deployments lack persistent chat. Reference revision:
`e7fd092a341b2ac7e6f68adf7a3070a3268cb9f3`.

## Chat integration proposal

Use a separate persistent-chat service owner alongside the voice session. Keep
the existing voice identity and `!Send` session ownership unchanged. A
maintained [Matrix Rust SDK](https://github.com/matrix-org/matrix-rust-sdk) is a
candidate for sync, history and encryption; pin and review its actual APIs,
dependency/license impact and storage behavior before adding it. The
[Matrix Client-Server specification](https://spec.matrix.org/latest/client-server-api/)
defines general protocol behavior but does not define TeamSpeak's custom JWT
issuance or contact association.

Compared with a handwritten Matrix client, the SDK avoids reimplementing sync
and encryption. A separate generic Matrix account/homeserver could support
Mirage-only conversations but would not prove official TeamSpeak contact
compatibility. Compatibility with the official account/contact system is the
target; those alternatives are not implemented here.

First prove authentication with a dedicated myTS test account and a second
controlled peer. Never request a production account password or recovery key
in chat. Do not reuse or extract an existing personal session. Determine the
actual central/decentral service discovery and supported account authorization
flow before sending credentials to a candidate endpoint.

After authentication is understood, validate in order:

1. Enumerate only the test account's rooms and establish account/contact ID
   mapping. A voice-server client ID is not a persistent contact ID.
2. Send a fixture to a private test room and independently observe receipt.
3. Disconnect the receiver, send another fixture, then reconnect and verify
   history without duplicate events. Persist sync state and message IDs.
4. Verify encrypted-room keys, device verification and restart recovery with
   protected storage. Never silently downgrade an encrypted room to plaintext.
5. Expose bounded, paginated room/history events through a versioned bridge
   capability. Keep account credentials and crypto keys out of UI events/logs.

Outstanding prerequisite: a dedicated account session and test group/contact
are needed for authenticated interoperability. No successful persistent login,
history retrieval or encrypted-room exchange has been established.

### Account requirement and login integration limits

The owner clarified that the installed official client is already signed in
and requested verification of whether Mirage needs an account/login flow.
That existing session is not a Mirage login and was not imported or extracted.

The evidence supports distinct conclusions:

| Feature | Account conclusion |
| --- | --- |
| Direct connection to the tested official TS6 server binary | Mirage connected, exchanged channel/direct text and transported voice using a local cryptographic identity without myTS login |
| Voice-server stream announcement | Setup and independent start/stop notification worked without myTS login; actual WebRTC media remains unverified |
| Official central contacts and groups | Official product documentation ties the account/contact flow to myTS usernames and account setup; Mirage has not authenticated to that service |
| Decentral persistent chat | Static resource code contains a separate service-discovery path; whether the owner's deployment provides it and what account credentials it needs remain unknown |

The official [getting-started guide](https://www.teamspeak.com/en/support/get-started/)
describes myTS account setup followed by contacts/groups. A TeamSpeak staff
[OAuth announcement and reply dated 2025-10-08](https://community.teamspeak.com/t/myteamspeak-update-2fa-gdpr-oauth-account-deletion/62397/7)
states that OAuth was limited to TeamSpeak services at that time, with possible
future expansion. This is dated first-party evidence, not proof that public
registration remains unavailable forever. This review found no verified public
Mirage client registration, client ID, redirect URI, scopes or token exchange.
Do not implement a purported working OAuth redirect using invented values or
reuse the official client's application identity.

An unauthenticated GET to the contact domain's standard Matrix discovery path
(`https://myteamspeak.com/.well-known/matrix/client`) returned HTTP 403. No
discovery JSON or chat API was validated by that request, and no workaround was
attempted. Static resources also contain `tschat-resolver.teamspeak.com`, but
its presence alone does not establish a request contract.

Computer-use inspection located the running installed TeamSpeak window. Its
accessibility snapshot did not expose matching navigation labels, and screenshot
capture failed with `FrameArrived timed out`; a fresh window selection,
activation and capture retry failed with `window capture timed out`. No account
settings, authentication dialog or messages were changed. No screenshot or
account data was saved. The extracted resources inspected above are separate
artifacts, not proof of the running installed client's exact build.

Implementation implication: keep voice access independent of myTS. Add an
optional account connection flow for central contacts/groups only after its
actual authorization mechanism is understood and validated. If a supported
external-browser flow is available, prefer it; otherwise investigate the
legitimate account authentication contract separately, including 2FA and
session renewal. A login page alone does not provide chat interoperability.

## Screen-signaling experiment

At Mirage revision `638d1fa4b087af07634996ff49ba0ca4f28ddea0`, two diagnostic
identities joined the owner-created, passwordless `Mirage Test` channel on the
authorized remote server. The server had previously identified itself as
`6.0.0-beta13.1 [Build: 1790080330]`.

The observer continuously polled CLI `probe`; the publisher used:

```text
ts6-cli stream <authorized-server>:9987 --type 2 --seconds 4 --channel "Mirage Test" --name <diagnostic-publisher> --identity <private-test-identity>
```

The publisher exited zero and reported `setupstream` acceptance. The independent
observer saw `notifystreamstarted`, then `notifystreamstopped` after publisher
disconnect. No `notifychatlogintoken` was observed in this bounded observer
window. Absence in this window does not establish lack of a chat service.
All diagnostic children were stopped. Endpoint, identities and private
orchestration remain ignored. No screen, camera or microphone was captured;
no media was transmitted. Explicit `stopstream`, viewer admission and WebRTC
were not exercised by this experiment.

## Streaming implementation proposal

The pinned [Go stream wire reference](https://github.com/honeybbq/teamspeak-go/blob/e7fd092a341b2ac7e6f68adf7a3070a3268cb9f3/stream/wire.go)
and the [existing reference review](protocol-reference-teamspeak-go.md) describe
setup, join request, publisher admission, SDP offer/answer, ICE and stop. These
are interoperability leads, not official API guarantees. Their licensing and
attribution requirements apply independently of protocol facts.

Implement typed signaling and lifecycle before selecting a media stack. Scope
each stream to the connection/session and publisher. Enforce capability and
permission checks, bound decoded and escaped payload sizes, and reject stale
events after disconnect. Keep SDP/ICE and service tokens out of ordinary logs.
Test refusal, cancellation and cleanup as carefully as success.

Next interoperability gates:

1. Announce, query and explicitly stop a stream with an independent observer.
2. Negotiate a viewer request and rejection/acceptance with bounded deadlines.
3. Validate SDP and ICE with an official client and a synthetic video source.
4. Confirm a decoded frame at the receiver before implementing desktop capture.
5. Add explicit window/screen selection, bitrate bounds and immediate stop;
   assess native capture and WebRTC-library build/license costs first.

A browser WebRTC spike can reduce initial media dependencies, while native Rust
or libwebrtc integration better matches the final native application. No media
stack has been selected or dependency added. Group-chat screen calls may use
another signaling plane; the successful voice-server P2P announcement does not
prove group-call compatibility. That distinction remains open.

## Implemented stream-control diagnostic

The owner subsequently deferred central groups and authorized continuing the
server-streaming path. Branch `codex/stream-control-validation` now adds:

- `ts6-core/src/streams.rs`: typed control notices using the installed TS
  command parser. It distinguishes stream start/info/stop, viewer requests and
  admission responses, rejecting duplicate fields, ambiguous batches, invalid
  peer IDs and oversized input. The 16 KiB input and 128-byte stream-ID bounds
  are local parser policy, not claims about universal server limits. Opaque
  stream IDs are unescaped and passed through the existing command encoder.
  SDP, ICE, names and arbitrary extra fields are not retained in these DTOs.
- `ts6-cli stream-check`: two dedicated owner threads/current-thread runtimes,
  each retaining its `!Send` session. Command queues hold eight entries and
  notice queues sixty-four. Owners have a ninety-second loop deadline, startup
  waits twenty-five seconds, command waits fifteen and notice waits ten. No
  short polling timeouts or competing receivers are introduced. Startup failure
  joins the owner thread after its disconnect path. Normal/error completion
  attempts explicit stream cleanup and waits for both owners.

On the authorized remote beta13.1 server, the final binary passed in the
dedicated test channel with two diagnostic identities:

```json
{"announcement_observed":true,"info_observed":true,"viewer_request_observed":true,"refusal_observed":true,"explicit_stop_observed":true,"media_transmitted":false}
```

The receiver independently observed announcement, queried information and stop.
The publisher observed the viewer's request; the viewer observed decision zero
after the publisher sent an explicit refusal with an empty offer. Neither
client captured screen/camera/audio or negotiated WebRTC. A missing explicit
channel was rejected before connection, and a remote nonexistent channel
returned a controlled startup error with no successful check summary.

Parser regression tests failed before implementation, then passed. The info
variant also failed its distinction test before correction. Core/CLI tests,
formatting, build and strict first-party Clippy are recorded in the associated
implementation plan. Existing vendored/profile warnings remain. Acceptance
with a real SDP offer, ICE exchange, viewer departure after admission and a
decoded video frame remain the next gates. No frontend sharing control or
working screen-video feature is advertised by this diagnostic.

## Synthetic video received through the TS6 signaling path

The next diagnostic, `stream-media-check`, reuses the Rust owners and adds
redacted, bounded signaling payloads plus a private Python/Chromium controller.
It does not depend on the proprietary client's code or active account session.
An installed Playwright 1.63.0 / Chromium 153.0.8010.12 generated the image.

On the authorized beta13.1 server, the final experiment observed:

- An accepted viewer request carrying a real SDP offer to the viewer.
- The viewer's SDP answer returned through `streamsignaling` to the publisher.
- Six ICE candidates relayed through that server and applied at the receiving
  browser peer only after receiving the corresponding server notification.
- Connected WebRTC state, five decoded 320x180 frames in the final run (six in
  earlier runs), and a center pixel matching
  the synthetic green fixture within the test's color tolerance.
- Viewer removal command acceptance, independently observed explicit stream
  stop, successful controller/session shutdown, and CLI exit zero.

The final Rust stdout was a sanitized numerical/boolean summary. No SDP, ICE,
endpoint or account credential was printed. A video-element screenshot of the
synthetic received image was saved privately and visually inspected. It shows
the green fixture and its moving dark marker; it is not a desktop screenshot.

The peers share one machine. The signaling traversed the remote TS6 server;
the media is P2P and is not proof that the server relayed video. No external
STUN, TURN, cross-machine NAT, official-client video, GPU encoder, screen
capture, audio synchronization or native render path was validated. VP8 was
explicitly selected for the browser fixture. Official-client video remains a
separate interoperability gate even though official-client text was confirmed.

Tokio process/IO features were enabled for bounded asynchronous subprocess
pipes. The lockfile adds only an edge to an already locked signal-hook-registry;
no package version changed. Cargo audit exited zero with six existing
unmaintained-crate warnings. No advisory ignores were added.

See [native streaming architecture](native-streaming-architecture.md) for the
owner's subsequent GPU/capture/audio direction, verified library capabilities
and the Windows process-loopback version constraint. Chromium is a diagnostic
engine here, not the selected production native media stack.

## Native viewer against the official client (2026-11-13) — NOT WORKING YET

Stage 1 of the native stream viewer
([spec](superpowers/specs/2026-11-13-stream-viewer-design.md),
[plan](superpowers/plans/2026-11-13-stream-viewer.md)) is implemented in
`ts6-bridge` (`streams.rs`, `stream_media/`) and the desktop (`app/stream.rs`),
but **watching a stream from the official TS6 client does not connect yet**.
The "AO VIVO" badge and "Assistir" are visible; clicking ends with "Não foi
possível conectar à transmissão (rede bloqueada?)". To be fixed later.

What works, observed live on the owner's beta13.1 server (same machine for
the viewer, official client on another PC):

- `client_is_streaming` reaches the UI; a stream already running when we join
  is found through `requeststreaminfo` → `notifystreaminfo`.
- `joinstreamrequest` is admitted automatically; the libwebrtc offer arrives
  (≈4.1 KB, AV1/H.264/VP9/VP8 + Opus stereo) and ~30 trickled candidates follow.
- webrtc-rs 0.21.1 accepts the offer, answers H.264 High (pt 102) and sends its
  answer and candidates through `streamsignaling` in the reference format
  (`{"cmd":"answer","args":{"answer":…}}`, `iceCandidate` with `sdp/mid/mLine`).
- Mirage ↔ Mirage on loopback (webrtc-rs both sides) connects and decodes.

What fails, from ICE traces (counts only; addresses were never recorded):

1. First attempts: our connectivity checks reached the publisher and some got
   success responses, but the publisher **never sent a single check of its
   own**, so nothing was nominated. Others were answered with STUN
   **401 Unauthorized** (read with a temporary diagnostic build of rtc-ice).
2. About 15 s after the first offer the official client sends a **new offer**
   through `streamsignaling` (`offer`/`reconnectOffer`, ≈4.3 KB). Mirage used
   to ignore it; it now answers it with a fresh peer.
3. After two fixes (accepting the audio m-line like a browser instead of
   rejecting it, and answering re-offers) the last attempt showed **no 401s**
   and four negotiations, but still no connection.

Reading: the official client behaves as if it never applies our answer (no
remote ICE credentials, hence no checks of its own, then a re-offer). Leads for
next time, in order:

- Confirm whether the server delivers our `streamsignaling` answer: send it
  with `send_command` (awaited) in a diagnostic and record the result code.
- Compare with what an official viewer sends: the answer may need another
  key, an extra argument, or the candidates may be expected inside the SDP
  (non-trickle answer after gathering completes).
- Bind only real LAN interfaces (WSL/Hyper-V 172.x and VPN adapters produce
  "network unreachable" noise) and add a STUN server for cross-home tests.
- If webrtc-rs ↔ libwebrtc DTLS/ICE interop is the blocker, the spec's
  fallback is LiveKit's libwebrtc bindings.

Diagnostic command kept for this work: `ts6-cli stream-offer <addr> <dir>
--channel <name>` asks to watch the next stream and prints only the offer's
codec/feedback/extension lines and candidate-type counts.

Overlap to resolve: PR #12 added a separate `ts6-stream` crate (webrtc-rs
publisher and viewer peers with the same `streamsignaling` JSON, tested on
loopback, not wired to the session). The bridge viewer in
`ts6-bridge/src/stream_media` is wired end to end. When the official-client
interop is fixed, keep one implementation (likely `ts6-stream` for the peers,
`ts6-bridge` for signaling and decoding). Note that `ts6-stream` holds remote
candidates that arrive before the remote description; check whether that
matters for the official client.
