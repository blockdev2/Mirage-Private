# Integrated backend live validation

## Scope and setup

Manual Windows experiment on 2026-11-12 at revision
`638d1fa4b087af07634996ff49ba0ca4f28ddea0`, after the owner integration.
The server snapshot identified TeamSpeak Server
`6.0.0-beta13.1 [Build: 1790080330]`.
This is a version-specific observation, not compatibility with every release.

A fresh isolated instance used the existing local official server executable
and libraries, without copying previous databases or credentials. Voice bound
to `127.0.0.1:9987`; file transfer bound to `127.0.0.1:30034`. Listener inspection
found no administrative TCP listeners. Default query interfaces remained
disabled. Server files, test identities and runtime output stayed ignored with
restricted directory ACLs. No microphone, speakers, administrative query API,
or official-client interaction was used.

## Commands and results

Build from `client`, with `CMAKE_POLICY_VERSION_MINIMUM=3.5`:

```powershell
cargo build --locked --package ts6-cli --package ts6-bridge
```

Build passed in 1m49s. Vendored warnings and unmatched development profile
package warnings remain. Invoking the PowerShell helper with `-p` first failed
because PowerShell interpreted it as an ambiguous helper parameter; the direct
Cargo command above succeeded.

From the repository root:

```powershell
python scripts/check_chat_delivery.py --binary client/target/debug/ts6-cli.exe --port 9987
python scripts/check_channel_chat.py --binary client/target/debug/ts6-bridge.exe --server-port 9987
python scripts/check_voice_progress.py --binary client/target/debug/ts6-cli.exe
```

| Experiment | Observed result |
| --- | --- |
| CLI chat, initial execution | Independent channel and direct-client reception passed; both sender echoes were observed |
| Bridge chat | One diagnostic execution of the same harness passed bidirectional delivery, one sender echo, Unicode/escaping, invalid input, offline sends, reconnect and previous-session rejection; multiple other executions failed |
| Bridge failure localization | State acquisition timed out during connection, before sending the chat fixture. A later diagnostic found a disconnected status with a connection/handshake refusal whose text included a ban-related term. Raw errors were withheld. Individual connection diagnostics also succeeded |
| CLI chat after repeated bridge attempts | Failed; the harness withholds raw output, so the exact failing phase was not established |
| Voice progress, receiver first | Passed: first positive sender-correlated report after 0.142 seconds; both processes exited zero |
| Additional synthetic tone, receiver first | Twelve-second receiver and eight-second sender: 401 frames sent, 401 packets received, both exits zero, nonzero decoded RMS |
| Additional synthetic tone, sender first | Inconclusive: the private orchestration timed out waiting for peer readiness; no late-join receive measurement was obtained |

The additional tone experiment used separate identities for each ordering,
waited for the first peer's readiness, and delayed the late receiver by two
seconds. It launched only diagnostic CLI peers and cleaned up its children.
Packet equality in the receiver-first run is evidence for that short interval,
not a general packet-loss guarantee. The 0.142-second report delay includes
diagnostic reporting behavior and is not an acoustic latency measurement.

## Interpretation and follow-up

The integrated backend can establish a real server session, deliver chat to
an independent peer, and transport/decode synthetic voice while transmission
is active. Repeated connection sequences are not reliably green. A handshake
refusal is evidence of a connection-stage failure, not a lost chat notification;
the precise refusal code, server admission behavior and timing still need a
controlled experiment. Do not disable flood/admission protection to make the
test pass or claim that a single successful retry resolves these failures.

The bridge harness currently waits for a state event even after a connection
error, which turns a refusal into a generic timeout. A focused follow-up should
preserve a sanitized connection-failure category and stop that wait promptly.
Keep successful delivery and connection admission as separate measurements.

No rendered native/browser interaction, microphone capture, playback quality,
DPDFNet acoustic quality, sustained call, raw batched-notification injection,
or successful late-join test is established by this experiment. The raw-batch
fix retains its parser-backed regression evidence; these ordinary live chat
messages do not specifically force that fallback path.

## Follow-up: connection diagnostics and owner-provided remote server

The harness now stops immediately on a disconnected `status` containing an
error. It emits only `connection_failed`, `handshake_failed`, or
`handshake_banned` (the last requires a specific known message), never raw
server-controlled text. An ordinary initial disconnected status with no error
does not abort. Three offline socket-based regressions cover refusal, privacy,
and normal startup. The refusal tests failed before the change and passed after.

On the same local instance, three consecutive bridge trials produced one full
pass followed by two immediate `handshake_failed` results. An additional probe
did not match an error description from the vendored protocol error table.
The previous ban-related substring is therefore insufficient to assert a
specific ban code or flood-protection cause. The server's numeric handshake
error identifier is discarded by the current vendored handshake error path;
preserving a typed identifier is a possible separate protocol improvement.

The owner then authorized an existing remote server and created an empty,
passwordless `Mirage Test` channel. Its address and other clients' information
are intentionally omitted. The server snapshot reported
`6.0.0-beta13.1 [Build: 1790080330]`. An initial probe connected without a server
password or myTS account. A subsequent probe verified that `--channel` selected
the test channel and that no other clients were present there at that instant.

| Remote experiment | Observed result |
| --- | --- |
| Direct messages between diagnostic peers | Independent reception, sender echo, Unicode and escaped separators passed; no channel message was sent in this initial test |
| Synthetic voice, receiver first | Receiver configured for twelve seconds, sender for eight: 400 frames sent and 400 packets received; nonzero decoded RMS and both exits zero |
| Synthetic voice, sender first | Sender configured for twelve seconds, receiver launched two seconds after sender readiness and configured for six: 600 frames sent and 444 packets received; nonzero decoded RMS and both exits zero |
| Bridge channel-chat regression | Passed bidirectional independent delivery, one sender echo per fixture, Unicode/escaping, invalid input, offline sends, reconnect and previous-session rejection |

The voice diagnostics used separate ignored identities and `--channel
"Mirage Test"` for both peers. Their different connection/synchronization and
measurement windows preclude a packet-loss estimate in the sender-first test.
This successful late-join run supersedes the inconclusive local attempt for
this particular remote setup, not for every server configuration.

The remote bridge check used the unchanged test assertions with a private
adapter that replaced the connect address and selected `Mirage Test`; it also
asserted the selected channel in every returned state snapshot. Endpoint data
and the adapter remain local. No query administration, server configuration
change, microphone capture or playback was used. All diagnostic clients and
the owned local server were stopped after testing.

Verification after the harness change:

```powershell
python -m unittest discover -s scripts -p 'test_*.py'
python scripts/smoke_bridge.py --binary client/target/debug/ts6-bridge.exe
python scripts/check_repository.py
git diff --check
```

All passed. No Rust source or dependency changed in this follow-up, so the
previous Rust build/test evidence remains separate; no new full workspace test
run is claimed. Rendered UI, real acoustic quality, sustained sessions and the
specific cause of the local handshake refusals remain unverified.

Official research references: [server README](https://github.com/teamspeak/teamspeak6-server/blob/main/README.md)
describes the server as beta and incomplete; [configuration documentation](https://github.com/teamspeak/teamspeak6-server/blob/main/CONFIG.md)
documents the default UDP port and server configuration. Neither substitutes
for the executed interoperability checks above.

## Follow-up: simultaneous voice and chat for sixty seconds

Two CLI `duplex-probe` peers transmitted and received synthetic Opus audio for
sixty seconds in the owner-created test channel. Two independent bridges
joined that channel with audio disabled, counted received packets, and sent
four chat fixtures during the voice interval. Every bridge state used for setup
was checked for the selected channel before starting the tone clients.

The private orchestration used two independent CLI identities with commands of
the following form (the endpoint is deliberately omitted):

```text
ts6-cli duplex-probe <authorized-server>:9987 60 --channel "Mirage Test" --name <diagnostic-peer> --identity <private-test-identity>
```

The four chat fixtures alternated between bridges at approximately five,
twenty, thirty-five and fifty seconds. One read owner continuously consumed
each bridge's WebSocket stream. Both bridges' audio events reported no capture
or playback streams. The CLI tone source uses no microphone or speakers.

| Measurement | Observed result |
| --- | --- |
| CLI peer A | 2,878 frames transmitted; 2,877 audio items received; exit zero |
| CLI peer B | 2,877 frames transmitted; 2,878 audio items received; exit zero |
| Bridge A | 5,755 packets received |
| Bridge B | 5,755 packets received |
| Concurrent chat | Four accepted requests; eight bridge observations, including each independent receiver |
| Recorded failures | No disconnected status, stream termination or chat rejection |

The CLI duplex path exercises the same core `poll_with` pattern, including an
open idle command queue, as the bridge. This experiment establishes concurrent
transport and bridge command handling for this bounded interval. It does not
exercise the bridge's own microphone-to-network path or real playback. The CLI
duplex tone producer sleeps between frames; these counts do not prove an exact
50 Hz source cadence. No jitter distribution, acoustic latency, decoded audio
quality or multi-hour stability measurement was made. The earlier receive-only
diagnostics provide the separate Opus decoding evidence.

All four diagnostic peers were stopped after the experiment. The endpoint,
identities, orchestration and raw/runtime material remain ignored. A minute is
a longer regression interval than the earlier short probes, not certification
of a long-running call.

Source inspection also found that selecting a missing input or output device
falls back to the corresponding default device in `devices.rs`. A nonexistent
output name therefore cannot be used to disable playback. This experiment
avoided that fallback by keeping bridge audio disabled; no device-selection
behavior was changed.

## Team baseline observed after these experiments

A read-only fetch found remote main at
`07b394794cf334b7de0b794bbb0389d24efe2611`, containing the owner's PR #5.
Relative to the prior integration `dcf37b3`, its changes affect native queue
limits, CI and documentation, with no change to `ts6-core/src/session.rs`.
It does not contain the separate `638d1fa` raw-message-batch fix from PR #4;
this is pending work, not evidence that PR #5 reverted that fix.

The owner's updated integration record independently reports a local
`ConnectFailedBanned` after consecutive suites and a successful restart retest.
That evidence supports investigating server admission protection rather than
assuming text delivery failure. Our own generic handshake refusals were not
resolved to that numeric code, so this record keeps that distinction.

The owner also implemented bounded native event/command queues and recorded
additional browser and DPDFNet measurements. Those are attributed to the
owner's integration record, not independently repeated measurements here.
Do not duplicate or replace the native queue work based on the older gap list.
The experiments above ran at `638d1fa`; no live/native validation of the newer
main tree is claimed, and no main update or competing PR merge was performed.

## User-observed official-client channel message

The owner reported not seeing earlier test messages. Those experiments used
the dedicated test channel or direct diagnostic peers, so they did not prove
visibility in the owner's current official-client conversation.

A new independent receiver joined the authorized remote server's default voice
channel. A separate CLI sender selected that same channel and sent this fixture:

```text
Mirage backend delivery test 2026-11-12: channel message, independent receiver verification.
```

The receiving CLI observed the exact text and the sender observed an echo.
The owner then explicitly confirmed that the message appeared in the official
TeamSpeak 6 client's channel chat. This adds a user-observed official-client
interop result to the diagnostic-peer evidence. It does not establish
persistent group/contact chat, which remains unimplemented and was deferred
by the owner. No official-client conversation history or account session was
extracted. Both diagnostic clients were stopped.
