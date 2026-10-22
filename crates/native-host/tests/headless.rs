//! Real headless Chrome over the debugging pipe, against a local test site. Skipped
//! when no Chrome is installed. One browser at a time.

use pocket_scraper_core::plan::RunOptions;
use pocket_scraper_core::protocol::Response;
use pocket_scraper_core::recipe::Recipe;
use pocket_scraper_core::records::PlainRecord;
use pocket_scraper_core::vault::{self, KdfParams};
use pocket_scraper_host::browser::{find_chrome, Browser};
use pocket_scraper_host::headless;
use pocket_scraper_host::driver::{self, Outcome};
use pocket_scraper_host::handler;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;

fn page(path: &str) -> (u16, String) {
    let list = |n: u32| {
        let items: String = (1..=2).map(|i| format!("<li class=card><span class=name>Item {}</span></li>", (n - 1) * 2 + i)).collect();
        let next = if n < 3 { format!("<a class=next href=\"/list?page={}\">Next</a>", n + 1) } else { String::new() };
        format!("<title>List</title><ul>{items}</ul>{next}")
    };
    match path {
        "/list" | "/list?page=1" => (200, list(1)),
        "/list?page=2" => (200, list(2)),
        "/list?page=3" => (200, list(3)),
        // "Load more" empties the list (a spinner), then redraws it with more items after 700 ms.
        "/ajax" => (200, r#"<title>Ajax</title><ul id=items><li class=card><span class=name>A1</span></li><li class=card><span class=name>A2</span></li></ul>
<button id=more>Load more</button><script>
let shown = 2;
document.getElementById('more').onclick = () => {
  const list = document.getElementById('items');
  const keep = list.innerHTML;
  list.innerHTML = '';
  setTimeout(() => {
    shown += 2;
    list.innerHTML = keep + [shown - 1, shown].map(n => `<li class=card><span class=name>A${n}</span></li>`).join('');
    if (shown >= 6) document.getElementById('more').remove();
  }, 700);
};
</script>"#.into()),
        "/workflow" => (200, r#"<title>Steps</title><span id=value>0</span><button id=next>Next</button><script>
let n = 0; document.getElementById('next').onclick = () => setTimeout(() => { document.getElementById('value').textContent = String(++n); }, 50);
</script>"#.into()),
        "/forbidden" => (403, "<title>Forbidden</title><p>No</p>".into()),
        "/verify" => (200, "<title>Just a moment...</title><p>Verify you are human by completing the action below.</p>".into()),
        _ => (404, "<title>Not found</title>".into()),
    }
}

fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).map(|n| n == 0).unwrap_or(true) || h == "\r\n" {
                        break;
                    }
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, body) = page(&path);
                let mut stream = stream;
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    base
}

fn recipe(json: serde_json::Value) -> Recipe {
    Recipe::from_json(&json.to_string()).unwrap()
}

fn names(outcome: Outcome) -> (Vec<String>, Vec<String>) {
    match outcome {
        Outcome::Finished { rows, warnings } => (rows.into_iter().map(|r| r.fields["name"].clone()).collect(), warnings),
        Outcome::Failed { message, .. } => panic!("run failed: {message}"),
    }
}

fn with_browser(profile: &PathBuf, f: impl FnOnce(&mut Browser)) {
    let chrome = find_chrome().unwrap();
    let mut browser = Browser::launch_headless(&chrome, profile).expect("launch headless Chrome");
    f(&mut browser);
    browser.close();
}

#[test]
fn recipes_run_in_headless_chrome_over_the_pipe() {
    // The runs' own browser: PSS_BROWSER_PATH, or the pinned headless shell installed
    // by `pocket-scraper-host install-browser` (looked up before the test data folder
    // replaces the real one).
    let Some(shell) = std::env::var_os("PSS_BROWSER_PATH").map(PathBuf::from).or_else(headless::installed) else {
        eprintln!("skipped: run `pocket-scraper-host install-browser` first");
        return;
    };
    std::env::set_var("PSS_BROWSER_PATH", &shell);
    assert!(find_chrome().is_some());
    let base = serve();
    let data = std::env::temp_dir().join(format!("pss-host-test-{}", std::process::id()));
    std::env::set_var("PSS_DATA_DIR", &data);
    let profile = data.join("profile");
    let timeout = RunOptions::default().run_timeout_ms;

    // URL pagination: three pages through a Next link.
    with_browser(&profile, |b| {
        let r = recipe(serde_json::json!({ "name": "list", "url": format!("{base}/list"), "rowSelector": "li.card", "fields": [{ "name": "name", "selector": ".name" }], "nextSelector": "a.next" }));
        let (rows, warnings) = names(driver::run(b, r, timeout));
        assert_eq!(rows, ["Item 1", "Item 2", "Item 3", "Item 4", "Item 5", "Item 6"]);
        assert!(warnings.is_empty(), "{warnings:?}");
    });

    // "Load more" that empties the list, then redraws it with more after 700 ms.
    with_browser(&profile, |b| {
        let r = recipe(serde_json::json!({ "name": "ajax", "url": format!("{base}/ajax"), "rowSelector": "li.card", "fields": [{ "name": "name", "selector": ".name" }], "nextSelector": "#more" }));
        let (rows, warnings) = names(driver::run(b, r, timeout));
        assert_eq!(rows, ["A1", "A2", "A3", "A4", "A5", "A6"]);
        assert!(warnings.is_empty(), "{warnings:?}");
    });

    // A click / wait / extract workflow.
    with_browser(&profile, |b| {
        let r = recipe(serde_json::json!({ "name": "steps", "url": format!("{base}/workflow"), "fields": [{ "name": "name", "selector": "#value" }],
            "workflow": { "steps": [{ "type": "click", "selector": "#next", "waitMs": 0 }, { "type": "wait", "waitMs": 200 }, { "type": "extract" }], "repeatCount": 3 } }));
        assert_eq!(names(driver::run(b, r, timeout)).0, ["1", "2", "3"]);
    });

    // Access errors and verification pages need the person.
    with_browser(&profile, |b| {
        let r = recipe(serde_json::json!({ "name": "403", "url": format!("{base}/forbidden"), "fields": [{ "name": "name", "selector": "p" }] }));
        match driver::run(b, r, timeout) {
            Outcome::Failed { message, attention } => {
                assert!(message.contains("HTTP 403"), "{message}");
                assert!(attention);
            }
            Outcome::Finished { .. } => panic!("403 should fail"),
        }
        let r = recipe(serde_json::json!({ "name": "verify", "url": format!("{base}/verify"), "fields": [{ "name": "name", "selector": "p" }] }));
        match driver::run(b, r, timeout) {
            Outcome::Failed { message, attention } => {
                assert!(message.contains("browser security verification"), "{message}");
                assert!(attention);
            }
            Outcome::Finished { .. } => panic!("verification page should fail"),
        }
    });

    // The whole request path: records come back sealed and open with the vault.
    let pass = "correct horse battery staple";
    let (header, _) = vault::create(pass, KdfParams::default()).unwrap();
    let request = serde_json::json!({
        "type": "run", "runId": "run-1", "observedAt": "2026-10-22T12:00:00.000Z", "runTimeoutMs": 60000,
        "publicKey": serde_json::to_value(&header).unwrap()["publicKey"],
        "recipe": { "id": "recipe-1", "name": "list", "url": format!("{base}/list"), "rowSelector": "li.card", "fields": [{ "name": "name", "selector": ".name" }], "nextSelector": "a.next", "maxPages": 2 }
    });
    let responses = handler::handle(request.to_string().as_bytes());
    let key = vault::unlock(&header, pass).unwrap();
    let mut opened = vec![];
    for r in &responses {
        if let Response::RunChunk { records, .. } = r {
            for rec in records {
                let plain: PlainRecord = serde_json::from_slice(&vault::open(&key, &vault::record_aad("recipe-1", &rec.key), &rec.sealed).unwrap()).unwrap();
                opened.push(plain.fields["name"].clone());
            }
        }
    }
    assert_eq!(opened, ["Item 1", "Item 2", "Item 3", "Item 4"]);
    assert!(matches!(responses.last(), Some(Response::RunDone { row_count: 4, .. })), "{responses:?}");
    // Profiles are per site and owner-only.
    let profiles = data.join("profiles");
    let entries: Vec<_> = std::fs::read_dir(&profiles).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1);
    // The folder name is salted: it isn't the plain hash of the origin.
    let unsalted = pocket_scraper_core::url::site_key(b"", &base);
    assert_ne!(entries[0].file_name().to_string_lossy(), unsalted);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(entries[0].metadata().unwrap().permissions().mode() & 0o777, 0o700);
        for dir in [&data, &profiles] {
            assert_eq!(std::fs::metadata(dir).unwrap().permissions().mode() & 0o777, 0o700, "{}", dir.display());
        }
        assert_eq!(std::fs::metadata(data.join("profile-salt")).unwrap().permissions().mode() & 0o777, 0o600);

        // A lock left by a killed browser doesn't block the site.
        let profile_dir = entries[0].path();
        std::os::unix::fs::symlink("somehost-999999", profile_dir.join("SingletonLock")).unwrap();
        assert!(!pocket_scraper_host::browser::profile_in_use(&profile_dir), "a dead pid's lock is stale");
        assert!(std::fs::symlink_metadata(profile_dir.join("SingletonLock")).is_err(), "and is removed");
        std::os::unix::fs::symlink(format!("somehost-{}", std::process::id()), profile_dir.join("SingletonLock")).unwrap();
        assert!(pocket_scraper_host::browser::profile_in_use(&profile_dir), "a live pid's lock is respected");
        std::fs::remove_file(profile_dir.join("SingletonLock")).unwrap();
    }
    #[cfg(windows)]
    {
        // A leftover lockfile nobody holds open is stale and cleared.
        let profile_dir = entries[0].path();
        std::fs::write(profile_dir.join("lockfile"), b"").unwrap();
        assert!(!pocket_scraper_host::browser::profile_in_use(&profile_dir));
    }
    let _ = std::fs::remove_dir_all(&data);
}
