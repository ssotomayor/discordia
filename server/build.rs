//! Make a `livekit-server` binary available at `$OUT_DIR/livekit-server` so
//! it can be `include_bytes!`'d into the dioxusfun binary for self-host mode.
//!
//! - Linux / Windows: downloads the release archive from GitHub.
//! - macOS: LiveKit doesn't publish darwin binaries on GitHub anymore (they
//!   ship via Homebrew, which builds from Go source). We do the same — clone
//!   the repo and `go build`. Requires `go` on PATH (`brew install go`).
//!
//! Overrides:
//! - `LIVEKIT_BUNDLE_VERSION=1.12.0` to pin a specific release
//! - `LIVEKIT_BUNDLE_SKIP=1` to skip entirely (writes an empty stub; the
//!   runtime treats voice as unavailable in this build)

use std::env;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_VERSION: &str = "1.12.0";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LIVEKIT_BUNDLE_VERSION");
    println!("cargo:rerun-if-env-changed=LIVEKIT_BUNDLE_SKIP");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let bin_name = if target_os == "windows" {
        "livekit-server.exe"
    } else {
        "livekit-server"
    };
    let bin_path = out_dir.join(bin_name);

    if env::var("LIVEKIT_BUNDLE_SKIP").is_ok() {
        println!(
            "cargo:warning=LIVEKIT_BUNDLE_SKIP set — self-host voice will not work in this build"
        );
        fs::write(&bin_path, b"").unwrap();
        return;
    }

    if bin_path.exists()
        && fs::metadata(&bin_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    {
        return;
    }

    let version = env::var("LIVEKIT_BUNDLE_VERSION").unwrap_or_else(|_| DEFAULT_VERSION.into());
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let result = match target_os.as_str() {
        "macos" => build_from_source(&out_dir, &version, &bin_path),
        _ => download_release(&target_os, &target_arch, &version, &bin_path),
    };

    if let Err(e) = result {
        println!(
            "cargo:warning=livekit bundle failed: {e} — self-host voice will not work in this build; set LIVEKIT_BUNDLE_SKIP=1 to silence"
        );
        fs::write(&bin_path, b"").unwrap();
    }

    #[cfg(unix)]
    if bin_path.exists() {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin_path, perms).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Linux / Windows: download prebuilt
// ---------------------------------------------------------------------------

fn download_release(
    target_os: &str,
    target_arch: &str,
    version: &str,
    bin_path: &Path,
) -> Result<(), String> {
    let (platform_str, archive_ext) = match (target_os, target_arch) {
        ("linux", "x86_64") => ("linux_amd64", "tar.gz"),
        ("linux", "aarch64") => ("linux_arm64", "tar.gz"),
        ("linux", "arm") => ("linux_armv7", "tar.gz"),
        ("windows", "x86_64") => ("windows_amd64", "zip"),
        ("windows", "aarch64") => ("windows_arm64", "zip"),
        _ => return Err(format!("no prebuilt for {target_os}/{target_arch}")),
    };

    let url = format!(
        "https://github.com/livekit/livekit/releases/download/v{version}/livekit_{version}_{platform_str}.{archive_ext}"
    );
    println!("cargo:warning=downloading {url}");

    let bytes = download_bytes(&url)?;
    let binary = match archive_ext {
        "tar.gz" => extract_tar_gz(&bytes, "livekit-server")?,
        "zip" => extract_zip(&bytes, "livekit-server.exe")?,
        _ => unreachable!(),
    };
    fs::write(bin_path, &binary).map_err(|e| format!("write: {e}"))?;
    Ok(())
}

fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::get(url).call().map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    Ok(body)
}

fn extract_tar_gz(bytes: &[u8], target: &str) -> Result<Vec<u8>, String> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.to_path_buf();
        if path.file_name().and_then(|n| n.to_str()) == Some(target) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            return Ok(buf);
        }
    }
    Err(format!("{target} not found in tar archive"))
}

fn extract_zip(bytes: &[u8], target: &str) -> Result<Vec<u8>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        if file.name().ends_with(target) {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            return Ok(buf);
        }
    }
    Err(format!("{target} not found in zip archive"))
}

// ---------------------------------------------------------------------------
// macOS: clone + `go build`
// ---------------------------------------------------------------------------

fn build_from_source(out_dir: &Path, version: &str, bin_path: &Path) -> Result<(), String> {
    let go = find_executable("go").ok_or_else(|| {
        "go not found in PATH or common install dirs. `brew install go`, or set LIVEKIT_BUNDLE_SKIP=1 to skip voice bundling.".to_string()
    })?;
    let git = find_executable("git").ok_or_else(|| {
        "git not found. Install Xcode CLT (`xcode-select --install`) or `brew install git`."
            .to_string()
    })?;

    let src_dir = out_dir.join("livekit-src");
    if !src_dir.exists() {
        println!("cargo:warning=cloning livekit v{version} source (~50MB, one-time)");
        let status = Command::new(&git)
            .args(["clone", "--depth", "1", "--branch"])
            .arg(format!("v{version}"))
            .arg("https://github.com/livekit/livekit")
            .arg(&src_dir)
            .status()
            .map_err(|e| format!("git clone: {e}"))?;
        if !status.success() {
            let _ = fs::remove_dir_all(&src_dir);
            return Err(format!("git clone of v{version} failed"));
        }
    }

    println!("cargo:warning=building livekit-server from source (~2-3 min one-time)");
    let status = Command::new(&go)
        .current_dir(&src_dir)
        .args(["build", "-ldflags", "-s -w", "-o"])
        .arg(bin_path)
        .arg("./cmd/server")
        .status()
        .map_err(|e| format!("go build: {e}"))?;
    if !status.success() {
        return Err("go build failed (see output above)".into());
    }
    if !bin_path.exists() {
        return Err("go build succeeded but produced no binary".into());
    }
    Ok(())
}

/// Look for a binary in PATH first, then in common Homebrew / official Go /
/// Xcode CLT install locations on macOS and Linux. Build scripts often run
/// with a minimal PATH (especially under `dx serve` or IDE launchers), so PATH
/// alone isn't enough.
fn find_executable(name: &str) -> Option<PathBuf> {
    // 1. PATH lookup via the shell helper, if it works.
    if Command::new(name).arg("--version").output().is_ok() {
        return Some(PathBuf::from(name));
    }
    if Command::new(name).arg("version").output().is_ok() {
        return Some(PathBuf::from(name));
    }

    // 2. Common install dirs.
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("/opt/homebrew/bin").join(name), // Apple Silicon brew
        PathBuf::from("/usr/local/bin").join(name),    // Intel brew + many Linux
        PathBuf::from("/usr/bin").join(name),          // system
        PathBuf::from("/usr/local/go/bin").join(name), // official Go installer
    ];
    if let Ok(home) = env::var("HOME") {
        candidates.push(PathBuf::from(&home).join("go/bin").join(name));
        candidates.push(PathBuf::from(&home).join(".cargo/bin").join(name));
    }
    candidates.into_iter().find(|p| p.exists() && p.is_file())
}
