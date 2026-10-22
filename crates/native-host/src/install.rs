//! `install` / `uninstall`: copy the host to a stable private location and register
//! it with the browsers, allowing only the Pocket Scraper Secure extension.

use crate::paths::{self, private_dir, HOST_NAME};
use std::fs;
use std::path::Path;

/// The extension's ID, fixed by the public key in its manifest.
pub const DEFAULT_EXTENSION_ID: &str = include_str!("../../../extension/extension-id.txt");

pub fn allowed_extension_id() -> String {
    fs::read_to_string(paths::allowed_id_file()).ok().map(|s| s.trim().to_string()).filter(|s| valid_id(s)).unwrap_or_else(|| DEFAULT_EXTENSION_ID.trim().to_string())
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

struct Options {
    browsers: Vec<String>,
    extension_id: String,
    purge_profiles: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options { browsers: vec![], extension_id: DEFAULT_EXTENSION_ID.trim().to_string(), purge_profiles: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--browser" => o.browsers.push(it.next().ok_or("--browser needs chrome, chromium, brave or edge")?.clone()),
            "--extension-id" => o.extension_id = it.next().ok_or("--extension-id needs an ID")?.clone(),
            "--purge-profiles" => o.purge_profiles = true,
            other => return Err(format!("Unknown option {other}")),
        }
    }
    if o.browsers.is_empty() {
        o.browsers.push("chrome".into());
    }
    if !valid_id(&o.extension_id) {
        return Err("An extension ID is 32 letters a–p (see chrome://extensions)".into());
    }
    for b in &o.browsers {
        if paths::manifest_dir(b).is_none() {
            return Err(format!("Unknown browser {b}: use chrome, chromium, brave or edge"));
        }
    }
    Ok(o)
}

fn write_file(path: &Path, content: &[u8], mode: u32) -> Result<(), String> {
    paths::write_private(path, content, mode).map_err(|e| format!("Could not write {}: {e}", path.display()))
}

/// Installs a program. Windows can't replace a running .exe (the bridge host runs
/// while Claude access is on, the MCP server while Claude is open) but can rename it,
/// so the old copy is moved aside first.
fn write_binary(path: &Path, content: &[u8]) -> Result<(), String> {
    if cfg!(windows) && path.exists() {
        let aside = path.with_extension("old");
        let _ = fs::remove_file(&aside);
        fs::rename(path, &aside).map_err(|e| format!("Could not move the old {} aside: {e}", path.display()))?;
    }
    write_file(path, content, 0o700)
}

pub fn install(args: &[String]) -> Result<(), String> {
    let o = parse(args)?;
    let bin = paths::installed_binary();
    private_dir(&paths::data_dir()).map_err(|e| e.to_string())?;
    private_dir(bin.parent().expect("bin dir")).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bytes = fs::read(&exe).map_err(|e| format!("Could not read {}: {e}", exe.display()))?;
    write_binary(&bin, &bytes)?;
    write_file(&paths::allowed_id_file(), format!("{}\n", o.extension_id).as_bytes(), 0o600)?;
    let manifest = serde_json::json!({
        "name": HOST_NAME,
        "description": "Pocket Scraper Secure native host",
        "path": bin,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{}/", o.extension_id)]
    });
    let manifest = serde_json::to_vec_pretty(&manifest).expect("manifest serialises");
    for browser in &o.browsers {
        let dir = paths::manifest_dir(browser).expect("checked");
        if !cfg!(windows) && !dir.parent().is_some_and(Path::exists) {
            eprintln!("Skipped {browser}: it doesn't look installed ({} is missing).", dir.parent().unwrap().display());
            continue;
        }
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{HOST_NAME}.json"));
        write_file(&path, &manifest, 0o644)?;
        if cfg!(windows) {
            // Windows browsers find hosts through the registry.
            let key = paths::registry_key(browser).expect("checked");
            let ok = std::process::Command::new("reg").args(["add", &key, "/ve", "/t", "REG_SZ", "/d"]).arg(&path).arg("/f").status().map_err(|e| e.to_string())?;
            if !ok.success() {
                return Err(format!("Could not register with {browser} (reg add {key})"));
            }
        }
        eprintln!("Registered with {browser}: {}", path.display());
    }
    eprintln!("Installed {} (allowed extension {}).", bin.display(), o.extension_id);
    // The MCP server, when it was built alongside: a stable path for Claude to start.
    let mcp_name = if cfg!(windows) { "pocket-scraper-mcp.exe" } else { "pocket-scraper-mcp" };
    let mcp = exe.with_file_name(mcp_name);
    if mcp.is_file() {
        let target = bin.with_file_name(mcp_name);
        let bytes = fs::read(&mcp).map_err(|e| format!("Could not read {}: {e}", mcp.display()))?;
        write_binary(&target, &bytes)?;
        eprintln!("Installed {}. To use it from Claude Code:\n  claude mcp add --scope user pocket-scraper-secure -- \"{}\"\nthen turn on Claude access in the dashboard.", target.display(), target.display());
    }
    let browser = crate::headless::install(&mut |m| eprintln!("{m}"))?;
    eprintln!("Runs use {}", browser.display());
    Ok(())
}

pub fn uninstall(args: &[String]) -> Result<(), String> {
    let o = parse(args)?;
    for browser in ["chrome", "chromium", "brave", "edge"] {
        let path = paths::manifest_dir(browser).expect("known").join(format!("{HOST_NAME}.json"));
        if fs::symlink_metadata(&path).is_ok() {
            fs::remove_file(&path).map_err(|e| e.to_string())?;
            if cfg!(windows) {
                let _ = std::process::Command::new("reg").args(["delete", &paths::registry_key(browser).expect("known"), "/f"]).status();
            }
            eprintln!("Unregistered from {browser}.");
        }
    }
    let _ = fs::remove_file(paths::installed_binary());
    let _ = fs::remove_file(paths::installed_binary().with_file_name(if cfg!(windows) { "pocket-scraper-mcp.exe" } else { "pocket-scraper-mcp" }));
    let _ = fs::remove_file(paths::allowed_id_file());
    if o.purge_profiles {
        let profiles = paths::data_dir().join("profiles");
        if profiles.exists() {
            fs::remove_dir_all(&profiles).map_err(|e| e.to_string())?;
            eprintln!("Deleted saved website sessions in {}.", profiles.display());
        }
    } else {
        eprintln!("Saved website sessions were kept; pass --purge-profiles to delete them.");
    }
    Ok(())
}
