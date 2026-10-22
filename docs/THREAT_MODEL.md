# Threat model

## Assets

1. **Scraped records.** These may include private account data from signed-in pages.
2. **Website sessions.** The cookies in the native host's per-site profiles are live logins.
3. **The vault passphrase and secret key.**
4. **The user's browser.** The extension must not become a way to script it
   for someone else.

## Adversaries and what stops them

| # | Adversary | Attack | Mitigation |
|---|---|---|---|
| A1 | Malicious website being scraped | Crafted DOM returns huge values, HTML/script payloads, or CSV formulas | Extractor output is capped and validated in `core`. Values are rendered with `textContent` only, under Trusted Types (`require-trusted-types-for 'script'`). CSV cells starting with `= + - @` (and their full-width forms), tab or CR are prefixed with a tab. |
| A2 | Malicious website | Fakes picker messages, or `postMessage`s the extension | Pages can't reach `chrome.runtime` in MV3. The picker runs in the isolated world, and its messages need the session nonce and the original tab ID. |
| A3 | Any local process (old architecture: the localhost API) | Calls the scraper API, reads results, triggers runs | No listener exists. Native messaging is reachable only by Chrome for the allowed extension ID. CDP uses a private pipe, not a port. |
| A4 | Another extension | Messages the service worker | `externally_connectable` is absent (no external messaging), and `sender.id` is checked on every message. |
| A5 | Local attacker who can read the Chrome profile on disk (backups, stolen disk without FileVault, other users with read access) | Reads stored records | Records are sealed. Without the passphrase, an attacker sees ciphertext, record counts and timestamps, and which rows are equal (salted hashes). |
| A6 | Same attacker | Brute-forces the passphrase | Argon2id (19 MiB, t=2, p=1 per OWASP's floor; raised when the device allows). Passphrases under 12 characters are refused. |
| A7 | Same attacker | Reads host browser profiles (live sessions) | Owner-only 0700 directories. **Not encrypted.** A session is only as safe as the OS account; this is documented for the user. |
| A8 | Compromised dependency (supply chain) | Malicious code at build or run time | Minimal dependency set. Exact pins, a lockfile with checksums, and a 14-day minimum age. npm `ignore-scripts=true`. No runtime network fetches. The extension bundles everything, and the CSP forbids remote code. |
| A9 | Someone with access to the unlocked dashboard tab | Reads records | Auto-lock after inactivity (default 10 min). The secret key is only in dashboard memory and is zeroized on lock or close. |
| A10 | Malicious recipe import | Points runs at `file:`, `chrome:` or internal URLs, or sets abusive limits | `core::recipe` accepts only `http(s)`, caps every field, and requires a host permission granted by the user per origin. |
| A11 | Memory-safety bugs | Exploit parser or crypto code | Rust core and host. `unsafe` is forbidden in `core`. The host's only `unsafe` is a single `dup2` in `pre_exec` to hand the pipe to Chrome, isolated and commented. |

## Out of scope

- An attacker running code as the user with the vault unlocked. They can read
  memory, keylog the passphrase, or use the sessions directly.
- A compromised Chrome or OS.
- Websites detecting automation. CAPTCHAs are not solved.
- Runs while Chrome is closed (v1).

## Residual risks the user should know

- Host browser profiles hold live logins in the clear. Use dedicated website
  accounts where possible.
- Record timestamps, counts and equality are visible without the passphrase.
- There is no passphrase recovery. Forgetting it loses the stored records,
  though not the recipes.
