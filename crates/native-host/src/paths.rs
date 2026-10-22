//! Where the host keeps its files, and where browsers look for native messaging hosts.

use std::path::PathBuf;

pub const HOST_NAME: &str = "com.pocketscraper.secure";

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// `PSS_DATA_DIR` overrides it (tests).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PSS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/PocketScraperSecure")
    } else if cfg!(windows) {
        // Per-user and private to the account by default.
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData/Local")).join("PocketScraperSecure")
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

/// Creates (or tightens) an owner-only directory. On Windows the per-user data folder's
/// inherited ACL already limits access to the account.
pub fn private_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes a file atomically: a new, exclusively created temporary file (never a
/// pre-existing file or symlink) with its final mode, then a rename. Refuses to
/// replace a symlink.
pub fn write_private(path: &std::path::Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    if std::fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return Err(std::io::Error::other(format!("refusing to replace symlink {}", path.display())));
    }
    let tmp = path.with_file_name(format!(".{}.{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("file"), pocket_scraper_core::hex(&pocket_scraper_core::random_bytes::<8>())));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let result = options.open(&tmp).and_then(|mut f| {
        f.write_all(content)?;
        f.sync_all()
    });
    // Windows can't rename over an existing file.
    #[cfg(windows)]
    let _ = std::fs::remove_file(path);
    let result = result.and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn installed_binary() -> PathBuf {
    data_dir().join("bin").join(if cfg!(windows) { "pocket-scraper-host.exe" } else { "pocket-scraper-host" })
}

pub fn allowed_id_file() -> PathBuf {
    data_dir().join("allowed-extension-id")
}

/// The registry key (under HKCU) where a browser looks for native messaging hosts on
/// Windows.
pub fn registry_key(browser: &str) -> Option<String> {
    let vendor = match browser {
        "chrome" => r"Google\Chrome",
        "chromium" => "Chromium",
        "brave" => r"BraveSoftware\Brave-Browser",
        "edge" => r"Microsoft\Edge",
        _ => return None,
    };
    Some(format!(r"HKCU\Software\{vendor}\NativeMessagingHosts\{HOST_NAME}"))
}

/// The NativeMessagingHosts folder for a browser, by short name. On Windows the
/// manifest lives in the data folder and the registry points to it.
pub fn manifest_dir(browser: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        registry_key(browser)?;
        return Some(data_dir().join("NativeMessagingHosts").join(browser));
    }
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
