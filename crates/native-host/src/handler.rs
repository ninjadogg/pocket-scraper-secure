//! The native messaging session: one request at a time from the extension.

use crate::browser::{self, Browser};
use crate::driver::{self, Outcome};
use crate::framing::{read_message, write_message};
use crate::paths;
use pocket_scraper_core::limits::HOST_MESSAGE_MAX;
use pocket_scraper_core::protocol::{self, chunk, seal_rows, Request, Response, PROTOCOL_VERSION};
use pocket_scraper_core::url;
use std::io::{self, Read, Write};

pub fn serve(input: &mut impl Read, output: &mut impl Write) -> io::Result<()> {
    while let Some(bytes) = read_message(input)? {
        for response in handle(&bytes) {
            write_message(output, &serde_json::to_vec(&response).expect("response serialises"))?;
        }
    }
    Ok(())
}

pub fn handle(bytes: &[u8]) -> Vec<Response> {
    let request = match Request::from_json(bytes) {
        Ok(r) => r,
        Err(e) => return vec![Response::Error { message: e.to_string() }],
    };
    match request {
        Request::Hello { .. } => vec![Response::Hello { version: PROTOCOL_VERSION, host_version: env!("CARGO_PKG_VERSION").into() }],
        Request::Run { run_id, recipe, public_key, run_timeout_ms, observed_at } => {
            let run = match protocol::validate_run(run_id.clone(), recipe, public_key, run_timeout_ms, observed_at) {
                Ok(run) => run,
                Err(e) => {
                    return vec![match protocol::check_id(&run_id) {
                        Ok(()) => Response::RunFailed { run_id, message: e.to_string(), attention: false },
                        Err(_) => Response::Error { message: e.to_string() },
                    }]
                }
            };
            let failed = |message: String, attention: bool| vec![Response::RunFailed { run_id: run.run_id.clone(), message, attention }];
            let Some(chrome) = browser::find_chrome() else {
                return failed("Chrome was not found. Install Google Chrome or set PSS_BROWSER_PATH.".into(), false);
            };
            let profile = match paths::profile_dir(&run.recipe.origin) {
                Ok(p) => p,
                Err(e) => return failed(format!("Could not create the browser profile: {e}"), false),
            };
            let mut browser = match Browser::launch_headless(&chrome, &profile) {
                Ok(b) => b,
                Err(message) => return failed(message, false),
            };
            // The planner checks its deadline between steps; the watchdog makes sure a
            // stuck browser call can't outlive the run (killing Chrome ends every call).
            let (done, cancelled) = std::sync::mpsc::channel::<()>();
            let pid = browser.pid();
            let limit = std::time::Duration::from_millis(run.run_timeout_ms + 5_000);
            let watchdog = std::thread::spawn(move || {
                if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = cancelled.recv_timeout(limit) {
                    browser::kill_process(pid);
                }
            });
            let recipe_id = run.recipe.id.clone();
            let outcome = driver::run(&mut browser, run.recipe, run.run_timeout_ms);
            let _ = done.send(());
            let _ = watchdog.join();
            browser.close();
            match outcome {
                Outcome::Failed { message, attention } => failed(message, attention),
                Outcome::Finished { rows, warnings } => {
                    let sealed = match seal_rows(&run.public_key, &recipe_id, &run.run_id, &run.observed_at, &rows) {
                        Ok(s) => s,
                        Err(e) => return failed(e.to_string(), false),
                    };
                    let mut out: Vec<Response> = chunk(sealed, HOST_MESSAGE_MAX - 64 * 1024)
                        .into_iter()
                        .map(|records| Response::RunChunk { run_id: run.run_id.clone(), records })
                        .collect();
                    out.push(Response::RunDone { run_id: run.run_id, row_count: rows.len(), warnings });
                    out
                }
            }
        }
        Request::OpenSession { url } => match url::parse(&url) {
            Err(e) => vec![Response::Error { message: e.to_string() }],
            Ok(u) => {
                let Some(chrome) = browser::find_chrome() else {
                    return vec![Response::Error { message: "Chrome was not found.".into() }];
                };
                let opened = paths::profile_dir(&u.origin).map_err(|e| e.to_string()).and_then(|p| browser::open_visible(&chrome, &p, &u.href));
                match opened {
                    Ok(()) => vec![Response::SessionOpened { origin: u.origin }],
                    Err(message) => vec![Response::Error { message }],
                }
            }
        },
        Request::CloseSession { url } => match url::parse(&url) {
            Err(e) => vec![Response::Error { message: e.to_string() }],
            Ok(u) if paths::profile_dir(&u.origin).map(|p| browser::profile_in_use(&p)).unwrap_or(false) => {
                vec![Response::Error { message: "Close the session window first; its sign-in is saved when it closes.".into() }]
            }
            Ok(u) => vec![Response::SessionClosed { origin: u.origin }],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_and_malformed_requests() {
        assert_eq!(handle(br#"{"type":"hello","version":1}"#), vec![Response::Hello { version: 1, host_version: env!("CARGO_PKG_VERSION").into() }]);
        assert!(matches!(&handle(br#"{"type":"hello","version":1,"x":2}"#)[0], Response::Error { .. }));
        assert!(matches!(&handle(b"not json")[0], Response::Error { .. }));
        assert!(matches!(&handle(br#"{"type":"openSession","url":"file:///etc/passwd"}"#)[0], Response::Error { .. }));
        let bad_run = br#"{"type":"run","runId":"r1","recipe":{"name":"t","url":"javascript:alert(1)","fields":[{"name":"a","selector":"x"}]},"publicKey":"AA==","runTimeoutMs":5000,"observedAt":"2026-10-22T00:00:00Z"}"#;
        assert!(matches!(&handle(bad_run)[0], Response::RunFailed { attention: false, .. }));
    }
}
