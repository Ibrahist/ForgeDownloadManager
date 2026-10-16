# Third-party notices

Phase 2 links crates under MIT, Apache-2.0, BSD, ISC, Zlib, or Unicode-3.0, plus public-domain SQLite. `aws-lc` is not a dependency. TLS uses `ring` so the build does not need cmake. `tokio-rustls` is a direct dependency with default features off (`ring` and `tls12` only); its default `aws_lc_rs` feature is not enabled. `cargo tree -i aws-lc-sys` and `cargo tree -i dbus` do not match any package.

The application license is MIT. It does not link GPL code. `r-efi` is `MIT OR Apache-2.0 OR LGPL-2.1-or-later`; the MIT / Apache-2.0 option covers our use.

## Direct dependencies

| Crate | Version | License |
| --- | --- | --- |
| base64 | 0.23.1 | MIT OR Apache-2.0 |
| clap | 4.6.7 | MIT OR Apache-2.0 |
| futures-util | 0.3.34 | MIT OR Apache-2.0 |
| getrandom | 0.4.3 | MIT OR Apache-2.0 |
| hex | 0.4.3 | MIT OR Apache-2.0 |
| keyring | 3.6.3 | MIT OR Apache-2.0 |
| md-5 | 0.11.0 | MIT OR Apache-2.0 |
| proptest (dev) | 1.12.0 | MIT OR Apache-2.0 |
| reqwest | 0.13.5 | MIT OR Apache-2.0 |
| rusqlite | 0.40.2 | MIT |
| rustls | 0.23.46 | Apache-2.0 OR ISC OR MIT |
| rustls-platform-verifier | 0.7.1 | MIT OR Apache-2.0 |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.154 | MIT OR Apache-2.0 |
| sha1 | 0.11.0 | MIT OR Apache-2.0 |
| sha2 | 0.11.0 | MIT OR Apache-2.0 |
| tempfile (dev) | 3 | MIT OR Apache-2.0 |
| thiserror | 2.0.21 | MIT OR Apache-2.0 |
| tokio | 1.53 | MIT |
| tokio-rustls | 0.26.6 | MIT OR Apache-2.0 |
| tokio-util | 0.7.20 | MIT |
| tracing | 0.1.44 | MIT |
| tracing-subscriber | 0.3.23 | MIT |
| url | 2.5.8 | MIT OR Apache-2.0 |
| uuid | 1.28.0 | MIT OR Apache-2.0 |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 |

`rusqlite` with the `bundled` feature compiles SQLite. SQLite is public domain.

## Called out on purpose

`ring` 0.17.14 is `Apache-2.0 AND ISC`. The engine installs ring as the rustls crypto provider (`rustls` feature `ring`, reqwest feature `rustls-no-provider`, `tokio-rustls` features `ring` and `tls12` with default features off). That avoids `aws-lc-rs` / `aws-lc-sys`, which need cmake. ISC is the non-MIT/Apache term on this crate.

`keyring` is compiled only for Windows (`windows-native`) and macOS (`apple-native`). Linux builds do not call it. The `sync-secret-service`, `async-secret-service`, and `linux-native` features are off, so dbus, `dbus-secret-service` (AFL/GPL), and `linux-keyutils` (GPL) are not linked. A Linux `KeyringSecrets` call returns an error that says the OS keychain is unavailable. `windows-sys` 0.61.2 reads the Windows proxy registry. Older `windows-sys` versions in the lockfile are transitive and use the same MIT OR Apache-2.0 license.

## Transitive licenses outside MIT / Apache-2.0 / BSD / ISC / public domain

These come in through crates above. They are not vendored, and this workspace does not add a direct dependency on them.

| Crate | License | Why it is here |
| --- | --- | --- |
| `icu_collections`, `icu_locale_core`, `icu_normalizer`, `icu_normalizer_data`, `icu_properties`, `icu_properties_data`, `icu_provider`, `litemap`, `potential_utf`, `tinystr`, `writeable`, `yoke`, `yoke-derive`, `zerofrom`, `zerofrom-derive`, `zerotrie`, `zerovec`, `zerovec-derive` | Unicode-3.0 | `url` → `idna` |
| `unicode-ident` | (MIT OR Apache-2.0) AND Unicode-3.0 | `proc-macro2` / `syn` |
| `foldhash` | Zlib | `hashbrown` |

`subtle` is BSD-3-Clause (inside the allowed set). `webpki-root-certs` (CDLA-Permissive-2.0), `jni`, and `sqlite-wasm-rs` are in `Cargo.lock` for other targets or optional features. `cargo tree` for this workspace does not link them.

Regenerate this list with `cargo metadata --format-version 1` after a dependency change.
