//! Full-screen UI built on ratatui + crossterm: the setup screens (folder and
//! filters) plus the artifact picker with live filtering and a one-key confirm.
//!
//! `run_app` is the bare-launch face of the tool (double-click in Explorer);
//! `pick_artifacts` is reused by `--tui`. Deletion itself stays in `cleaner`,
//! driven by `main`.

use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, anyhow};
use humansize::{DECIMAL, format_size};
use log::info;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::{cursor, execute, terminal};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};

use devclean::models::{Artifact, Ecosystem, total_size};
use devclean::scanner::{ArtifactFilter, ScanProgress};
use devclean::ui;

use crate::launcher::{read_last_path, save_last_path};
use crate::{guard_root, plain_path, run_removal};

/// Cyclic filter presets. The setup screen only has three keys, so the whole
/// flag surface of the CLI is compressed into these few steps.
const OLDER_PRESETS: [(&str, Option<u64>); 5] = [
    ("any age", None),
    ("7 days", Some(7)),
    ("14 days", Some(14)),
    ("30 days", Some(30)),
    ("1 year", Some(365)),
];
const SIZE_PRESETS: [(&str, Option<u64>); 4] = [
    ("any size", None),
    ("10 MB", Some(10_000_000)),
    ("100 MB", Some(100_000_000)),
    ("1 GB", Some(1_000_000_000)),
];
const ECO_PRESETS: [(&str, Option<Ecosystem>); 7] = [
    ("all", None),
    ("node", Some(Ecosystem::Node)),
    ("rust", Some(Ecosystem::Rust)),
    ("python", Some(Ecosystem::Python)),
    ("java", Some(Ecosystem::Java)),
    ("dotnet", Some(Ecosystem::Dotnet)),
    ("generic", Some(Ecosystem::Generic)),
];

/// Picker state, kept free of terminal IO so the key handling stays testable.
struct Selection {
    checked: Vec<bool>,
    /// Index into the *visible* list, not into `artifacts`.
    cursor: usize,
    filter: String,
    typing: bool,
    confirm: bool,
    sort: Sort,
}

/// Order of the picker rows. Each key has one sensible direction for cleanup:
/// size biggest first, age oldest first, the rest alphabetical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    Size,
    Age,
    Ecosystem,
    Path,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Self::Size => Self::Age,
            Self::Age => Self::Ecosystem,
            Self::Ecosystem => Self::Path,
            Self::Path => Self::Size,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Age => "age",
            Self::Ecosystem => "ecosystem",
            Self::Path => "path",
        }
    }

    fn compare(self, a: &Artifact, b: &Artifact) -> std::cmp::Ordering {
        match self {
            Self::Size => b.size_bytes.cmp(&a.size_bytes),
            Self::Age => a.last_modified.cmp(&b.last_modified),
            Self::Ecosystem => a.artifact_type.title().cmp(b.artifact_type.title()),
            Self::Path => a.path.cmp(&b.path),
        }
    }
}

impl Selection {
    fn new(len: usize) -> Self {
        Self {
            checked: vec![false; len],
            cursor: 0,
            filter: String::new(),
            typing: false,
            confirm: false,
            sort: Sort::Size,
        }
    }

    /// Artifact indices matching the current filter (all of them when empty),
    /// in the currently selected sort order.
    fn visible(&self, artifacts: &[Artifact]) -> Vec<usize> {
        let mut visible: Vec<usize> = if self.filter.is_empty() {
            (0..artifacts.len()).collect()
        } else {
            let needle = self.filter.to_lowercase();
            artifacts
                .iter()
                .enumerate()
                .filter(|(_, artifact)| {
                    ui::display_path(&artifact.path)
                        .to_lowercase()
                        .contains(&needle)
                        || artifact
                            .artifact_type
                            .title()
                            .to_lowercase()
                            .contains(&needle)
                })
                .map(|(idx, _)| idx)
                .collect()
        };
        visible.sort_by(|&a, &b| self.sort.compare(&artifacts[a], &artifacts[b]));
        visible
    }

    fn selected_indices(&self) -> Vec<usize> {
        self.checked
            .iter()
            .enumerate()
            .filter(|(_, checked)| **checked)
            .map(|(idx, _)| idx)
            .collect()
    }

    fn toggle(&mut self, visible: &[usize]) {
        if let Some(&idx) = visible.get(self.cursor) {
            self.checked[idx] = !self.checked[idx];
        }
    }

    fn set_all(&mut self, visible: &[usize], on: bool) {
        for &idx in visible {
            self.checked[idx] = on;
        }
    }

    fn move_cursor(&mut self, delta: isize, visible_len: usize) {
        if visible_len == 0 {
            self.cursor = 0;
            return;
        }
        let last = visible_len - 1;
        let current = self.cursor.min(last);
        self.cursor = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            (current + delta.unsigned_abs()).min(last)
        };
    }
}

/// What a keypress asks the event loop to do.
enum Action {
    None,
    Quit,
    Delete,
}

fn handle_key(sel: &mut Selection, artifacts: &[Artifact], key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }
    if sel.typing {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                sel.typing = false;
                sel.cursor = 0;
            }
            KeyCode::Backspace => {
                sel.filter.pop();
                sel.cursor = 0;
            }
            KeyCode::Char(c) => {
                sel.filter.push(c);
                sel.cursor = 0;
            }
            _ => {}
        }
        return Action::None;
    }
    if sel.confirm {
        match key.code {
            KeyCode::Char('y' | 'Y') => return Action::Delete,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => sel.confirm = false,
            _ => {}
        }
        return Action::None;
    }
    let visible = sel.visible(artifacts);
    // Caps Lock turns `q` into `Q`; command keys are case-insensitive, while
    // typed text (filter, path) keeps its case.
    let code = match key.code {
        KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
        code => code,
    };
    match code {
        KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
        KeyCode::Up | KeyCode::Char('k') => sel.move_cursor(-1, visible.len()),
        KeyCode::Down | KeyCode::Char('j') => sel.move_cursor(1, visible.len()),
        KeyCode::Char(' ') => sel.toggle(&visible),
        KeyCode::Char('a') => sel.set_all(&visible, true),
        KeyCode::Char('n') => sel.set_all(&visible, false),
        KeyCode::Char('/') => sel.typing = true,
        KeyCode::Char('s') => {
            // Keep the cursor on the same artifact after the rows move around.
            let anchor = visible.get(sel.cursor).copied();
            sel.sort = sel.sort.next();
            if let Some(anchor) = anchor {
                let reordered = sel.visible(artifacts);
                sel.cursor = reordered.iter().position(|&idx| idx == anchor).unwrap_or(0);
            }
        }
        KeyCode::Enter if !sel.selected_indices().is_empty() => sel.confirm = true,
        _ => {}
    }
    Action::None
}

fn draw(frame: &mut Frame, root: &Path, artifacts: &[Artifact], sel: &Selection) {
    let [header_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    let checked = sel.selected_indices();
    let checked_size: u64 = checked
        .iter()
        .filter_map(|&idx| artifacts.get(idx))
        .map(|artifact| artifact.size_bytes)
        .sum();

    let title = Line::from(format!(
        "devclean — {} artifact(s), {} reclaimable",
        artifacts.len(),
        format_size(total_size(artifacts), DECIMAL)
    ));
    let subtitle = if sel.filter.is_empty() {
        format!("root: {}", ui::display_path(root))
    } else {
        format!(
            "root: {}  ·  filter: {}",
            ui::display_path(root),
            sel.filter
        )
    };
    frame.render_widget(
        Paragraph::new(vec![title, Line::from(subtitle)]),
        header_area,
    );

    let visible = sel.visible(artifacts);
    let rows = visible
        .iter()
        .filter_map(|&idx| artifacts.get(idx).map(|a| (idx, a)));
    let rows: Vec<Row> = rows
        .map(|(idx, artifact)| {
            let mark = if sel.checked.get(idx).copied().unwrap_or(false) {
                Cell::from("[x]").style(Style::new().fg(Color::Green))
            } else {
                Cell::from("[ ]").style(Style::new().fg(Color::DarkGray))
            };
            Row::new(vec![
                mark,
                Cell::from(artifact.artifact_type.title()).style(Style::new().fg(Color::Cyan)),
                Cell::from(format!("{:>10}", format_size(artifact.size_bytes, DECIMAL)))
                    .style(Style::new().fg(Color::Green)),
                Cell::from(ui::fmt_age(artifact.last_modified)),
                Cell::from(shown_path(root, &artifact.path)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(14),
            Constraint::Length(11),
            Constraint::Length(14),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(["", "Ecosystem", "Size", "Age", "Path"])
            .style(Style::new().add_modifier(Modifier::BOLD)),
    )
    .block(Block::new().borders(Borders::ALL).title(format!(
        " {}/{} shown · sort: {} ",
        visible.len(),
        artifacts.len(),
        sel.sort.label()
    )))
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default();
    if !visible.is_empty() {
        state = state.with_selected(Some(sel.cursor.min(visible.len() - 1)));
    }
    frame.render_stateful_widget(table, table_area, &mut state);

    let help = Line::from(
        "Space toggle · a all · n none · / filter · s sort · Enter delete · q quit".to_string(),
    );
    let status = if sel.confirm {
        Line::from(format!(
            "Delete {} item(s), {}?  y = yes · n/Esc = cancel",
            checked.len(),
            format_size(checked_size, DECIMAL)
        ))
        .style(Style::new().fg(Color::Red).add_modifier(Modifier::BOLD))
    } else if sel.typing {
        Line::from(format!("filter: {}▏", sel.filter)).style(Style::new().fg(Color::Yellow))
    } else {
        Line::from(format!(
            "{} selected ({})",
            checked.len(),
            format_size(checked_size, DECIMAL)
        ))
        .style(Style::new().fg(Color::Yellow))
    };
    frame.render_widget(Paragraph::new(vec![help, status]), footer_area);
}

/// Path as shown in the table: relative to the scan root when possible, so
/// the narrow path column still fits deep trees.
fn shown_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| ui::display_path(path),
        |relative| relative.to_string_lossy().replace('\\', "/"),
    )
}

/// Setup state for the bare-launch app: folder, three filter presets and the
/// current step. Free of terminal IO, so key handling stays testable.
struct Setup {
    path: String,
    older: usize,
    min_size: usize,
    ecosystem: usize,
    step: Step,
    notice: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Path,
    Filters,
}

enum SetupAction {
    None,
    Scan,
    Quit,
}

impl Setup {
    /// Starts on the remembered folder (when it still exists) or the cwd.
    fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let start = read_last_path().filter(|path| path.is_dir()).unwrap_or(cwd);
        Self {
            path: start.to_string_lossy().into_owned(),
            older: 0,
            min_size: 0,
            ecosystem: 0,
            step: Step::Path,
            notice: None,
        }
    }

    fn filter(&self) -> ArtifactFilter {
        ArtifactFilter {
            older_than: OLDER_PRESETS[self.older]
                .1
                .map(|days| Duration::from_secs(days * 86_400)),
            types: ECO_PRESETS[self.ecosystem].1.into_iter().collect(),
            min_size: SIZE_PRESETS[self.min_size].1,
            exclude: Vec::new(),
        }
    }
}

fn handle_setup_key(setup: &mut Setup, key: KeyEvent) -> SetupAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return SetupAction::Quit;
    }
    match setup.step {
        Step::Path => match key.code {
            KeyCode::Esc => return SetupAction::Quit,
            KeyCode::Enter => {
                if setup.path.trim().is_empty() {
                    setup.notice = Some("enter a folder to scan".to_string());
                } else {
                    setup.notice = None;
                    setup.step = Step::Filters;
                }
            }
            KeyCode::Backspace => {
                setup.path.pop();
            }
            KeyCode::Char(c) => setup.path.push(c),
            _ => {}
        },
        Step::Filters => {
            // Caps Lock must not disable `o`/`m`/`t`.
            let code = match key.code {
                KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
                code => code,
            };
            match code {
                KeyCode::Esc => {
                    setup.step = Step::Path;
                    setup.notice = None;
                }
                KeyCode::Enter => return SetupAction::Scan,
                KeyCode::Char('o') => setup.older = (setup.older + 1) % OLDER_PRESETS.len(),
                KeyCode::Char('m') => setup.min_size = (setup.min_size + 1) % SIZE_PRESETS.len(),
                KeyCode::Char('t') => setup.ecosystem = (setup.ecosystem + 1) % ECO_PRESETS.len(),
                _ => {}
            }
        }
    }
    SetupAction::None
}

fn draw_setup(frame: &mut Frame, setup: &Setup) {
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let step_line = match setup.step {
        Step::Path => "Step 1/2 — folder to scan",
        Step::Filters => "Step 2/2 — filters",
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("devclean — full-screen mode")
                .style(Style::new().add_modifier(Modifier::BOLD)),
            Line::from(step_line).style(Style::new().fg(Color::DarkGray)),
        ]),
        header_area,
    );

    let (title, body) = match setup.step {
        Step::Path => (
            " folder ",
            vec![Line::from("Path:"), Line::from(format!("{}▏", setup.path))],
        ),
        Step::Filters => (
            " filters ",
            vec![
                Line::from(format!("Path: {}", setup.path)),
                Line::from(format!(
                    "Older than   [o]: {}",
                    OLDER_PRESETS[setup.older].0
                )),
                Line::from(format!(
                    "Min size     [m]: {}",
                    SIZE_PRESETS[setup.min_size].0
                )),
                Line::from(format!(
                    "Ecosystem    [t]: {}",
                    ECO_PRESETS[setup.ecosystem].0
                )),
            ],
        ),
    };
    frame.render_widget(
        Paragraph::new(body).block(Block::new().borders(Borders::ALL).title(title)),
        body_area,
    );

    let help = match setup.step {
        Step::Path => "type the folder · Enter next · Esc quit",
        Step::Filters => "o older · m min size · t ecosystem · Enter scan · Esc back",
    };
    let mut footer = vec![Line::from(help)];
    if let Some(notice) = &setup.notice {
        footer.push(Line::from(notice.clone()).style(Style::new().fg(Color::Yellow)));
    }
    frame.render_widget(Paragraph::new(footer), footer_area);
}

/// Canonicalizes the typed folder, then applies the same safety gate as the
/// CLI (`guard_root`), so the TUI cannot be talked into scanning `C:\`.
fn resolve_root(input: &str) -> anyhow::Result<PathBuf> {
    // Explorer's "Copy as path" wraps the folder in double quotes.
    let path = PathBuf::from(input.trim().trim_matches('"'));
    if !path.is_dir() {
        return Err(anyhow!("'{}' is not a directory", path.display()));
    }
    let canonical = fs::canonicalize(&path)
        .with_context(|| format!("resolving scan root '{}'", path.display()))?;
    let root = plain_path(&canonical);
    guard_root(&root)?;
    Ok(root)
}

fn open_terminal() -> anyhow::Result<(Screen, Terminal<CrosstermBackend<std::io::Stdout>>)> {
    let screen = Screen::enter()?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let terminal = Terminal::new(backend).context("opening the terminal")?;
    Ok((screen, terminal))
}

/// One setup session: returns `Scan` (with the state filled in) or `Quit`.
fn run_setup(setup: &mut Setup) -> anyhow::Result<SetupAction> {
    let (_screen, mut terminal) = open_terminal()?;
    loop {
        terminal.draw(|frame| draw_setup(frame, setup))?;
        if !event::poll(Duration::from_millis(250)).context("polling terminal events")? {
            continue;
        }
        let Event::Key(key) = event::read().context("reading a terminal event")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match handle_setup_key(setup, key) {
            SetupAction::None => {}
            action => return Ok(action),
        }
    }
}

/// Bare-launch entry point (double-click): setup → scan → picker → removal,
/// looping back to setup so the window stays usable after a clean.
pub fn run_app() -> ExitCode {
    crate::logging::init();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_ctrlc = Arc::clone(&stop);
    if let Err(err) = ctrlc::set_handler(move || {
        stop_ctrlc.store(true, Ordering::Relaxed);
        info!("Ctrl+C: stopping after the current item");
    }) {
        eprintln!("devclean: note: Ctrl+C handler unavailable: {err}");
    }

    let mut setup = Setup::new();
    let mut failed = false;
    loop {
        match run_setup(&mut setup) {
            Ok(SetupAction::Quit) => {
                info!("tui app: quit from setup");
                // A double-click window must not vanish on the failure message,
                // so the exit code is only reported after the user quits.
                return if failed {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                };
            }
            Ok(SetupAction::Scan) => {}
            Ok(SetupAction::None) => continue,
            Err(err) => {
                eprintln!("devclean: error: {err:#}");
                return ExitCode::FAILURE;
            }
        }

        let root = match resolve_root(&setup.path) {
            Ok(root) => root,
            Err(err) => {
                setup.step = Step::Path;
                setup.notice = Some(format!("{err:#}"));
                continue;
            }
        };
        info!("tui app: scan root = {}", root.display());
        save_last_path(&root);
        stop.store(false, Ordering::Relaxed);

        let progress = ScanProgress::default();
        let spinner = ui::ScanSpinner::new(&root, &progress);
        let scan = devclean::scanner::scan(&root, &setup.filter(), &stop, &progress);
        drop(spinner);

        if scan.interrupted {
            setup.notice = Some("scan interrupted — results were partial".to_string());
            continue;
        }
        if scan.artifacts.is_empty() {
            setup.notice = Some("nothing found with these filters".to_string());
            continue;
        }
        match pick_artifacts(&root, &scan.artifacts) {
            Ok(Some(selected)) if !selected.is_empty() => {
                if run_removal(&selected, &stop) == ExitCode::SUCCESS {
                    setup.notice = Some(format!("removed {} item(s)", selected.len()));
                } else {
                    failed = true;
                    setup.notice = Some(
                        "some items could not be removed — details in devclean.log".to_string(),
                    );
                }
            }
            Ok(_) => setup.notice = None,
            Err(err) => {
                eprintln!("devclean: error: {err:#}");
                return ExitCode::FAILURE;
            }
        }
    }
}

/// Raw mode + alternate screen; `Drop` restores both even on an early error.
struct Screen;

impl Screen {
    fn enter() -> anyhow::Result<Self> {
        terminal::enable_raw_mode().context("entering raw mode")?;
        execute!(
            std::io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide
        )
        .context("entering the alternate screen")?;
        Ok(Self)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            cursor::Show,
            terminal::LeaveAlternateScreen
        );
    }
}

/// Opens the full-screen picker and returns the ticked artifacts, or `None`
/// when the user quits without confirming a deletion.
///
/// # Errors
/// Propagates terminal IO problems (and refuses to run with piped stdout).
pub fn pick_artifacts(
    root: &Path,
    artifacts: &[Artifact],
) -> anyhow::Result<Option<Vec<Artifact>>> {
    if !std::io::stdout().is_terminal() {
        return Err(anyhow!(
            "--tui needs a terminal; use -i or --all --yes without one"
        ));
    }
    info!("tui: opening picker with {} artifact(s)", artifacts.len());
    let (_screen, mut terminal) = open_terminal()?;
    let mut sel = Selection::new(artifacts.len());
    loop {
        terminal.draw(|frame| draw(frame, root, artifacts, &sel))?;
        if !event::poll(Duration::from_millis(250)).context("polling terminal events")? {
            continue;
        }
        let Event::Key(key) = event::read().context("reading a terminal event")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match handle_key(&mut sel, artifacts, key) {
            Action::None => {}
            Action::Quit => {
                info!("tui: quit without deleting");
                return Ok(None);
            }
            Action::Delete => {
                let picked: Vec<Artifact> = sel
                    .selected_indices()
                    .into_iter()
                    .filter_map(|idx| artifacts.get(idx).cloned())
                    .collect();
                info!("tui: {} item(s) picked for removal", picked.len());
                return Ok(Some(picked));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    use devclean::models::ArtifactType;

    use super::{
        Action, Selection, Setup, SetupAction, Sort, Step, draw, draw_setup, handle_key,
        handle_setup_key, resolve_root,
    };

    fn artifact(path: &str, size: u64, artifact_type: ArtifactType) -> super::Artifact {
        super::Artifact {
            path: PathBuf::from(path),
            artifact_type,
            size_bytes: size,
            last_modified: SystemTime::now(),
        }
    }

    fn artifacts() -> Vec<super::Artifact> {
        vec![
            artifact(
                "D:/proj/webshop/node_modules",
                2048,
                ArtifactType::NodeModules,
            ),
            artifact(
                "D:/proj/rust-service/target",
                1024,
                ArtifactType::RustTarget,
            ),
        ]
    }

    #[test]
    fn filter_matches_path_and_title_case_insensitively() {
        let arts = artifacts();
        let mut sel = Selection::new(arts.len());
        sel.filter = "TARGET".to_string();
        assert_eq!(sel.visible(&arts), vec![1]);
        sel.filter = "node".to_string();
        assert_eq!(sel.visible(&arts), vec![0], "title match works too");
        sel.filter = "nomatch".to_string();
        assert!(sel.visible(&arts).is_empty());
    }

    #[test]
    fn toggle_and_set_all_only_touch_visible_rows() {
        let arts = artifacts();
        let mut sel = Selection::new(arts.len());
        let visible = sel.visible(&arts);
        sel.move_cursor(1, visible.len());
        sel.toggle(&visible);
        assert_eq!(sel.selected_indices(), vec![1]);

        sel.filter = "webshop".to_string();
        let visible = sel.visible(&arts);
        sel.set_all(&visible, true);
        assert_eq!(sel.selected_indices(), vec![0, 1], "all stays ticked");
        sel.set_all(&visible, false);
        assert_eq!(
            sel.selected_indices(),
            vec![1],
            "only the visible one cleared"
        );
    }

    #[test]
    fn cursor_clamps_when_the_filter_shrinks_the_list() {
        let arts = artifacts();
        let mut sel = Selection::new(arts.len());
        sel.move_cursor(5, 2);
        assert_eq!(sel.cursor, 1);
        sel.move_cursor(-5, 2);
        assert_eq!(sel.cursor, 0);
        sel.filter = "target".to_string();
        sel.cursor = 0;
        let visible = sel.visible(&arts);
        assert_eq!(visible.len(), 1);
    }

    #[test]
    fn enter_needs_a_ticked_row_and_y_confirms() {
        let arts = artifacts();
        let mut sel = Selection::new(arts.len());
        let visible = sel.visible(&arts);
        assert!(matches!(
            handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Enter)),
            Action::None
        ));
        assert!(!sel.confirm, "nothing ticked, no confirm prompt");

        sel.set_all(&visible, true);
        assert!(matches!(
            handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Enter)),
            Action::None
        ));
        assert!(sel.confirm);
        assert!(matches!(
            handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Char('y'))),
            Action::Delete
        ));
    }

    #[test]
    fn sort_key_cycles_and_orders_the_rows() {
        let arts = sortable_artifacts();
        let mut sel = Selection::new(arts.len());
        assert_eq!(
            sel.visible(&arts),
            vec![1, 2, 0],
            "biggest first by default"
        );

        handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(sel.sort, Sort::Age);
        assert_eq!(sel.visible(&arts), vec![0, 2, 1], "oldest first");

        handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(sel.sort, Sort::Ecosystem);
        assert_eq!(
            sel.visible(&arts),
            vec![2, 0, 1],
            "titles sort .NET < Node.js < Rust"
        );

        handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(sel.sort, Sort::Path);
        assert_eq!(
            sel.visible(&arts),
            vec![2, 1, 0],
            "D:/a/bin < D:/a/target < D:/b/node_modules"
        );

        handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(sel.sort, Sort::Size, "cycles back to size");
    }

    #[test]
    fn sort_keeps_the_cursor_on_the_same_artifact() {
        let arts = sortable_artifacts();
        let mut sel = Selection::new(arts.len());
        sel.cursor = 0; // first row = artifact 1 (the biggest one)
        handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Char('s')));
        let visible = sel.visible(&arts);
        assert_eq!(
            visible[sel.cursor], 1,
            "cursor follows the artifact, not the row number"
        );
    }

    fn sortable_artifacts() -> Vec<super::Artifact> {
        let at = |secs| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        vec![
            super::Artifact {
                path: PathBuf::from("D:/b/node_modules"),
                artifact_type: ArtifactType::NodeModules,
                size_bytes: 100,
                last_modified: at(1_000),
            },
            super::Artifact {
                path: PathBuf::from("D:/a/target"),
                artifact_type: ArtifactType::RustTarget,
                size_bytes: 300,
                last_modified: at(3_000),
            },
            super::Artifact {
                path: PathBuf::from("D:/a/bin"),
                artifact_type: ArtifactType::DotnetBuild,
                size_bytes: 200,
                last_modified: at(2_000),
            },
        ]
    }

    #[test]
    fn setup_filter_keys_work_with_caps_lock() {
        let mut state = setup("D:/proj");
        state.step = Step::Filters;
        handle_setup_key(&mut state, KeyEvent::from(KeyCode::Char('O')));
        handle_setup_key(&mut state, KeyEvent::from(KeyCode::Char('M')));
        handle_setup_key(&mut state, KeyEvent::from(KeyCode::Char('T')));
        assert_eq!(state.older, 1);
        assert_eq!(state.min_size, 1);
        assert_eq!(state.ecosystem, 1);
    }

    #[test]
    fn picker_command_keys_work_with_caps_lock() {
        let arts = sortable_artifacts();
        let mut sel = Selection::new(arts.len());
        handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Char('S')));
        assert_eq!(sel.sort, Sort::Age, "S sorts like s");
        handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Char('A')));
        assert_eq!(sel.selected_indices().len(), arts.len(), "A selects all");
        handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Char('N')));
        assert!(sel.selected_indices().is_empty(), "N clears the selection");
        assert!(matches!(
            handle_key(&mut sel, &arts, KeyEvent::from(KeyCode::Char('Q'))),
            Action::Quit
        ));
    }

    #[test]
    fn draw_renders_rows_and_help() {
        let arts = artifacts();
        let sel = Selection::new(arts.len());
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, std::path::Path::new("D:/proj"), &arts, &sel))
            .expect("draw frame");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("node_modules"), "paths are visible");
        assert!(text.contains("Space toggle"), "help line is visible");
        assert!(text.contains("2 artifact(s)"), "header shows the count");
    }

    fn setup(path: &str) -> Setup {
        Setup {
            path: path.to_string(),
            older: 0,
            min_size: 0,
            ecosystem: 0,
            step: Step::Path,
            notice: None,
        }
    }

    #[test]
    fn setup_types_a_path_and_enter_advances() {
        let mut state = setup("");
        for c in "D:/proj".chars() {
            handle_setup_key(&mut state, KeyEvent::from(KeyCode::Char(c)));
        }
        handle_setup_key(&mut state, KeyEvent::from(KeyCode::Backspace));
        assert_eq!(state.path, "D:/pro");
        assert!(
            matches!(
                handle_setup_key(&mut state, KeyEvent::from(KeyCode::Enter)),
                SetupAction::None
            ),
            "advancing is not a scan yet"
        );
        assert_eq!(state.step, Step::Filters);
    }

    #[test]
    fn setup_refuses_an_empty_path() {
        let mut state = setup("   ");
        handle_setup_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert_eq!(state.step, Step::Path);
        assert!(state.notice.is_some(), "the user gets an explanation");
    }

    #[test]
    fn setup_filter_keys_cycle_and_wrap() {
        let mut state = setup("D:/proj");
        state.step = Step::Filters;
        for _ in 0..super::OLDER_PRESETS.len() {
            handle_setup_key(&mut state, KeyEvent::from(KeyCode::Char('o')));
        }
        assert_eq!(state.older, 0, "cycles back to any age");
        handle_setup_key(&mut state, KeyEvent::from(KeyCode::Char('m')));
        assert_eq!(state.min_size, 1);
        handle_setup_key(&mut state, KeyEvent::from(KeyCode::Char('t')));
        assert_eq!(state.ecosystem, 1);
        assert!(matches!(
            handle_setup_key(&mut state, KeyEvent::from(KeyCode::Enter)),
            SetupAction::Scan
        ));
    }

    #[test]
    fn setup_esc_goes_back_then_quits() {
        let mut state = setup("D:/proj");
        state.step = Step::Filters;
        handle_setup_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert_eq!(state.step, Step::Path);
        assert!(matches!(
            handle_setup_key(&mut state, KeyEvent::from(KeyCode::Esc)),
            SetupAction::Quit
        ));
    }

    #[test]
    fn setup_maps_presets_to_the_scan_filter() {
        let mut state = setup("D:/proj");
        state.older = 1;
        state.min_size = 3;
        state.ecosystem = 2;
        let filter = state.filter();
        assert_eq!(
            filter.older_than,
            Some(std::time::Duration::from_secs(7 * 86_400))
        );
        assert_eq!(filter.min_size, Some(1_000_000_000));
        assert_eq!(filter.types, vec![devclean::models::Ecosystem::Rust]);
        assert!(filter.exclude.is_empty());
    }

    #[test]
    fn resolve_root_rejects_files_and_accepts_dirs() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let file = dir.path().join("note.txt");
        std::fs::write(&file, "x").expect("write fixture");
        let err = resolve_root(&file.to_string_lossy()).expect_err("a file is not a scan root");
        assert!(err.to_string().contains("not a directory"), "{err:#}");

        let root = resolve_root(&format!("\"{}\"", dir.path().display()))
            .expect("quoted temp dir resolves");
        assert!(root.is_dir());
    }

    #[test]
    fn draw_setup_shows_the_folder_and_the_presets() {
        let mut state = setup("D:/proj");
        state.step = Step::Filters;
        state.notice = Some("nothing found with these filters".to_string());
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).expect("test terminal");
        terminal
            .draw(|frame| draw_setup(frame, &state))
            .expect("draw frame");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("D:/proj"), "the chosen folder is visible");
        assert!(text.contains("Older than"), "filter rows are visible");
        assert!(text.contains("any age"), "preset values are visible");
        assert!(text.contains("nothing found"), "the notice is visible");
    }
}
