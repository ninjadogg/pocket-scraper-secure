//! Pocket Scraper Secure core: everything that decides or protects, and nothing that
//! does I/O. Compiled to WebAssembly for the extension and natively for the host, so
//! both run the same tested code.

pub mod csv;
pub mod error;
pub mod limits;
pub mod plan;
pub mod protocol;
pub mod recipe;
pub mod records;
pub mod url;
pub mod vault;

pub use error::Error;

/// Fills `buf` from the operating system (or `crypto.getRandomValues` in WASM).
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).expect("system random number generator unavailable");
    buf
}

/// Lower-case hex, used for record keys, IDs and profile folder names.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}
