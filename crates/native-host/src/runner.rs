//! The standalone runner: runs taught recipes on a schedule, on this machine or on a
//! VM or server, with no Chrome or extension. It knows only what bundles tell it:
//! recipes signed by the one vault it trusts, and that vault's *public* keys. Results
//! are sealed before they touch disk and leave as batches signed by the runner.
//!
//! Layout under `<data>/runner/` (all owner-only):
//!   runner-key        the runner's Ed25519 key (32 bytes)
//!   trusted-vault     the vault signing key it accepts bundles from (32 bytes)
//!   recipes/<id>.pssbundle   bundles as received; re-verified on every load
//!   state.json        last and next run per recipe
//!   outbox/*.pssbatch signed result batches waiting to be collected
//!   run.lock          held while a run is in progress

use crate::browser::{self, Browser};
use crate::driver::{self, Outcome};
use crate::paths::{self, private_dir, write_private};
use pocket_scraper_core::bundle::{self, Batch, BatchStatus, Bundle, RunnerKey};
use pocket_scraper_core::protocol::seal_rows;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn dir() -> PathBuf {
    paths::data_dir().join("runner")
}

fn ensure_dirs() -> Result<PathBuf, String> {
    let d = dir();
    for p in [paths::data_dir(), d.clone(), d.join("recipes"), d.join("outbox")] {
        private_dir(&p).map_err(|e| format!("Could not create {}: {e}", p.display()))?;
    }
    Ok(d)
}

/// The runner's key, created on first use.
pub fn identity() -> Result<RunnerKey, String> {
    let path = ensure_dirs()?.join("runner-key");
    match fs::read(&path) {
        Ok(bytes) => RunnerKey::from_bytes(&bytes).map_err(|e| e.to_string()),
        Err(_) => {
            let key = RunnerKey::generate();
            write_private(&path, key.to_bytes().as_slice(), 0o600).map_err(|e| e.to_string())?;
            Ok(key)
        }
    }
}

pub fn public_key_b64(key: &RunnerKey) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(key.public())
}

fn trusted_vault() -> Option<[u8; 32]> {
    fs::read(dir().join("trusted-vault")).ok().and_then(|b| b.try_into().ok())
}

/// Adds (or replaces) a recipe from a bundle. The first bundle sets which vault this
/// runner trusts, and only when `trust` matches its fingerprint; later bundles must
/// come from the same vault.
pub fn add(bundle_json: &str, trust: Option<&str>) -> Result<Bundle, String> {
    let d = ensure_dirs()?;
    let pinned = trusted_vault();
    let b = bundle::open_bundle(bundle_json, pinned.as_ref()).map_err(|e| e.to_string())?;
    if pinned.is_none() {
        let fingerprint = bundle::fingerprint(&b.vault_signing_key);
        match trust {
            Some(t) if t.trim().eq_ignore_ascii_case(&fingerprint) => {
                write_private(&d.join("trusted-vault"), &b.vault_signing_key, 0o600).map_err(|e| e.to_string())?;
            }
            Some(t) => return Err(format!("The bundle's vault fingerprint is {fingerprint}, not {t}. Nothing was added.")),
            None => {
                return Err(format!(
                    "This runner doesn't trust a vault yet. Check that the dashboard shows vault fingerprint {fingerprint}, then run: add <file> --trust {fingerprint}"
                ))
            }
        }
    }
    write_private(&d.join("recipes").join(format!("{}.pssbundle", b.recipe.id)), bundle_json.as_bytes(), 0o600).map_err(|e| e.to_string())?;
    Ok(b)
}

/// Every stored recipe, re-verified against the trusted vault.
pub fn recipes() -> Result<Vec<Bundle>, String> {
    let pinned = trusted_vault().ok_or("No recipes yet: add a bundle first")?;
    let mut out = vec![];
    let entries = fs::read_dir(dir().join("recipes")).map_err(|e| e.to_string())?;
    for e in entries.flatten() {
        if e.path().extension().and_then(|x| x.to_str()) != Some("pssbundle") {
            continue;
        }
        let json = fs::read_to_string(e.path()).map_err(|e| e.to_string())?;
        match bundle::open_bundle(&json, Some(&pinned)) {
            Ok(b) => out.push(b),
            Err(err) => eprintln!("Skipping {}: {err}", e.path().display()),
        }
    }
    out.sort_by(|a, b| a.recipe.name.cmp(&b.recipe.name));
    Ok(out)
}

pub fn remove(recipe_id: &str) -> Result<(), String> {
    pocket_scraper_core::protocol::check_id(recipe_id).map_err(|e| e.to_string())?;
    fs::remove_file(dir().join("recipes").join(format!("{recipe_id}.pssbundle"))).map_err(|_| format!("No recipe {recipe_id}"))?;
    let mut state = State::load();
    state.recipes.remove(recipe_id);
    state.save()
}

#[derive(Default, Serialize, Deserialize)]
pub struct State {
    pub recipes: BTreeMap<String, RecipeState>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct RecipeState {
    /// Unix seconds.
    pub last_run: Option<u64>,
    pub last_status: Option<BatchStatus>,
}

impl State {
    pub fn load() -> State {
        fs::read_to_string(dir().join("state.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn save(&self) -> Result<(), String> {
        write_private(&dir().join("state.json"), serde_json::to_string_pretty(self).expect("state").as_bytes(), 0o600).map_err(|e| e.to_string())
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// When a scheduled recipe is next due (unix seconds); `None` for manual-only.
pub fn next_due(bundle: &Bundle, state: &RecipeState) -> Option<u64> {
    let every = bundle.recipe.schedule_minutes as u64 * 60;
    if every == 0 {
        return None;
    }
    Some(state.last_run.map(|t| t + every).unwrap_or(0))
}

/// ISO-8601 UTC for unix seconds (no date library needed).
pub fn iso(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Holds `run.lock` so two invocations (a timer and a person) never run at once.
pub struct RunLock(#[allow(dead_code)] File);

pub fn lock() -> Result<RunLock, String> {
    let path = ensure_dirs()?.join("run.lock");
    let file = File::options().create(true).truncate(false).write(true).open(&path).map_err(|e| e.to_string())?;
    match file.try_lock() {
        Ok(()) => Ok(RunLock(file)),
        Err(_) => Err("Another run is in progress".into()),
    }
}

/// Runs one recipe and writes its signed batch to the outbox. Failed runs produce a
/// batch too, so the dashboard sees the failure.
pub fn run(bundle: &Bundle, key: &RunnerKey) -> Result<(PathBuf, Batch), String> {
    let started = now_secs();
    let run_id = format!("run-{}", pocket_scraper_core::hex(&pocket_scraper_core::random_bytes::<12>()));
    let outcome = match browser::find_chrome() {
        None => Outcome::Failed { message: "The runner's browser isn't installed. Run: pocket-scraper-runner install-browser".into(), attention: false },
        Some(chrome) => match paths::profile_dir(&bundle.recipe.origin).map_err(|e| e.to_string()).and_then(|p| Browser::launch_headless(&chrome, &p)) {
            Err(message) => Outcome::Failed { message, attention: false },
            Ok(mut b) => {
                let watchdog = Watchdog::start(b.pid(), Duration::from_millis(bundle.run_timeout_ms + 5_000));
                let outcome = driver::run(&mut b, bundle.recipe.clone(), bundle.run_timeout_ms);
                watchdog.stop();
                b.close();
                outcome
            }
        },
    };
    let finished = now_secs();
    let (status, row_count, warnings, error, records) = match outcome {
        Outcome::Finished { rows, warnings } => {
            let sealed = seal_rows(&bundle.vault_public_key, &bundle.recipe.id, &run_id, &iso(started), &rows).map_err(|e| e.to_string())?;
            (BatchStatus::Success, rows.len(), warnings, None, sealed)
        }
        Outcome::Failed { message, attention } => (if attention { BatchStatus::NeedsAttention } else { BatchStatus::Error }, 0, vec![], Some(message.chars().take(900).collect()), vec![]),
    };
    let batch = Batch {
        batch_id: pocket_scraper_core::hex(&pocket_scraper_core::random_bytes::<16>()),
        runner_key: key.public().to_vec(),
        vault_public_key: bundle.vault_public_key.to_vec(),
        recipe_id: bundle.recipe.id.clone(),
        run_id,
        started_at: iso(started),
        finished_at: iso(finished),
        status,
        row_count,
        warnings,
        error,
        records,
    };
    let json = bundle::sign_batch(key, &batch).map_err(|e| e.to_string())?;
    let path = ensure_dirs()?.join("outbox").join(format!("{}-{}.pssbatch", iso(started).replace(':', ""), &batch.batch_id[..8]));
    write_private(&path, json.as_bytes(), 0o600).map_err(|e| e.to_string())?;
    let mut state = State::load();
    state.recipes.insert(bundle.recipe.id.clone(), RecipeState { last_run: Some(started), last_status: Some(status) });
    state.save()?;
    Ok((path, batch))
}

/// Runs every scheduled recipe that is due, one after another.
pub fn run_due(key: &RunnerKey, report: &mut dyn FnMut(&str)) -> Result<usize, String> {
    let _lock = lock()?;
    let state = State::load();
    let now = now_secs();
    let mut ran = 0;
    for b in recipes()? {
        let s = state.recipes.get(&b.recipe.id).cloned().unwrap_or_default();
        if next_due(&b, &s).is_some_and(|due| due <= now) {
            let (path, batch) = run(&b, key)?;
            report(&format!("{}: {:?}, {} rows → {}", b.recipe.name, batch.status, batch.row_count, path.display()));
            ran += 1;
        }
    }
    Ok(ran)
}

pub fn outbox() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir().join("outbox")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("pssbatch")).collect();
    files.sort();
    files
}

/// Kills the browser if a run outlives its deadline.
struct Watchdog(std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>);

impl Watchdog {
    fn start(pid: u32, limit: Duration) -> Watchdog {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(limit) {
                browser::kill_process(pid);
            }
        });
        Watchdog(tx, handle)
    }
    fn stop(self) {
        let _ = self.0.send(());
        let _ = self.1.join();
    }
}

pub fn bundle_file(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    if meta.len() > bundle::BUNDLE_MAX as u64 {
        return Err("That file is too large to be a recipe bundle".into());
    }
    fs::read_to_string(path).map_err(|e| format!("Could not read {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates_match_known_values() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_792_700_238), "2026-10-22T20:17:18Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
    }
}
