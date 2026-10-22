# Dependencies

The rules for every dependency:
- **Pinned.** Exact versions in the lockfiles, with checksums: `Cargo.lock`, and
  `package-lock.json` with `save-exact`.
- **At least 14 days old** when it was added. Four transitive crates were pinned
  back to meet this: `inout`, `libc`, `syn` and `unicode-ident`.
- **Published by the upstream project.** Licences are permissive (MIT, Apache-2.0
  or BSD).
- **Build-time only for npm.** npm runs with `ignore-scripts=true`, and the
  extension bundles everything it uses. Nothing is fetched at runtime.
- **Pinned tools.** `wasm-bindgen-cli` 0.2.129 is installed into `.tools/` with
  `cargo install --locked`, matching the `wasm-bindgen` crate version.

Re-run the audit (crates.io API, about one request per second) after changing a lockfile:

```
python3 -I scripts/audit-crates.py Cargo.lock YYYY-MM-DD   # cut-off = today minus 14 days
```

## npm (build tools only)

| Package | Version | Published | Licence |
|---|---|---|---|
| typescript | 6.0.3 | 2026-04-16 | Apache-2.0 |
| esbuild | 0.28.2 | 2026-08-08 | MIT |
| @esbuild/darwin-arm64 | 0.28.2 | 2026-08-08 | MIT |
| @types/chrome | 0.3.4 | 2026-09-29 | MIT |
| @types/filesystem | 0.0.36 | 2024-03-21 | MIT |
| @types/filewriter | 0.0.33 | 2024-01-30 | MIT |
| @types/har-format | 1.2.16 | 2024-09-29 | MIT |

## Rust crates (audited 2026-10-22, cut-off 2026-10-08)

| Crate | Version | Published | Licence | Repository |
|---|---|---|---|---|
| aead | 0.6.1 | 2026-06-17 | MIT OR Apache-2.0 | RustCrypto/traits |
| argon2 | 0.6.0 | 2026-08-27 | MIT OR Apache-2.0 | RustCrypto/password-hashes |
| base64 | 0.23.1 | 2026-08-04 | MIT OR Apache-2.0 | marshallpierce/rust-base64 |
| base64ct | 1.8.3 | 2026-01-12 | Apache-2.0 OR MIT | RustCrypto/formats |
| blake2 | 0.11.0 | 2026-08-26 | MIT OR Apache-2.0 | RustCrypto/hashes |
| block-buffer | 0.12.1 | 2026-06-11 | MIT OR Apache-2.0 | RustCrypto/utils |
| bumpalo | 3.20.3 | 2026-05-22 | MIT OR Apache-2.0 | fitzgen/bumpalo |
| cfg-if | 1.0.5 | 2026-09-16 | MIT OR Apache-2.0 | rust-lang/cfg-if |
| chacha20 | 0.10.2 | 2026-08-27 | MIT OR Apache-2.0 | RustCrypto/stream-ciphers |
| chacha20poly1305 | 0.11.0 | 2026-06-28 | Apache-2.0 OR MIT | RustCrypto/AEADs |
| cipher | 0.5.2 | 2026-05-19 | MIT OR Apache-2.0 | RustCrypto/traits |
| cmov | 0.5.4 | 2026-05-28 | Apache-2.0 OR MIT | RustCrypto/utils |
| const-oid | 0.10.2 | 2026-01-07 | Apache-2.0 OR MIT | RustCrypto/formats |
| cpufeatures | 0.3.1 | 2026-08-26 | MIT OR Apache-2.0 | RustCrypto/utils |
| crypto-common | 0.2.2 | 2026-05-19 | MIT OR Apache-2.0 | RustCrypto/traits |
| ctutils | 0.4.3 | 2026-10-05 | Apache-2.0 OR MIT | RustCrypto/utils |
| curve25519-dalek | 5.0.0 | 2026-07-06 | BSD-3-Clause | dalek-cryptography/curve25519-dalek/tree/main/curve25519-dalek |
| curve25519-dalek-derive | 0.1.1 | 2023-10-31 | MIT/Apache-2.0 | dalek-cryptography/curve25519-dalek |
| digest | 0.11.3 | 2026-05-03 | MIT OR Apache-2.0 | RustCrypto/traits |
| fiat-crypto | 0.3.0 | 2025-06-04 | MIT OR Apache-2.0 OR BSD-1-Clause | mit-plv/fiat-crypto |
| getrandom | 0.4.3 | 2026-06-17 | MIT OR Apache-2.0 | rust-random/getrandom |
| hkdf | 0.13.0 | 2026-03-30 | MIT OR Apache-2.0 | RustCrypto/KDFs/ |
| hmac | 0.13.0 | 2026-03-29 | MIT OR Apache-2.0 | RustCrypto/MACs |
| hybrid-array | 0.4.15 | 2026-09-08 | MIT OR Apache-2.0 | RustCrypto/hybrid-array |
| inout | 0.2.2 | 2025-12-27 | MIT OR Apache-2.0 | RustCrypto/utils |
| itoa | 1.0.18 | 2026-03-20 | MIT OR Apache-2.0 | dtolnay/itoa |
| js-sys | 0.3.106 | 2026-09-25 | MIT OR Apache-2.0 | wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys |
| libc | 0.2.190 | 2026-10-02 | MIT OR Apache-2.0 | rust-lang/libc |
| memchr | 2.8.3 | 2026-07-08 | Unlicense OR MIT | BurntSushi/memchr |
| once_cell | 1.21.4 | 2026-03-12 | MIT OR Apache-2.0 | matklad/once_cell |
| password-hash | 0.6.1 | 2026-04-08 | MIT OR Apache-2.0 | RustCrypto/traits |
| phc | 0.6.1 | 2026-03-09 | Apache-2.0 OR MIT | RustCrypto/formats |
| poly1305 | 0.9.1 | 2026-07-08 | Apache-2.0 OR MIT | RustCrypto/universal-hashes |
| proc-macro2 | 1.0.107 | 2026-07-19 | MIT OR Apache-2.0 | dtolnay/proc-macro2 |
| quote | 1.0.47 | 2026-07-19 | MIT OR Apache-2.0 | dtolnay/quote |
| r-efi | 6.0.0 | 2026-02-03 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | r-efi/r-efi |
| rand_core | 0.10.1 | 2026-04-13 | MIT OR Apache-2.0 | rust-lang-nursery/rand |
| rustc_version | 0.4.1 | 2024-08-28 | MIT OR Apache-2.0 | djc/rustc-version-rs |
| rustversion | 1.0.23 | 2026-07-07 | MIT OR Apache-2.0 | dtolnay/rustversion |
| semver | 1.0.28 | 2026-04-04 | MIT OR Apache-2.0 | dtolnay/semver |
| serde | 1.0.229 | 2026-07-18 | MIT OR Apache-2.0 | serde-rs/serde |
| serde_core | 1.0.229 | 2026-07-18 | MIT OR Apache-2.0 | serde-rs/serde |
| serde_derive | 1.0.229 | 2026-07-18 | MIT OR Apache-2.0 | serde-rs/serde |
| serde_json | 1.0.151 | 2026-07-20 | MIT OR Apache-2.0 | serde-rs/json |
| sha2 | 0.11.0 | 2026-03-25 | MIT OR Apache-2.0 | RustCrypto/hashes |
| subtle | 2.6.1 | 2024-06-24 | BSD-3-Clause | dalek-cryptography/subtle |
| syn | 2.0.119 | 2026-07-15 | MIT OR Apache-2.0 | dtolnay/syn |
| syn | 3.0.6 | 2026-09-16 | MIT OR Apache-2.0 | dtolnay/syn |
| typenum | 1.20.1 | 2026-05-29 | MIT OR Apache-2.0 | paholg/typenum |
| unicode-ident | 1.0.26 | 2026-09-17 | (MIT OR Apache-2.0) AND Unicode-3.0 | dtolnay/unicode-ident |
| universal-hash | 0.6.1 | 2026-02-27 | MIT OR Apache-2.0 | RustCrypto/traits |
| wasm-bindgen | 0.2.129 | 2026-09-25 | MIT OR Apache-2.0 | wasm-bindgen/wasm-bindgen |
| wasm-bindgen-macro | 0.2.129 | 2026-09-25 | MIT OR Apache-2.0 | wasm-bindgen/wasm-bindgen/tree/master/crates/macro |
| wasm-bindgen-macro-support | 0.2.129 | 2026-09-25 | MIT OR Apache-2.0 | wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support |
| wasm-bindgen-shared | 0.2.129 | 2026-09-25 | MIT OR Apache-2.0 | wasm-bindgen/wasm-bindgen/tree/master/crates/shared |
| x25519-dalek | 3.0.0 | 2026-07-06 | BSD-3-Clause | dalek-cryptography/curve25519-dalek/tree/main/x25519-dalek |
| zeroize | 1.9.1 | 2026-10-06 | Apache-2.0 OR MIT | RustCrypto/utils |
| zeroize_derive | 1.5.0 | 2026-06-12 | Apache-2.0 OR MIT | RustCrypto/utils |
| zmij | 1.0.23 | 2026-07-13 | MIT | dtolnay/zmij |
