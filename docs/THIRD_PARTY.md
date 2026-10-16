# Third-party notices

The engine crates use MIT, Apache-2.0, BSD, ISC, Zlib, or Unicode-3.0, plus public-domain SQLite. The desktop shell adds the exceptions in the Desktop section. `aws-lc` is not a dependency. TLS uses `ring` so the build does not need cmake. `tokio-rustls` is a direct dependency with default features off (`ring` and `tls12` only); its default `aws_lc_rs` feature is not enabled. `cargo tree -i aws-lc-sys` and `cargo tree -i dbus` do not match any package on the root workspace or on `apps/desktop/src-tauri` (this host, and `--target x86_64-unknown-linux-gnu`). `dbus-secret-service` does not match either. The Tauri `dbus` feature is off.

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

Several transitive crates offer `Unlicense OR MIT` (including `aho-corasick`, `byteorder`, `memchr`, and `walkdir` as `Unlicense/MIT`). Unlicense is a public-domain dedication, and MIT is the other option.

## Desktop shell

`apps/desktop/src-tauri` is a separate workspace. Its direct crates:

| Crate | Version | License |
| --- | --- | --- |
| forgedownloadmanager-core | path | MIT |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.154 | MIT OR Apache-2.0 |
| tauri | 2.12.4 | Apache-2.0 OR MIT |
| tauri-build (build) | 2.7.3 | Apache-2.0 OR MIT |
| tokio | 1.53.3 | MIT |

Window backend crates on this graph: `wry` 0.57.0 and `tao` 0.37.1 (Apache-2.0 OR MIT / Apache-2.0), `webview2-com` 0.39.1 (MIT). Linux CI also compiles `webkit2gtk` 2.0.2, `gtk` 0.18.2, `soup3` 0.5.0, and `javascriptcore-rs` 1.1.2, which are MIT crates. They call the system WebKitGTK library. That system library is LGPL-2.1 and is not vendored. CI installs `libwebkit2gtk-4.1-dev`.

`dirs` 7.0.0 (MIT OR Apache-2.0) is pulled in by Tauri. Application code resolves the downloads folder itself and does not depend on `dirs` directly.

The engine exceptions above (`ring` Apache-2.0 AND ISC, Unicode-3.0 via `url` → `idna`, `unicode-ident`, `foldhash` Zlib, `r-efi` with an LGPL alternative) are on this graph too. `adler2` 2.0.1 is `0BSD OR MIT OR Apache-2.0` through the PNG encoder Tauri uses for icons. 0BSD is permissive and is outside the MIT / Apache-2.0 / BSD / ISC list; MIT or Apache-2.0 covers this use. `target-lexicon` 0.12.16 is `Apache-2.0 WITH LLVM-exception`. The LLVM exception adds permission on top of Apache-2.0. `dunce` is `CC0-1.0 OR MIT-0 OR Apache-2.0`; Apache-2.0 covers this use. CC0 is a public-domain dedication.

`cargo metadata` also lists `zlib-rs` (Zlib), `jni`, and `webpki-root-certs` (CDLA-Permissive-2.0). `cargo tree -i` for those names prints nothing on this host and on `x86_64-unknown-linux-gnu`, so the desktop build does not link them.

### MPL-2.0 pulled in by Tauri and Tailwind

These are weak copyleft, file by file. This repository does not modify them. They arrive through Tauri 2.12 and Tailwind CSS 4, which the desktop plan uses.

| Package | Version | How it arrives |
| --- | --- | --- |
| cssparser | 0.37.0 | `tauri-utils` → `dom_query` |
| cssparser-macros | 0.7.1 | `cssparser` |
| dtoa-short | 0.3.5 | `cssparser` |
| selectors | 0.38.0 | `tauri-utils` → `dom_query` |
| option-ext | 0.2.0 | `tauri` → `dirs` → `dirs-sys` |
| lightningcss | 1.32.0 | Tailwind CSS 4 (npm) |
| lightningcss-win32-x64-msvc | 1.32.0 | optional platform package of `lightningcss` |

Other `lightningcss-*` platform packages in the npm lock use the same MPL-2.0 license. A distributed binary that includes the Rust MPL crates needs those crates' source available. They are published on crates.io.

### Other npm packages outside MIT / Apache-2.0 / BSD / ISC

Direct npm packages are inside that set: `@tauri-apps/api` 2.12.4 and `@tauri-apps/cli` 2.12.3 are Apache-2.0 OR MIT; React 19.3.1, React DOM 19.3.1, i18next 26.4.7, react-i18next 17.0.19, Tailwind CSS 4.3.3, Vite 8.3.4, and Vitest 5.0.5 are MIT; TypeScript 5.9.3 is Apache-2.0.

Transitive npm packages outside the set:

| Package | Version | License | Note |
| --- | --- | --- | --- |
| lru-cache | 11.5.3 | BlueOak-1.0.0 | Permissive license, outside the listed set. Dev dependency. |
| @csstools/color-helpers | 6.1.2 | MIT-0 | Permissive MIT variant. Through CSS tooling. |
| @csstools/css-syntax-patches-for-csstree | 1.1.15 | MIT-0 | Permissive MIT variant. Through CSS tooling. |
| mdn-data | 2.27.1 | CC0-1.0 | Public-domain dedication. |

Regenerate the crate list with `cargo metadata --format-version 1` after a dependency change. Read npm `license` fields from each installed `package.json`.
