<a href="README.md">Русский</a> · <b>English</b>

# devclean

<!-- BANNER SLOT: assets/banner.png (2560×720, show as width="1280") then uncomment.
<p align="center">
  <img src="assets/banner.png" alt="devclean — find and safely remove build artifacts" width="1280">
</p>
-->

![License: MIT](https://img.shields.io/badge/license-MIT-green)
![Rust](https://img.shields.io/badge/rust-1.88%2B-orange)
![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey)

**Finds and safely removes build artifacts and caches** — `node_modules`, `target`, `.venv`, `__pycache__`, `bin`/`obj`, `dist` and more. Launched without arguments on a terminal, it opens a full-screen TUI: folder → filters → scan → pick → delete.

## Demo

<p align="center">
  <img src="assets/demo.gif" alt="devclean demo" width="1000">
</p>

## Screenshot

<p align="center">
  <img src="assets/screenshot.png" alt="devclean full-screen TUI with the artifact list" width="1000">
</p>

## Why

After a few months of work a projects folder easily grows to tens of gigabytes: a `node_modules` per frontend, a `target` per Rust crate, `bin`/`obj` per .NET build. Cleaning it by hand is slow and risky — it is easy to hit the wrong directory.

devclean walks the tree, finds only known artifacts (by name **and** by marker files next to them), shows the size and age of each one, and deletes only what you tick.

## Features

| | |
|---|---|
| **Full-screen TUI** | Double-click the exe or run `devclean` with no arguments: folder, filters, scan, checklist, delete. The window stays open after a clean. |
| **Parallel scan** | `jwalk` + `rayon`: tree walking and size measurement run in parallel, and found artifacts are not walked again. |
| **Marker-aware detection** | `target` counts only next to `Cargo.toml` or `pom.xml`; `bin`/`obj` only next to a project file. Personal folders with the same names are left alone. |
| **Filters** | By ecosystem, minimum size, age and glob excludes. Type and exclude filters run before measurement, so irrelevant gigabytes are never walked. |
| **Safe deletion** | Symlinks and junctions are deleted as links, never followed. Disk roots, the home root and system directories are refused. Confirmation before deleting. |
| **CLI and TUI** | One engine: scripts use flags, humans use the full-screen UI. |
| **Log next to the exe** | `devclean.log` is written next to the binary (fallback: working directory) and rotates at 10 MB. |

## Compatibility and requirements

- **Windows 10/11** — the primary, tested platform.
- **Linux and macOS** — the code is cross-platform but has mostly been tested on Windows.
- **Build:** Rust 1.88+ (edition 2024).
- A prebuilt `devclean.exe` is available under Releases (after the first release).

## Installation

### Prebuilt binary

Download `devclean.exe` from Releases and run it. No installation: the program writes nothing except the log and the last-folder file next to the exe.

### From source

```sh
git clone https://github.com/lepex1/devclean
cd devclean
cargo build --release
```

The binary ends up in `target/release/devclean.exe` (Windows) or `target/release/devclean`.

## Usage

### Full-screen mode

Double-click the exe, or run `devclean` with no arguments in a terminal.

1. **Folder** — type a path (Enter to continue). The last used folder is remembered.
2. **Filters** — `o` age, `m` minimum size, `t` ecosystem; Enter to scan.
3. **List** — tick what you need, press Enter, then `y`.

| Key | Action |
|---|---|
| `↑`/`↓` or `j`/`k` | Move through the list |
| `Space` | Tick/untick the row |
| `a` / `n` | Tick all visible / untick all visible |
| `/` | Filter by path and ecosystem |
| `s` | Sort: size ↓ → age (oldest first) → ecosystem → path |
| `Enter` | Delete ticked items (then `y` to confirm) |
| `q` / `Esc` | Quit without deleting |

Command keys work regardless of Caps Lock.

### CLI

```sh
devclean                                  # full-screen TUI
devclean D:/projects                      # scan and print a table
devclean D:/projects --older-than 30d --min-size 100MB
devclean D:/projects --type node,rust -i  # interactive checklist
devclean D:/projects --tui                # full-screen picker after a CLI scan
devclean D:/projects --all --dry-run      # show what would be removed
devclean D:/projects --all --yes          # remove everything found, no prompts
devclean --menu                           # the old step-by-step menu
```

| Flag | Meaning |
|---|---|
| `--older-than 30d` | Only artifacts unchanged for that long (`s`, `min`, `h`, `d`, `w`, `m`, `y`; combined as `1y2w3d`) |
| `--min-size 100MB` | Minimum size (decimal MB/GB and binary MiB/GiB) |
| `--type node,rust` | Ecosystems: `node`, `rust`, `python`, `java`, `dotnet`, `generic` |
| `--exclude "**/backup/**"` | Exclude paths; `*` stays inside one segment, `**` crosses segments, a plain pattern is a substring match |
| `-i, --interactive` | Terminal checklist |
| `--tui` | Full-screen picker after the CLI scan |
| `--all` + `--yes` | Remove everything found; without `--yes` the command refuses |
| `--dry-run` | Delete nothing |
| `--menu` | Step-by-step menu instead of the TUI |

Exit codes: `0` success, `1` error or some artifacts failed to delete, `2` invalid arguments, `130` interrupted by Ctrl+C (partial result).

### What gets detected

| Ecosystem | Directories |
|---|---|
| Node.js | `node_modules`, `.next`, `.nuxt`, `.turbo`, `dist` (with `package.json` / config) |
| Rust | `target` (with `Cargo.toml`) |
| Java / Kotlin | `build`, `.gradle` (with `build.gradle`/`settings.gradle`), `target` (with `pom.xml`) |
| .NET | `bin`, `obj` (with `.csproj`, `.fsproj`, `.vbproj` or `.sln`) |
| C++ (Visual Studio) | `bin`, `obj` (with `.vcxproj`) |
| Python | `.venv`, `venv`, `env`, `__pycache__`, `.pytest_cache`, `.mypy_cache` |
| Generic | `dist` (with `pyproject.toml`) |

## How it works

- The tree walk is parallel; a found artifact is pruned from the walk immediately so nested directories are not counted twice.
- Size and modification time are measured in parallel; symlinks are skipped during measurement.
- Before deletion the path is canonicalized (Windows long paths), read-only attributes are cleared, and the removal is retried on transient locks.
- A symlink or junction is removed as a link: its target stays untouched.
- Every decision (found / measured / filtered / removed / failed) goes to `devclean.log`.

## Limitations and safety

- **Deletion is permanent** — no recycle bin. Check `--dry-run` or the TUI list first.
- **`--all --yes` deletes without per-item confirmation.** Use it deliberately.
- **Only the artifact types listed above are supported.** CMake `build`, Swift `.build`, Elixir `_build`, Ruff caches and similar are intentionally not detected, to avoid deleting the wrong thing.
- **Backups and `tmp` folders inside the scan root are found too** — exclude them with `--exclude`.
- **Linux/macOS** build, but are tested less than Windows.
- The log runs at Trace level: it grows quickly during active use, but rotates at 10 MB (one previous file is kept).

## FAQ

**Why did a folder fail to delete?**
Another process holds it (IDE, build, antivirus). Close it and retry; the path and the reason are in `devclean.log`.

**Can I undo a deletion?**
No. No recycle bin is used; deletion is final.

**Where is the log?**
Next to the exe: `devclean.log` (and `devclean.log.1` after rotation). If the exe folder is read-only, it goes to the current working directory.

**Why was a CMake `build` not found?**
That type is not supported yet: the `build` rule requires Gradle markers, otherwise a personal folder with the same name could be deleted.

**Does it follow symlinks?**
No. When deleting, the link itself is removed and its target stays.

## License

MIT — see [LICENSE](LICENSE). Dependencies are distributed under MIT, Apache-2.0, BSD and Unlicense.

---

Made by [lepex1](https://lepex.vercel.app/)
