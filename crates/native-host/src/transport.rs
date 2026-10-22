//! Moving batches through a bucket: runners upload their outbox; the desktop host
//! collects new batches for the dashboard, which verifies them like an import.

use crate::paths::{self, private_dir, write_private};
use crate::runner;
use crate::s3::{self, Config};
use pocket_scraper_core::bundle::{fingerprint, BATCH_MAX};
use pocket_scraper_core::protocol::Response;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

pub fn runner_config_path() -> PathBuf {
    runner::dir().join("upload.json")
}

pub fn collector_config_path() -> PathBuf {
    paths::data_dir().join("collector.json")
}

fn seen_path() -> PathBuf {
    paths::data_dir().join("collector-seen.json")
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

fn seen() -> BTreeSet<String> {
    fs::read_to_string(seen_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// New batches from the bucket, as text split into parts that fit native messages.
/// At most `max` per call; the dashboard acks them and asks again for the rest.
pub fn collect(max: usize) -> Vec<Response> {
    let result = (|| -> Result<Vec<Response>, String> {
        let cfg = Config::load(&collector_config_path())?;
        let scratch = paths::data_dir();
        let seen = seen();
        let fresh: Vec<String> = s3::list(&cfg, &cfg.prefix, &scratch)?.into_iter().filter(|k| k.ends_with(".pssbatch") && !seen.contains(k)).collect();
        let mut out = vec![];
        for key in fresh.iter().take(max) {
            let bytes = s3::get(&cfg, key, &scratch)?;
            if bytes.len() > BATCH_MAX {
                out.push(Response::Error { message: format!("{key} is larger than a batch can be; skipped") });
                continue;
            }
            let text = String::from_utf8(bytes).map_err(|_| format!("{key} is not text"))?;
            let parts = split(&text, 700 * 1024);
            let n = parts.len();
            out.extend(parts.into_iter().enumerate().map(|(i, t)| Response::BatchPart { key: key.clone(), part: i, parts: n, text: t }));
        }
        out.push(Response::CollectDone { count: fresh.len().min(max), remaining: fresh.len().saturating_sub(max) });
        Ok(out)
    })();
    result.unwrap_or_else(|message| vec![Response::Error { message }])
}

pub fn ack(keys: Vec<String>) -> Response {
    let mut seen = seen();
    let count = keys.len();
    seen.extend(keys.into_iter().filter(|k| k.len() < 1024));
    match write_private(&seen_path(), serde_json::to_string(&seen).expect("seen").as_bytes(), 0o600) {
        Ok(()) => Response::Acked { count },
        Err(e) => Response::Error { message: e.to_string() },
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
