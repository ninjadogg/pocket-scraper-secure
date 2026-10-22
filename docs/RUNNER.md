# Runners: teach once, run anywhere

You teach a recipe in Chrome with the extension. A **runner** then runs it on its own
schedule, on a VM, a server or the same computer, with no browser window and no
extension. Runners never get your passphrase or the vault's secret key: they receive
only signed recipes and the vault's public key, and the results they send back are
encrypted and signed.

```
 Chrome + extension (your desktop)                 Runner (VM / server / this Mac)
 ─────────────────────────────────                 ───────────────────────────────
 1. Teach the recipe, save it
 2. Export for runner ──── recipe.pssbundle ─────▶ 3. add (pins the vault)
    (signed by the vault)                          4. run on a timer, in a pinned
                                                      headless browser
                                                   5. seal rows to the vault's
                                                      public key; sign the batch
 7. Import / Collect ◀──── *.pssbatch ──────────── 6. outbox folder, and optionally
    (checks runner + vault + replay)                  upload to your bucket
 8. Unlock to read
```

## What protects what

| Threat | Protection |
|---|---|
| Someone gets the runner's disk | It holds sealed records only. Without the passphrase they can't be opened. The runner's key, trusted vault key, recipes and website profiles are owner-only. |
| A forged or altered recipe | A runner accepts bundles only from the vault it pinned at first `add` (`--trust <fingerprint>`). Every stored bundle is re-verified before each run. |
| A forged or altered result | The dashboard accepts a batch only if it is signed by an enrolled runner and sealed to this vault, and only once (batch IDs are remembered). |
| The bucket or the network | Batches are already encrypted and signed, so the bucket gets only availability, not trust. Uploads use HTTPS (TLS 1.2+). |
| A recipe wandering off its site | The planner fails any read from another origin. |
| The runner's own browser | Google's Chrome for Testing headless shell, pinned by SHA-256 and never auto-updated. The sandbox is on unless you explicitly turn it off. |

The runner's website sessions (cookies in its profiles) are protected by file
permissions, not encryption, as on the desktop (see `THREAT_MODEL.md`, A7).

## Set up a runner

Binaries for Linux x64, Linux arm64, macOS and Windows are attached to every CI run
(Actions → the run → Artifacts). You can also build one with
`cargo build --release -p pocket-scraper-host`.

```
pocket-scraper-runner init
```

`init` creates the runner's key, copies the runner to a stable private path, and
downloads and verifies the pinned headless shell. It then prints a **key** and a
**fingerprint**.

In the dashboard, under **Runners**, give the runner a name and paste the key. Check
that the fingerprint the dashboard shows matches the one the runner printed.

### Add recipes

In the dashboard, unlock the vault and click **Export for runner** on a recipe. Copy
the `.pssbundle` file to the runner, then run:

```
pocket-scraper-runner add Quotes.pssbundle --trust 3f2a-9c41-07bd-e5f2-6a18
pocket-scraper-runner list
pocket-scraper-runner run <recipe-id>     # try it once
```

`--trust` is needed only for the first bundle. It must equal the vault fingerprint
shown on the dashboard's vault card. Re-export and `add` again after you change a
recipe. A recipe's schedule (every 15 minutes … 30 days) comes from the recipe;
manual-only recipes run with `run`.

### Run on a timer

```
pocket-scraper-runner schedule install --every 5    # checks for due recipes every 5 min
pocket-scraper-runner schedule print                # see exactly what it installs
pocket-scraper-runner schedule uninstall
```

| Platform | Timer |
|---|---|
| macOS | launchd agent `~/Library/LaunchAgents/com.pocketscraper.runner.plist`. Log in `<data>/runner/runner.log`. |
| Linux | systemd user timer `pocket-scraper-runner.timer`. On a server, `loginctl enable-linger $USER` keeps it running without a login. Or use the cron line from `schedule print`. |
| Windows | Task Scheduler task "PocketScraperSecure Runner" (runs while you're logged on). |

### Get results back

**Files.** Results land in `<data>/runner/outbox/*.pssbatch`. Move them however you
like (scp, Syncthing, a USB stick), then use **Import results** in the dashboard.
Importing works while the vault is locked.

**Bucket (optional).** Any S3-compatible store works: AWS S3, Cloudflare R2, Backblaze
B2 or MinIO. Use two keys:

- **On the runner:** a key that can only *write* to the bucket (or prefix).

  ```
  PSS_S3_SECRET_ACCESS_KEY=… pocket-scraper-runner upload configure \
    --endpoint https://s3.us-east-1.amazonaws.com --bucket my-results --region us-east-1 \
    --prefix pss/ --access-key-id AKIA…
  ```

  Batches then upload after every run, under `pss/<runner fingerprint>/`. Uploaded
  files move to `outbox/sent/` and are deleted after 7 days. A failed upload leaves
  the file in the outbox for the next attempt, or for `pocket-scraper-runner upload`.

- **On your desktop:** a key that can only *list and read*, given to the native host.

  ```
  PSS_S3_SECRET_ACCESS_KEY=… pocket-scraper-host collector configure \
    --endpoint https://s3.us-east-1.amazonaws.com --bucket my-results --region us-east-1 \
    --prefix pss/ --access-key-id AKIA…
  ```

  The dashboard then shows **Collect from bucket**.

Secrets are read from `PSS_S3_SECRET_ACCESS_KEY` or stdin, never from the command
line. They are stored owner-only, and request signatures reach curl through a
private file, not the process list.

## Platform notes

**Data folder.** It is `PSS_DATA_DIR` if set, otherwise:
- macOS: `~/Library/Application Support/PocketScraperSecure`
- Linux: `$XDG_DATA_HOME/pocket-scraper-secure`
- Windows: `%LOCALAPPDATA%\PocketScraperSecure`

**Linux libraries.** The headless shell needs a few system libraries. On
Debian/Ubuntu:

```
sudo apt-get install -y --no-install-recommends libnss3 libnspr4 libatk1.0-0t64 \
  libatk-bridge2.0-0t64 libcups2t64 libdrm2 libxkbcommon0 libxcomposite1 libxdamage1 \
  libxfixes3 libxrandr2 libgbm1 libpango-1.0-0 libcairo2 libasound2t64 libdbus-1-3
```

On older releases the names have no `t64` suffix.

**Linux sandbox.** Chrome's sandbox needs unprivileged user namespaces. Ubuntu 24.04
restricts them through AppArmor. Prefer an AppArmor profile that allows `userns` for
the headless shell's path, or `sysctl kernel.apparmor_restrict_unprivileged_userns=0`
on a dedicated VM.

In a container that has no user namespaces at all, `PSS_BROWSER_NO_SANDBOX=1` turns
the sandbox off. It is a last resort: the sandbox is what contains a malicious page.
Don't combine it with signed-in sessions you care about.

**Windows.** Requires Windows 10 1803 or later (curl ships with it), x64.

**Session sign-in for runners.** A runner has no window for signing in. Recipes for
public pages work as-is. For sites that need a login, sign in through session setup
on a desktop. Copying a profile to a runner is not supported yet.
