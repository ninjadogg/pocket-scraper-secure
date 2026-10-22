//! pocket-scraper-runner: runs taught recipes on its own schedule, on a desktop, VM or
//! server. See docs/RUNNER.md.

use pocket_scraper_core::bundle::fingerprint;
use pocket_scraper_host::paths::write_private;
use pocket_scraper_host::{headless, runner, transport};

/// After a run: upload if a bucket is configured. A failed upload leaves the batch in
/// the outbox for the next attempt.
fn upload_if_configured() {
    if transport::runner_config_path().is_file() {
        if let Err(e) = transport::upload_outbox(&mut |m| println!("{m}")) {
            eprintln!("Upload failed (batches stay in the outbox): {e}");
        }
    }
}
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const USAGE: &str = "pocket-scraper-runner: runs Pocket Scraper Secure recipes on a schedule, without a browser window.

  init                         create this runner's key, install its browser, show the key to enrol
  id                           show this runner's key and fingerprint
  add <file.pssbundle> [--trust FINGERPRINT]
                               add or update a recipe exported from the dashboard
  list                         recipes with their schedules
  remove <recipe-id>
  run <recipe-id>              run one recipe now
  run-due                      run every scheduled recipe that is due (what the timer calls)
  outbox                       result batches waiting to be collected
  schedule install [--every MINUTES] | uninstall | print
                               run run-due on a timer (launchd on macOS, systemd on Linux)
  upload configure --endpoint URL --bucket NAME [--region R] [--prefix P] --access-key-id ID
                               send batches to an S3-compatible bucket (secret from
                               PSS_S3_SECRET_ACCESS_KEY or stdin; uploads after every run)
  upload                       upload waiting batches now
  install-browser              download and verify the pinned headless browser

Data lives in PSS_DATA_DIR if set, otherwise the platform's per-user data folder.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("init") => init(),
        Some("id") => id(),
        Some("add") => add(&args[1..]),
        Some("list") => list(),
        Some("remove") => args.get(1).ok_or("remove needs a recipe ID".to_string()).and_then(|id| runner::remove(id)).map(|_| println!("Removed.")),
        Some("run") => run_one(args.get(1)),
        Some("run-due") => runner::identity().and_then(|key| runner::run_due(&key, &mut |m| println!("{m}"))).map(|n| {
            if n == 0 {
                println!("Nothing was due.");
            }
            upload_if_configured();
        }),
        Some("outbox") => {
            let files = runner::outbox();
            println!("{} batch file(s) waiting in {}", files.len(), runner::dir().join("outbox").display());
            for f in files {
                println!("  {}", f.file_name().unwrap_or_default().to_string_lossy());
            }
            Ok(())
        }
        Some("schedule") => schedule(&args[1..]),
        Some("upload") if args.get(1).map(String::as_str) == Some("configure") => transport::configure(&args[2..], &transport::runner_config_path()),
        Some("upload") => transport::upload_outbox(&mut |m| println!("{m}")).map(|n| println!("{n} batch(es) uploaded.")),
        Some("install-browser") => headless::install(&mut |m| eprintln!("{m}")).map(|p| println!("Browser ready: {}", p.display())),
        Some("--version") => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

/// Where `init` keeps its own copy, so timers don't depend on a build folder.
fn installed_exe() -> PathBuf {
    let name = if cfg!(windows) { "pocket-scraper-runner.exe" } else { "pocket-scraper-runner" };
    pocket_scraper_host::paths::data_dir().join("bin").join(name)
}

fn init() -> Result<(), String> {
    let key = runner::identity()?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let target = installed_exe();
    if exe != target {
        pocket_scraper_host::paths::private_dir(target.parent().expect("bin dir")).map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&exe).map_err(|e| e.to_string())?;
        write_private(&target, &bytes, 0o700).map_err(|e| e.to_string())?;
        println!("Installed {}", target.display());
    }
    headless::install(&mut |m| eprintln!("{m}"))?;
    println!("Runner ready. Enrol it in the dashboard (Runners → Add runner) with:");
    println!("  key:         {}", runner::public_key_b64(&key));
    println!("  fingerprint: {}", fingerprint(&key.public()));
    Ok(())
}

fn id() -> Result<(), String> {
    let key = runner::identity()?;
    println!("key:         {}", runner::public_key_b64(&key));
    println!("fingerprint: {}", fingerprint(&key.public()));
    Ok(())
}

fn add(args: &[String]) -> Result<(), String> {
    let file = args.first().ok_or("add needs a .pssbundle file")?;
    let trust = match args.get(1).map(String::as_str) {
        Some("--trust") => Some(args.get(2).ok_or("--trust needs the vault fingerprint")?.as_str()),
        Some(other) => return Err(format!("Unknown option {other}")),
        None => None,
    };
    let b = runner::add(&runner::bundle_file(Path::new(file))?, trust)?;
    let schedule = match b.recipe.schedule_minutes {
        0 => "manual only (run it with: run <id>)".to_string(),
        m => format!("every {m} minutes"),
    };
    println!("Added \"{}\" ({}) from vault {}: {}, {schedule}.", b.recipe.name, b.recipe.id, fingerprint(&b.vault_signing_key), b.recipe.url);
    Ok(())
}

fn list() -> Result<(), String> {
    let state = runner::State::load();
    let recipes = runner::recipes()?;
    if recipes.is_empty() {
        println!("No recipes. Add one with: add <file.pssbundle>");
    }
    for b in recipes {
        let s = state.recipes.get(&b.recipe.id).cloned().unwrap_or_default();
        let last = s.last_run.map(|t| format!("last run {} ({:?})", runner::iso(t), s.last_status.expect("status with run"))).unwrap_or_else(|| "never run".into());
        let next = match runner::next_due(&b, &s) {
            None => "manual only".to_string(),
            Some(t) if t <= runner::now_secs() => "due now".to_string(),
            Some(t) => format!("next {}", runner::iso(t)),
        };
        println!("{}  {}  ({}; {last}; {next})", b.recipe.id, b.recipe.name, b.recipe.url);
    }
    Ok(())
}

fn run_one(id: Option<&String>) -> Result<(), String> {
    let id = id.ok_or("run needs a recipe ID (see: list)")?;
    let key = runner::identity()?;
    let b = runner::recipes()?.into_iter().find(|b| &b.recipe.id == id).ok_or_else(|| format!("No recipe {id}"))?;
    let _lock = runner::lock()?;
    let (path, batch) = runner::run(&b, &key)?;
    println!("{:?}: {} rows{} → {}", batch.status, batch.row_count, batch.error.map(|e| format!(" ({e})")).unwrap_or_default(), path.display());
    drop(_lock);
    upload_if_configured();
    Ok(())
}

// ── timers ───────────────────────────────────────────────────────────────────

const LABEL: &str = "com.pocketscraper.runner";

fn schedule(args: &[String]) -> Result<(), String> {
    let every = match (args.get(1).map(String::as_str), args.get(2)) {
        (Some("--every"), Some(m)) => m.parse::<u32>().ok().filter(|m| (1..=1440).contains(m)).ok_or("--every takes 1–1440 minutes")?,
        (None, _) => 5,
        _ => return Err("Usage: schedule install [--every MINUTES] | uninstall | print".into()),
    };
    let exe = Some(installed_exe()).filter(|p| p.is_file()).map(Ok).unwrap_or_else(|| std::env::current_exe().map_err(|e| e.to_string()))?;
    match args.first().map(String::as_str) {
        Some("print") if cfg!(windows) => {
            println!("{}", schtasks_create(&exe, every).join(" "));
            Ok(())
        }
        Some("print") => {
            print!("{}", if cfg!(target_os = "macos") { launchd_plist(&exe, every) } else { systemd_units(&exe, every).join("\n") });
            println!("\n# Or with cron:\n*/{every} * * * * {} run-due >> {} 2>&1", shell_quote(&exe), shell_quote(&runner::dir().join("runner.log")));
            Ok(())
        }
        Some("install") if cfg!(windows) => {
            let args = schtasks_create(&exe, every);
            let ok = Command::new(&args[0]).args(&args[1..]).status().map_err(|e| format!("schtasks: {e}"))?;
            if !ok.success() {
                return Err(format!("schtasks failed ({ok})"));
            }
            println!("Scheduled: run-due every {every} minutes (Task Scheduler: {TASK}).");
            Ok(())
        }
        Some("uninstall") if cfg!(windows) => {
            let _ = Command::new("schtasks").args(["/Delete", "/TN", TASK, "/F"]).status();
            println!("Schedule removed.");
            Ok(())
        }
        Some("install") if cfg!(target_os = "macos") => {
            let plist = launchd_plist_path()?;
            write_private(&plist, launchd_plist(&exe, every).as_bytes(), 0o600).map_err(|e| e.to_string())?;
            let domain = format!("gui/{}", uid());
            let _ = Command::new("launchctl").args(["bootout", &format!("{domain}/{LABEL}")]).status();
            let ok = Command::new("launchctl").args(["bootstrap", &domain]).arg(&plist).status().map_err(|e| e.to_string())?;
            if !ok.success() {
                return Err(format!("launchctl bootstrap failed ({ok})"));
            }
            println!("Scheduled: run-due every {every} minutes ({}). Log: {}", plist.display(), runner::dir().join("runner.log").display());
            Ok(())
        }
        Some("install") => {
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
            println!("Scheduled: run-due every {every} minutes (systemd user timer).");
            Ok(())
        }
        Some("uninstall") if cfg!(target_os = "macos") => {
            let plist = launchd_plist_path()?;
            let _ = Command::new("launchctl").args(["bootout", &format!("gui/{}/{LABEL}", uid())]).status();
            let _ = std::fs::remove_file(&plist);
            println!("Schedule removed.");
            Ok(())
        }
        Some("uninstall") => {
            let _ = Command::new("systemctl").args(["--user", "disable", "--now", "pocket-scraper-runner.timer"]).status();
            let dir = systemd_dir()?;
            let _ = std::fs::remove_file(dir.join("pocket-scraper-runner.service"));
            let _ = std::fs::remove_file(dir.join("pocket-scraper-runner.timer"));
            println!("Schedule removed.");
            Ok(())
        }
        _ => Err("Usage: schedule install [--every MINUTES] | uninstall | print".into()),
    }
}

const TASK: &str = "PocketScraperSecure Runner";

/// Task Scheduler: every N minutes, as the current user, only while logged on (no
/// stored password).
fn schtasks_create(exe: &Path, every: u32) -> Vec<String> {
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

fn systemd_units(exe: &Path, every: u32) -> [String; 2] {
    let data = std::env::var("PSS_DATA_DIR").map(|d| format!("Environment=PSS_DATA_DIR={d}\n")).unwrap_or_default();
    [
        format!(
            "[Unit]\nDescription=Pocket Scraper Secure: run due recipes\n\n[Service]\nType=oneshot\n{data}ExecStart=\"{}\" run-due\nNoNewPrivileges=yes\nPrivateTmp=yes\n",
            exe.display()
        ),
        format!("[Unit]\nDescription=Pocket Scraper Secure runner timer\n\n[Timer]\nOnBootSec=2min\nOnUnitActiveSec={every}min\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"),
    ]
}
