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

/// One browser profile per site; the folder name is a hash of the origin.
pub fn profile_dir(origin: &str) -> PathBuf {
    data_dir().join("profiles").join(pocket_scraper_core::url::site_key(origin))
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
