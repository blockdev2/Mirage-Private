//! Windows screen capture and GPU H.264 encoding for TeamSpeak 6 streams.
//!
//! Windows Graphics Capture frames are scaled and converted on the GPU and
//! encoded by a hardware Media Foundation transform into Annex-B access
//! units, with no CPU readback of the picture. The units are the input of a
//! `ts6-stream` publisher. Only hardware encoders are used.
//!
//! The pure helpers (`config`, `nal`) build everywhere; the GPU parts exist
//! on Windows only.

pub mod config;
pub mod nal;

#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod convert;
#[cfg(windows)]
mod encoder;

pub use config::{fit_size, CaptureConfig, EncodedUnit};

#[cfg(windows)]
pub use capture::ScreenCapture;
