# Pocket Scraper Secure

A hardened rebuild of Pocket Scraper. You
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

**Status: early (0.1).** The code has been through several in-depth security reviews
during development, and their findings are fixed or documented in the threat model.
It has **not** had an independent third-party audit. Read
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md)
before relying on it, and report problems as [SECURITY.md](SECURITY.md) describes.

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

The browser end-to-end test needs Chrome for Testing, because Chrome stable ignores
`--load-extension`. Point `PSS_E2E_BROWSER` at its executable. Without it, the test
is skipped and the rest still run.

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

   This copies the host (and `pocket-scraper-mcp`, if built) into the data folder's
   `bin/` and registers it for this extension's ID only. The data folder is:
   - macOS: `~/Library/Application Support/PocketScraperSecure`;
   - Linux: `$XDG_DATA_HOME/pocket-scraper-secure`, or `~/.local/share/pocket-scraper-secure`;
   - Windows: `%LOCALAPPDATA%\PocketScraperSecure`.
   `PSS_DATA_DIR` overrides it. `uninstall` removes it again, and
   `--purge-profiles` also deletes the saved website sessions.

## Use

1. On a website, open the toolbar popup and follow its four steps:
   1. **What to collect:** a name, the starting page, and **Pick on page** for one
      repeating item (a card or a row). Each one becomes a record.
   2. **Fields:** name a field, then **Pick** its value inside one record.
   3. **Pages and clicks:** optionally pick a **Next** or **Load more** button, add
      clicks before collecting, or build a step-by-step workflow.
   4. **When it runs:** a schedule, and optionally **Run in a private browser**
      (the native host's headless browser, with its own profile per site).
2. **Save.** Chrome asks you to allow the recipe's site. Recipes only run on sites you
   allowed.
3. In the dashboard:
   - **Recipes** lists them, with their last run and recent runs.
   - **Records** opens after you unlock the vault. You can search, sort and page
     through them, and export CSV (formula-safe) or JSON.
   - **Runners** handles runners and imports.
   - **Settings** covers the passphrase, the auto-lock (default 10 minutes) and the
     run time limit, and Claude access.
   Use **Session setup** on a recipe to sign in to its private-browser profile.

Picking elements on a page needs a mouse or trackpad. With only a keyboard, you can
type a CSS selector into each box instead.

Schedules in the extension run while Chrome is open. If a site starts asking for
sign-in or verification, its schedules pause until you deal with it in the dashboard.

## Run on its own: VMs, servers, this Mac

Teach a recipe once, then let a **runner** run it on a timer without Chrome. Runners
exist for Linux (x64 and arm64), macOS and Windows.

1. On the runner, `pocket-scraper-runner init`. Enrol its key under **Runners** in the
   dashboard.
2. **Export for runner** on a recipe, copy the file over, then
   `pocket-scraper-runner add <file> --trust <vault fingerprint>`.
3. `pocket-scraper-runner schedule install`.
4. Bring results back with **Import results** (files) or **Collect from bucket**
   (S3-compatible bucket).

Runners never get your passphrase: results are sealed before they touch the runner's
disk. See [docs/RUNNER.md](docs/RUNNER.md).

## Manage it from Claude

`pocket-scraper-mcp` is an MCP server. Through it, Claude can create and run recipes,
kick off runner runs, import result files or collect from a bucket, and change bucket,
timer and extension settings. It works only once you tick **Let Claude manage Pocket
Scraper Secure** in the dashboard. Records can be read only while the dashboard is
unlocked. Recipes Claude creates or redirects, runners it enrols and bucket settings
it proposes all wait for your approval in the dashboard. Claude is told to ask you
before deleting anything. See [docs/MCP.md](docs/MCP.md).

## Layout

| Path | What |
|---|---|
| `crates/core` | Validation, run planner, records, CSV, vault, host protocol |
| `crates/core-wasm` | WebAssembly bindings for the extension |
| `crates/native-host` | Native messaging host, the standalone runner, the headless browser driver and the bucket transport |
| `browser/headless-shell.json` | The pinned headless browser (version and SHA-256 per platform) |
| `extension/src` | Service worker, popup, dashboard, picker, shared extractor |
| `tests/` | WASM tests and the browser end-to-end test |

Licence: MIT.
