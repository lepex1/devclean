//! End-to-end tests: build a realistic project tree in temp dirs, scan it,
//! apply filters, and delete.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use devclean::cleaner::remove_artifacts;
use devclean::detector::{self};
use devclean::models::{self, ArtifactType, Ecosystem};
use devclean::scanner::{ArtifactFilter, Exclude, ScanProgress, scan};

/// Writes `content` to `root/relative` creating intermediate dirs.
fn put(root: &Path, relative: &str, content: &str) {
    let target = root.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(target, content).expect("write fixture file");
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    /// A tree exercising every detector family plus negative cases.
    fn build() -> Self {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dir.path().to_path_buf();
        let write = |relative: &str, content: &str| put(&root, relative, content);

        write("rust_proj/Cargo.toml", "[package]");
        write("rust_proj/target/debug/proj.exe", &"x".repeat(2_000));
        write("rust_proj/src/main.rs", "fn main() {}");

        write("node_proj/package.json", "{}");
        write("node_proj/node_modules/pkg/index.js", &"y".repeat(1_500));
        // Nested artifact must NOT surface as a second hit (pruned subtree).
        write("node_proj/node_modules/pkg/node_modules/dep/i.js", "zzzz");

        write("py_proj/pyproject.toml", "");
        // venv confirmed only by the python executable inside.
        write("py_proj/.venv/Scripts/python.exe", "PE");
        write("py_proj/script/__pycache__/m.pyc", "cache-bytes");

        write("gradle_proj/build.gradle", "");
        write("gradle_proj/build/classes/A.class", "javabytes");
        write("gradle_proj/.gradle/cache/jar", "gradlecache");

        write("dotnet_proj/App.sln", "");
        write("dotnet_proj/bin/App.dll", "netbytes");
        write("dotnet_proj/obj/tmp", "netbytes");

        write("web_proj/package.json", "{}");
        write("web_proj/dist/bundle.js", "bundle");

        // Negative cases: same names, no confirming markers.
        write("plain_env/env/readme.txt", "just a folder");
        write("plain_bin/bin/notes.md", "just a folder");
        write("plain_build/build/makeout", "c build dir");

        Self { _dir: dir, root }
    }
}

fn empty_filter() -> ArtifactFilter {
    ArtifactFilter::default()
}

#[test]
fn scan_finds_all_artifacts_without_duplicates() {
    let fixture = Fixture::build();
    let result = scan(
        &fixture.root,
        &empty_filter(),
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    let paths: Vec<String> = result
        .artifacts
        .iter()
        .map(|a| a.path.to_string_lossy().to_lowercase())
        .collect();

    // Exactly the nine positive cases, each once.
    for needle in [
        "rust_proj\\target",
        "node_proj\\node_modules",
        "py_proj\\.venv",
        "py_proj\\script\\__pycache__",
        "gradle_proj\\build",
        "gradle_proj\\.gradle",
        "dotnet_proj\\bin",
        "dotnet_proj\\obj",
        "web_proj\\dist",
    ] {
        let needle_norm = needle.replace('\\', "/");
        assert!(
            paths
                .iter()
                .any(|p| p.replace('\\', "/").ends_with(&needle_norm)),
            "missing {needle_norm}"
        );
    }
    assert_eq!(result.artifacts.len(), 9, "unexpected extra artifacts");

    // Sizes came from the written contents (no zero measurement).
    assert!(
        result.artifacts.iter().all(|a| a.size_bytes > 0),
        "sizes must be measured"
    );
    // Sanity: types stamped correctly at least for the rust project.
    let rust = result
        .artifacts
        .iter()
        .find(|a| a.path.to_string_lossy().contains("target"))
        .expect("rust artifact present");
    assert_eq!(rust.artifact_type, ArtifactType::RustTarget);
}

#[test]
fn negative_cases_never_match() {
    let fixture = Fixture::build();
    let result = scan(
        &fixture.root,
        &empty_filter(),
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    let flat: String = result
        .artifacts
        .iter()
        .map(|a| a.path.to_string_lossy().to_lowercase())
        .collect();
    assert!(!flat.contains("plain_env"), "bare `env` is not a venv");
    assert!(!flat.contains("plain_bin"), "bare `bin` is not .NET");
    assert!(!flat.contains("plain_build"), "bare `build` is not Java");
    assert_eq!(result.artifacts.len(), 9);
}

#[test]
fn filters_narrow_down_results() {
    let fixture = Fixture::build();
    // min-size kills every small find except the rust target.
    let mut filter = empty_filter();
    filter.min_size = Some(1_800);
    let result = scan(
        &fixture.root,
        &filter,
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    assert_eq!(result.artifacts.len(), 1);
    assert_eq!(result.artifacts[0].artifact_type, ArtifactType::RustTarget);

    // --type node keeps only node stuff (dist lives in `generic` by design).
    let fixture = Fixture::build();
    let mut filter = empty_filter();
    filter.types = vec![Ecosystem::Node];
    let result = scan(
        &fixture.root,
        &filter,
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    assert_eq!(result.artifacts.len(), 1, "node_modules only");
    assert_eq!(result.artifacts[0].artifact_type, ArtifactType::NodeModules);

    // Adding `generic` brings dist back.
    let fixture = Fixture::build();
    let mut filter = empty_filter();
    filter.types = vec![Ecosystem::Node, Ecosystem::Generic];
    let result = scan(
        &fixture.root,
        &filter,
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    assert_eq!(result.artifacts.len(), 2, "node_modules and dist");
    assert!(
        result
            .artifacts
            .iter()
            .all(|a| a.artifact_type.ecosystem() != Ecosystem::Rust)
    );

    // exclude drops whole subtrees by substring.
    let fixture = Fixture::build();
    let mut filter = empty_filter();
    filter.exclude = vec![Exclude::new("rust_proj"), Exclude::new("gradle_proj")];
    let result = scan(
        &fixture.root,
        &filter,
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    let flat = result
        .artifacts
        .iter()
        .map(|a| a.path.to_string_lossy())
        .collect::<String>();
    assert!(!flat.contains("rust_proj"), "excluded subtree vanished");
    assert!(!flat.contains("gradle_proj"), "excluded subtree vanished");
    assert_eq!(result.artifacts.len(), 9 - 3);
}

#[test]
fn type_filter_skips_measurement_of_other_ecosystems() {
    let fixture = Fixture::build();
    let mut filter = empty_filter();
    filter.types = vec![Ecosystem::Rust];
    let progress = ScanProgress::default();
    let result = scan(
        &fixture.root,
        &filter,
        &Arc::new(AtomicBool::new(false)),
        &progress,
    );
    assert_eq!(result.artifacts.len(), 1);
    assert_eq!(
        progress.measured.load(Ordering::Relaxed),
        1,
        "only the Rust artifact was sized; node_modules/.venv never walked"
    );
}

#[test]
fn older_than_fresh_artifacts_are_skipped() {
    let fixture = Fixture::build();
    // Everything is ~now, so a big window keeps nothing.
    let mut filter = empty_filter();
    filter.older_than = Some(std::time::Duration::from_secs(86_400 * 30));
    let result = scan(
        &fixture.root,
        &filter,
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    assert!(result.artifacts.is_empty(), "fresh artifacts excluded");
}

#[test]
fn delete_removes_picked_artifacts_and_keeps_the_rest() {
    let fixture = Fixture::build();
    let result = scan(
        &fixture.root,
        &empty_filter(),
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    let stop = AtomicBool::new(false);
    let outcome = remove_artifacts(&result.artifacts, &stop);
    assert!(outcome.failed.is_empty(), "no failures expected");
    assert_eq!(outcome.removed, 9);
    assert!(outcome.freed_bytes > 0);

    assert!(!fixture.root.join("rust_proj").join("target").exists());
    assert!(!fixture.root.join("node_proj").join("node_modules").exists());
    assert!(!fixture.root.join("py_proj").join(".venv").exists());
    assert!(
        !fixture
            .root
            .join("py_proj")
            .join("script")
            .join("__pycache__")
            .exists()
    );
    // Sources and configs untouched.
    assert!(fixture.root.join("rust_proj").join("Cargo.toml").exists());
    assert!(fixture.root.join("node_proj").join("package.json").exists());
    assert!(
        fixture
            .root
            .join("gradle_proj")
            .join("build.gradle")
            .exists()
    );
}

#[test]
fn delete_is_stopped_by_interrupt_flag() {
    let fixture = Fixture::build();
    let result = scan(
        &fixture.root,
        &empty_filter(),
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    let stop = AtomicBool::new(true);
    let outcome = remove_artifacts(&result.artifacts, &stop);
    assert_eq!(outcome.removed, 0);
    assert!(fixture.root.join("rust_proj").join("target").exists());
}

#[cfg(windows)]
#[test]
fn delete_handles_readonly_files() {
    let fixture = Fixture::build();
    let stuck = fixture
        .root
        .join("rust_proj")
        .join("target")
        .join("locked.dll");
    fs::write(&stuck, "readonly bytes").expect("write");
    let mut perms = fs::metadata(&stuck).expect("stat").permissions();
    perms.set_readonly(true);
    fs::set_permissions(&stuck, perms).expect("set readonly");

    let result = scan(
        &fixture.root,
        &empty_filter(),
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    let outcome = remove_artifacts(&result.artifacts, &AtomicBool::new(false));
    assert!(
        outcome.failed.is_empty(),
        "readonly files must not block: {outcome:?}"
    );
    assert!(!fixture.root.join("rust_proj").join("target").exists());
}

#[test]
fn detector_rules_cover_all_names() {
    // Each rule's name must produce a hit when its marker dir exists.
    let fixture = Fixture::build();
    let hits = scan(
        &fixture.root,
        &empty_filter(),
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    )
    .artifacts
    .iter()
    .map(|a| a.artifact_type)
    .collect::<Vec<_>>();
    assert!(hits.contains(&ArtifactType::NodeModules));
    assert!(hits.contains(&ArtifactType::PythonVenv));
    assert!(hits.contains(&ArtifactType::Dist));

    // total_size sums the same artifacts (used by the report footer).
    let all = scan(
        &fixture.root,
        &empty_filter(),
        &Arc::new(AtomicBool::new(false)),
        &ScanProgress::default(),
    );
    assert_eq!(
        models::total_size(&all.artifacts),
        all.artifacts.iter().map(|a| a.size_bytes).sum::<u64>()
    );
}

#[test]
fn tui_refuses_piped_output() {
    let fixture = Fixture::build();
    let exe = env!("CARGO_BIN_EXE_devclean");
    let out = std::process::Command::new(exe)
        .current_dir(&fixture.root)
        .arg(".")
        .arg("--tui")
        .output()
        .expect("run the devclean binary");
    assert!(!out.status.success(), "piped --tui must not run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("needs a terminal"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn menu_refuses_piped_output() {
    let exe = env!("CARGO_BIN_EXE_devclean");
    let out = std::process::Command::new(exe)
        .arg("--menu")
        .output()
        .expect("run the devclean binary");
    assert!(!out.status.success(), "piped --menu must not run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("needs a terminal"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn detector_detect_is_case_insensitive_for_env_dir_with_python() {
    let dir = tempfile::TempDir::new().expect("temp");
    fs::create_dir_all(dir.path().join("Scripts")).expect("mkdir");
    fs::write(dir.path().join("Scripts").join("python.exe"), "x").expect("write");
    assert_eq!(
        detector::detect::<&str>("ENV", dir.path(), &[]),
        Some(ArtifactType::PythonVenv)
    );
}
