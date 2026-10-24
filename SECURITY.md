# Security policy

Pocket Scraper Secure is a single-user tool for your own computer. The threat model,
covering what it protects against and what it doesn't, is in
[docs/THREAT_MODEL.md](docs/THREAT_MODEL.md).

## Reporting

Use GitHub private vulnerability reporting: the **Security** tab → **Report a
vulnerability**. If that isn't available, open an issue that only asks for a private
contact, with no details. Don't put exploit details, passphrases, vault headers,
cookies, browser profiles or scraped data in public issues. Test only systems and
accounts you own or may test.

## Guarantees this project keeps

- **No listener.** Nothing listens on a network port. The native host speaks only
  stdio to Chrome, and a pipe to its own headless browser.
- **Encrypted records.** Records are stored sealed. Plaintext exists only:
  - in the page being scraped;
  - in the run's memory before sealing;
  - in the unlocked dashboard;
  - in files you export (CSV or JSON, from the dashboard or `export_records`);
  - if you use Claude access: in `pocket-scraper-mcp`'s memory and in your Claude
    conversation, which is sent to Anthropic's API, for records Claude reads.
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
  native host and runner it is limited to `crates/native-host/src/browser.rs` (the
  pipe hand-off to Chrome, and `kill` for the lock check and watchdog) and one
  `getuid` for launchd.

## Things to know

- Native host browser profiles hold live website sign-ins, protected by owner-only
  folder permissions, not encryption. Prefer dedicated accounts for scraping.
- Record counts per recipe, run history and recipes are visible without the
  passphrase; record contents and when each was observed are not.
- Anyone who can write your Chrome profile can make later runs seal to their own
  key. You'd notice at the next unlock.
- A forgotten passphrase cannot be recovered.
- While **Claude access** is on, programs running as your OS user can drive the
  extension through the MCP mailbox. Records stay unreadable while the vault is
  locked. Recipes, runners and buckets set up that way wait for your approval. Keep
  it off when you aren't using it (see docs/MCP.md).
- The extension's ID comes from the public key in its manifest. Any unpacked copy
  carrying that key gets the same ID, and the native host trusts it. Only load the
  extension from a source you trust; installing one is equivalent to having write
  access to your Chrome profile (A12 in the threat model).
- A runner machine that is taken over can't read results, but it can use its website
  sessions and send false data in its own name. Remove it under Runners to stop
  accepting its batches.
