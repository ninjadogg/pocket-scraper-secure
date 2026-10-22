//! Where the host keeps its files, and where browsers look for native messaging hosts.

use std::path::PathBuf;

pub const HOST_NAME: &str = "com.pocketscraper.secure";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// `PSS_DATA_DIR` overrides it (tests).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PSS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/PocketScraperSecure")
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("pocket-scraper-secure")
    }
}

/// One browser profile per site, named by a salted hash of the origin. Creates the
/// owner-only folders (and the per-install salt) on first use.
pub fn profile_dir(origin: &str) -> std::io::Result<PathBuf> {
    let data = data_dir();
    private_dir(&data)?;
    let profiles = data.join("profiles");
    private_dir(&profiles)?;
    let salt_file = data.join("profile-salt");
    let salt = match std::fs::read(&salt_file) {
        Ok(s) if s.len() == 32 => s,
        _ => {
            let salt = pocket_scraper_core::random_bytes::<32>().to_vec();
            write_private(&salt_file, &salt, 0o600)?;
            salt
        }
    };
    Ok(profiles.join(pocket_scraper_core::url::site_key(&salt, origin)))
}

/// Creates (or tightens) an owner-only directory.
pub fn private_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Writes a file atomically: a new, exclusively created temporary file (never a
/// pre-existing file or symlink) with its final mode, then a rename. Refuses to
/// replace a symlink.
pub fn write_private(path: &std::path::Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if std::fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return Err(std::io::Error::other(format!("refusing to replace symlink {}", path.display())));
    }
    let tmp = path.with_file_name(format!(".{}.{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("file"), pocket_scraper_core::hex(&pocket_scraper_core::random_bytes::<8>())));
    let result = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp).and_then(|mut f| {
        f.write_all(content)?;
        f.sync_all()
    });
    let result = result.and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn installed_binary() -> PathBuf {
    data_dir().join("bin").join("pocket-scraper-host")
}

pub fn allowed_id_file() -> PathBuf {
    data_dir().join("allowed-extension-id")
}

/// The NativeMessagingHosts folder for a browser, by short name.
pub fn manifest_dir(browser: &str) -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        let support = home().join("Library/Application Support");
        match browser {
            "chrome" => support.join("Google/Chrome"),
            "chromium" => support.join("Chromium"),
            "brave" => support.join("BraveSoftware/Brave-Browser"),
            "edge" => support.join("Microsoft Edge"),
            _ => return None,
        }
    } else {
        let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
        match browser {
            "chrome" => config.join("google-chrome"),
            "chromium" => config.join("chromium"),
            "brave" => config.join("BraveSoftware/Brave-Browser"),
            "edge" => config.join("microsoft-edge"),
            _ => return None,
        }
    };
    Some(base.join("NativeMessagingHosts"))
}
