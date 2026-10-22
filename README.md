# Pocket Scraper Secure

A hardened rebuild of [Pocket Scraper](https://github.com/ninjadogg/pocket-scraper). You
click the elements to collect, replay pagination and click workflows on a schedule, and
the results stay encrypted, all without a network listener.

| | Pocket Scraper | Pocket Scraper Secure |
|---|---|---|
| Control channel | Localhost HTTP API with a connection token | Chrome native messaging (stdio), with no listener |
| Browser automation | Playwright | Your own tab, or headless Chrome over a private pipe (no debugging port) |
| Validation, run logic, crypto | JavaScript | One Rust crate (WebAssembly in the extension, native in the host) |
| Stored results | Plain JSON | Sealed per record (X25519 + XChaCha20-Poly1305); opened with your passphrase |
| Site access | Any site | Only origins you grant, per recipe |
| Extension pages | CSP | CSP plus Trusted Types, no HTML strings |

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md)
before relying on it.

## Build

You need:
- Node 22 or later;
- rustup's stable toolchain with the `wasm32-unknown-unknown` target;
- Google Chrome.

```
npm ci                                                     # build tools only; install scripts are disabled
cargo install --locked --root .tools wasm-bindgen-cli --version 0.2.129
npm run build                                              # → extension/dist
npm test                                                   # Rust tests, WASM tests, type-check, browser end-to-end
```

## Install

1. **Load the extension.** Open `chrome://extensions`, turn on Developer mode, click
   **Load unpacked** and choose `extension/dist`. The key in its manifest fixes the
   extension ID (`extension/extension-id.txt`).
2. **Create the vault.** Open the dashboard (the extension's options page) and choose a
   passphrase of at least 12 characters. There is no recovery.
3. **Optionally, install the native host** for headless scheduled runs in a separate
   browser profile:

   ```
   cargo build --release -p pocket-scraper-host
   target/release/pocket-scraper-host install            # add --browser chromium|brave|edge as needed
   ```

   This copies the host to `~/Library/Application Support/PocketScraperSecure/bin/` and
   registers it for this extension's ID only. `uninstall` removes it again, and
   `--purge-profiles` also deletes the saved website sessions.

## Use

1. On a website, open the toolbar popup.
   1. Pick a **record container**, then the **fields** inside it.
   2. Optionally record clicks, a **Next** or **Load more** button, or a step-by-step
      workflow.
2. **Save.** Chrome asks you to allow the recipe's site. Recipes only run on sites you
   allowed.
3. **Run now**, or choose a schedule. Runs use an inactive background tab. If you ticked
   **Run in the native host's private browser**, they run in headless Chrome with that
   site's own profile instead. Use **Session setup** in the dashboard to sign in to that
   profile.
4. Read the records in the dashboard after unlocking, and export them as CSV
   (formula-safe) or JSON. The vault locks itself after 10 minutes of inactivity.

Schedules run while Chrome is open. If a site starts asking for sign-in or
verification, its schedules pause until you deal with it in the dashboard.

## Layout

| Path | What |
|---|---|
| `crates/core` | Validation, run planner, records, CSV, vault, host protocol |
| `crates/core-wasm` | WebAssembly bindings for the extension |
| `crates/native-host` | Native messaging host and its headless Chrome driver |
| `extension/src` | Service worker, popup, dashboard, picker, shared extractor |
| `tests/` | WASM tests and the browser end-to-end test |

Licence: MIT.
