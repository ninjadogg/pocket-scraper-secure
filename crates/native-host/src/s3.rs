//! S3-compatible object storage (AWS S3, Cloudflare R2, Backblaze B2, MinIO…) for moving
//! sealed, signed batches from runners to the desktop. Requests are signed here with AWS
//! Signature Version 4 and sent by the system's curl, so this binary has no TLS stack
//! and no network listener. Batches are already encrypted and signed: the bucket needs
//! no trust beyond availability.

use crate::paths::write_private;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// e.g. https://s3.us-east-1.amazonaws.com or https://<account>.r2.cloudflarestorage.com
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    #[serde(default)]
    pub prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl Config {
    /// Checks the settings; https only, except a local test endpoint.
    pub fn check(&self) -> Result<(), String> {
        // scheme://host[:port], nothing else: no userinfo, path, query or spaces.
        let (scheme, authority) = self.endpoint.split_once("://").ok_or("The endpoint must start with https://")?;
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
            _ => (authority, None),
        };
        if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
            return Err("Give the endpoint as https://host[:port] with no path, e.g. https://s3.us-east-1.amazonaws.com".into());
        }
        let local = scheme == "http" && (host == "127.0.0.1" || host == "localhost") && port.is_some();
        if scheme != "https" && !local {
            return Err("The endpoint must start with https://".into());
        }
        if !(3..=63).contains(&self.bucket.len()) || !self.bucket.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.') {
            return Err("Bucket names are 3–63 lower-case letters, digits, dots and dashes".into());
        }
        if self.region.is_empty() || !self.region.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err("Give the region, e.g. us-east-1 (R2: auto)".into());
        }
        if self.prefix.len() > 64 || self.prefix.contains("..") || self.prefix.starts_with('/') || !self.prefix.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b)) {
            return Err("The prefix is up to 64 letters, digits and - _ . /".into());
        }
        if self.access_key_id.is_empty() || self.access_key_id.len() > 128 || !self.access_key_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Err("The access key ID is letters, digits, - and _".into());
        }
        if self.secret_access_key.is_empty() || self.secret_access_key.chars().any(char::is_control) {
            return Err("The secret access key is missing or malformed".into());
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Config, String> {
        let text = Zeroizing::new(std::fs::read_to_string(path).map_err(|_| format!("No bucket is configured ({})", path.display()))?);
        let c: Config = serde_json::from_str(&text).map_err(|e| format!("{} is malformed: {e}", path.display()))?;
        c.check()?;
        Ok(c)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.check()?;
        let json = Zeroizing::new(serde_json::to_string_pretty(self).expect("config"));
        write_private(path, json.as_bytes(), 0o600).map_err(|e| e.to_string())
    }

    fn host(&self) -> &str {
        self.endpoint.split("://").nth(1).unwrap_or_default()
    }
}

/// RFC 3986 encoding as SigV4 requires; `/` kept in paths.
fn encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) || (keep_slash && b == b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(data.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn hex_sha256(data: &[u8]) -> String {
    pocket_scraper_core::hex(&Sha256::digest(data))
}

pub struct Signed {
    pub authorization: String,
    pub signed_headers: Vec<(String, String)>,
}

/// AWS Signature Version 4. `path` is already URI-encoded; `query` pairs are raw;
/// `headers` must include host and x-amz-date. `amz_date` is YYYYMMDD'T'HHMMSS'Z'.
#[allow(clippy::too_many_arguments)]
pub fn sign(method: &str, path: &str, query: &[(&str, &str)], headers: &[(&str, &str)], payload_hash: &str, amz_date: &str, region: &str, service: &str, access_key: &str, secret: &str) -> Signed {
    let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (encode(k, false), encode(v, false))).collect();
    q.sort();
    let canonical_query = q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let mut h: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" "))).collect();
    h.sort();
    let canonical_headers: String = h.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = h.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical_request = format!("{method}\n{path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let date = &amz_date[..8];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", hex_sha256(canonical_request.as_bytes()));
    let k_secret = Zeroizing::new(format!("AWS4{secret}"));
    let k_date = Zeroizing::new(hmac(k_secret.as_bytes(), date));
    let k_region = Zeroizing::new(hmac(&k_date, region));
    let k_service = Zeroizing::new(hmac(&k_region, service));
    let k_signing = Zeroizing::new(hmac(&k_service, "aws4_request"));
    let signature = pocket_scraper_core::hex(&hmac(&k_signing, &string_to_sign));
    Signed {
        authorization: format!("AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"),
        signed_headers: h,
    }
}

fn amz_date() -> String {
    // runner::iso gives 2026-10-22T20:17:18Z; SigV4 wants 20261022T201718Z.
    crate::runner::iso(crate::runner::now_secs()).replace(['-', ':'], "")
}

/// Sends one signed request with curl. Headers go through a private temporary file
/// (curl's `-H @file`), so the signature never appears in the process list.
/// `limit` caps the response size (curl stops reading beyond it).
fn curl(cfg: &Config, method: &str, key: Option<&str>, query: &[(&str, &str)], body: Option<&Path>, scratch: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let path = match key {
        Some(k) => format!("/{}/{}", cfg.bucket, encode(k, true)),
        None => format!("/{}", cfg.bucket),
    };
    let payload_hash = match body {
        Some(p) => hex_sha256(&std::fs::read(p).map_err(|e| e.to_string())?),
        None => hex_sha256(b""),
    };
    let date = amz_date();
    let signed = sign(method, &path, query, &[("host", cfg.host()), ("x-amz-date", &date), ("x-amz-content-sha256", &payload_hash)], &payload_hash, &date, &cfg.region, "s3", &cfg.access_key_id, &cfg.secret_access_key);
    let header_file = scratch.join(format!(".s3-headers-{}", pocket_scraper_core::hex(&pocket_scraper_core::random_bytes::<6>())));
    let headers: String = signed.signed_headers.iter().filter(|(k, _)| k != "host").map(|(k, v)| format!("{k}: {v}\n")).collect::<String>() + &format!("authorization: {}\n", signed.authorization);
    write_private(&header_file, headers.as_bytes(), 0o600).map_err(|e| e.to_string())?;
    let mut url = format!("{}{path}", cfg.endpoint);
    if !query.is_empty() {
        let mut q: Vec<String> = query.iter().map(|(k, v)| format!("{}={}", encode(k, false), encode(v, false))).collect();
        q.sort();
        url.push('?');
        url.push_str(&q.join("&"));
    }
    let mut cmd = Command::new("curl");
    // The status comes back on the last line (works with curl older than 7.76, which
    // lacks --fail-with-body).
    cmd.args(["--silent", "--show-error", "--max-time", "120", "--max-filesize", &limit.to_string(), "--write-out", "\n%{http_code}", "-X", method, "-H"]).arg(format!("@{}", header_file.display()));
    if cfg.endpoint.starts_with("https://") {
        cmd.args(["--proto", "=https", "--tlsv1.2"]);
    }
    if let Some(p) = body {
        cmd.arg("--data-binary").arg(format!("@{}", p.display()));
    }
    let out = cmd.arg(&url).output();
    let _ = std::fs::remove_file(&header_file);
    let out = out.map_err(|e| format!("Could not run curl: {e}"))?;
    let mut body = out.stdout;
    let split = body.iter().rposition(|&b| b == b'\n').unwrap_or(0);
    let status: u16 = std::str::from_utf8(&body[split..]).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    body.truncate(split);
    if !out.status.success() || !(200..300).contains(&status) {
        let text = String::from_utf8_lossy(&body);
        let code = text.split("<Code>").nth(1).and_then(|s| s.split('<').next()).unwrap_or("");
        return Err(format!("{method} {} failed: HTTP {status} {code} {}", key.unwrap_or("(list)"), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(body)
}

const LISTING_MAX: u64 = 16 * 1024 * 1024;

pub fn put_file(cfg: &Config, key: &str, file: &Path, scratch: &Path) -> Result<(), String> {
    curl(cfg, "PUT", Some(key), &[], Some(file), scratch, 1024 * 1024).map(|_| ())
}

/// Fetches an object of at most `limit` bytes; larger objects are refused unread.
pub fn get(cfg: &Config, key: &str, scratch: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let body = curl(cfg, "GET", Some(key), &[], None, scratch, limit)?;
    if body.len() as u64 > limit {
        return Err(format!("{key} is larger than {limit} bytes"));
    }
    Ok(body)
}

/// One page (at most `max` keys) of objects under `prefix`, after `start_after`.
pub fn list_after(cfg: &Config, prefix: &str, start_after: Option<&str>, max: usize, scratch: &Path) -> Result<Vec<String>, String> {
    let max = max.clamp(1, 1000).to_string();
    let mut query = vec![("list-type", "2"), ("prefix", prefix), ("max-keys", max.as_str())];
    if let Some(after) = start_after {
        query.push(("start-after", after));
    }
    let xml = String::from_utf8(curl(cfg, "GET", None, &query, None, scratch, LISTING_MAX)?).map_err(|_| "The bucket listing is not text")?;
    Ok(tags(&xml, "Key").into_iter().map(|k| unescape(&k)).collect())
}

/// The "folders" (common prefixes) directly under `prefix`.
pub fn folders(cfg: &Config, prefix: &str, scratch: &Path) -> Result<Vec<String>, String> {
    let xml = String::from_utf8(curl(cfg, "GET", None, &[("list-type", "2"), ("prefix", prefix), ("delimiter", "/")], None, scratch, LISTING_MAX)?).map_err(|_| "The bucket listing is not text")?;
    Ok(tags(&xml, "CommonPrefixes").iter().flat_map(|c| tags(c, "Prefix")).map(|p| unescape(&p)).take(1000).collect())
}

fn tags(xml: &str, name: &str) -> Vec<String> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    xml.split(&open).skip(1).filter_map(|s| s.split(&close).next()).map(str::to_string).collect()
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Official vectors from the AWS SigV4 test suite (botocore tests/unit/auth/aws4_testsuite).
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn matches_the_get_vanilla_vector() {
        let s = sign("GET", "/", &[], &[("Host", "example.amazonaws.com"), ("X-Amz-Date", "20150830T123600Z")], EMPTY, "20150830T123600Z", "us-east-1", "service", "AKIDEXAMPLE", SECRET);
        assert_eq!(s.authorization, "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31");
    }

    #[test]
    fn matches_the_query_order_vector() {
        let s = sign("GET", "/", &[("Param2", "value2"), ("Param1", "value1")], &[("Host", "example.amazonaws.com"), ("X-Amz-Date", "20150830T123600Z")], EMPTY, "20150830T123600Z", "us-east-1", "service", "AKIDEXAMPLE", SECRET);
        assert!(s.authorization.ends_with("Signature=b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"), "{}", s.authorization);
    }

    #[test]
    fn config_checks() {
        let base = Config { endpoint: "https://s3.example.com".into(), bucket: "bucket".into(), region: "us-east-1".into(), prefix: String::new(), access_key_id: "AK".into(), secret_access_key: "s".into() };
        assert!(Config { bucket: "ab".into(), ..base.clone() }.check().is_err(), "bucket names are 3-63 characters");
        assert!(Config { bucket: "a".repeat(64), ..base.clone() }.check().is_err());
        assert!(Config { prefix: "p".repeat(65), ..base.clone() }.check().is_err(), "a prefix can't carry much data");
        assert!(Config { prefix: "pss/".into(), ..base.clone() }.check().is_ok());
        let ok = Config { endpoint: "https://s3.us-east-1.amazonaws.com".into(), bucket: "my-bucket".into(), region: "us-east-1".into(), prefix: "pss/".into(), access_key_id: "AK".into(), secret_access_key: "S".into() };
        assert!(ok.check().is_ok());
        for bad in [
            Config { endpoint: "http://s3.example.com".into(), ..ok.clone() },
            Config { endpoint: "https://s3.example.com/".into(), ..ok.clone() },
            Config { endpoint: "http://localhost:@evil.example".into(), ..ok.clone() },
            Config { endpoint: "http://localhost:9000@evil.example".into(), ..ok.clone() },
            Config { endpoint: "https://user@s3.example.com".into(), ..ok.clone() },
            Config { access_key_id: "AK\nx-evil: 1".into(), ..ok.clone() },
            Config { bucket: "My_Bucket".into(), ..ok.clone() },
            Config { prefix: "../x".into(), ..ok.clone() },
            Config { secret_access_key: "".into(), ..ok.clone() },
        ] {
            assert!(bad.check().is_err());
        }
        assert!(Config { endpoint: "http://127.0.0.1:9000".into(), ..ok }.check().is_ok(), "local test endpoints may use http");
        assert_eq!(encode("a b/c+d", true), "a%20b/c%2Bd");
        assert_eq!(tags("<a><Key>x&amp;y</Key><Key>z</Key></a>", "Key"), ["x&amp;y", "z"]);
    }
}
