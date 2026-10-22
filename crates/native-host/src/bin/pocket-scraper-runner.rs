//! pocket-scraper-runner: runs taught recipes on its own schedule, on a desktop, VM or
//! server. See docs/RUNNER.md.

use pocket_scraper_core::bundle::fingerprint;
use pocket_scraper_host::{headless, runner, schedule, transport};

/// After a run: upload if a bucket is configured. A failed upload leaves the batch in
/// the outbox for the next attempt.
fn upload_if_configured() {
    if transport::runner_config_path().is_file() {
        if let Err(e) = transport::upload_outbox(&mut |m| println!("{m}")) {
            eprintln!("Upload failed (batches stay in the outbox): {e}");
        }
    }
}
use std::path::Path;
use std::process::ExitCode;

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

fn init() -> Result<(), String> {
    let key = runner::identity()?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let target = schedule::install_runner_copy(&exe)?;
    if exe != target {
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

fn schedule(args: &[String]) -> Result<(), String> {
    let every = match (args.get(1).map(String::as_str), args.get(2)) {
        (Some("--every"), Some(m)) => m.parse::<u32>().map_err(|_| "--every takes a number of minutes".to_string())?,
        (None, _) => 5,
        _ => return Err("Usage: schedule install [--every MINUTES] | uninstall | print".into()),
    };
    let message = match args.first().map(String::as_str) {
        Some("print") => schedule::print(every)?,
        Some("install") => schedule::install(every)?,
        Some("uninstall") => schedule::uninstall()?,
        _ => return Err("Usage: schedule install [--every MINUTES] | uninstall | print".into()),
    };
    println!("{message}");
    Ok(())
}
