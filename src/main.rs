//! devclean entry point: wires CLI flags to scan/clean with the safety gates.
//!
//! Started without arguments on a terminal (double-click in Explorer), the
//! binary opens the full-screen TUI app; `--menu` brings back the older
//! step-by-step dialoguer menu. With any other argument the plain CLI path is
//! taken, scripts stay unaffected.

use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, anyhow};
use clap::Parser;

use devclean::cleaner;
use devclean::cli::Cli;
use devclean::logging;
use devclean::models::Artifact;
use devclean::scanner::{ArtifactFilter, Exclude, ScanProgress};
use devclean::ui;

use log::{debug, error, info, warn};

mod launcher;
mod tui;

const SYSTEM_DIRS: [&str; 15] = [
    "/usr", "/etc", "/bin", "/sbin", "/var", "/opt", "/lib", "/lib64", "/boot", "/dev", "/proc",
    "/sys", "/root", "/System", "/Library",
];
const WIN_BANNED: [&str; 3] = [
    "c:\\windows",
    "c:\\program files",
    "c:\\program files (x86)",
];

fn main() -> ExitCode {
    // Bare launch (double-click in Explorer, or `devclean` without flags on a
    // terminal): the full-screen TUI is the default face of the tool. With
    // any argument the plain CLI path is taken, scripts stay unaffected.
    if std::env::args().count() == 1 && std::io::stdout().is_terminal() {
        return tui::run_app();
    }
    let cli = Cli::parse();
    if cli.menu {
        if !std::io::stdout().is_terminal() {
            eprintln!("devclean: error: --menu needs a terminal");
            return ExitCode::FAILURE;
        }
        // `run_menu` opens the shared logger itself, so a piped `--menu`
        // (rejected above) leaves no log file behind.
        return launcher::run_menu();
    }
    logging::init();
    match run(&cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("devclean: error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<ExitCode> {
    let start = cli.start_dir();
    // Canonicalize, then drop the verbatim prefix: users see plain paths,
    // and the cleaner re-canonicalizes per artifact before deleting.
    let root = plain_path(
        fs::canonicalize(&start)
            .with_context(|| format!("resolving scan root '{}'", start.display()))?
            .as_path(),
    );
    info!("scan root: {}", root.display());
    debug!("cli = {cli:?}");
    if let Err(err) = guard_root(&root) {
        error!("scan root rejected: {err:#}");
        return Err(err);
    }

    let stop = Arc::new(AtomicBool::new(false));
    // Cooperative Ctrl+C: hand scanning/deletion a stop flag, exit 130 later.
    let stop_ctrlc = Arc::clone(&stop);
    if let Err(err) = ctrlc::set_handler(move || {
        stop_ctrlc.store(true, Ordering::Relaxed);
        info!("Ctrl+C: stopping after the current item");
    }) {
        eprintln!("devclean: note: Ctrl+C handler unavailable: {err}");
    }

    let filter = ArtifactFilter {
        older_than: cli.older_than,
        types: cli.types.clone(),
        min_size: cli.min_size,
        exclude: cli.exclude.iter().map(|s| Exclude::new(s)).collect(),
    };

    let painter = ui::Painter::stdout();
    let progress = ScanProgress::default();

    let spinner = ui::ScanSpinner::new(&root, &progress);
    let scan = devclean::scanner::scan(&root, &filter, &stop, &progress);
    drop(spinner);

    // The full-screen picker draws its own header, so the printed table would
    // only be a flash before the alternate screen opens.
    if !cli.tui || scan.artifacts.is_empty() {
        ui::print_scan_results(&painter, &root, &scan);
    }

    if scan.interrupted {
        eprintln!("devclean: interrupted, results shown are partial");
        warn!("interrupted: showing partial results, exit 130");
        return Ok(ExitCode::from(130));
    }
    if scan.artifacts.is_empty() {
        debug!("no artifacts survived the filters, nothing to clean");
        return Ok(ExitCode::SUCCESS);
    }

    // Spec §2.1: `--all` deletes without further prompts, so the crash-proof
    // `--yes` is mandatory unless the run is a harmless dry-run.
    if cli.all && !cli.yes && !cli.dry_run {
        return Err(anyhow!(
            "--all deletes everything found; confirm with --yes (or preview with --dry-run)"
        ));
    }

    if cli.tui {
        let Some(selected) = tui::pick_artifacts(&root, &scan.artifacts)? else {
            debug!("tui: quit without deleting");
            println!("Nothing deleted.");
            return Ok(ExitCode::SUCCESS);
        };
        if selected.is_empty() {
            println!("Nothing deleted.");
            return Ok(ExitCode::SUCCESS);
        }
        Ok(run_removal(&selected, &stop))
    } else if cli.interactive {
        let Some(selected) = ui::select_artifacts(&scan.artifacts)? else {
            debug!("interactive: user skipped selection");
            println!("Nothing selected.");
            return Ok(ExitCode::SUCCESS);
        };
        debug!("interactive: {} item(s) selected", selected.len());
        if selected.is_empty() {
            println!("Nothing selected.");
            return Ok(ExitCode::SUCCESS);
        }
        if cli.dry_run {
            ui::print_would_remove(&selected);
            return Ok(ExitCode::SUCCESS);
        }
        let prompt = format!(
            "Delete {} selected item(s), {}?",
            selected.len(),
            humansize::format_size(devclean::models::total_size(&selected), humansize::DECIMAL)
        );
        let confirmed = ui::confirm_removal(prompt)?;
        debug!("removal confirmed: {confirmed}");
        if !confirmed {
            println!("Nothing deleted.");
            return Ok(ExitCode::SUCCESS);
        }
        Ok(run_removal(&selected, &stop))
    } else if cli.all {
        if cli.dry_run {
            ui::print_would_remove(&scan.artifacts);
            return Ok(ExitCode::SUCCESS);
        }
        Ok(run_removal(&scan.artifacts, &stop))
    } else {
        // Plain analysis never deletes: point at the destructive verbs.
        println!("Run with -i (checklist), --tui (full-screen picker) or --all to clean up.");
        Ok(ExitCode::SUCCESS)
    }
}

pub(crate) fn run_removal(targets: &[Artifact], stop: &AtomicBool) -> ExitCode {
    let outcome = cleaner::remove_artifacts(targets, stop);
    ui::print_removal_outcome(&outcome);
    if !outcome.failed.is_empty() {
        ExitCode::FAILURE
    } else if stop.load(Ordering::Relaxed) {
        ExitCode::from(130)
    } else {
        ExitCode::SUCCESS
    }
}

/// Refuses to scan filesystem/system roots per spec §2.4. Deeper paths under
/// the banned roots stay allowed: an explicit subfolder is what a user chose.
pub(crate) fn guard_root(root: &Path) -> anyhow::Result<()> {
    // `/`, `C:\` and every other drive/mount root end up here.
    if root.parent().is_none() {
        return Err(anyhow!(
            "refusing to scan filesystem root '{}' — pass an explicit project folder",
            root.display()
        ));
    }
    if SYSTEM_DIRS.iter().any(|dir| root == Path::new(dir)) {
        return Err(anyhow!(
            "refusing to scan system directory '{}' — pass an explicit project folder",
            root.display()
        ));
    }
    // Windows roots, compared over the canonicalized path without verbatim prefix.
    let norm = root
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_ascii_lowercase();
    if WIN_BANNED.iter().any(|dir| norm == *dir) {
        return Err(anyhow!(
            "refusing to scan system directory '{}' — pass an explicit project folder",
            root.display()
        ));
    }
    // Canonicalized on both sides: `$HOME` may hold a symlinked or differently
    // cased path than the resolved scan root.
    let home = ui::home_dir().map(|home| {
        fs::canonicalize(&home).map_or_else(|_| plain_path(&home), |real| plain_path(&real))
    });
    if home.is_some_and(|home| devclean::paths::same_path(root, &home)) {
        return Err(anyhow!(
            "refusing to scan the home directory root — pass an explicit subfolder (e.g. ~/Projects)"
        ));
    }
    Ok(())
}

/// Verbatim (`\\?\`) paths back to their plain display form. UNC shares keep
/// their `\\server\share` shape.
#[must_use]
pub(crate) fn plain_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}
