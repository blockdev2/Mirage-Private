#[cfg(windows)]
mod encoder;
#[cfg(windows)]
mod fixture;
#[cfg(windows)]
mod mft;
#[cfg(windows)]
mod native;

const USAGE: &str = "usage: mirage-native-capture-probe [--codecs | --encode <new.mp4> | \
--stream <new-dir> | --rtp-send <127.0.0.1:port> <h264|hevc|av1> <frames> [drop-packet-index] | \
--rtp-recv <port> <h264|hevc|av1> <frames> <new-dir>]";

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use anyhow::Context;
        use mirage_native_capture_probe::bitstream::Codec;

        let args: Vec<String> = std::env::args().skip(1).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let codec = |name: &str| Codec::parse(name).context("codec must be h264, hevc or av1");
        let mode = match args.as_slice() {
            [] => native::Mode::Capture,
            ["--codecs"] => return mft::codecs(),
            ["--encode", path] => native::Mode::Mp4(path.into()),
            ["--stream", directory] => native::Mode::Stream(directory.into()),
            ["--rtp-send", destination, name, frames, rest @ ..] if rest.len() <= 1 => {
                native::Mode::RtpSend {
                    destination: destination.parse().context("destination must be ip:port")?,
                    codec: codec(name)?,
                    frames: frames.parse().context("frames must be a number")?,
                    drop_packet: rest
                        .first()
                        .map(|n| n.parse().context("drop index must be a number"))
                        .transpose()?,
                }
            }
            ["--rtp-recv", port, name, frames, directory] => {
                let report = mft::rtp_receive(
                    codec(name)?,
                    port.parse().context("port must be a number")?,
                    frames.parse().context("frames must be a number")?,
                    std::path::Path::new(directory),
                )?;
                println!("{report}");
                return Ok(());
            }
            _ => anyhow::bail!(USAGE),
        };
        native::run(mode)
    }
    #[cfg(not(windows))]
    anyhow::bail!("this native capture probe requires Windows")
}
