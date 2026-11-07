//! Interactive launcher: the older step-by-step face of devclean, kept behind
//! the `--menu` flag now that the full-screen TUI is the default.
//!
//! Drives the exact same `run()` as the plain CLI, but replaces flag typing
//! with a menu. Only reached via `--menu` on a real terminal (see `main`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, MultiSelect, Select};
use owo_colors::OwoColorize;

use clap::Parser;
use log::{debug, error, info};

use crate::ui;
use devclean::cli::{Cli, parse_duration, parse_size};

/// Saved next to the exe with the last scanned directory, so the most common
/// repeat run is a single Enter press.
const LAST_PATH_FILE: &str = "last-scan-directory.txt";

const ECOSYSTEMS: [&str; 6] = ["node", "rust", "python", "java", "dotnet", "generic"];

/// Menu loop. Esc/Ctrl+C at any prompt leaves the program politely.
pub fn run_menu() -> ExitCode {
    // Menu runs share the logger: the first init opens the file, rest no-op.
    devclean::logging::init();
    let theme = ColorfulTheme::default();
    loop {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        print_banner(&cwd);
        let Some(dir) = pick_directory(&theme, &cwd) else {
            debug!("menu: directory step cancelled by the user");
            return ExitCode::SUCCESS;
        };
        info!("menu: scan dir = {}", dir.display());
        let mut flags: Vec<String> = Vec::new();
        collect_filters(&theme, &mut flags);
        debug!("menu: flags = {flags:?}");
        let Some(mode) = pick_mode(&theme) else {
            debug!("menu: mode step cancelled by the user");
            return ExitCode::SUCCESS;
        };
        info!("menu: mode flags = {mode:?}");
        let argv = build_argv(&dir.to_string_lossy(), &flags, &mode);
        save_last_path(&dir);

        println!();
        let code = execute_and_report(&argv);

        let again = Confirm::with_theme(&theme)
            .with_prompt("Scan again?")
            .default(false)
            .interact_opt();
        match again {
            Ok(Some(true)) => {}
            _ => return code,
        }
    }
}

fn print_banner(cwd: &Path) {
    println!();
    println!(
        "{} {}",
        "devclean v0.1.0".bright_white().bold(),
        "· Dev Cache & Artifact Cleaner".dimmed()
    );
    println!(
        "cwd {} — Enter accepts it, pick another path to scan a different folder",
        ui::display_path(cwd).dimmed()
    );
    let log = devclean::paths::state_file("devclean.log")
        .map_or_else(|| "unavailable".to_string(), |path| ui::display_path(&path));
    println!(
        "log: {} — verbose trace appended on every scan",
        log.dimmed()
    );
    println!("any rare flag (e.g. --help) fits the manual flag item");
}

/// Directory choice: remembered -> cwd -> free-form entry (only that entry
/// validates the path; the menu stays open on a bad input).
fn pick_directory(theme: &ColorfulTheme, cwd: &Path) -> Option<PathBuf> {
    let last = read_last_path();
    let mut choices: Vec<String> = Vec::new();
    let mut values: Vec<PathBuf> = Vec::new();
    if let Some(last_path) = &last
        && last_path != cwd
    {
        choices.push(format!("last used — {}", ui::display_path(last_path)));
        values.push(last_path.clone());
    }
    choices.push(format!("current dir — {}", ui::display_path(cwd)));
    values.push(cwd.to_path_buf());
    choices.push("enter a path…".to_string());

    let Ok(Some(sel)) = Select::with_theme(theme)
        .with_prompt("Directory to scan")
        .items(&choices)
        .default(0)
        .interact_opt()
    else {
        return None;
    };
    if let Some(path) = values.get(sel) {
        return Some(path.clone());
    }
    let initial: &Path = last.as_deref().unwrap_or(cwd);
    loop {
        let Ok(entered) = Input::<String>::with_theme(theme)
            .with_prompt("Directory path")
            .default(initial.to_string_lossy().to_string())
            .interact_text()
        else {
            return None;
        };
        let path = PathBuf::from(entered.trim());
        if path.is_dir() {
            return Some(path);
        }
        eprintln!(
            "{}",
            "not a directory — re-enter (Esc leaves the menu)".red()
        );
    }
}

/// Repeating filter picker; every pick appends a flag token, Esc/Done returns.
fn collect_filters(theme: &ColorfulTheme, flags: &mut Vec<String>) {
    loop {
        println!();
        let summary: std::borrow::Cow<str> = if flags.is_empty() {
            "nothing yet — scanning everything".into()
        } else {
            flags.join(" ").into()
        };
        println!("Filters now: {}", summary.dimmed());
        let items = [
            "done for now — run the scan",
            "older than 7 days",
            "older than 14 days",
            "older than 30 days",
            "older than 1 year",
            "older than: custom…",
            "min size 10 MB",
            "min size 100 MB",
            "min size: custom…",
            "ignore by pattern (--exclude)",
            "ecosystems only (--type)",
            "type a flag manually",
        ];
        let Ok(Some(sel)) = Select::with_theme(theme)
            .with_prompt("Add a filter")
            .items(&items)
            .default(0)
            .interact_opt()
        else {
            return;
        };
        match sel {
            0 => return,
            1..=4 => {
                let value = ["7d", "14d", "30d", "1y"][sel - 1];
                set_unique_flag(flags, "--older-than", value);
            }
            5 => {
                let Some(age) = input_checked(
                    theme,
                    "Idle threshold (14d, 2w, 6m=months, 1y, 30min)",
                    |v| parse_duration(v).map(|_| ()),
                ) else {
                    continue;
                };
                set_unique_flag(flags, "--older-than", &age);
            }
            6 => set_unique_flag(flags, "--min-size", "10MB"),
            7 => set_unique_flag(flags, "--min-size", "100MB"),
            8 => {
                let Some(size) =
                    input_checked(theme, "Smallest size to keep (100MB, 1.5GB, 256KiB)", |v| {
                        parse_size(v).map(|_| ())
                    })
                else {
                    continue;
                };
                set_unique_flag(flags, "--min-size", &size);
            }
            9 => {
                let Some(pattern) = input_checked(
                    theme,
                    "Pattern to ignore (** crosses folders, plain text = substring)",
                    |_| Ok(()),
                ) else {
                    continue;
                };
                flags.push(format!("--exclude={pattern}"));
            }
            10 => pick_ecosystems(theme, flags),
            // shortcut: manual flags are split on whitespace, so a value with
            // spaces ("--exclude a b") breaks; quote nothing here, upgrade
            // only when it actually hurts.
            _ => {
                let Ok(manual) = Input::<String>::with_theme(theme)
                    .with_prompt("Flags (space separated, e.g. --dry-run --exclude=**/x/**)")
                    .allow_empty(true)
                    .default(String::new())
                    .interact_text()
                else {
                    return;
                };
                flags.extend(manual.split_whitespace().map(str::to_string));
            }
        }
    }
}

/// Ecosystem `MultiSelect`; empty selection means "all ecosystems" (no flag).
fn pick_ecosystems(theme: &ColorfulTheme, flags: &mut Vec<String>) {
    let Ok(Some(picks)) = MultiSelect::with_theme(theme)
        .with_prompt("Ecosystems (nothing picked = all)")
        .items(&ECOSYSTEMS)
        .interact_opt()
    else {
        return;
    };
    if picks.is_empty() {
        return;
    }
    let joined = picks
        .iter()
        .map(|&i| ECOSYSTEMS[i])
        .collect::<Vec<_>>()
        .join(",");
    set_unique_flag(flags, "--type", &joined);
}

/// Replaces any previous value of a one-shot flag (`--x=old` dropped) so
/// picking the same filter twice cannot produce clap duplicate errors.
fn set_unique_flag(flags: &mut Vec<String>, name: &str, value: &str) {
    let prefix = format!("{name}=");
    flags.retain(|f| !f.starts_with(&prefix));
    flags.push(format!("{prefix}{value}"));
}

/// Prompts until a non-empty value that passes `check`, or Esc/Ctrl+C.
fn input_checked(
    theme: &ColorfulTheme,
    prompt: &str,
    check: impl Fn(&str) -> Result<(), String>,
) -> Option<String> {
    loop {
        let Ok(typed) = Input::<String>::with_theme(theme)
            .with_prompt(prompt)
            .interact_text()
        else {
            return None;
        };
        let trimmed = typed.trim();
        if trimmed.is_empty() {
            return None;
        }
        match check(trimmed) {
            Ok(()) => return Some(trimmed.to_string()),
            Err(err) => eprintln!("{}", err.red()),
        }
    }
}

/// What to do with the findings: mapped straight onto CLI flags.
fn pick_mode(theme: &ColorfulTheme) -> Option<Vec<String>> {
    let items = [
        "analyze only — report, delete nothing  (default)",
        "full-screen picker: choose & delete (--tui)",
        "preview the removal list (--dry-run)",
        "pick what to delete interactively (-i)",
        "delete everything found (--all --yes)",
    ];
    let Ok(Some(sel)) = Select::with_theme(theme)
        .with_prompt("What to do with the findings?")
        .items(&items)
        .default(0)
        .interact_opt()
    else {
        return None;
    };
    let mode = match sel {
        0 => vec![],
        1 => vec!["--tui".to_string()],
        2 => vec!["--dry-run".to_string()],
        3 => vec!["-i".to_string()],
        _ => vec!["--all".to_string(), "--yes".to_string()],
    };
    Some(mode)
}

/// Joins the chosen pieces into an argv equivalent to the plain CLI call.
/// The directory stays a single token, so paths with spaces survive.
#[must_use]
pub fn build_argv(directory: &str, flags: &[String], mode_flags: &[String]) -> Vec<String> {
    let mut argv = vec!["devclean".to_string(), directory.to_string()];
    argv.extend(flags.iter().cloned());
    argv.extend(mode_flags.iter().cloned());
    argv
}

/// Same parsing/validation as the plain CLI; a bad menu-built argv would be
/// our bug, so the menu stays alive in that case too.
fn execute_and_report(argv: &[String]) -> ExitCode {
    match Cli::try_parse_from(argv) {
        Ok(cli) => match crate::run(&cli) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("devclean: error: {err:#}");
                ExitCode::FAILURE
            }
        },
        Err(err) => {
            error!("menu argv rejected: {err}");
            ExitCode::SUCCESS
        }
    }
}

fn last_path_file() -> Option<PathBuf> {
    devclean::paths::state_file(LAST_PATH_FILE)
}

pub(crate) fn read_last_path() -> Option<PathBuf> {
    let file = last_path_file()?;
    fs::read_to_string(file)
        .ok()?
        .lines()
        .next()
        .map(|line| PathBuf::from(line.trim()))
        .filter(|p| !p.as_os_str().is_empty())
}

pub(crate) fn save_last_path(dir: &Path) {
    let Some(file) = last_path_file() else { return };
    let Ok(mut out) = fs::File::create(file) else {
        return;
    };
    let _ = writeln!(out, "{}", dir.display());
}

#[cfg(test)]
mod tests {
    use super::build_argv;

    #[test]
    fn argv_keeps_spaces_in_path() {
        let argv = build_argv(
            "D:/dir with space",
            &["--exclude=x".to_string()],
            &["--dry-run".to_string()],
        );
        assert_eq!(
            argv,
            vec![
                "devclean".to_string(),
                "D:/dir with space".to_string(),
                "--exclude=x".to_string(),
                "--dry-run".to_string(),
            ]
        );
    }
}
