# Claude access (MCP)

`pocket-scraper-mcp` is an MCP server (stdio) that lets Claude manage Pocket Scraper
Secure: create, edit and run recipes, start runs on this machine's runner, import
results from files or a bucket, and change the bucket, timer and extension settings.

## Set up

1. Build or download the binaries, then install the host (this also installs the MCP
   server beside it and prints the next command):
   `pocket-scraper-host install --extension-id <id>`
2. Register it with Claude Code:
   `claude mcp add --scope user pocket-scraper-secure -- "<data dir>/bin/pocket-scraper-mcp"`
3. In the dashboard, tick **Let Claude manage Pocket Scraper Secure**. This setting is
   off by default and stays off until you turn it on. Turning it off disconnects
   Claude straight away.

## What Claude can do

| Area | Tools | Needs |
|---|---|---|
| Overview | `status` | — (with access off it only says so) |
| Extension settings | `get_settings`, `update_settings` (run timeout, auto-lock) | Claude access |
| Recipes | `list_recipes`, `save_recipe`⁴, `delete_recipe`¹ | Claude access |
| Runs | `run_recipe` (optionally waits for the result), `list_runs` | Claude access |
| Records | `read_records`, `export_records` (to a new owner-only file) | Claude access and **the dashboard unlocked** |
| Runners | `list_runners`, `enroll_runner`², `remove_runner`¹ | Claude access |
| Bundles | `export_bundle` (to a file and/or this machine's runner) | Claude access, the dashboard unlocked, and the site allowed |
| Imports | `import_batches` (files or folders), `collect_from_bucket` | Claude access |
| This machine's runner | `runner_status`, `runner_add_bundle`, `runner_remove_recipe`¹, `runner_run`, `runner_run_due`, `runner_upload` | Claude access |
| Bucket | `configure_bucket`³ (runner or collector), `remove_bucket_config`¹, `test_bucket` | Claude access |
| Timer, browser | `schedule` (install, uninstall¹, status, print), `install_browser` | Claude access |

¹ Deleting anything needs `confirm: true`. The server refuses without it, and Claude
is told to ask you first.
² A runner Claude enrols stays **pending**: its results are refused until you compare
its fingerprint and click **Approve** under Runners.
³ Bucket settings from Claude are only a **proposal**. They take effect once you
approve them in the dashboard (Settings → Helper app). The prefix is at most 64
characters and the bucket name 3–63.
⁴ A recipe Claude creates, or changes in where it goes or what it clicks (address,
clicks, Next button, workflow, where it runs), **waits for your review**. Until you
open **Review** on the Recipes tab and approve it, it doesn't run (on demand or on
schedule) and can't be signed for runners. Renaming it or changing its fields or
schedule doesn't need a new review.

Tools that act on this machine (runner, bucket, timer, browser) need Claude access on
too, even though they don't go through Chrome.

Some things Claude can't do:
- Turn Claude access on.
- Unlock the vault, or learn the passphrase or secret key.
- Lengthen the auto-lock. It can only shorten it.
- Grant a site permission. A recipe Claude saves for a new site shows **Allow access**
  in the dashboard, and only your click grants it. Until then, the recipe can't run
  and can't be signed for a runner.
- Make this machine's runner trust another vault. It pins only the fingerprint the
  extension reports.
- Send data somewhere new. A new bucket waits for your approval, and paths must be on
  this computer (no Windows network shares).
- Write exports anywhere it likes. Exports go only to new `.csv`, `.json` or
  `.pssbundle` files, and never to hidden files or folders.

Reading records doesn't count as activity, so the vault still locks on its own timer.

## How it connects

The extension never listens. When Claude access is on, it opens a native-messaging
connection to the host and keeps it open as the *bridge*:

```
Claude ⇄ pocket-scraper-mcp ──writes──▶ <data>/mailbox/requests/<id>.json
                                              │ (the bridge host polls)
                       Chrome ◀── native messaging ── pocket-scraper-host (bridge)
                         │  service worker runs the command; record reads go
                         │  to the open, unlocked dashboard
                         └──▶ reply ──▶ <data>/mailbox/responses/<id>.json ──▶ MCP
```

- **No sockets or ports.** The mailbox folders are owner-only (0700, files 0600).
- **One bridge at a time.** The bridge holds `mailbox/bridge.lock`.
- **Sealed answers.** Each request carries a one-time public key. The extension
  seals its answer to that key, bound to the request ID, so only the asking process
  can open it, and opened records never sit in the mailbox. Uncollected answers are
  deleted after 3 minutes.
- **No late replays.** Each request has a deadline (its caller's timeout). Waiting
  requests are dropped when a bridge starts or ends.
- **Limits:**
  - Requests are at most 16 MiB and are split, by their escaped size, to fit Chrome's
    1 MiB message limit.
  - Answers are at most 15 MiB; ask for fewer records, or use `export_records`.
- **Local tools.** Tools that act on this machine (runner, bucket, timer, browser)
  do their work directly, but only while the bridge is connected.

## Security notes

- With Claude access on, **any program running as your OS user** can send the same
  commands through the mailbox, just as it could run `pocket-scraper-mcp`. It can't
  read records while the vault is locked. Keep the toggle off when you aren't using it.
- Records Claude reads pass through the conversation. Read only what you would paste in.
  Results that carry website, runner or bucket text are labelled as data, not
  instructions.
- `configure_bucket` can take the secret from `secret_file`, which keeps the secret
  out of the conversation. Use a write-only key for a runner and a read-only key for
  the collector.
- `update_settings` can shorten the auto-lock but not lengthen it.
