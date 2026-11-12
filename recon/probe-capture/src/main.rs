#[cfg(windows)]
mod encoder;
#[cfg(windows)]
mod fixture;
#[cfg(windows)]
mod mft;
#[cfg(windows)]
mod native;

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let mut args = std::env::args_os().skip(1);
        let mode = match args.next() {
            None => native::Mode::Capture,
            Some(flag) if flag == "--encode" => {
                native::Mode::Mp4(std::path::PathBuf::from(args.next().ok_or_else(|| {
                    anyhow::anyhow!("--encode requires a new private MP4 path")
                })?))
            }
            Some(flag) if flag == "--stream" => {
                native::Mode::Stream(std::path::PathBuf::from(args.next().ok_or_else(|| {
                    anyhow::anyhow!("--stream requires a new private directory")
                })?))
            }
            Some(flag) if flag == "--codecs" => {
                anyhow::ensure!(args.next().is_none(), "unexpected extra argument");
                return mft::codecs();
            }
            Some(_) => anyhow::bail!(
                "expected no arguments, --codecs, --encode <new.mp4> or --stream <new-directory>"
            ),
        };
        anyhow::ensure!(args.next().is_none(), "unexpected extra argument");
        native::run(mode)
    }
    #[cfg(not(windows))]
    anyhow::bail!("this native capture probe requires Windows")
}
