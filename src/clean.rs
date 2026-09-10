//! Deleting artifacts, with the guard rails that make that acceptable.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::size;

#[derive(Debug, Default)]
pub struct Outcome {
    pub freed: u64,
    pub removed: Vec<PathBuf>,
    pub errors: Vec<String>,
}

/// Reasons a target is refused before any deletion happens.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    RootTooShallow,
    RootIsHome,
    NotInsideRoot,
    IsRoot,
}

impl Refusal {
    pub fn message(&self, target: &Path) -> String {
        let path = target.display();
        match self {
            Self::RootTooShallow => format!(
                "refusing to touch {path}: project root is too close to the filesystem root"
            ),
            Self::RootIsHome => {
                format!("refusing to touch {path}: project root is the home directory")
            }
            Self::NotInsideRoot => format!("refusing to delete {path}: outside its project root"),
            Self::IsRoot => format!("refusing to delete {path}: it is the project root itself"),
        }
    }
}

/// Verify that `target` is a sane thing to delete for project `root`.
///
/// The checks are deliberately paranoid: a bad configuration entry or a crafted
/// symlink must not turn this tool into `rm -rf ~`.
pub fn check(root: &Path, target: &Path) -> Result<(), Refusal> {
    let depth = root
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if depth < 2 {
        return Err(Refusal::RootTooShallow);
    }
    if dirs::home_dir().is_some_and(|home| home == root) {
        return Err(Refusal::RootIsHome);
    }
    if target == root {
        return Err(Refusal::IsRoot);
    }
    // Both paths come from the scanner, which canonicalises roots and builds
    // targets by joining plain components, so a prefix test is sufficient.
    if !target.starts_with(root) || target.components().any(|c| c == Component::ParentDir) {
        return Err(Refusal::NotInsideRoot);
    }
    Ok(())
}

/// Delete every path, collecting what was freed and what failed.
///
/// A missing path is not an error: the artifact may have been removed since the
/// scan, and the goal is idempotence.
pub fn remove_all(root: &Path, paths: &[PathBuf]) -> Outcome {
    let mut outcome = Outcome::default();
    let no_cancel = Arc::new(AtomicBool::new(false));

    for path in paths {
        if let Err(refusal) = check(root, path) {
            outcome.errors.push(refusal.message(path));
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(path) else {
            continue; // Already gone.
        };

        // Measure immediately before deleting so the reported number reflects
        // what actually left the disk, not a stale scan result.
        let freed = if meta.is_dir() && !meta.is_symlink() {
            size::measure(path, &no_cancel).bytes
        } else {
            0
        };

        let result = if meta.is_dir() && !meta.is_symlink() {
            fs::remove_dir_all(path)
        } else {
            // For a symlink this removes the link, never its target.
            fs::remove_file(path)
        };

        match result {
            Ok(()) => {
                outcome.freed += freed;
                outcome.removed.push(path.clone());
            }
            Err(err) => outcome.errors.push(format!("{}: {err}", path.display())),
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dangerous_targets() {
        let root = Path::new("/Users/someone/dev/app");
        assert_eq!(check(root, root), Err(Refusal::IsRoot));
        assert_eq!(
            check(root, Path::new("/Users/someone/dev/other/target")),
            Err(Refusal::NotInsideRoot)
        );
        assert_eq!(
            check(Path::new("/tmp"), Path::new("/tmp/target")),
            Err(Refusal::RootTooShallow)
        );
        assert!(check(root, Path::new("/Users/someone/dev/app/target")).is_ok());
    }

    #[test]
    fn rejects_parent_traversal() {
        let root = Path::new("/Users/someone/dev/app");
        assert_eq!(
            check(root, Path::new("/Users/someone/dev/app/../secrets")),
            Err(Refusal::NotInsideRoot)
        );
    }

    #[test]
    fn removes_a_directory_and_reports_the_size() {
        let root = std::env::temp_dir().join(format!("rb-clean-{}/proj", std::process::id()));
        let artifact = root.join("target/debug");
        fs::create_dir_all(&artifact).unwrap();
        fs::write(artifact.join("bin"), vec![1u8; 8192]).unwrap();

        let outcome = remove_all(&root, &[root.join("target")]);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert!(outcome.freed >= 8192, "freed {}", outcome.freed);
        assert!(!root.join("target").exists());

        // Deleting again is a no-op rather than a failure.
        let again = remove_all(&root, &[root.join("target")]);
        assert!(again.errors.is_empty());
        assert_eq!(again.freed, 0);

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }
}
