//! Moving batches through a bucket: runners upload their outbox; the desktop host
//! collects new batches for the dashboard, which verifies them like an import.

use crate::paths::{self, private_dir, write_private};
use crate::runner;
use crate::s3::{self, Config};
use pocket_scraper_core::bundle::{fingerprint, BATCH_MAX};
use pocket_scraper_core::protocol::Response;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

pub fn runner_config_path() -> PathBuf {
    runner::dir().join("upload.json")
}

pub fn collector_config_path() -> PathBuf {
    paths::data_dir().join("collector.json")
}

fn state_path() -> PathBuf {
    paths::data_dir().join("collector-state.json")
}

/// Reads `--endpoint E --bucket B --region R [--prefix P] --access-key-id K`; the
/// secret comes from PSS_S3_SECRET_ACCESS_KEY or one line on stdin, never argv.
pub fn configure(args: &[String], path: &std::path::Path) -> Result<(), String> {
    let get = |name: &str| -> Option<String> {
        let i = args.iter().position(|a| a == name)?;
        args.get(i + 1).cloned()
    };
    let secret = match std::env::var("PSS_S3_SECRET_ACCESS_KEY") {
        Ok(s) if !s.is_empty() => s,
        _ => {
            eprintln!("Secret access key (one line on stdin):");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
            line.trim().to_string()
        }
    };
    let cfg = Config {
        endpoint: get("--endpoint").ok_or("--endpoint is required")?,
        bucket: get("--bucket").ok_or("--bucket is required")?,
        region: get("--region").unwrap_or_else(|| "us-east-1".into()),
        prefix: get("--prefix").unwrap_or_default(),
        access_key_id: get("--access-key-id").ok_or("--access-key-id is required")?,
        secret_access_key: secret,
    };
    private_dir(path.parent().expect("config dir")).map_err(|e| e.to_string())?;
    cfg.save(path)?;
    eprintln!("Saved {} (owner-only).", path.display());
    Ok(())
}

/// Uploads every outbox batch, moving each to outbox/sent once stored; prunes sent
/// files after 7 days. Returns how many were uploaded.
pub fn upload_outbox(report: &mut dyn FnMut(&str)) -> Result<usize, String> {
    let cfg = Config::load(&runner_config_path())?;
    let key = runner::identity()?;
    let folder = format!("{}{}/", cfg.prefix, fingerprint(&key.public()));
    let sent = runner::dir().join("outbox").join("sent");
    private_dir(&sent).map_err(|e| e.to_string())?;
    let mut uploaded = 0;
    for file in runner::outbox() {
        let name = file.file_name().expect("file name").to_string_lossy().to_string();
        s3::put_file(&cfg, &format!("{folder}{name}"), &file, &runner::dir())?;
        fs::rename(&file, sent.join(&name)).map_err(|e| e.to_string())?;
        report(&format!("Uploaded {name}"));
        uploaded += 1;
    }
    let week = Duration::from_secs(7 * 86_400);
    for e in fs::read_dir(&sent).into_iter().flatten().flatten() {
        if e.metadata().and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|age| age > week) {
            let _ = fs::remove_file(e.path());
        }
    }
    Ok(uploaded)
}

/// What the desktop collector remembers, all of it bounded: per runner folder the
/// last key handled (keys sort by time within a folder, so S3's start-after lists only
/// newer ones), keys set aside for later, and what the last collect handed out.
#[derive(Default, Serialize, Deserialize)]
struct CollectorState {
    marks: BTreeMap<String, String>,
    deferred: BTreeSet<String>,
    pending: Vec<String>,
}

const DEFERRED_MAX: usize = 1000;

impl CollectorState {
    fn load() -> CollectorState {
        fs::read_to_string(state_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn save(&self) -> Result<(), String> {
        write_private(&state_path(), serde_json::to_string(self).expect("state").as_bytes(), 0o600).map_err(|e| e.to_string())
    }
}

fn folder_of(key: &str) -> &str {
    key.rfind('/').map(|i| &key[..=i]).unwrap_or("")
}

/// New batches from the bucket, as text in parts that fit native messages; at most
/// `max` per call. A key that can't be fetched is reported and skipped, never the
/// whole collect; the dashboard acks what it handled and asks again for more.
pub fn collect(max: usize, retry_deferred: bool) -> Vec<Response> {
    let result = (|| -> Result<Vec<Response>, String> {
        let cfg = Config::load(&collector_config_path())?;
        let scratch = paths::data_dir();
        let mut state = CollectorState::load();
        let mut keys: Vec<String> = if retry_deferred { state.deferred.iter().take(max).cloned().collect() } else { vec![] };
        let mut more = false;
        for folder in s3::folders(&cfg, &cfg.prefix, &scratch)? {
            if keys.len() >= max {
                more = true;
                break;
            }
            let page = s3::list_after(&cfg, &folder, state.marks.get(&folder).map(String::as_str), max - keys.len() + 1, &scratch)?;
            let fresh: Vec<String> = page.into_iter().filter(|k| k.ends_with(".pssbatch") && !state.deferred.contains(k)).collect();
            if fresh.len() > max - keys.len() {
                more = true;
            }
            keys.extend(fresh.into_iter().take(max - keys.len()));
        }
        let mut out = vec![];
        for key in &keys {
            match s3::get(&cfg, key, &scratch, BATCH_MAX as u64) {
                Err(reason) => out.push(Response::BatchSkipped { key: key.clone(), reason }),
                Ok(bytes) => match String::from_utf8(bytes) {
                    Err(_) => out.push(Response::BatchSkipped { key: key.clone(), reason: "not a batch file (not text)".into() }),
                    Ok(text) => {
                        let parts = split(&text, 700 * 1024);
                        let n = parts.len();
                        out.extend(parts.into_iter().enumerate().map(|(i, t)| Response::BatchPart { key: key.clone(), part: i, parts: n, text: t }));
                    }
                },
            }
        }
        state.pending = keys.clone();
        state.save()?;
        out.push(Response::CollectDone { count: keys.len(), more });
        Ok(out)
    })();
    result.unwrap_or_else(|message| vec![Response::Error { message }])
}

/// Records what the dashboard did with the last collect. A folder's mark only moves
/// past keys that were handled or deferred, in order, so a key that failed to fetch
/// is tried again next time.
pub fn ack(keys: Vec<String>, deferred: Vec<String>) -> Response {
    let mut state = CollectorState::load();
    let handled: BTreeSet<String> = keys.into_iter().collect();
    for key in &handled {
        state.deferred.remove(key);
    }
    for key in deferred.iter().filter(|k| state.pending.contains(k)) {
        if state.deferred.len() < DEFERRED_MAX {
            state.deferred.insert(key.clone());
        }
    }
    let mut blocked: BTreeSet<String> = BTreeSet::new();
    for key in std::mem::take(&mut state.pending) {
        let folder = folder_of(&key).to_string();
        if blocked.contains(&folder) {
            continue;
        }
        if handled.contains(&key) || state.deferred.contains(&key) {
            let mark = state.marks.entry(folder).or_default();
            if key > *mark {
                *mark = key;
            }
        } else {
            blocked.insert(folder);
        }
    }
    match state.save() {
        Ok(()) => Response::Acked { count: handled.len() },
        Err(message) => Response::Error { message },
    }
}

/// Splits at character boundaries into pieces of at most `max` bytes.
fn split(text: &str, max: usize) -> Vec<String> {
    let mut parts = vec![];
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(max);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    if parts.is_empty() {
        parts.push(String::new());
    }
    parts
}

#[cfg(test)]
mod tests {
    #[test]
    fn splits_at_character_boundaries() {
        let text = "ab“cd”ef".repeat(3);
        let parts = super::split(&text, 5);
        assert!(parts.iter().all(|p| p.len() <= 5));
        assert_eq!(parts.concat(), text);
    }
}
