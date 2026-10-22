//! A small blocking Chrome DevTools Protocol client over the debugging pipe.
//! Messages are JSON terminated by a NUL byte.

use serde_json::{json, Value};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Larger replies (huge pages) are refused rather than buffered.
const MESSAGE_MAX: usize = 64 * 1024 * 1024;

pub struct Cdp {
    writer: File,
    replies: Receiver<Value>,
    next_id: u64,
}

impl Cdp {
    pub fn new(writer: File, reader: File) -> Cdp {
        let (tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let mut buf = Vec::new();
                match reader.by_ref().take(MESSAGE_MAX as u64 + 1).read_until(0, &mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                if buf.last() != Some(&0) {
                    return; // oversized or truncated: stop reading; calls will time out
                }
                buf.pop();
                // Only replies (with an id) are used; events are ignored.
                if let Ok(v) = serde_json::from_slice::<Value>(&buf) {
                    if v.get("id").is_some() && tx.send(v).is_err() {
                        return;
                    }
                }
            }
        });
        Cdp { writer, replies, next_id: 1 }
    }

    /// Sends a command and waits for its reply.
    pub fn call(&mut self, session: Option<&str>, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            message["sessionId"] = json!(s);
        }
        let mut bytes = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
        bytes.push(0);
        self.writer.write_all(&bytes).map_err(|_| "The browser closed unexpectedly".to_string())?;
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(left) {
                Ok(v) if v["id"] == id => {
                    if let Some(err) = v.get("error") {
                        return Err(err["message"].as_str().unwrap_or("Browser command failed").to_string());
                    }
                    return Ok(v["result"].clone());
                }
                Ok(_) => continue, // a late reply to an earlier, timed-out call
                Err(RecvTimeoutError::Timeout) => return Err(format!("The browser did not answer {method} in time")),
                Err(RecvTimeoutError::Disconnected) => return Err("The browser closed unexpectedly".into()),
            }
        }
    }
}
