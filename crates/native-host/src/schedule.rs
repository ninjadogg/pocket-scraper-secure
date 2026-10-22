//! Running `run-due` on a timer: launchd on macOS, a systemd user timer on Linux, Task
//! Scheduler on Windows (or a printed cron line). Shared by the runner command and the
//! MCP server.

use crate::paths::{self, write_private};
use crate::runner;
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL: &str = "com.pocketscraper.runner";

/// How often to check for due recipes (each recipe keeps its own schedule). Divisors of
/// 60 keep the cron line exact.
pub fn check_every(minutes: u32) -> Result<u32, String> {
    if minutes > 0 && 60 % minutes == 0 {
        Ok(minutes)
    } else {
        Err("Check every 1, 2, 3, 4, 5, 6, 10, 12, 15, 20, 30 or 60 minutes".into())
    }
}

/// Where the runner keeps its own copy, so timers don't depend on a build folder.
pub fn installed_runner() -> PathBuf {
    paths::data_dir().join("bin").join(if cfg!(windows) { "pocket-scraper-runner.exe" } else { "pocket-scraper-runner" })
}

/// Copies `from` (a runner binary) to the stable path, if it isn't already there.
pub fn install_runner_copy(from: &Path) -> Result<PathBuf, String> {
    let target = installed_runner();
    if from != target {
        paths::private_dir(target.parent().expect("bin dir")).map_err(|e| e.to_string())?;
        let bytes = std::fs::read(from).map_err(|e| format!("Could not read {}: {e}", from.display()))?;
        write_private(&target, &bytes, 0o700).map_err(|e| e.to_string())?;
    }
    Ok(target)
}

/// The runner binary timers should start: the installed copy, else one beside this
/// program (installed on the way).
pub fn runner_exe() -> Result<PathBuf, String> {
    let installed = installed_runner();
    if installed.is_file() {
        return Ok(installed);
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let name = if cfg!(windows) { "pocket-scraper-runner.exe" } else { "pocket-scraper-runner" };
    let candidate = if me.file_name().is_some_and(|n| n == name) { me } else { me.with_file_name(name) };
    if !candidate.is_file() {
        return Err(format!("pocket-scraper-runner wasn't found beside {}; run `pocket-scraper-runner init` first", candidate.display()));
    }
    install_runner_copy(&candidate)
}

/// What `install` would set up, without installing it.
pub fn print(every: u32) -> Result<String, String> {
    let every = check_every(every)?;
    let exe = runner_exe()?;
    Ok(if cfg!(windows) {
        schtasks_create(&exe, every).join(" ")
    } else {
        let unit = if cfg!(target_os = "macos") { launchd_plist(&exe, every) } else { systemd_units(&exe, every).join("\n") };
        format!("{unit}\n# Or with cron:\n{}", cron_line(&exe, &runner::dir().join("runner.log"), every))
    })
}

pub fn install(every: u32) -> Result<String, String> {
    let every = check_every(every)?;
    let exe = runner_exe()?;
    if cfg!(windows) {
        let args = schtasks_create(&exe, every);
        let ok = Command::new(&args[0]).args(&args[1..]).status().map_err(|e| format!("schtasks: {e}"))?;
        if !ok.success() {
            return Err(format!("schtasks failed ({ok})"));
        }
        return Ok(format!("Scheduled: run-due every {every} minutes (Task Scheduler: {TASK})."));
    }
    if cfg!(target_os = "macos") {
        let plist = launchd_plist_path()?;
        write_private(&plist, launchd_plist(&exe, every).as_bytes(), 0o600).map_err(|e| e.to_string())?;
        let domain = format!("gui/{}", uid());
        let _ = Command::new("launchctl").args(["bootout", &format!("{domain}/{LABEL}")]).status();
        let ok = Command::new("launchctl").args(["bootstrap", &domain]).arg(&plist).status().map_err(|e| e.to_string())?;
        if !ok.success() {
            return Err(format!("launchctl bootstrap failed ({ok})"));
        }
        return Ok(format!("Scheduled: run-due every {every} minutes ({}). Log: {}", plist.display(), runner::dir().join("runner.log").display()));
    }
    let [service, timer] = systemd_units(&exe, every);
    let dir = systemd_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    write_private(&dir.join("pocket-scraper-runner.service"), service.as_bytes(), 0o644).map_err(|e| e.to_string())?;
    write_private(&dir.join("pocket-scraper-runner.timer"), timer.as_bytes(), 0o644).map_err(|e| e.to_string())?;
    for a in [&["--user", "daemon-reload"][..], &["--user", "enable", "--now", "pocket-scraper-runner.timer"][..]] {
        let ok = Command::new("systemctl").args(a).status().map_err(|e| format!("systemctl: {e}"))?;
        if !ok.success() {
            return Err(format!("systemctl {} failed. On a server, `loginctl enable-linger $USER` lets user timers run without a login; or use the cron line from `schedule print`.", a.join(" ")));
        }
    }
    Ok(format!("Scheduled: run-due every {every} minutes (systemd user timer)."))
}

pub fn uninstall() -> Result<String, String> {
    if cfg!(windows) {
        let _ = Command::new("schtasks").args(["/Delete", "/TN", TASK, "/F"]).status();
    } else if cfg!(target_os = "macos") {
        let plist = launchd_plist_path()?;
        let _ = Command::new("launchctl").args(["bootout", &format!("gui/{}/{LABEL}", uid())]).status();
        let _ = std::fs::remove_file(&plist);
    } else {
        let _ = Command::new("systemctl").args(["--user", "disable", "--now", "pocket-scraper-runner.timer"]).status();
        let dir = systemd_dir()?;
        let _ = std::fs::remove_file(dir.join("pocket-scraper-runner.service"));
        let _ = std::fs::remove_file(dir.join("pocket-scraper-runner.timer"));
    }
    Ok("Schedule removed.".into())
}

/// Whether a timer is set up.
pub fn installed() -> bool {
    if cfg!(windows) {
        Command::new("schtasks").args(["/Query", "/TN", TASK]).output().is_ok_and(|o| o.status.success())
    } else if cfg!(target_os = "macos") {
        launchd_plist_path().is_ok_and(|p| p.is_file())
    } else {
        systemd_dir().is_ok_and(|d| d.join("pocket-scraper-runner.timer").is_file())
    }
}

const TASK: &str = "PocketScraperSecure Runner";

/// Task Scheduler: every N minutes, as the current user, only while logged on (no
/// stored password).
pub(crate) fn schtasks_create(exe: &Path, every: u32) -> Vec<String> {
    ["schtasks", "/Create", "/SC", "MINUTE", "/MO", &every.to_string(), "/TN", TASK, "/TR", &format!("\"{}\" run-due", exe.display()), "/F"].iter().map(|s| s.to_string()).collect()
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

#[cfg(unix)]
fn uid() -> u32 {
    #[allow(unsafe_code)]
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe {
        libc::getuid()
    }
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| "HOME is not set".into())
}

fn launchd_plist_path() -> Result<PathBuf, String> {
    Ok(home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

fn systemd_dir() -> Result<PathBuf, String> {
    Ok(std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or(home()?.join(".config")).join("systemd/user"))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// In a crontab `%` ends the command, so it is escaped.
pub(crate) fn cron_line(exe: &Path, log: &Path, every: u32) -> String {
    format!("*/{every} * * * * {} run-due >> {} 2>&1", shell_quote(exe), shell_quote(log)).replace('%', "\\%")
}

fn launchd_plist(exe: &Path, every: u32) -> String {
    let log = runner::dir().join("runner.log");
    let data = std::env::var("PSS_DATA_DIR").ok();
    let env = data.map(|d| format!("  <key>EnvironmentVariables</key>\n  <dict><key>PSS_DATA_DIR</key><string>{}</string></dict>\n", xml(&d))).unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{}</string><string>run-due</string></array>
  <key>StartInterval</key><integer>{}</integer>
  <key>ProcessType</key><string>Background</string>
  <key>LowPriorityIO</key><true/>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
{env}</dict>
</plist>
"#,
        xml(&exe.display().to_string()),
        every * 60,
        xml(&log.display().to_string()),
        xml(&log.display().to_string())
    )
}

/// Quotes a value for a systemd unit line (and escapes `%` specifiers).
pub(crate) fn systemd_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"))
}

pub(crate) fn systemd_units(exe: &Path, every: u32) -> [String; 2] {
    let data = std::env::var("PSS_DATA_DIR").map(|d| format!("Environment={}\n", systemd_quote(&format!("PSS_DATA_DIR={d}")))).unwrap_or_default();
    [
        // No PrivateTmp: in a user unit it needs user namespaces, which some servers lack.
        format!(
            "[Unit]\nDescription=Pocket Scraper Secure: run due recipes\n\n[Service]\nType=oneshot\n{data}ExecStart={} run-due\nNoNewPrivileges=yes\n",
            systemd_quote(&exe.display().to_string())
        ),
        format!("[Unit]\nDescription=Pocket Scraper Secure runner timer\n\n[Timer]\nOnBootSec=2min\nOnUnitActiveSec={every}min\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_lines_quote_and_escape_paths() {
        assert_eq!(
            cron_line(Path::new("/opt/pss 1/runner"), Path::new("/var/log/100%/r.log"), 15),
            r"*/15 * * * * '/opt/pss 1/runner' run-due >> '/var/log/100\%/r.log' 2>&1"
        );
        assert_eq!(systemd_quote(r#"/srv/a "b"\c 100%"#), r#""/srv/a \"b\"\\c 100%%""#);
        let [service, timer] = systemd_units(Path::new("/opt/pss 1/runner"), 5);
        assert!(service.contains(r#"ExecStart="/opt/pss 1/runner" run-due"#), "{service}");
        assert!(!service.contains("PrivateTmp"));
        assert!(timer.contains("OnUnitActiveSec=5min"));
    }
}
