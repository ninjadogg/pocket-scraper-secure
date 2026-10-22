//! A deliberately small URL check: recipes may only point at `http(s)` pages, and the
//! origin decides which host permission and which browser profile a run uses.

use crate::limits::URL_MAX;
use crate::Error;
use sha2::{Digest, Sha256};

/// A validated `http(s)` URL and its origin (`scheme://host[:port]`, lower case).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebUrl {
    pub href: String,
    pub origin: String,
}

pub fn parse(input: &str) -> Result<WebUrl, Error> {
    let href = input.trim();
    if href.is_empty() || href.len() > URL_MAX {
        return Err(Error::invalid("Enter a web address up to 2048 characters"));
    }
    if href.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(Error::invalid("The web address contains spaces or control characters"));
    }
    let lower = href.to_ascii_lowercase();
    let (scheme, rest) = if let Some(rest) = lower.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = lower.strip_prefix("http://") {
        ("http", rest)
    } else {
        return Err(Error::invalid("Only http and https web addresses are supported"));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Userinfo ("user@host") makes the visible host misleading; refuse it.
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
        return Err(Error::invalid("The web address has no valid host"));
    }
    let (host, port) = split_port(authority)?;
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '[' | ']' | ':')) {
        return Err(Error::invalid("The web address has no valid host"));
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    let origin = match port {
        Some(p) if p != default_port => format!("{scheme}://{host}:{p}"),
        _ => format!("{scheme}://{host}"),
    };
    Ok(WebUrl { href: href.to_string(), origin })
}

fn split_port(authority: &str) -> Result<(&str, Option<u16>), Error> {
    // IPv6 literals keep their colons inside brackets.
    let colon = if authority.starts_with('[') {
        authority.find(']').map(|end| authority[end..].find(':').map(|i| end + i)).unwrap_or(None)
    } else {
        authority.rfind(':')
    };
    match colon {
        None => Ok((authority, None)),
        Some(i) => {
            let port: u16 = authority[i + 1..].parse().map_err(|_| Error::invalid("The web address has an invalid port"))?;
            Ok((&authority[..i], Some(port)))
        }
    }
}

/// The folder name for a site's browser profile: a salted hash, so origins never
/// become paths and a folder listing doesn't reveal which sites are scraped.
pub fn site_key(salt: &[u8], origin: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(origin.as_bytes());
    crate::hex(&h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_and_https_and_normalises_the_origin() {
        let u = parse("HTTPS://Docs.Example.com:443/a/b?c#d").unwrap();
        assert_eq!(u.origin, "https://docs.example.com");
        assert_eq!(parse("http://127.0.0.1:8080/x").unwrap().origin, "http://127.0.0.1:8080");
        assert_eq!(parse("http://[::1]:3000/").unwrap().origin, "http://[::1]:3000");
        assert_eq!(parse("https://example.org").unwrap().origin, "https://example.org");
    }

    #[test]
    fn refuses_other_schemes_userinfo_and_junk() {
        for bad in ["file:///etc/passwd", "chrome://settings", "javascript:alert(1)", "https://user@evil.example/",
            "https://", "https://exa mple.org", "https://a.example:99999/", "https://ex\u{0}ample.org", "ftp://x.org",
            "https://a\\b.example/"] {
            assert!(parse(bad).is_err(), "{bad} should be refused");
        }
        assert!(parse(&format!("https://e.org/{}", "a".repeat(URL_MAX))).is_err());
    }

    #[test]
    fn site_keys_are_hex_hashes_of_the_origin() {
        let k = site_key(b"salt", "https://example.org");
        assert_eq!(k.len(), 64);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(k, site_key(b"salt", "https://example.com"));
        assert_ne!(k, site_key(b"other", "https://example.org"));
    }
}
