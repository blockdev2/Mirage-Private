//! Standalone `ts6-bridge` executable. The same code is also linked into the
//! desktop client, which hosts the bridge in-process; see `lib.rs`.
//!
//! Run:  cargo run -p ts6-bridge -- [--port 8787] [--ui <dir>]

fn main() -> anyhow::Result<()> {
    ts6_bridge::main_entry()
}
