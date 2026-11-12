#[cfg(windows)]
mod encoder;
#[cfg(windows)]
mod fixture;
#[cfg(windows)]
mod native;

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let mut args = std::env::args_os().skip(1);
        let output = match args.next() {
            None => None,
            Some(flag) if flag == "--encode" => {
                Some(std::path::PathBuf::from(args.next().ok_or_else(|| {
                    anyhow::anyhow!("--encode requires a new private MP4 path")
                })?))
            }
            Some(_) => anyhow::bail!("expected --encode <new-private-output.mp4> or no arguments"),
        };
        anyhow::ensure!(args.next().is_none(), "unexpected extra argument");
        native::run(output.as_deref())
    }
    #[cfg(not(windows))]
    anyhow::bail!("this native capture probe requires Windows")
}
