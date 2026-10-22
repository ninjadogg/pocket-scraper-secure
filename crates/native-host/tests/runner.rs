//! The runner binary end to end: real headless shell, local test site, signed bundles
//! in, signed sealed batches out. Skipped when no browser is installed.

use pocket_scraper_core::bundle::{self, fingerprint, sign_bundle};
use pocket_scraper_core::records::PlainRecord;
use pocket_scraper_core::recipe::Recipe;
use pocket_scraper_core::vault::{self, KdfParams};
use pocket_scraper_host::headless;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

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
                let n: u32 = line.split("page=").nth(1).and_then(|s| s.split_whitespace().next()).and_then(|s| s.parse().ok()).unwrap_or(1);
                let cards: String = (1..=2).map(|i| format!("<div class=quote><span class=text>Quote {}</span></div>", (n - 1) * 2 + i)).collect();
                let next = if n < 2 { format!("<ul class=pager><li class=next><a href=\"/?page={}\">Next</a></li></ul>", n + 1) } else { String::new() };
                let body = format!("<title>Q</title>{cards}{next}");
                let mut stream = stream;
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    base
}

struct Runner {
    data: PathBuf,
    browser: PathBuf,
}

impl Runner {
    fn cmd(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_pocket-scraper-runner"))
            .args(args)
            .env("PSS_DATA_DIR", &self.data)
            .env("PSS_BROWSER_PATH", &self.browser)
            .output()
            .unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

fn write(dir: &Path, name: &str, content: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p.display().to_string()
}

#[test]
fn bundles_in_signed_sealed_batches_out() {
    let Some(browser) = std::env::var_os("PSS_BROWSER_PATH").map(PathBuf::from).or_else(headless::installed) else {
        eprintln!("skipped: run `pocket-scraper-runner install-browser` first");
        return;
    };
    let base = serve();
    let data = std::env::temp_dir().join(format!("pss-runner-test-{}", std::process::id()));
    let files = data.join("files");
    std::fs::create_dir_all(&files).unwrap();
    let r = Runner { data: data.clone(), browser };

    // The runner's identity.
    let (ok, out) = r.cmd(&["id"]);
    assert!(ok, "{out}");
    let key_b64 = out.lines().find_map(|l| l.strip_prefix("key:")).unwrap().trim().to_string();

    // A vault exports a bundle (scheduled hourly).
    let pass = "correct horse battery staple";
    let (header, secret) = vault::create(pass, KdfParams::default()).unwrap();
    let recipe = Recipe::from_json(&serde_json::json!({ "id": "quotes", "name": "Quotes", "url": format!("{base}/"), "rowSelector": "div.quote",
        "fields": [{ "name": "quote", "selector": "span.text" }], "nextSelector": "li.next > a", "maxPages": 3, "scheduleMinutes": 60 }).to_string()).unwrap();
    let bundle_path = write(&files, "quotes.pssbundle", &sign_bundle(&secret, &recipe, "2026-10-22T12:00:00Z", 60_000).unwrap());
    let vault_fp = fingerprint(&header.signing_key);

    // Not trusted until the person confirms the fingerprint.
    let (ok, out) = r.cmd(&["add", &bundle_path]);
    assert!(!ok && out.contains(&vault_fp), "{out}");
    let (ok, out) = r.cmd(&["add", &bundle_path, "--trust", "0000-0000-0000-0000-0000"]);
    assert!(!ok && out.contains("Nothing was added"), "{out}");
    let (ok, out) = r.cmd(&["add", &bundle_path, "--trust", &vault_fp]);
    assert!(ok, "{out}");

    // A different vault's bundle, and a tampered one, are refused from now on.
    let (_, other) = vault::create(pass, KdfParams::default()).unwrap();
    let foreign = write(&files, "foreign.pssbundle", &sign_bundle(&other, &recipe, "2026-10-22T12:00:00Z", 60_000).unwrap());
    let (ok, out) = r.cmd(&["add", &foreign]);
    assert!(!ok && out.contains("different vault"), "{out}");
    let tampered = std::fs::read_to_string(&bundle_path).unwrap().replacen("\"signature\": \"", "\"signature\": \"AAAA", 1);
    let (ok, _) = r.cmd(&["add", &write(&files, "tampered.pssbundle", &tampered)]);
    assert!(!ok);

    // Due straight away (never run), then not again within the hour.
    let (ok, out) = r.cmd(&["run-due"]);
    assert!(ok && out.contains("Quotes: Success, 4 rows"), "{out}");
    let (ok, out) = r.cmd(&["run-due"]);
    assert!(ok && out.contains("Nothing was due"), "{out}");
    let (ok, out) = r.cmd(&["list"]);
    assert!(ok && out.contains("quotes  Quotes") && out.contains("next "), "{out}");

    // The batch is signed by this runner and opens with the vault.
    let outbox: Vec<PathBuf> = std::fs::read_dir(data.join("runner/outbox")).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(outbox.len(), 1);
    let json = std::fs::read_to_string(&outbox[0]).unwrap();
    assert!(!json.contains("Quote 1"), "records are sealed");
    use base64::Engine;
    let runner_key: [u8; 32] = base64::engine::general_purpose::STANDARD.decode(&key_b64).unwrap().try_into().unwrap();
    let batch = bundle::open_batch(&json, &[runner_key], &header.public_key().unwrap()).unwrap();
    assert_eq!(batch.recipe_id, "quotes");
    let key = vault::unlock(&header, pass).unwrap();
    let mut quotes: Vec<String> = batch.records.iter().map(|rec| {
        let plain: PlainRecord = serde_json::from_slice(&vault::open(&key, &vault::record_aad("quotes", &rec.key), &rec.sealed).unwrap()).unwrap();
        plain.fields["quote"].clone()
    }).collect();
    quotes.sort();
    assert_eq!(quotes, ["Quote 1", "Quote 2", "Quote 3", "Quote 4"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [data.join("runner"), data.join("runner/runner-key"), data.join("runner/trusted-vault"), outbox[0].clone()] {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert!(mode == 0o700 || mode == 0o600, "{} is {mode:o}", p.display());
        }
    }

    let (ok, _) = r.cmd(&["run", "quotes"]);
    assert!(ok);
    let (ok, out) = r.cmd(&["outbox"]);
    assert!(ok && out.contains("2 batch file(s)"), "{out}");
    let (ok, _) = r.cmd(&["remove", "quotes"]);
    assert!(ok);
    let (ok, out) = r.cmd(&["list"]);
    assert!(ok && out.contains("No recipes"), "{out}");
    let _ = std::fs::remove_dir_all(&data);
}
