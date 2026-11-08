# Third-party notices

`devclean` is distributed under the [MIT license](LICENSE). It statically links the Rust crates
listed below; each one keeps its own license.

## Direct dependencies

| Crate | Version | License |
|---|---|---|
| anyhow | 1.0.104 | MIT OR Apache-2.0 |
| clap | 4.6.7 | MIT OR Apache-2.0 |
| jwalk | 0.8.1 | MIT |
| walkdir | 2.5.0 | Unlicense/MIT |
| rayon | 1.12.0 | MIT OR Apache-2.0 |
| dialoguer | 0.11.0 | MIT |
| indicatif | 0.17.11 | MIT |
| comfy-table | 7.2.2 | MIT |
| humansize | 2.1.3 | MIT/Apache-2.0 |
| owo-colors | 4.4.0 | MIT |
| ctrlc | 3.5.2 | MIT/Apache-2.0 |
| log | 0.4.35 | MIT OR Apache-2.0 |
| simplelog | 0.12.2 | MIT OR Apache-2.0 |
| ratatui | 0.30.2 | MIT |
| tempfile (dev only) | 3.27.0 | MIT OR Apache-2.0 |

Versions match `Cargo.lock` at release 0.1.0; run `cargo tree` for the current state.

## Transitive dependencies

The dependency graph pulls in further crates under MIT, Apache-2.0, Boost Software License 1.0,
Zlib, Unlicense, MPL-2.0, WTFPL and Unicode-3.0. The complete, pinned list is in `Cargo.lock`.

## Build and release tooling

`.github/workflows/release.yml` uses `actions/checkout`, `dtolnay/rust-toolchain`,
`actions/upload-artifact`, `actions/download-artifact` and `softprops/action-gh-release`, all MIT
licensed. These run in CI and are not part of the shipped binary.

`devclean` is not affiliated with GitHub, Microsoft or any of the crate authors.
