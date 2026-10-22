//! Runner upload and desktop collection through a local S3-compatible stub. The stub
//! re-derives every request's SigV4 signature from the headers it actually received,
//! so the curl plumbing (header file, URL encoding, payload hash) is checked for real.

use pocket_scraper_core::protocol::Response;
use pocket_scraper_host::{s3, transport};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::{Arc, Mutex};

const SECRET: &str = "test-secret-key";

fn stub() -> (String, Arc<Mutex<BTreeMap<String, Vec<u8>>>>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let host = endpoint.trim_start_matches("http://").to_string();
    let objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Default::default();
    let problems: Arc<Mutex<Vec<String>>> = Default::default();
    let (o, p) = (objects.clone(), problems.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut parts = line.split_whitespace();
            let (method, target) = (parts.next().unwrap().to_string(), parts.next().unwrap().to_string());
            let mut headers = BTreeMap::new();
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h == "\r\n" {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
            let mut body = vec![0u8; headers.get("content-length").map(|l| l.parse().unwrap()).unwrap_or(0)];
            reader.read_exact(&mut body).unwrap();
            let (path, query) = target.split_once('?').unwrap_or((&target, ""));
            let pairs: Vec<(String, String)> = query.split('&').filter(|s| !s.is_empty()).map(|kv| {
                let (k, v) = kv.split_once('=').unwrap();
                (decode(k), decode(v))
            }).collect();
            // Re-sign what arrived and compare.
            let date = headers.get("x-amz-date").cloned().unwrap_or_default();
            let hash = headers.get("x-amz-content-sha256").cloned().unwrap_or_default();
            let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let expected = s3::sign(&method, path, &refs, &[("host", &host), ("x-amz-date", &date), ("x-amz-content-sha256", &hash)], &hash, &date, "us-east-1", "s3", "AKTEST", SECRET);
            if headers.get("authorization") != Some(&expected.authorization) {
                p.lock().unwrap().push(format!("bad signature for {method} {target}"));
            }
            if hash != pocket_scraper_core::hex(&Sha256::digest(&body)) {
                p.lock().unwrap().push(format!("payload hash mismatch for {target}"));
            }
            let key = decode(path.trim_start_matches("/bucket").trim_start_matches('/'));
            let (status, response) = match method.as_str() {
                "PUT" => {
                    o.lock().unwrap().insert(key, body);
                    (200, vec![])
                }
                "GET" if key.is_empty() => {
                    let get = |name: &str| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
                    let prefix = get("prefix").unwrap_or_default();
                    let objects = o.lock().unwrap();
                    let body = if get("delimiter").as_deref() == Some("/") {
                        let folders: std::collections::BTreeSet<String> = objects.keys().filter_map(|k| k.strip_prefix(&prefix).and_then(|rest| rest.find('/').map(|i| format!("{prefix}{}", &rest[..=i])))).collect();
                        folders.iter().map(|f| format!("<CommonPrefixes><Prefix>{f}</Prefix></CommonPrefixes>")).collect::<String>()
                    } else {
                        let after = get("start-after").unwrap_or_default();
                        let max: usize = get("max-keys").and_then(|m| m.parse().ok()).unwrap_or(1000);
                        objects.keys().filter(|k| k.starts_with(&prefix) && **k > after).take(max).map(|k| format!("<Contents><Key>{k}</Key></Contents>")).collect::<String>()
                    };
                    (200, format!("<ListBucketResult><IsTruncated>false</IsTruncated>{body}</ListBucketResult>").into_bytes())
                }
                "GET" => match o.lock().unwrap().get(&key) {
                    Some(b) => (200, b.clone()),
                    None => (404, b"<Error><Code>NoSuchKey</Code></Error>".to_vec()),
                },
                _ => (405, vec![]),
            };
            let mut s = stream;
            let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len());
            let _ = s.write_all(&response);
        }
    });
    (endpoint, objects, problems)
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

#[test]
fn runners_upload_and_the_desktop_collects() {
    let (endpoint, objects, problems) = stub();
    let base = std::env::temp_dir().join(format!("pss-bucket-test-{}", std::process::id()));
    let (runner_data, desktop_data) = (base.join("runner"), base.join("desktop"));
    let configure = |exe: &str, cmd: &[&str], data: &std::path::Path| {
        let out = Command::new(exe).args(cmd).args(["--endpoint", &endpoint, "--bucket", "bucket", "--region", "us-east-1", "--prefix", "pss/", "--access-key-id", "AKTEST"])
            .env("PSS_DATA_DIR", data).env("PSS_S3_SECRET_ACCESS_KEY", SECRET).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    let runner = env!("CARGO_BIN_EXE_pocket-scraper-runner");
    configure(runner, &["upload", "configure"], &runner_data);
    let config = std::fs::read_to_string(runner_data.join("runner/upload.json")).unwrap();
    assert!(config.contains(SECRET), "stored for the runner's own use…");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(runner_data.join("runner/upload.json")).unwrap().permissions().mode() & 0o777, 0o600, "…owner-only");
    }

    // Two batches waiting (contents are opaque to the transport; one has non-ASCII and
    // is big enough to need several native-messaging parts).
    let outbox = runner_data.join("runner/outbox");
    std::fs::create_dir_all(&outbox).unwrap();
    let big = format!("{{\"format\":\"pss-batch-1\",\"note\":\"{}\"}}", "“quoted” ".repeat(150_000));
    std::fs::write(outbox.join("2026-10-22T120000Z-aaaa.pssbatch"), &big).unwrap();
    std::fs::write(outbox.join("2026-10-22T120100Z-bbbb.pssbatch"), "{\"small\":true}").unwrap();
    let out = Command::new(runner).arg("upload").env("PSS_DATA_DIR", &runner_data).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("2 batch(es) uploaded"));
    assert_eq!(std::fs::read_dir(&outbox).unwrap().flatten().filter(|e| e.path().is_file()).count(), 0, "uploaded batches leave the outbox");
    assert_eq!(std::fs::read_dir(outbox.join("sent")).unwrap().count(), 2);
    let keys: Vec<String> = objects.lock().unwrap().keys().cloned().collect();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|k| k.starts_with("pss/") && k.matches('/').count() == 2), "{keys:?}: prefix/runner-fingerprint/file");

    // The desktop host collects them, in parts, and not again after acking.
    configure(env!("CARGO_BIN_EXE_pocket-scraper-host"), &["collector", "configure"], &desktop_data);
    std::env::set_var("PSS_DATA_DIR", &desktop_data);
    let collect = |retry: bool| {
        let mut texts: BTreeMap<String, String> = BTreeMap::new();
        let (mut skipped, mut done) = (vec![], None);
        for r in transport::collect(20, retry) {
            match r {
                Response::BatchPart { key, text, parts, .. } => {
                    if parts > 1 {
                        assert!(text.len() <= 700 * 1024);
                    }
                    texts.entry(key).or_default().push_str(&text);
                }
                Response::BatchSkipped { key, .. } => skipped.push(key),
                Response::CollectDone { count, more } => done = Some((count, more)),
                other => panic!("unexpected {other:?}"),
            }
        }
        (texts, skipped, done.expect("collect finishes"))
    };
    let (texts, skipped, done) = collect(false);
    assert_eq!(done, (2, false));
    assert!(skipped.is_empty());
    assert!(texts.values().any(|t| t == &big), "the big batch is split and reassembled byte for byte");
    let keys: Vec<String> = texts.keys().cloned().collect();
    // Defer one (say its runner isn't enrolled yet), handle the other.
    assert!(matches!(transport::ack(vec![keys[1].clone()], vec![keys[0].clone()]), Response::Acked { count: 1 }));
    assert_eq!(collect(false).2, (0, false), "nothing new, and the deferred one doesn't block");

    // Newer uploads, an oversized object and a non-batch arrive in the same folder.
    let folder = keys[0].rsplit_once('/').unwrap().0.to_string();
    objects.lock().unwrap().insert(format!("{folder}/2026-10-22T120200Z-cccc.pssbatch"), b"{\"third\":true}".to_vec());
    objects.lock().unwrap().insert(format!("{folder}/2026-10-22T120150Z-huge.pssbatch"), vec![b'x'; 9 * 1024 * 1024]);
    objects.lock().unwrap().insert(format!("{folder}/notes.txt"), b"ignored".to_vec());
    let (texts, skipped, done) = collect(false);
    assert_eq!(done, (2, false));
    assert_eq!(skipped.len(), 1, "the oversized object is skipped, not fatal: {skipped:?}");
    assert!(skipped[0].ends_with("huge.pssbatch"));
    assert_eq!(texts.len(), 1);
    // The huge one is acked as refused for good; the third imported.
    let mut handled: Vec<String> = texts.keys().cloned().collect();
    handled.extend(skipped);
    transport::ack(handled, vec![]);
    assert_eq!(collect(false).2, (0, false));
    // The deferred batch comes back on request.
    let (texts, _, done) = collect(true);
    assert_eq!(done.0, 1);
    assert!(texts.contains_key(&keys[0]));
    transport::ack(vec![keys[0].clone()], vec![]);
    assert_eq!(collect(true).2, (0, false));
    assert!(problems.lock().unwrap().is_empty(), "{:?}", problems.lock().unwrap());
    let _ = std::fs::remove_dir_all(&base);
}
