# Current-channel chat

The native and browser frontends use the existing Rust bridge and TS-compatible
text commands to chat in the connected voice channel. This does not implement
persistent group chat or a dedicated private-message conversation view.

## Contract

The bridge advertises `channel_chat: true` in `hello`. Older bridges do not
advertise support, so the composer stays disabled. A new WebSocket requests the
active session's current status and snapshot after subscribing to events.

The `chat` command contains `session_id`, `channel`, `text`, and `request_id`.
Channel IDs are decimal strings to avoid JavaScript integer precision loss in
this new contract. Session IDs identify one connection lifetime; they are
correlation data, not authentication credentials. A previous session's queued
send is rejected even when the next server uses the same channel ID.

The bridge validates session/channel selection and rejects whitespace-only
messages, NUL characters, and text exceeding 1024 UTF-8 bytes. The existing
16 KiB WebSocket limit still applies. Server permissions and flood protection
can reject otherwise valid messages.

`chat_result` echoes `request_id` and contains a nullable English `error` plus a
nullable machine-readable `code`: `not_connected`, `stale_session`,
`stale_channel`, `invalid` or `not_confirmed`. A `not_confirmed` send may still
have reached the server and must not be presented as rejected. Success means
the server accepted the command; it does not independently prove recipient
delivery.

Sender echoes are not universal: isolated beta13.1 runs observed them, while a
manual test on another TS6 server saw the message arrive in the official client
without an echo to the sender. The browser frontend renders `chat` events only,
so without an echo it shows no local copy. The native frontend shows its own
copy as pending, resolves it by `request_id`, and treats a matching echo within
30 seconds as confirmation rather than a second message. Received channel events
include the channel, sender name, text, self-echo flag, and timestamp. Core
messages retain their original scope; private/server/poke messages do not appear
in the channel conversation.
If the library falls back to a raw batched text notification, each supported
record is preserved in order; unknown targets and client messages without a
target ID are skipped instead of being assigned a fabricated client ID.

Drafts stay editable after a rejected send. During a pending send they are
disabled; after 20 seconds without confirmation the UI warns the user to check
the conversation before retrying. Requests are not automatically retried.
This is not an exactly-once or offline-delivery guarantee. Switching channel or
session resets the composer. Frontend history is limited to 200 received
messages in memory and is not restored after page/application reload.

## Verification

For the integrated revision's later live checks, including connection-stage
failures across repeated runs, see [integrated live validation](live-validation-integrated.md).
The historical results below do not establish a consistently passing integrated
connection/reconnect sequence.

Against an isolated loopback TeamSpeak Server 6.0.0-beta13.1 instance:

- `scripts/check_channel_chat.py` exercised two independent bridge identities,
  bidirectional delivery, one sender echo per fixture, escaping/Unicode, and
  invalid channel/content/offline/previous-session rejection. Audio stayed off.
- Installed Python Playwright/Chromium exercised two browser pages, Send and
  Enter, literal script-like text, oversize draft retention, and sending after
  page reload. The initial reload failure was reproduced and corrected.
- Desktop and compact browser screenshots were inspected. Native wire types
  have serialization/parsing coverage; native live chat interaction was not
  freshly exercised through its rendered window.

The browser test used `scripts/check_chat_browser.py` with the installed Python
Playwright/Chromium runtime because no Browser plugin was available. Private profiles and captures remain under ignored `.audit-local`.
Do not publish those identities or raw captures. No official-client equivalence,
long-running load, arbitrary server-version support, or acoustic test is claimed.

Reproduce the rendered check with a trusted server already running:

```powershell
python scripts/check_chat_browser.py --binary .audit-local/full-audit-target/debug/ts6-bridge.exe --server-port 19987
```

This optional check requires Playwright and its Chromium runtime to be installed
separately. It does not install dependencies or start a TeamSpeak server.

Final local checks: 77 Rust workspace tests, workspace build, strict first-party
Clippy, explicit-package formatting, repository policy check, UI build, and npm
audit passed. The smoke regression was updated to assert disconnected `status`
events for refresh, replacing its prior log-only expectation; rejection of
oversized WebSocket input remains checked. Vendored compiler warnings remain.
One combined back-to-back wire/CLI run produced a CLI harness failure. The
standalone CLI recheck against a restarted test server passed channel and direct
reception plus both sender echoes. No cause is assigned to that combined-run
failure; reconnect/load/flood behavior needs broader investigation.
