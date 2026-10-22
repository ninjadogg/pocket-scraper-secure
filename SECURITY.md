# Security policy

Pocket Scraper Secure is a single-user tool for your own computer. The threat model,
covering what it protects against and what it doesn't, is in
[docs/THREAT_MODEL.md](docs/THREAT_MODEL.md).

## Reporting

Use GitHub private vulnerability reporting on this repository. Don't put exploit
details, passphrases, vault headers, cookies, browser profiles or scraped data in
public issues. Test only systems and accounts you own or may test.

## Guarantees this project keeps

- **No listener.** Nothing listens on a network port. The native host speaks only
  stdio to Chrome, and a pipe to its own headless browser.
- **Encrypted records.** Records are stored sealed. Plaintext exists only:
  - in the page being scraped;
  - in the run's memory before sealing;
  - in the unlocked dashboard.
- **Key handling.** The vault's secret key is never written unwrapped and never
  leaves the dashboard's WebAssembly memory. It is zeroized on lock. Passphrase
  copies inside WebAssembly are wiped after use, but the browser's own JavaScript
  strings can't be.
- **Untrusted input.** The Rust core validates, with unknown fields refused:
  recipes (also re-checked from storage before every run), extractor output,
  planner observations, native host messages and records. Messages between the
  extension's own pages and its service worker are checked in TypeScript.
- **No remote code.** Extension pages run under `script-src 'self'
  'wasm-unsafe-eval'` and Trusted Types with no policies.
- **Dependencies.** Pinned, at least 14 days old when added, with npm install
  scripts disabled (see [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md)).
- **Unsafe code.** `unsafe` is forbidden in the core and the WASM bindings. In the
  native host it is limited to `crates/native-host/src/browser.rs`: the pipe
  hand-off to Chrome and the `kill` calls for the lock check and watchdog.

## Things to know

- Native host browser profiles hold live website sign-ins, protected by owner-only
  folder permissions, not encryption. Prefer dedicated accounts for scraping.
- Record counts per recipe, run history and recipes are visible without the
  passphrase; record contents and when each was observed are not.
- Anyone who can write your Chrome profile can make later runs seal to their own
  key. You'd notice at the next unlock.
- A forgotten passphrase cannot be recovered.
