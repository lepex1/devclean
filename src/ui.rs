//! Terminal output: color handling, progress spinner, results table and
//! the interactive deletion checklist.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use comfy_table::{Cell, Color, ContentArrangement, Table};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, MultiSelect};
use humansize::{DECIMAL, format_size};
use indicatif::ProgressBar;
use owo_colors::OwoColorize;

use crate::models::{Artifact, total_size};
use crate::scanner::{ScanProgress, ScanResult};

/// Paints text only when stdout is a real terminal, so piped output stays
/// free of ANSI escape noise.
#[derive(Debug, Clone, Copy)]
pub struct Painter {
    enabled: bool,
}

impl Painter {
    #[must_use]
    pub fn stdout() -> Self {
        Self {
            enabled: std::io::stdout().is_terminal(),
        }
    }

    /// Ecosystem/kind column style.
    #[must_use]
    pub fn kind(&self, text: &str) -> String {
        if self.enabled {
            text.cyan().to_string()
        } else {
            text.to_string()
        }
    }

    /// Size/bytes style.
    #[must_use]
    pub fn size(&self, text: &str) -> String {
        if self.enabled {
            text.bright_green().to_string()
        } else {
            text.to_string()
        }
    }
}

/// Live scan spinner; `Drop` stops the poll thread and clears the line.
pub struct ScanSpinner {
    bar: ProgressBar,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl ScanSpinner {
    #[must_use]
    pub fn new(root: &Path, progress: &ScanProgress) -> Self {
        let bar = ProgressBar::new_spinner();
        let style = indicatif::ProgressStyle::with_template("{spinner} {msg}")
            .unwrap_or(indicatif::ProgressStyle::default_spinner());
        bar.set_style(style);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let bar_thread = bar.clone();
        let dirs = Arc::clone(&progress.dirs);
        let measured = Arc::clone(&progress.measured);
        let root = root.to_string_lossy().to_string();
        let join = thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                bar_thread.set_message(format!(
                    "scanning {} | dirs {} | measured {}",
                    root,
                    dirs.load(Ordering::Relaxed),
                    measured.load(Ordering::Relaxed)
                ));
                thread::sleep(Duration::from_millis(100));
            }
        });
        Self {
            bar,
            stop,
            join: Some(join),
        }
    }
}

impl Drop for ScanSpinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
        self.bar.finish_and_clear();
    }
}

/// Prints the scan report table with the totals footer.
pub fn print_scan_results(painter: &Painter, root: &Path, scan: &ScanResult) {
    if scan.artifacts.is_empty() {
        println!(
            "No cleanable artifacts under {} ({} dirs in {}).",
            display_path(root),
            scan.dirs_seen,
            fmt_duration(scan.elapsed)
        );
        return;
    }
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::ASCII_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    // ANSI only paints on a real terminal; comfy-table keeps column widths
    // correct with or without the escape codes.
    let colored_cell = |text: &str, color: Color| {
        if painter.enabled {
            Cell::new(text).fg(color)
        } else {
            Cell::new(text)
        }
    };
    table.set_header(vec![
        colored_cell("#", Color::Yellow),
        colored_cell("Ecosystem", Color::Yellow),
        colored_cell("Path", Color::Yellow),
        colored_cell("Size", Color::Yellow),
        colored_cell("Last Modified", Color::Yellow),
    ]);
    for (idx, artifact) in scan.artifacts.iter().enumerate() {
        table.add_row(vec![
            Cell::new((idx + 1).to_string()),
            colored_cell(artifact.artifact_type.title(), Color::Cyan),
            Cell::new(display_path(&artifact.path)),
            colored_cell(&format_size(artifact.size_bytes, DECIMAL), Color::Green),
            Cell::new(fmt_age(artifact.last_modified)),
        ]);
    }
    println!("{table}");
    println!();
    println!(
        "Total reclaimable space: {} in {} directories (scanned {} dirs in {}).",
        painter.size(&format_size(total_size(&scan.artifacts), DECIMAL)),
        scan.artifacts.len(),
        scan.dirs_seen,
        fmt_duration(scan.elapsed)
    );
}

/// Interactive checklist (space to toggle, enter to confirm).
/// Returns `None` when the user bailed out of the prompt (Esc/Ctrl+C).
///
/// # Errors
/// Propagates terminal IO problems (and refuses to run piped).
pub fn select_artifacts(artifacts: &[Artifact]) -> anyhow::Result<Option<Vec<Artifact>>> {
    if !std::io::stdout().is_terminal() {
        return Err(anyhow::anyhow!(
            "interactive mode needs a terminal; use --all --yes for non-interactive cleanup"
        ));
    }
    let items: Vec<String> = artifacts
        .iter()
        .map(|artifact| {
            format!(
                "{:>10}  {:<13} {}",
                format_size(artifact.size_bytes, DECIMAL),
                artifact.artifact_type.title(),
                display_path(&artifact.path)
            )
        })
        .collect();
    let picked = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select artifacts to delete (space toggles, enter accepts)")
        .items(&items)
        .interact_opt()
        .context("interactive selection failed")?;
    Ok(picked.map(|indices| {
        indices
            .into_iter()
            .map(|idx| artifacts[idx].clone())
            .collect()
    }))
}

/// Yes/no confirmation for the removal batch. Esc/Ctrl+C counts as "no".
///
/// # Errors
/// Propagates terminal IO problems.
pub fn confirm_removal(prompt: String) -> anyhow::Result<bool> {
    Ok(Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(false)
        .interact_opt()?
        .unwrap_or(false))
}

/// Dry-run printout of the exact removal set.
pub fn print_would_remove(artifacts: &[Artifact]) {
    println!(
        "Would remove {} item(s) [{}]:",
        artifacts.len(),
        format_size(total_size(artifacts), DECIMAL)
    );
    for artifact in artifacts {
        println!(
            "  - {} [{}]",
            artifact.path.display(),
            artifact.artifact_type.title()
        );
    }
}

/// Prints removal results: failures to stderr (they are errors), normal
/// summary to stdout.
pub fn print_removal_outcome(outcome: &crate::cleaner::RemovalOutcome) {
    for (path, err) in &outcome.failed {
        eprintln!("failed to remove {}: {err:#}", path.display());
    }
    println!(
        "Removed {} item(s), freed {}.",
        outcome.removed,
        format_size(outcome.freed_bytes, DECIMAL)
    );
}

/// Shortens the home prefix to `~/` and normalizes separators for display.
#[must_use]
pub fn display_path(path: &Path) -> String {
    if let Some(home) = home_dir()
        && let Ok(rel) = path.strip_prefix(&home)
    {
        let rel = rel.to_string_lossy().replace('\\', "/");
        return if rel.is_empty() {
            "~".to_string()
        } else {
            format!("~/{rel}")
        };
    }
    path.to_string_lossy().replace('\\', "/")
}

/// Home directory without the deprecated `std::env::home_dir`.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Human age of a modification timestamp, e.g. `5 days ago`.
#[must_use]
pub fn fmt_age(mtime: SystemTime) -> String {
    let Ok(age) = mtime.elapsed() else {
        return "in the future".to_string();
    };
    let mins = age.as_secs() / 60;
    match mins {
        0 => "just now".to_string(),
        1..=59 => format!("{mins} min ago"),
        60..=1_439 => format!("{} h ago", mins / 60),
        _ => format!("{} days ago", mins / 1_440),
    }
}

/// Compact up-time display for the scan itself.
#[must_use]
pub fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0 => "<1s".to_string(),
        1..=59 => format!("{secs}s"),
        60..=3_599 => format!("{} min", secs / 60),
        3_600..=172_799 => format!("{} h", secs / 3_600),
        _ => format!("{} days", secs / 86_400),
    }
}
