//! Parallel directory traversal, artifact detection, artifact measurement
//! and user-filter application.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use jwalk::WalkDir as JwalkDir;
use log::{debug, info, trace};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::detector::detect;
use crate::models::{Artifact, ArtifactType, Ecosystem};

/// Shared live counters the UI spinner can poll while scanning runs.
#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    /// Directories read so far during the walk.
    pub dirs: Arc<AtomicU64>,
    /// Artifacts measured so far during the sizing phase.
    pub measured: Arc<AtomicU64>,
}

/// Everything that narrows down which artifacts survive the scan.
#[derive(Debug, Default)]
pub struct ArtifactFilter {
    pub older_than: Option<Duration>,
    pub types: Vec<Ecosystem>,
    pub min_size: Option<u64>,
    pub exclude: Vec<Exclude>,
}

impl ArtifactFilter {
    /// Cheap checks that need no measurement: ecosystem and path excludes.
    /// Applied before sizing so `--type rust` never walks millions of files
    /// inside `node_modules` it would drop afterwards anyway.
    fn wanted(&self, path: &Path, kind: ArtifactType) -> bool {
        if !self.types.is_empty() && !self.types.contains(&kind.ecosystem()) {
            debug!(
                "skipped {}: ecosystem {:?} not in {:?}",
                path.display(),
                kind.ecosystem(),
                self.types
            );
            return false;
        }
        if self.exclude.iter().any(|e| e.matches(path)) {
            debug!(
                "skipped {}: path matches an --exclude pattern",
                path.display()
            );
            return false;
        }
        true
    }

    /// Size/age checks, run after measurement. Ecosystem and excludes are
    /// already applied by [`Self::wanted`].
    fn keep_artifact(&self, artifact: &Artifact, now: SystemTime) -> bool {
        if let Some(min) = self.min_size
            && artifact.size_bytes < min
        {
            debug!(
                "dropped {}: size {} < min-size {min}",
                artifact.path.display(),
                artifact.size_bytes
            );
            return false;
        }
        if let Some(older) = self.older_than {
            // Clock skew (mtime in the future) counts as "just modified".
            let age = now
                .duration_since(artifact.last_modified)
                .unwrap_or(Duration::ZERO);
            if age < older {
                debug!(
                    "dropped {}: age {age:?} < older-than {older:?}",
                    artifact.path.display()
                );
                return false;
            }
        }
        debug!("kept {}", artifact.path.display());
        true
    }
}

/// One `--exclude` path pattern (see `glob_match` for the supported subset).
#[derive(Debug, Clone)]
pub struct Exclude {
    pattern: String,
}

impl Exclude {
    #[must_use]
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
        }
    }

    /// Matches the pattern against the path normalized to lowercased
    /// forward slashes, so both `C:\dev` and `/home/mia/...` compare alike.
    #[must_use]
    pub fn matches(&self, path: &Path) -> bool {
        let norm = path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        let pat = self.pattern.replace('\\', "/").to_ascii_lowercase();
        if pat.contains('*') || pat.contains('?') {
            // gitignore convention: a leading "**/" also matches at the very
            // root, which the segment-strict matcher alone cannot express.
            if let Some(stripped) = pat.strip_prefix("**/") {
                if !stripped.contains('*') {
                    // A bare name after "**/" means "a path segment named so".
                    return norm.split('/').any(|seg| seg == stripped);
                }
                if glob_match(stripped.as_bytes(), norm.as_bytes()) {
                    return true;
                }
            }
            glob_match(pat.as_bytes(), norm.as_bytes())
        } else {
            norm.contains(&pat)
        }
    }
}

/// Glob subset matcher: `**` matches any run including `/`, plain `*` and
/// `?` never cross a `/`, every other character must match literally.
/// Both inputs are expected lowercase.
#[must_use]
fn glob_match(mut pattern: &[u8], mut text: &[u8]) -> bool {
    loop {
        match (pattern.first(), text.first()) {
            (None, None) => return true,
            (None, Some(_)) | (Some(_), None) => return false,
            (Some(b'*'), _) => {
                let double = pattern.get(1) == Some(&b'*');
                let rest = if double { &pattern[2..] } else { &pattern[1..] };
                if rest.is_empty() {
                    // Trailing star: `**` swallows everything, `*` stops at '/'.
                    if double {
                        return true;
                    }
                    return !text.contains(&b'/');
                }
                // Anchor `rest` at every possible offset behind the star-run.
                let mut i = 0;
                loop {
                    if glob_match(rest, &text[i..]) {
                        return true;
                    }
                    if i == text.len() {
                        return false;
                    }
                    if !double && text[i] == b'/' {
                        return false;
                    }
                    i += 1;
                }
            }
            (Some(b'?'), Some(t)) => {
                if *t == b'/' {
                    return false;
                }
                pattern = &pattern[1..];
                text = &text[1..];
            }
            (Some(p), Some(t)) => {
                if p != t {
                    return false;
                }
                pattern = &pattern[1..];
                text = &text[1..];
            }
        }
    }
}

/// Outcome of one scan run.
#[derive(Debug)]
pub struct ScanResult {
    pub artifacts: Vec<Artifact>,
    pub dirs_seen: u64,
    /// True when the process saw an interrupt signal; results may be partial.
    pub interrupted: bool,
    pub elapsed: Duration,
}

/// Walks `root` looking for artifact directories, then measures the finds.
///
/// Artifact subtrees are pruned during the walk (their contents hold no new
/// detection targets), so nested artifacts never double count. Directory
/// traversal happens on jwalk's parallel pool; per-artifact measurements on
/// the rayon pool. Interrupt (`stop`) ends the walk early and skips
/// remaining measurements.
#[must_use]
pub fn scan(
    root: &Path,
    filter: &ArtifactFilter,
    stop: &Arc<AtomicBool>,
    progress: &ScanProgress,
) -> ScanResult {
    let started = Instant::now();
    let found: Arc<Mutex<Vec<(PathBuf, ArtifactType)>>> = Arc::new(Mutex::new(Vec::new()));
    let found_walk = Arc::clone(&found);
    let stop_walk = Arc::clone(stop);
    let dirs_counter = Arc::clone(&progress.dirs);

    let walker = JwalkDir::new(root)
        .follow_links(false)
        .skip_hidden(false)
        .process_read_dir(move |_depth, path, _state, children| {
            dirs_counter.fetch_add(1, Ordering::Relaxed);
            trace!("read {} child(ren) in {}", children.len(), path.display());
            if stop_walk.load(Ordering::Relaxed) {
                // Nothing below here gets read anymore: the walk drains fast.
                children.clear();
                return;
            }
            // Pass 1: collect sibling names and detection decisions.
            let names: Vec<String> = children
                .iter()
                .filter_map(|child| child.as_ref().ok())
                .map(|entry| entry.file_name.to_string_lossy().into_owned())
                .collect();
            let detections: Vec<(usize, ArtifactType)> = children
                .iter()
                .enumerate()
                .filter_map(|(idx, child)| {
                    let entry = child.as_ref().ok()?;
                    if !entry.file_type.is_dir() {
                        return None;
                    }
                    let kind = detect(&entry.file_name.to_string_lossy(), &entry.path(), &names)?;
                    Some((idx, kind))
                })
                .collect();
            drop(names);
            // Pass 2: apply pruning and remember the hits.
            for (idx, entry) in children
                .iter_mut()
                .enumerate()
                .filter_map(|(idx, child)| child.as_mut().ok().map(|entry| (idx, entry)))
            {
                if let Some(kind) = detections.iter().find(|(i, _)| *i == idx).map(|(_, k)| *k) {
                    entry.read_children_path = None;
                    debug!(
                        "matched {} -> {:?}, pruned subtree",
                        entry.path().display(),
                        kind
                    );
                    if let Ok(mut list) = found_walk.lock() {
                        list.push((entry.path(), kind));
                    }
                }
            }
        });

    // Drive the walk; all results arrive through process_read_dir above.
    for entry in walker {
        let _ = entry;
    }

    let raw: Vec<(PathBuf, ArtifactType)> = match found.lock() {
        Ok(mut list) => std::mem::take(&mut *list),
        Err(_) => Vec::new(),
    };
    let found_total = raw.len();

    // Ecosystem/exclude filters need no sizes: drop those candidates before
    // the (expensive) sizing pass instead of walking them for nothing.
    let raw: Vec<(PathBuf, ArtifactType)> = raw
        .into_iter()
        .filter(|(path, kind)| filter.wanted(path, *kind))
        .collect();
    let measured_total = raw.len();

    // Measure each artifact on the rayon pool while stamping progress.
    let stop_measure = Arc::clone(stop);
    let measured_counter = Arc::clone(&progress.measured);
    let mut artifacts: Vec<Artifact> = raw
        .into_par_iter()
        .map(|(path, kind)| {
            let measured = measure_dir(&path, &stop_measure, &measured_counter);
            debug!(
                "measured {}: {} bytes, last modified {:?}",
                path.display(),
                measured.size_bytes,
                measured.last_modified
            );
            Artifact {
                path,
                artifact_type: kind,
                size_bytes: measured.size_bytes,
                last_modified: measured.last_modified.unwrap_or(SystemTime::UNIX_EPOCH),
            }
        })
        .collect();
    artifacts.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
    let now = SystemTime::now();
    artifacts.retain(|artifact| filter.keep_artifact(artifact, now));
    info!(
        "scan: {found_total} artifact(s) found, {measured_total} measured, {} kept after filters, {} dirs read, elapsed {:?}",
        artifacts.len(),
        progress.dirs.load(Ordering::Relaxed),
        started.elapsed()
    );

    ScanResult {
        dirs_seen: progress.dirs.load(Ordering::Relaxed),
        interrupted: stop.load(Ordering::Relaxed),
        artifacts,
        elapsed: started.elapsed(),
    }
}

struct Measurement {
    size_bytes: u64,
    last_modified: Option<SystemTime>,
}

/// Sums file sizes and picks the newest mtime inside one artifact.
/// Permission errors on individual entries are skipped, not fatal.
fn measure_dir(path: &Path, stop: &AtomicBool, counter: &AtomicU64) -> Measurement {
    let mut result = Measurement {
        size_bytes: 0,
        last_modified: None,
    };
    for entry in WalkDir::new(path).follow_links(false) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entry) = entry else { continue };
        // Symlinks stay out of the numbers on purpose, per spec §2.4.
        if entry.file_type().is_symlink() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if entry.file_type().is_file() {
            result.size_bytes = result.size_bytes.saturating_add(metadata.len());
        }
        if let Ok(modified) = metadata.modified() {
            result.last_modified = Some(
                result
                    .last_modified
                    .map_or(modified, |old| old.max(modified)),
            );
        }
    }
    counter.fetch_add(1, Ordering::Relaxed);
    result
}

#[cfg(test)]
mod tests {
    use super::{Exclude, glob_match};

    #[test]
    fn single_star_stays_in_segment() {
        assert!(glob_match(b"*/target/*", b"a/target/bin"));
        // Crossing more than one segment needs `**`, per gitignore semantics.
        assert!(!glob_match(b"*/target/*", b"a/b/target/c"));
        assert!(!glob_match(b"*/target/*", b"a/target/b/c"));
    }

    #[test]
    fn double_star_crosses_segments() {
        assert!(glob_match(
            b"**/node_modules/**",
            b"c:/a/b/node_modules/x/y"
        ));
        assert!(glob_match(b"**/node_modules/**", b"x/node_modules/y"));
        assert!(!glob_match(b"**/target/**", b"/tolerate/targetish/x"));
    }

    #[test]
    fn excludes_cover_root_and_nested_hits() {
        // gitignore convention: leading "**/" matches at the very root too.
        let e = Exclude::new("**/target/**");
        assert!(e.matches(std::path::Path::new("target/bin")), "root hit");
        assert!(e.matches(std::path::Path::new("D:/a/b/target/bin/deep")));
        assert!(e.matches(std::path::Path::new("D:/a/target/x")));
        assert!(!e.matches(std::path::Path::new("D:/dev/other/target")));
    }

    #[test]
    fn question_mark_never_crosses_slash() {
        assert!(glob_match(b"a?c", b"abc"));
        assert!(!glob_match(b"a?c", b"a/c"));
    }

    #[test]
    fn plain_patterns_are_substrings() {
        let e = Exclude::new("ml-test");
        assert!(e.matches(std::path::Path::new("D:/dev/ml-test/target")));
        assert!(!e.matches(std::path::Path::new("D:/dev/other/target")));

        let e = Exclude::new("**/ml-test/**");
        assert!(e.matches(std::path::Path::new("D:/dev/ml-test/target")));
        assert!(!e.matches(std::path::Path::new("D:/dev/ml-testx/target")));
    }

    #[test]
    fn matching_is_case_insensitive_and_separator_agnostic() {
        let e = Exclude::new("**\\OLD-PROJ\\**");
        assert!(e.matches(std::path::Path::new("d:/dev/old-proj/obj/x")));
        assert!(!e.matches(std::path::Path::new("d:/dev/new-proj/obj/x")));
    }
}
