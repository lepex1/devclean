//! Safe deletion of artifact directories.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, anyhow};
use log::{debug, error, info, warn};
use walkdir::WalkDir;

use crate::models::Artifact;

/// Result of removing a batch of artifacts.
#[derive(Debug, Default)]
pub struct RemovalOutcome {
    pub freed_bytes: u64,
    pub removed: usize,
    pub failed: Vec<(PathBuf, anyhow::Error)>,
}

/// Removes artifact directories one by one. An interrupt request stops the
/// batch between items; single-item failures never abort the whole run.
pub fn remove_artifacts(artifacts: &[Artifact], stop: &AtomicBool) -> RemovalOutcome {
    info!("removal batch: {} target(s)", artifacts.len());
    let mut outcome = RemovalOutcome::default();
    // shortcut: progress is per artifact, not per file; a huge single tree
    // still looks quiet — upgrade to a byte-level bar if that ever matters.
    for (index, artifact) in artifacts.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            warn!(
                "removal interrupted before {} ({} left untouched)",
                artifact.path.display(),
                artifacts.len() - outcome.removed - outcome.failed.len()
            );
            break;
        }
        let size_bytes = artifact.size_bytes;
        println!(
            "[{}/{}] removing {}",
            index + 1,
            artifacts.len(),
            artifact.path.display()
        );
        debug!("removing {}", artifact.path.display());
        match remove_tree(&artifact.path) {
            Ok(()) => {
                info!("removed {} ({} bytes)", artifact.path.display(), size_bytes);
                outcome.freed_bytes += size_bytes;
                outcome.removed += 1;
            }
            Err(err) => {
                error!("failed to remove {}: {err:#}", artifact.path.display());
                outcome.failed.push((artifact.path.clone(), err));
            }
        }
    }
    info!(
        "removal done: {} ok, {} failed, {} bytes freed",
        outcome.removed,
        outcome.failed.len(),
        outcome.freed_bytes
    );
    outcome
}

/// Removes one artifact tree. The path gets canonicalized first so Windows
/// absolute paths become `\\?\`-verbatim and long paths keep working.
///
/// Links are the exception: canonicalizing one would resolve to its target,
/// and `remove_dir_all` would then wipe the target instead of the link.
fn remove_tree(path: &Path) -> anyhow::Result<()> {
    if is_link_like(path) {
        debug!(
            "removing the link itself, not its target: {}",
            path.display()
        );
        return remove_link(path);
    }
    let real = fs::canonicalize(path).with_context(|| format!("resolving '{}'", path.display()))?;
    clear_readonly_attributes(&real);
    remove_with_retry(&real)
}

/// True for symlinks and Windows reparse points (junctions included), which
/// must never be followed when deleting.
fn is_link_like(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Deletes the link entry only; the target stays untouched.
fn remove_link(path: &Path) -> anyhow::Result<()> {
    let context = || format!("removing link '{}'", path.display());
    #[cfg(windows)]
    {
        // Directory links/junctions need remove_dir; file links need remove_file.
        fs::remove_dir(path)
            .or_else(|_| fs::remove_file(path))
            .with_context(context)
    }
    #[cfg(not(windows))]
    {
        fs::remove_file(path).with_context(context)
    }
}

/// Retries a few times around transient Windows locks (AV scanners and
/// running toolchains hold handles on hot build directories).
fn remove_with_retry(path: &Path) -> anyhow::Result<()> {
    const ATTEMPTS: usize = 3;
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(100));
        }
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(err) => {
                warn!(
                    "remove attempt {}/{} on {}: {err}",
                    attempt + 1,
                    ATTEMPTS,
                    path.display()
                );
                last_err = Some(err);
            }
        }
    }
    let base = last_err.ok_or_else(|| anyhow!("removal never reached the filesystem"))?;
    Err(anyhow::Error::from(base)).with_context(|| format!("removing '{}'", path.display()))
}

/// Windows: readonly attributes block deletion, so clear them tree-wide.
/// Best-effort: any leftover problem surfaces as a removal error afterwards.
// set_readonly(false) is exactly the tool's job on Windows (attribute clear);
// the clippy concern (world-writable files) is Unix-specific.
#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn clear_readonly_attributes(root: &Path) {
    for entry in WalkDir::new(root).follow_links(false) {
        let Ok(entry) = entry else { continue };
        // Touching a symlink's target permissions from here is wrong; skip.
        if entry.file_type().is_symlink() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let perms = metadata.permissions();
        if perms.readonly() {
            debug!("clearing readonly flag on {}", entry.path().display());
            let mut writable = perms.clone();
            writable.set_readonly(false);
            let _ = fs::set_permissions(entry.path(), writable);
        }
    }
}

#[cfg(not(windows))]
fn clear_readonly_attributes(_root: &Path) {}

#[cfg(test)]
mod tests {
    use super::remove_tree;

    #[cfg(unix)]
    #[test]
    fn symlinked_artifact_is_unlinked_without_touching_the_target() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let target = dir.path().join("real-target");
        std::fs::create_dir_all(&target).expect("create target");
        std::fs::write(target.join("keep.txt"), "keep").expect("write file");
        let link = dir.path().join("node_modules");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        remove_tree(&link).expect("remove link");

        assert!(!link.exists(), "the link is gone");
        assert!(target.join("keep.txt").exists(), "the target survived");
    }

    #[cfg(windows)]
    #[test]
    fn junctioned_artifact_is_unlinked_without_touching_the_target() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let target = dir.path().join("real-target");
        std::fs::create_dir_all(&target).expect("create target");
        std::fs::write(target.join("keep.txt"), "keep").expect("write file");
        let link = dir.path().join("node_modules");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .status()
            .expect("run mklink");
        assert!(status.success(), "mklink /J works without admin rights");
        assert!(link.exists(), "the junction resolves before deletion");

        remove_tree(&link).expect("remove junction");

        assert!(!link.exists(), "the junction is gone");
        assert!(target.join("keep.txt").exists(), "the target survived");
    }
}
