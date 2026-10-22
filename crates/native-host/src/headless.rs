//! The runs' own browser: Google's Chrome for Testing headless shell, pinned by
//! version and SHA-256 in `browser/headless-shell.json`. It is separate from the
//! person's Chrome (which it never starts or touches), never updates itself, and
//! needs no display, so the same runs work on desktops, VMs and servers.

use crate::paths::{self, private_dir};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

const LOCK: &str = include_str!("../../../browser/headless-shell.json");

#[derive(Deserialize)]
struct Lock {
    version: String,
    base_url: String,
    platforms: BTreeMap<String, Pin>,
}

#[derive(Deserialize, Clone)]
pub struct Pin {
    pub sha256: String,
    pub size: u64,
}

fn lock() -> Lock {
    serde_json::from_str(LOCK).expect("browser/headless-shell.json is valid")
}

pub fn version() -> String {
    lock().version
}

/// Chrome for Testing's name for this machine, if it builds for it.
pub fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("mac-arm64"),
        ("macos", "x86_64") => Some("mac-x64"),
        ("linux", "x86_64") => Some("linux64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("windows", "x86_64") => Some("win64"),
        _ => None,
    }
}

fn executable(dir: &Path, platform: &str) -> PathBuf {
    let name = if cfg!(windows) { "chrome-headless-shell.exe" } else { "chrome-headless-shell" };
    dir.join(format!("chrome-headless-shell-{platform}")).join(name)
}

fn version_dir() -> PathBuf {
    paths::data_dir().join("browser").join(version())
}

/// The installed headless shell for the pinned version, if present.
pub fn installed() -> Option<PathBuf> {
    let exe = executable(&version_dir(), platform()?);
    exe.is_file().then_some(exe)
}

/// Downloads, verifies and unpacks the pinned headless shell (or reuses it).
pub fn install(progress: &mut dyn FnMut(&str)) -> Result<PathBuf, String> {
    if let Some(exe) = installed() {
        return Ok(exe);
    }
    let lock = lock();
    let platform = platform().ok_or("Chrome for Testing has no headless shell for this platform")?;
    let pin = lock.platforms.get(platform).ok_or("No pinned headless shell for this platform")?.clone();
    let root = paths::data_dir().join("browser");
    private_dir(&paths::data_dir()).and_then(|_| private_dir(&root)).map_err(|e| e.to_string())?;
    let tag = pocket_scraper_core::hex(&pocket_scraper_core::random_bytes::<6>());
    let zip_path = root.join(format!(".download-{tag}.zip"));
    let staging = root.join(format!(".staging-{tag}"));
    let url = format!("{}/{}/{platform}/chrome-headless-shell-{platform}.zip", lock.base_url, lock.version);
    progress(&format!("Downloading chrome-headless-shell {} for {platform} ({} MB)…", lock.version, pin.size / 1_000_000));
    let result = (|| -> Result<PathBuf, String> {
        download(&url, &zip_path, pin.size)?;
        verify(&zip_path, &pin)?;
        progress("Verified the download against its pinned SHA-256.");
        unpack(&zip_path, &staging)?;
        let target = version_dir();
        let _ = fs::remove_dir_all(&target);
        fs::rename(&staging, &target).map_err(|e| format!("Could not install the browser: {e}"))?;
        Ok(executable(&target, platform))
    })();
    let _ = fs::remove_file(&zip_path);
    let _ = fs::remove_dir_all(&staging);
    let exe = result?;
    if !exe.is_file() {
        return Err("The downloaded archive did not contain chrome-headless-shell".into());
    }
    // Older pinned versions are no longer used.
    if let Ok(entries) = fs::read_dir(&root) {
        for e in entries.flatten() {
            let name = e.file_name();
            if name.to_string_lossy() != lock.version && !name.to_string_lossy().starts_with('.') && e.path().is_dir() {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(exe)
}

/// HTTPS only, failing on HTTP errors, refusing anything larger than the pin.
fn download(url: &str, to: &Path, size: u64) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error", "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2"])
        .args(["--max-filesize", &(size + 1).to_string(), "--output"])
        .arg(to)
        .arg(url)
        .status()
        .map_err(|e| format!("Could not run curl: {e}"))?;
    if !status.success() {
        return Err(format!("Download failed ({status}): {url}"));
    }
    Ok(())
}

/// Checks size and SHA-256 against the pin.
pub fn verify(path: &Path, pin: &Pin) -> Result<(), String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len != pin.size {
        return Err(format!("The download is {len} bytes, expected {}", pin.size));
    }
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got = pocket_scraper_core::hex(&hasher.finalize());
    if got != pin.sha256 {
        return Err(format!("The download's SHA-256 is {got}, expected {}. Nothing was installed.", pin.sha256));
    }
    Ok(())
}

/// Unpacks with paths confined to `to` (entries escaping it are refused) and keeps
/// executable bits.
fn unpack(zip_path: &Path, to: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(File::open(zip_path).map_err(|e| e.to_string())?).map_err(|e| format!("Not a zip archive: {e}"))?;
    private_dir(to).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(format!("The archive has an unsafe path: {}", entry.name()));
        };
        let out = to.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if entry.is_symlink() {
            return Err(format!("The archive has an unexpected symlink: {}", entry.name()));
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = File::create(&out).map_err(|e| e.to_string())?;
        io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if entry.unix_mode().is_some_and(|m| m & 0o111 != 0) { 0o700 } else { 0o600 };
            fs::set_permissions(&out, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_pins_every_supported_platform() {
        let lock = lock();
        for p in ["mac-arm64", "mac-x64", "linux64", "linux-arm64", "win64"] {
            let pin = &lock.platforms[p];
            assert_eq!(pin.sha256.len(), 64, "{p}");
            assert!(pin.size > 50_000_000, "{p}");
        }
        assert!(lock.base_url.starts_with("https://"));
    }

    #[test]
    fn verification_refuses_wrong_size_or_hash() {
        let dir = std::env::temp_dir().join(format!("pss-verify-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.zip");
        fs::write(&f, b"hello").unwrap();
        let good = Pin { sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into(), size: 5 };
        assert!(verify(&f, &good).is_ok());
        assert!(verify(&f, &Pin { size: 6, ..good.clone() }).unwrap_err().contains("expected 6"));
        assert!(verify(&f, &Pin { sha256: "0".repeat(64), ..good }).unwrap_err().contains("Nothing was installed"));
        let _ = fs::remove_dir_all(&dir);
    }
}
