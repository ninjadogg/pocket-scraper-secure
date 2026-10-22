//! Pocket Scraper Secure's native messaging host. Chrome starts it for the extension
//! (stdio, no network); it runs recipes in a private headless browser over a pipe
//! and returns records already sealed to the vault's public key.

pub mod browser;
pub mod cdp;
pub mod driver;
pub mod framing;
pub mod handler;
pub mod install;
pub mod paths;
