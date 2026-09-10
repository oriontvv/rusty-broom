//! Asking git which of our candidate paths it actually ignores.
//!
//! This is the safety net that gives the tool its meaning: only paths git
//! reports as ignored are ever deleted. `git check-ignore` without
//! `--no-index` deliberately does not report tracked paths, so a committed
//! `dist/` or `vendor/` survives even though the configuration lists it.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::project::GitStatus;

/// Split `candidates` into the paths git ignores and the rest.
///
/// Returns [`GitStatus::NoRepo`] / [`GitStatus::Unavailable`] together with all
/// candidates untouched when the question cannot be answered.
pub fn partition_ignored(
    root: &Path,
    candidates: Vec<PathBuf>,
) -> (Vec<PathBuf>, Vec<PathBuf>, GitStatus) {
    if candidates.is_empty() {
        return (Vec::new(), Vec::new(), GitStatus::Ignored);
    }

    let relative: Vec<String> = candidates
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    match run_check_ignore(root, &relative) {
        Ok(ignored) => {
            let (keep, dropped): (Vec<_>, Vec<_>) = candidates
                .into_iter()
                .zip(&relative)
                .partition(|(_, rel)| ignored.contains(rel.as_str()));
            (
                keep.into_iter().map(|(p, _)| p).collect(),
                dropped.into_iter().map(|(p, _)| p).collect(),
                GitStatus::Ignored,
            )
        }
        Err(status) => (candidates, Vec::new(), status),
    }
}

/// Names, relative to `root`, that git considers ignored.
fn run_check_ignore(root: &Path, relative: &[String]) -> Result<HashSet<String>, GitStatus> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| GitStatus::Unavailable)?;

    {
        let mut stdin = child.stdin.take().ok_or(GitStatus::Unavailable)?;
        // The candidate list is a handful of paths per project, well below any
        // pipe buffer, so writing before reading cannot deadlock here.
        for path in relative {
            if stdin.write_all(path.as_bytes()).is_err() || stdin.write_all(&[0]).is_err() {
                break;
            }
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|_| GitStatus::Unavailable)?;
    match output.status.code() {
        // 0: some paths are ignored, 1: none of them are.
        Some(0) | Some(1) => {}
        // 128 is "not a git repository" and anything else is a git we cannot
        // reason about; both mean we have no verdict.
        Some(128) => return Err(GitStatus::NoRepo),
        _ => return Err(GitStatus::Unavailable),
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(normalise)
        .collect())
}

/// git echoes the path as given; strip a trailing slash so comparisons match.
fn normalise(path: &str) -> String {
    path.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_candidates_is_trivially_ignored() {
        let (keep, dropped, status) = partition_ignored(Path::new("/tmp"), Vec::new());
        assert!(keep.is_empty() && dropped.is_empty());
        assert_eq!(status, GitStatus::Ignored);
    }

    #[test]
    fn outside_a_repo_keeps_everything() {
        let dir = std::env::temp_dir();
        let candidates = vec![dir.join("target")];
        let (keep, dropped, status) = partition_ignored(&dir, candidates.clone());
        // Either the temp dir is not a repo, or git is missing; both keep all.
        if status != GitStatus::Ignored {
            assert_eq!(keep, candidates);
            assert!(dropped.is_empty());
        }
    }
}
