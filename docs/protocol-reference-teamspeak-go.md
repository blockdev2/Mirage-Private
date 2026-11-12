# TeamSpeak Go reference review

Review date: 2026-11-12. This is protocol research for Mirage, not a proposal to
replace its Rust transport or copy another client wholesale.

## Provenance and scope

- Repository: https://github.com/honeybbq/teamspeak-go
- Inspected commit: `e7fd092a341b2ac7e6f68adf7a3070a3268cb9f3`.
- [Pinned README](https://github.com/honeybbq/teamspeak-go/blob/e7fd092a341b2ac7e6f68adf7a3070a3268cb9f3/README.md)
  describes an independent client implementation, not a fork of TeamSpeak's
  proprietary desktop client. This description is the project's own claim.
- [Pinned license](https://github.com/honeybbq/teamspeak-go/blob/e7fd092a341b2ac7e6f68adf7a3070a3268cb9f3/LICENSE)
  is MIT. Preserve attribution if any substantial implementation is later
  adapted; this review adds links and original analysis, not third-party code.
- The default pkg.go.dev page displayed v0.2.0. The pinned README recommends
  v1.0.0-beta.25 and explains why an unversioned Go install selects the older
  stable version. Do not combine the old root-level API examples with the newer
  `client`, `identity`, and `stream` packages. The version-specific pkg.go.dev
  page could not be retrieved during this review; the pinned source is the
  evidence for the newer API. `go.mod` requires Go 1.26.0.

Inventory covered 355 tracked reference files. The review read the entrypoint,
license, dependency manifest, selected workflow configuration, and code/docs/tests
across commands, events, capabilities, voice, transport, wire parsing, S3, and
streams. It is not a line-by-line security certification of every file. No Go
compiler was available on PATH, and no Go tests, install scripts, generators,
container fixtures, or reference binaries were executed. No proprietary binary
was decompiled in this review: the open source resolves the questions examined.

## Server compatibility target

Mirage must interoperate with official TeamSpeak 6 servers. The
[teamspeak/teamspeak6-server repository](https://github.com/teamspeak/teamspeak6-server)
is TeamSpeak's beta-server distribution/configuration repository, not evidence of
a separate open-source server implementation. Record the actual binary release,
platform, enabled extensions, permissions, and test ordering for each experiment.
Do not infer that every hosted TS6 server behaves like a local beta13.1 fixture.

The Go source reports experiments on TS6 beta13.1 and TS3 3.13.8. Those are
upstream observations, not Mirage test results. A successful Go-only exchange
also cannot prove that Mirage interoperates with an official desktop peer.

## Findings mapped to Mirage

| Area | Pinned reference files | Application to Mirage | Evidence needed before changing behavior |
| --- | --- | --- | --- |
| Flood and reconnect | `backoff.go`, `docs/flood-control.md`, `client/reconnect.go`, `client/antiflood_integration_test.go` | Separate connection admission failures from command throttling; pace optional commands while keeping the event stream alive | Capture sanitized numeric error and server wait, command class, timing, and version; do not diagnose a timeout as flood |
| Response correlation | `client/commands.go`, `client/responses.go`, `internal/servererr/servererr.go` | Preserve return-code correlation and distinguish pushed notifications from command answers | Interleave state/chat/voice with multiple command results, delayed rows, timeout, and disconnect |
| Capability discovery | `client/capabilities.go` | Record extensions from the handshake before experimenting with TS6-specific commands | Verify the actual initserver fields and the vendored Rust model; unknown capabilities are not permission grants |
| Identity association | `client/capabilities.go`, `identity` | Upstream distinguishes TS3 SHA-1 and TS6 SHA-256 server-reported client UID behavior | Compare actual server-reported UID and identity association; do not replace P-256 identity or RustCrypto code based on a comment |
| Voice lifetime | `client/voice_receiver.go`, `client/voice_receiver_integration_test.go` | Bound per-peer queues, count loss, handle sequence wrap and session changes | Listener-first/late-join, two speakers, wrap/reorder/loss, reconnect, and independent receiver evidence |
| Wire parsing | `internal/wire/wire.go`, `internal/wire/fuzz_test.go` | Generate Mirage-owned cases for inherited batch fields, single unescape, Unicode values, tabs, and bounded amplification | Compare real wire interpretation before modifying vendored parsing; current Rust parser treats some ASCII whitespace as separators |
| Streams | `stream/wire.go`, `stream/streams.go`, `stream/peer.go`, `stream/e2e/e2e_test.go` | A source-backed path beyond Mirage's setupstream-only diagnostic | Capability check, command/event state machine, authorization, cancellation, official-peer observation, then media |
| Files and S3 | `client/s3.go`, `client/s3_integration_test.go`, `client/s3_avatars.go` | Keep classic transfer and S3 control flows separate; signed URLs are credentials | Controlled HTTPS storage, URL/destination restrictions, redirects, byte limits, cancellation, and completion semantics |
| Test architecture | `client/conn_seam_test.go`, `internal/transport/packetconn_test.go`, `.github/workflows/ci.yml` | Reproducible transport seams and optional integration suites can accelerate diagnosis | Review downloads, fixtures, credentials, hooks and runner privilege before adopting a workflow |

These paths are relative to the inspected commit. Use this
[pinned tree](https://github.com/honeybbq/teamspeak-go/tree/e7fd092a341b2ac7e6f68adf7a3070a3268cb9f3)
when following them, not a moving main branch.

## Concrete stream leads

[stream/wire.go](https://github.com/honeybbq/teamspeak-go/blob/e7fd092a341b2ac7e6f68adf7a3070a3268cb9f3/stream/wire.go)
models `setupstream`, `joinstreamrequest`, `respondjoinstreamrequest`,
`streamsignaling`, `requeststreaminfo`, and `stopstream`. It provides field-level
examples and bounded encoded-command checks. Its JSON envelopes use `cmd` and
`args`, with offer/answer strings and an iceCandidate payload containing `sdp`,
`mid`, and `mLine`. Treat these as leads to validate on the target release,
not an official protocol guarantee or permission to forward arbitrary payloads.

[Stream documentation](https://github.com/honeybbq/teamspeak-go/blob/e7fd092a341b2ac7e6f68adf7a3070a3268cb9f3/docs/streams.md)
describes STREAM_P2P capability gating, per-viewer WebRTC connections and
publisher-side admission. It reports a manual official-client beta5/macOS
observation separately from server integration tests. The e2e module uses a
separate WebRTC dependency; the signalling package alone does not encode video,
capture a screen, supply a codec, or establish a working media call in Mirage.

The reference's command-size measurements are release-specific. Do not transplant
9150 or its reported boundary as a universal TeamSpeak limit. Bound escaped UTF-8
bytes, including correlation fields, and test boundary behavior independently.

## Flood investigation, without a premature conclusion

[The flood-control notes](https://github.com/honeybbq/teamspeak-go/blob/e7fd092a341b2ac7e6f68adf7a3070a3268cb9f3/docs/flood-control.md)
distinguish command error 524 from connection bans discussed in `backoff.go`.
They also clarify that the current SDK exposes middleware recipes rather than
silently pacing/retrying every command. Older documentation describes a built-in
limiter; do not assume that older behavior exists in the inspected API.

Mirage's previous back-to-back integration failure remains unexplained. The Go
observations make flood/admission control a useful hypothesis, not a diagnosis.
Use one isolated server, record attempt ordering, and identify the returned code
before adding any retry. A timed-out mutation may already have succeeded. Never
blindly resend chat, channel creation, file completion, or moderation commands.
Disconnect and network progress must not be trapped behind a sleeping limiter.

## Proposed implementation sequence

1. Add sanitized, typed command-failure classification and session-scoped
   diagnostics. Keep user content, passwords, identity material and signed URLs
   out of logs. Preserve the existing `!Send` session ownership.
2. Build a protocol fixture matrix using independently authored inputs. Cover
   correlated results, notifications received during waits, batch inheritance,
   escaping, cancellation, and bounded buffers. Preserve existing behavior until
   a regression and server evidence justify a change.
3. After a reviewed Go toolchain/runtime setup, run cross-implementation tests:
   Mirage sender to Go receiver and Go sender to Mirage receiver, each against
   an official local TS6 release. Use fresh protected identities, controlled
   timing and no microphone. Do not execute the reference with team credentials
   or inherit a full private environment.
4. Verify returned capabilities and add a CLI-only stream signalling experiment
   before choosing a WebRTC/media dependency or modifying a frontend. Keep
   admission decisions explicit and reject malformed/oversized SDP and ICE.
5. Investigate S3 separately with isolated HTTPS storage and restrictive outbound
   policy. Do not weaken Mirage's private-destination guard globally to make a
   presigned-URL test pass.
6. For every stage publish a focused PR with its base/head SHAs and evidence;
   let the owner compare it with overlapping team submissions before integration.

## Required interoperability evidence

| Pairing | Minimum checks | Status of this review |
| --- | --- | --- |
| Mirage / official local TS6 beta release | identity, handshake, state, channel/direct text, independent Opus reception | Existing Mirage evidence is documented separately; not rerun here |
| Mirage / official hosted deployment | actual target version, permitted operations, capabilities and network conditions | Not tested; requires a suitable authorized endpoint |
| Mirage / Go peer through official TS6 | both send directions, two speakers, reconnect and error propagation | Planned; Go runtime unavailable during review |
| Mirage / official desktop peer through TS6 | selected text/voice/stream behavior with sanitized ordering and versions | Not newly tested |
| TS3 reference server | optional negative-capability and regression comparison | Not a substitute for the TS6 acceptance target |

## Follow-up reverse engineering

Start from a specific unanswered interoperability question and exhaust the
version-pinned open sources and controlled protocol tests first. If inspecting
an installed official binary becomes necessary, record its version/hash,
question, tools, offsets and sanitized observations locally. Do not commit
proprietary code, extracted resources, identities, tokens, raw memory, packet
captures, or account data. Shared documentation should contain original protocol
notes and reproducible synthetic fixtures with provenance, not decompiled code.
Do not change authentication or signature checks merely to obtain a green test.
