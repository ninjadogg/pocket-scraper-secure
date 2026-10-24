//! The MCP server's way to reach the extension, which only ever calls out (to the
//! native host) and never listens. When the person turns on Claude access, the
//! extension keeps a native-messaging connection open as the *bridge*: that host
//! process watches an owner-only mailbox folder, hands each request to the extension
//! and writes its answer back. No socket or port is involved.
//!
//!   <data>/mailbox/requests/<id>.json    written by the MCP server
//!   <data>/mailbox/responses/<id>.json   written by the bridge
//!   <data>/mailbox/bridge.lock           held while a bridge is connected
//!
//! Each request carries a one-time public key and a deadline. Answers are sealed to
//! that key (only the asking process can open them), so opened records never sit in
//! the mailbox; requests past their deadline are dropped, never replayed later.

use crate::paths::{self, private_dir, write_private};
use base64::Engine;
use pocket_scraper_core::protocol::Response;
use pocket_scraper_core::vault::{self, SecretKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

const REQUEST_MAX: u64 = 16 * 1024 * 1024;
/// Answers nobody collected (the caller gave up) are cleared after this.
const STALE: Duration = Duration::from_secs(3 * 60);
/// Pieces of a request sent to Chrome, measured as escaped JSON text.
const PIECE: usize = 700 * 1024;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn dir() -> PathBuf {
    paths::data_dir().join("mailbox")
}

fn ensure() -> Result<PathBuf, String> {
    let d = dir();
    for p in [paths::data_dir(), d.clone(), d.join("requests"), d.join("responses")] {
        private_dir(&p).map_err(|e| format!("Could not create {}: {e}", p.display()))?;
    }
    Ok(d)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Envelope {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Value,
    /// Base64 X25519 key the answer is sealed to.
    pub reply_key: String,
    /// Milliseconds since the epoch after which the request is void.
    pub expires: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    ok: bool,
    #[serde(default)]
    sealed: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Holds the bridge lock for as long as the bridge runs.
pub struct BridgeLock(#[allow(dead_code)] File);

/// Becomes the bridge. Retries briefly: a bridge that just closed (access toggled
/// off and on) or a `bridge_connected` check may hold the lock for a moment. Requests
/// left from before are dropped, never delivered late.
pub fn claim_bridge() -> Result<BridgeLock, String> {
    let d = ensure()?;
    let file = File::options().create(true).truncate(false).write(true).open(d.join("bridge.lock")).map_err(|e| e.to_string())?;
    let end = Instant::now() + Duration::from_millis(1500);
    while file.try_lock().is_err() {
        if Instant::now() > end {
            return Err("Another Chrome window already holds the Claude bridge".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    clear_requests();
    Ok(BridgeLock(file))
}

/// Drops every waiting request (the bridge starting or ending).
pub fn clear_requests() {
    for e in fs::read_dir(dir().join("requests")).into_iter().flatten().flatten() {
        let _ = fs::remove_file(e.path());
    }
}

/// True while an extension bridge is connected.
pub fn bridge_connected() -> bool {
    let Ok(d) = ensure() else { return false };
    match File::options().create(true).truncate(false).write(true).open(d.join("bridge.lock")) {
        // A shared probe: probes never block each other, only a bridge's exclusive lock.
        Ok(f) => match f.try_lock_shared() {
            Ok(()) => {
                let _ = f.unlock();
                false
            }
            Err(_) => true,
        },
        Err(_) => false,
    }
}

/// MCP side: sends a command to the extension and waits for its answer.
pub fn call(command: &str, args: Value, timeout: Duration) -> Result<Value, String> {
    let d = ensure()?;
    if !bridge_connected() {
        return Err("The extension isn't connected. Open Chrome with Pocket Scraper Secure and turn on Claude access in its dashboard.".into());
    }
    clear_stale_answers();
    let id = format!("{}-{}", now_ms(), pocket_scraper_core::hex(&pocket_scraper_core::random_bytes::<8>()));
    let key = SecretKey::generate();
    let request = d.join("requests").join(format!("{id}.json"));
    let response = d.join("responses").join(format!("{id}.json"));
    let envelope = Envelope {
        id: id.clone(),
        command: command.into(),
        args,
        reply_key: base64::engine::general_purpose::STANDARD.encode(key.public_key()),
        expires: now_ms() + timeout.as_millis() as u64,
    };
    let body = serde_json::to_vec(&envelope).map_err(|e| e.to_string())?;
    if body.len() as u64 > REQUEST_MAX {
        return Err("The request is too large".into());
    }
    write_private(&request, &body, 0o600).map_err(|e| e.to_string())?;
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if let Ok(text) = fs::read(&response) {
            let _ = fs::remove_file(&response);
            let reply: Reply = serde_json::from_slice(&text).map_err(|e| format!("The extension's answer is malformed: {e}"))?;
            if !reply.ok {
                return Err(reply.error.unwrap_or_else(|| "The extension refused".into()));
            }
            let sealed = reply.sealed.ok_or("The extension's answer wasn't sealed")?;
            let plain = vault::open_reply(&key, &id, &sealed).map_err(|e| e.to_string())?;
            return serde_json::from_slice(&plain).map_err(|e| format!("The extension's answer is malformed: {e}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = fs::remove_file(&request);
    let _ = fs::remove_file(&response);
    Err(format!("The extension didn't answer within {}s.", timeout.as_secs()))
}

fn clear_stale_answers() {
    for e in fs::read_dir(dir().join("responses")).into_iter().flatten().flatten() {
        if e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|age| age > STALE) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Bridge side: requests waiting in the mailbox, oldest first, each removed as it is
/// taken, as native-messaging responses split to fit Chrome's 1 MiB limit. Expired
/// requests and unclaimed answers are cleared.
pub fn take_requests() -> Vec<Response> {
    let Ok(d) = ensure() else { return vec![] };
    let mut files: Vec<PathBuf> = fs::read_dir(d.join("requests")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json")).collect();
    files.sort();
    let mut out = vec![];
    for path in files {
        let too_big = fs::metadata(&path).map(|m| m.len() > REQUEST_MAX).unwrap_or(true);
        let text = if too_big { None } else { fs::read_to_string(&path).ok() };
        let _ = fs::remove_file(&path);
        let Some(text) = text else { continue };
        let Ok(envelope) = serde_json::from_str::<Envelope>(&text) else { continue };
        if !valid_id(&envelope.id) || envelope.expires < now_ms() {
            continue;
        }
        let parts = crate::transport::split(&text, PIECE);
        let n = parts.len();
        out.extend(parts.into_iter().enumerate().map(|(i, t)| Response::BridgeRequest { id: envelope.id.clone(), part: i, parts: n, text: t }));
    }
    clear_stale_answers();
    out
}

/// Bridge side: stores the extension's answer for the waiting MCP server.
pub fn write_reply(id: &str, ok: bool, sealed: Option<String>, error: Option<String>) -> Result<(), String> {
    if !valid_id(id) {
        return Err("Malformed request ID".into());
    }
    let body = serde_json::to_vec(&Reply { ok, sealed, error }).map_err(|e| e.to_string())?;
    write_private(&ensure()?.join("responses").join(format!("{id}.json")), &body, 0o600).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_restricted() {
        assert!(valid_id("1792700238429-0a1b2c3d4e5f6a7b"));
        assert!(!valid_id("../../x"));
        assert!(!valid_id(""));
        assert!(!valid_id(&"a".repeat(65)));
    }
}
