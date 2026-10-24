# Architecture

Pocket Scraper Secure is a rebuild of Pocket Scraper
around a smaller attack surface:

- **No network listener.** There is no localhost HTTP API, connection token or CORS
  pairing. The extension talks to an optional native host over Chrome native
  messaging (stdio), and the native host talks to its headless browser over a pipe
  (`--remote-debugging-pipe`), never a debugging port.
- **Memory-safe core.** Validation, record handling, pagination decisions and all
  cryptography live in one Rust crate. It is compiled to WebAssembly for the
  extension and natively for the host, so both sides run the same tested code.
- **Encrypted at rest.** Scraped records are sealed to a public key as they are
  collected. Reading them needs the vault passphrase.

```
┌────────────────────────────── Chrome ──────────────────────────────┐
│                                                                    │
│  Extension (TypeScript, MV3, strict CSP)                           │
│  ┌─────────────┐   ┌──────────────────────┐   ┌─────────────────┐  │
│  │ picker      │──▶│ service worker       │◀──│ popup/dashboard │  │
│  │ (closed     │   │ • message router     │   │ • unlock vault  │  │
│  │ shadow DOM) │   │ • tab runner driver  │   │ • view/export   │  │
│  └─────────────┘   │ • alarms scheduler   │   └────────┬────────┘  │
│  ┌─────────────┐   └──────────┬───────────┘            │           │
│  │ extractor   │◀── scripting ┤                        │           │
│  │ (isolated   │              │ core.wasm              │ core.wasm │
│  │ world)      │              ▼ (seal, validate, plan) ▼ (open)    │
│  └─────────────┘   ┌──────────────────────┐                        │
│                    │ IndexedDB: sealed    │                        │
│                    │ records only         │                        │
│                    └──────────────────────┘                        │
└──────────────┬─────────────────────────────────────────────────────┘
               │ native messaging (stdio, length-prefixed JSON,
               │ allowed_origins = this extension's ID only)
               ▼
┌──────────────────────────────────────┐   pipe (fd 3/4)   ┌──────────────────┐
│ native host (Rust)                   │──────────────────▶│ headless Chrome  │
│ • same core crate (plan, seal)       │   CDP, no port    │ per-site profile │
│ • returns sealed records only        │                   │ (0700)           │
└──────────────────────────────────────┘                   └──────────────────┘
```

## Components

| Path | Language | Role |
|---|---|---|
| `crates/core` | Rust | Recipe validation, record canonicalisation and keys, CSV/JSON export, the run planner (state machine), the vault (sealing and opening), and protocol message validation. No I/O. |
| `crates/core-wasm` | Rust → WASM | Thin `wasm-bindgen` bindings over `core` for the extension. |
| `crates/native-host` | Rust | Native messaging host. Launches headless Chrome over a pipe, drives the run planner with CDP, and seals results. `install` / `uninstall` subcommands write the host manifest. |
| `extension/` | TypeScript | MV3 extension: service worker, picker, extractor, popup, dashboard. |
| `pocket-scraper-runner` (in `crates/native-host`) | Rust | Standalone runner for VMs, servers and desktops: signed recipe bundles in, sealed and signed result batches out, on a timer, optionally through an S3-compatible bucket. See `RUNNER.md`. |
| `pocket-scraper-mcp` (in `crates/native-host`) | Rust | MCP server for Claude. Reaches the extension through an owner-only mailbox and a native-messaging *bridge* that the extension opens only while Claude access is on. See `MCP.md`. |
| `browser/headless-shell.json` | JSON | The pinned Chrome for Testing headless shell (version and SHA-256 per platform) used for every run outside the person's own tabs. |

### The run planner

Pagination, "Load more", and click/wait/extract workflows are decided by
`core::plan::Run`, a state machine that never touches a browser:

```
let mut run = Run::new(recipe, limits);
let mut action = run.start();
loop {
    let observation = driver.perform(action);   // navigate, click, wait, read records
    action = run.observe(observation);           // decide what to do next
    if action is Done or Failed { break }
}
```

Two drivers implement the same handful of primitives:

- **Tab driver** (service worker). It opens the page in a background tab, uses
  `chrome.scripting.executeScript` for reads and clicks, and waits on `chrome.tabs`
  events.
- **CDP driver** (native host). It opens a target in headless Chrome and uses
  `Runtime.evaluate` with the same bundled extractor.

The planner carries the rules fixed in Pocket Scraper PR #1:
- After a Next click it waits for a stable, non-empty change.
- New rows are taken relative to the previous page only.
- An error that persists past the deadline fails the run.
- The overall deadline is cooperative, so the driver closes the tab or browser
  when it passes. In the native host, a watchdog also kills the browser at the
  deadline plus 5 s.
- Warnings are capped, and selector health is reported once per run.
- Records only come from the recipe's own origin. A redirect or click that lands
  on another site fails the run, which matters because the native host's profile
  may be signed in.
- Reaching 5000 rows or 2 MB stops the run and keeps what was collected. One
  record is capped at 512 KB, so it always fits in a native message.

### Data at rest

| Data | Where | Protection |
|---|---|---|
| Records | IndexedDB `records` | Sealed: ephemeral X25519 → HKDF-SHA256 → XChaCha20-Poly1305. The AAD binds the recipe ID and record key. The observation time is inside the ciphertext. |
| Record keys | IndexedDB, clear | Random, 32 bytes each. They reveal nothing about the contents. |
| Vault secret key | `chrome.storage.local` (restricted to extension contexts) | Wrapped by XChaCha20-Poly1305 under an Argon2id key (19 MiB, 2 passes) derived from the passphrase. |
| Vault public key | `chrome.storage.local` | Clear. Public by design, so runs can seal while the vault is locked. |
| Unlocked secret key | Dashboard page memory (WASM linear memory) | Freed and zeroized on lock, auto-lock and page close. It never reaches the service worker or the native host. Passphrase copies inside WASM are wiped after use; JavaScript strings can't be. |
| Recipes | `chrome.storage.local` | Clear. They are re-validated by `core` before every run, because storage is never trusted. |
| Run history | IndexedDB `runs` | Clear: times, status, row counts, warnings and error messages. |
| Host and runner browser profiles | `<data>/profiles/<salted hash of the origin>` | Owner-only (0700) folders. The salt is per install (0600), so folder names don't reveal the sites. |
| Runner key, trusted vault key, recipes | `<data>/runner/` | Owner-only. Bundles are re-verified against the pinned vault key before every run. |
| Result batches | Runner `outbox/`, optional bucket, then the extension's IndexedDB | Records are sealed inside each batch, and the batch is signed by the runner. The extension remembers imported batch IDs. |
| Bucket credentials | Runner `upload.json`, desktop `collector.json` | Owner-only. Write-only key on runners, read-only key on the desktop. |

Scheduled runs work while the vault is locked, because sealing needs only the
public key. Because keys are random, storage can't tell a repeated row from a new
one, so every run stores everything it collected. When you open a recipe's records
in the unlocked dashboard, repeat copies from later runs are deleted
(`records::duplicates`). A row is kept as many times as it appeared in any one
run, because repeats on one page are real records.

Changing the passphrase re-wraps the same key. An old copy of the vault header
(from a backup, for example) still opens with the old passphrase.

### Trust boundaries and validation

The Rust core parses everything it acts on with `deny_unknown_fields` and size
caps: recipes, extractor output, planner observations, host requests and replies,
and records. Messages that stay inside the extension (page and picker requests to
the service worker) are checked in TypeScript as described below.

1. **Page → extractor result.** The extractor runs in the isolated world, but the
   page controls the DOM it reads. Results are capped (rows, fields, bytes) and
   treated as untrusted strings. They are never inserted as HTML.
2. **Picker → service worker.** Only accepted from the tab the picking session
   was started on, with the session's one-time nonce.
3. **Extension pages → service worker.** `sender.id` must be this extension and
   `sender.url` one of its own pages.
4. **Service worker ↔ native host.** Chrome lets only this extension's ID start
   the host (`allowed_origins`). The host still validates every request, caps
   frames at 1 MiB, and runs only `http(s)` recipes.
5. **Native host ↔ headless Chrome.** A private pipe. The browser runs with a
   dedicated profile, no extensions, and no remote debugging port.

### Scheduling

`chrome.alarms` fires per-recipe schedules while Chrome is running.
- If the native host is installed and the recipe opts in, the run happens in
  headless Chrome with the recipe's own profile.
- Otherwise it runs in an inactive background tab of the user's browser.

Alarms are only created when missing or changed. Saving a recipe or restarting
Chrome doesn't push other recipes' next runs back, and a recreated alarm resumes
from the recipe's last run. A recipe never runs twice at once, and at most two
runs happen at a time.

For runs while Chrome is closed, and on other machines, use a runner (`RUNNER.md`).
It runs on its own timer (launchd, systemd or Task Scheduler) and doesn't need Chrome
or the extension.

## Build

```
npm run build        # rustup wasm32 build → wasm-bindgen → esbuild → extension/dist
npm test             # cargo tests (incl. headless Chrome), tsc, WASM + extension end-to-end
cargo run --release -p pocket-scraper-host -- install --browser chrome
```

Dependencies are pinned. npm runs with `ignore-scripts=true`, and every locked
crate and package was at least 14 days old when it was added (see
`docs/DEPENDENCIES.md`).
