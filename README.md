# Pocket Scraper Secure

A hardened rebuild of [Pocket Scraper](https://github.com/ninjadogg/pocket-scraper): click
elements to define what to collect, replay pagination and click workflows, and keep the
results encrypted, with no network listener.

- **TypeScript MV3 extension** with a strict CSP and Trusted Types. Host permissions are
  requested per site.
- **Rust core** (WebAssembly in the extension, native in the host). Validation, the run
  planner and all cryptography live here.
- **Optional Rust native host.** It is reached by Chrome native messaging (no port), and
  runs scheduled jobs in a headless browser over a pipe (no debugging port).
- **Encrypted at rest.** Records are sealed to a public key as they are collected, and
  read with your passphrase.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md).

Status: under construction (v0.1).
