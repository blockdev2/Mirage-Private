//! Puts the Mirage icon into the Windows executable (Explorer, shortcuts,
//! the installer's shortcuts). Uses the Windows SDK's `rc.exe`, so no extra
//! crate is needed; without the SDK the build goes on with no file icon.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target != "windows" || env != "msvc" {
        return;
    }
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found: the executable gets no icon");
        return;
    };
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let icon = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("manifest"))
        .join("assets")
        .join("icon.ico");
    let script = out.join("mirage.rc");
    let res = out.join("mirage.res");
    let path = icon.display().to_string().replace('\\', "\\\\");
    std::fs::write(&script, format!("1 ICON \"{path}\"\n")).expect("write rc");
    let status = std::process::Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(&script)
        .status();
    match status {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg-bins={}", res.display());
        }
        _ => println!("cargo:warning=rc.exe failed: the executable gets no icon"),
    }
}

/// `rc.exe` on PATH, or the newest x64 one in the Windows 10/11 SDK.
fn find_rc() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let kits = std::path::Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<_> = std::fs::read_dir(kits)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("x64").join("rc.exe"))
        .filter(|path| path.is_file())
        .collect();
    versions.sort();
    versions.pop()
}
