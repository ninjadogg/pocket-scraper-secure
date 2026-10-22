//! The MCP ↔ extension bridge through the real host binary: a request dropped in the
//! mailbox reaches "the extension" over native messaging, and its reply comes back.

use pocket_scraper_core::vault;
use pocket_scraper_host::framing::read_message;
use pocket_scraper_host::mailbox;
use serde_json::{json, Value};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

const ORIGIN: &str = "chrome-extension://jlahbgbapoiedphhhpnceaajoibjpkdm/";

fn host(data: &std::path::Path) -> (Child, ChildStdin, ChildStdout) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pocket-scraper-host"))
        .arg(ORIGIN)
        .env("PSS_DATA_DIR", data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (stdin, stdout) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
    (child, stdin, stdout)
}

/// Writes a frame as the extension would: Chrome allows extension → host messages far
/// larger than the 1 MiB host → extension limit `write_message` enforces.
fn send(stdin: &mut ChildStdin, v: Value) {
    use std::io::Write;
    let body = v.to_string().into_bytes();
    stdin.write_all(&(body.len() as u32).to_ne_bytes()).unwrap();
    stdin.write_all(&body).unwrap();
    stdin.flush().unwrap();
}
fn recv(stdout: &mut ChildStdout) -> Value {
    serde_json::from_slice(&read_message(stdout).unwrap().unwrap()).unwrap()
}

#[test]
fn commands_reach_the_extension_and_answers_come_back() {
    let data = std::env::temp_dir().join(format!("pss-bridge-test-{}", std::process::id()));
    std::env::set_var("PSS_DATA_DIR", &data);

    // No bridge yet: calls fail at once with a useful message.
    let err = mailbox::call("ping", json!({}), Duration::from_secs(5)).unwrap_err();
    assert!(err.contains("turn on Claude access"), "{err}");

    // Requests past their deadline are never delivered.
    let requests = data.join("mailbox").join("requests");
    let envelope = |id: &str, expires: u64| json!({ "id": id, "command": "x", "args": {}, "replyKey": "", "expires": expires }).to_string();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    std::fs::write(requests.join("1-old.json"), envelope("1-old", now - 1)).unwrap();
    std::fs::write(requests.join("2-new.json"), envelope("2-new", now + 60_000)).unwrap();
    let taken = mailbox::take_requests();
    assert_eq!(taken.len(), 1);
    assert!(matches!(&taken[0], pocket_scraper_core::protocol::Response::BridgeRequest { id, .. } if id == "2-new"));
    assert_eq!(std::fs::read_dir(&requests).unwrap().count(), 0);

    let (mut child, mut stdin, mut stdout) = host(&data);
    send(&mut stdin, json!({ "type": "bridge" }));
    assert_eq!(recv(&mut stdout)["type"], "bridgeReady");
    assert!(mailbox::bridge_connected());

    // A second bridge is refused while this one is open.
    let (mut other, mut other_in, mut other_out) = host(&data);
    send(&mut other_in, json!({ "type": "bridge" }));
    assert!(recv(&mut other_out)["message"].as_str().unwrap().contains("already holds"));
    drop(other_in);
    let _ = other.wait();

    // The "extension": answers whatever arrives, sealed to the request's one-time key,
    // with a large reply to check the size limit.
    let extension = std::thread::spawn(move || {
        for _ in 0..3 {
            let mut text = String::new();
            let id = loop {
                let m = recv(&mut stdout);
                assert_eq!(m["type"], "bridgeRequest");
                text.push_str(m["text"].as_str().unwrap());
                if m["part"].as_u64().unwrap() + 1 == m["parts"].as_u64().unwrap() {
                    break m["id"].as_str().unwrap().to_string();
                }
            };
            let request: Value = serde_json::from_str(&text).unwrap();
            let seal = |v: Value| vault::seal_reply(request["replyKey"].as_str().unwrap(), &id, v.to_string().as_bytes()).unwrap();
            let reply = match request["command"].as_str().unwrap() {
                "echo" => json!({ "type": "bridgeReply", "id": id, "ok": true, "sealed": seal(json!({ "len": request["args"]["blob"].as_str().unwrap().len(), "big": "y".repeat(3 * 1024 * 1024) })) }),
                // An answer not sealed to the asker is refused.
                "unsealed" => json!({ "type": "bridgeReply", "id": id, "ok": true }),
                _ => json!({ "type": "bridgeReply", "id": id, "ok": false, "error": "Unknown command" }),
            };
            send(&mut stdin, reply);
        }
        stdin
    });

    // A request bigger than one native message (it is split, and quotes double in
    // size when escaped) and a 3 MB answer.
    let result = mailbox::call("echo", json!({ "blob": "\"".repeat(2 * 1024 * 1024) }), Duration::from_secs(20)).unwrap();
    assert_eq!(result["len"], 2 * 1024 * 1024);
    assert_eq!(result["big"].as_str().unwrap().len(), 3 * 1024 * 1024);
    let err = mailbox::call("nope", json!({}), Duration::from_secs(20)).unwrap_err();
    assert_eq!(err, "Unknown command");
    let err = mailbox::call("unsealed", json!({}), Duration::from_secs(20)).unwrap_err();
    assert!(err.contains("wasn't sealed"), "{err}");
    // Nothing readable was left behind.
    assert_eq!(std::fs::read_dir(data.join("mailbox").join("responses")).unwrap().count(), 0);

    // When the extension goes away, the bridge closes with it.
    drop(extension.join().unwrap());
    let _ = child.wait();
    assert!(!mailbox::bridge_connected());
    let _ = std::fs::remove_dir_all(&data);
}
