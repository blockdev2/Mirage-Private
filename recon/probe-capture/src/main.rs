#[cfg(windows)]
mod devices;
#[cfg(windows)]
mod encoder;
#[cfg(windows)]
mod fixture;
#[cfg(windows)]
mod mft;
#[cfg(windows)]
mod native;

const USAGE: &str = "usage: mirage-native-capture-probe [--codecs | --devices | --encode <new.mp4> | \
--stream <new-dir> | \
--rtp-send <127.0.0.1:port> <h264|hevc|av1> <frames> [--backend auto|hardware|software] \
[--adapter auto|high-performance|minimum-power|<index>] [--drop <packet-index>] | \
--rtp-recv <port> <h264|hevc|av1> <frames> <new-dir>]";

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use anyhow::{Context, bail};
        use mirage_native_capture_probe::{
            bitstream::Codec,
            selection::{AdapterChoice, Backend},
        };

        let args: Vec<String> = std::env::args().skip(1).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let codec = |name: &str| Codec::parse(name).context("codec must be h264, hevc or av1");
        let mode = match args.as_slice() {
            [] => native::Mode::Capture,
            ["--codecs"] => return mft::codecs(),
            ["--devices"] => {
                println!("{}", mft::devices_report()?);
                return Ok(());
            }
            ["--encode", path] => native::Mode::Mp4(path.into()),
            ["--stream", directory] => native::Mode::Stream(directory.into()),
            ["--rtp-send", destination, name, frames, options @ ..] => {
                let mut preference = mft::Preference::default();
                let mut drop_packet = None;
                let mut rest = options.iter();
                while let Some(flag) = rest.next() {
                    let value = rest
                        .next()
                        .with_context(|| format!("{flag} needs a value"))?;
                    match *flag {
                        "--backend" => {
                            preference.backend =
                                Backend::parse(value).context("backend: auto|hardware|software")?
                        }
                        "--adapter" => {
                            preference.adapter = AdapterChoice::parse(value)
                                .context("adapter: auto|high-performance|minimum-power|<index>")?
                        }
                        "--drop" => drop_packet = Some(value.parse().context("drop index")?),
                        _ => bail!("unknown option {flag}\n{USAGE}"),
                    }
                }
                native::Mode::RtpSend(mft::SendRequest {
                    destination: destination.parse().context("destination must be ip:port")?,
                    codec: codec(name)?,
                    frames: frames.parse().context("frames must be a number")?,
                    drop_packet,
                    preference,
                })
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
            _ => bail!(USAGE),
        };
        native::run(mode)
    }
    #[cfg(not(windows))]
    anyhow::bail!("this native capture probe requires Windows")
}
