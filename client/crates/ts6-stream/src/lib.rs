//! Native WebRTC peers for TeamSpeak 6 streams.
//!
//! The peers never touch the TeamSpeak session: they take and produce SDP
//! strings and [`Signal`] values, and the caller relays those through the
//! `respondjoinstreamrequest` and `streamsignaling` commands. Media enters as
//! already-encoded Annex-B H.264, so the encoder can change without touching
//! the transport.

mod peer;
mod signal;

pub use peer::{configuration, Publisher, Viewer, STUN_SERVERS};
pub use signal::{Signal, SignalError, MAX_SIGNAL_BYTES};
