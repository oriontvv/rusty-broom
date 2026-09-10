//! Minimal glob support for marker and artifact patterns.
//!
//! A pattern is a `/`-separated path relative to the project root. Each segment
//! may use `*` and `?`; a segment that is exactly `**` matches any number of
//! intermediate directories.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// How deep a `**` segment is allowed to expand.
const RECURSIVE_DEPTH_LIMIT: usize = 12;
/// Directories never entered while expanding `**`.
const RECURSIVE_PRUNE: [&str; 5] = [".git", ".hg", ".svn", "node_modules", ".venv"];

/// Wildcard match of a single path component against `pattern`.
pub fn matches(pattern: &str, name: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = name.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // Position of the last `*` in the pattern and where it resumed in the text,
    // so that a failed match can backtrack instead of recursing.
    let mut star: Option<usize> = None;
    let mut resume = 0usize;

    while ti < text.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = Some(pi);
            pi += 1;
            resume = ti;
        } else if let Some(s) = star {
            pi = s + 1;
            resume += 1;
            ti = resume;
        } else {
            return false;
        }
    }
    pat[pi..].iter().all(|c| *c == '*')
}

pub fn has_wildcard(segment: &str) -> bool {
    segment.contains('*') || segment.contains('?')
}

/// Expand `pattern` against `root` and return the paths that exist.
///
/// Patterns escaping the root (absolute or containing `..`) are rejected and
/// yield nothing.
pub fn resolve(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let segments: Vec<&str> = pattern
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if segments.is_empty()
        || segments.contains(&"..")
        || pattern.starts_with('/')
        || pattern.starts_with('~')
    {
        return Vec::new();
    }

    let mut current = vec![root.to_path_buf()];
    for (index, segment) in segments.iter().enumerate() {
        let is_last = index + 1 == segments.len();
        let mut next: Vec<PathBuf> = Vec::new();

        if *segment == "**" {
            for base in &current {
                next.push(base.clone());
                collect_dirs(base, 1, &mut next);
            }
        } else if has_wildcard(segment) {
            for base in &current {
                let Ok(entries) = fs::read_dir(base) else {
                    continue;
                };
                for entry in entries.flatten() {
                    if matches(segment, &entry.file_name().to_string_lossy()) {
                        next.push(entry.path());
                    }
                }
            }
        } else {
            for base in &current {
                let candidate = base.join(segment);
                if candidate.symlink_metadata().is_ok() {
                    next.push(candidate);
                }
            }
        }

        if !is_last {
            next.retain(|p| p.is_dir());
        }
        if next.is_empty() {
            return Vec::new();
        }
        current = next;
    }

    current.sort();
    current.dedup();
    current
}

/// True when at least one path matching `pattern` exists.
pub fn exists(root: &Path, pattern: &str) -> bool {
    !resolve(root, pattern).is_empty()
}

fn collect_dirs(base: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > RECURSIVE_DEPTH_LIMIT {
        return;
    }
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if RECURSIVE_PRUNE.contains(&name.as_ref()) {
            continue;
        }
        // `file_type` comes from the directory entry and does not follow links,
        // so symlinked trees are not traversed.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            let path = entry.path();
            collect_dirs(&path, depth + 1, out);
            out.push(path);
        }
    }
}

/// Drop paths that live inside another path of the same set, so that overlapping
/// patterns (`dist` and `*/dist`, `**/__pycache__`) are not counted twice.
pub fn drop_nested(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    let kept: HashSet<PathBuf> = paths.iter().cloned().collect();
    paths
        .into_iter()
        .filter(|path| !path.ancestors().skip(1).any(|a| kept.contains(a)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(matches("target", "target"));
        assert!(!matches("target", "targets"));
        assert!(matches("*.egg-info", "rusty.egg-info"));
        assert!(matches("cmake-build-*", "cmake-build-debug"));
        assert!(matches("*", "anything"));
        assert!(matches("*.tar.gz", "a.tar.gz"));
        assert!(!matches("*.tar.gz", "a.tar.bz2"));
        assert!(matches("?ar", "bar"));
        assert!(!matches("?ar", "bbar"));
        assert!(matches("a*b*c", "azzbzzc"));
        assert!(!matches("a*b*c", "azzbzz"));
    }

    #[test]
    fn drops_nested_paths() {
        let paths = vec![
            PathBuf::from("/p/dist"),
            PathBuf::from("/p/dist/inner"),
            PathBuf::from("/p/build"),
        ];
        let kept = drop_nested(paths);
        assert_eq!(
            kept,
            vec![PathBuf::from("/p/build"), PathBuf::from("/p/dist")]
        );
    }

    #[test]
    fn rejects_escaping_patterns() {
        let root = Path::new("/tmp");
        assert!(resolve(root, "../etc").is_empty());
        assert!(resolve(root, "/etc").is_empty());
        assert!(resolve(root, "").is_empty());
    }
}
