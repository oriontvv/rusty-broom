//! Discovering projects under a set of roots.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use crate::config::{Config, ProjectType};
use crate::glob;

/// Upper bound on the files inspected while dating a single project. Source
/// trees are far smaller than this; the cap only protects against pathological
/// directories that are not really projects.
const MAX_DATED_ENTRIES: u64 = 50_000;

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub roots: Vec<PathBuf>,
    pub max_depth: usize,
    pub skip_hidden: bool,
    pub prune_dirs: HashSet<String>,
    pub descend_into_projects: bool,
    pub types: Vec<ProjectType>,
}

impl ScanOptions {
    /// Build options from the configuration, overriding what the CLI supplied.
    pub fn from_config(
        config: &Config,
        roots: Vec<PathBuf>,
        only_types: &[String],
        max_depth: Option<usize>,
    ) -> Self {
        Self {
            roots,
            max_depth: max_depth.unwrap_or(config.settings.max_depth),
            skip_hidden: config.settings.skip_hidden,
            prune_dirs: config.settings.prune_dirs.iter().cloned().collect(),
            descend_into_projects: config.settings.descend_into_projects,
            types: config.active_types(only_types),
        }
    }
}

/// A project as the walker found it, before git filtering and sizing.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub root: PathBuf,
    pub scan_root: PathBuf,
    pub types: Vec<String>,
    /// `(pattern, path)` pairs for every artifact that exists on disk.
    pub artifacts: Vec<(String, PathBuf)>,
    pub last_activity: Option<SystemTime>,
    pub source_files: u64,
}

/// Walk `options.roots`, invoking `on_project` for each project found and
/// `on_progress` after every directory. Both run on the calling thread.
pub fn walk<P, G>(
    options: &ScanOptions,
    cancel: &Arc<AtomicBool>,
    mut on_project: P,
    mut on_progress: G,
) where
    P: FnMut(Candidate),
    G: FnMut(u64, &Path),
{
    let mut visited = 0u64;
    for root in &options.roots {
        // Canonicalising once keeps `..` out of every path we later display or
        // hand to `git -C`, and resolves a symlinked root deliberately.
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        let mut stack = vec![(root.clone(), 0usize)];

        while let Some((dir, depth)) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            visited += 1;
            on_progress(visited, &dir);

            let Some(entries) = list_dir(&dir) else {
                continue;
            };
            let names: HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();

            let matched = match_types(&dir, &names, &options.types);
            if !matched.is_empty() {
                let candidate = build_candidate(&dir, &root, &matched, options, cancel);
                let descend = options.descend_into_projects;
                on_project(candidate);
                if !descend {
                    continue;
                }
            }

            if depth >= options.max_depth {
                continue;
            }
            for entry in &entries {
                if !entry.is_dir {
                    continue;
                }
                if options.skip_hidden && entry.name.starts_with('.') {
                    continue;
                }
                if options.prune_dirs.contains(&entry.name) {
                    continue;
                }
                stack.push((dir.join(&entry.name), depth + 1));
            }
        }
    }
}

struct Entry {
    name: String,
    is_dir: bool,
}

/// Read a directory once and reuse the listing for detection and descent.
fn list_dir(dir: &Path) -> Option<Vec<Entry>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).ok()?.flatten() {
        // `file_type` on the entry does not follow symlinks, so a symlinked
        // subdirectory is not walked into.
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        out.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
        });
    }
    Some(out)
}

/// Type names whose markers are satisfied by `dir`.
fn match_types(dir: &Path, names: &HashSet<&str>, types: &[ProjectType]) -> Vec<String> {
    types
        .iter()
        .filter(|ty| !ty.markers.is_empty())
        .filter(|ty| {
            let mut present = ty.markers.iter().map(|m| marker_present(dir, names, m));
            if ty.require_all_markers {
                present.all(|found| found)
            } else {
                present.any(|found| found)
            }
        })
        .map(|ty| ty.name.clone())
        .collect()
}

fn marker_present(dir: &Path, names: &HashSet<&str>, marker: &str) -> bool {
    if marker.contains('/') || glob::has_wildcard(marker) {
        glob::exists(dir, marker)
    } else {
        names.contains(marker)
    }
}

fn build_candidate(
    dir: &Path,
    scan_root: &Path,
    matched: &[String],
    options: &ScanOptions,
    cancel: &Arc<AtomicBool>,
) -> Candidate {
    let mut artifacts: Vec<(String, PathBuf)> = Vec::new();
    for ty in options.types.iter().filter(|t| matched.contains(&t.name)) {
        for pattern in &ty.artifacts {
            for path in glob::resolve(dir, pattern) {
                artifacts.push((pattern.clone(), path));
            }
        }
    }

    // Two patterns (or two types) can name the same tree; keep the outermost.
    let kept = glob::drop_nested(artifacts.iter().map(|(_, p)| p.clone()).collect());
    let kept: HashSet<PathBuf> = kept.into_iter().collect();
    artifacts.retain(|(_, path)| kept.contains(path));
    artifacts.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    artifacts.dedup_by(|a, b| a.1 == b.1);

    let (last_activity, source_files) = date_project(dir, &kept, options, cancel);

    Candidate {
        root: dir.to_path_buf(),
        scan_root: scan_root.to_path_buf(),
        types: matched.to_vec(),
        artifacts,
        last_activity,
        source_files,
    }
}

/// Newest modification time among the project's own files.
///
/// Artifacts, `.git` and pruned directories are excluded on purpose: a rebuild
/// or a `git gc` must not make an abandoned project look fresh, whereas editing
/// or pulling source does.
fn date_project(
    dir: &Path,
    artifacts: &HashSet<PathBuf>,
    options: &ScanOptions,
    cancel: &Arc<AtomicBool>,
) -> (Option<SystemTime>, u64) {
    let mut newest: Option<SystemTime> = None;
    let mut files = 0u64;
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        if files >= MAX_DATED_ENTRIES || cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if artifacts.contains(&path) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if name == ".git" || options.prune_dirs.contains(&name) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if file_type.is_symlink() {
                continue;
            }
            files += 1;
            if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                newest = newest.max(Some(modified));
            }
        }
    }

    // A project whose files are all inside artifacts still has a mtime on the
    // root directory itself; better than reporting "unknown".
    if newest.is_none() {
        newest = fs::metadata(dir).and_then(|m| m.modified()).ok();
    }
    (newest, files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rb-scan-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn options(root: &Path) -> ScanOptions {
        let config = Config::embedded();
        ScanOptions::from_config(&config, vec![root.to_path_buf()], &[], Some(4))
    }

    fn collect(options: &ScanOptions) -> Vec<Candidate> {
        let mut found = Vec::new();
        walk(
            options,
            &Arc::new(AtomicBool::new(false)),
            |c| found.push(c),
            |_, _| {},
        );
        found
    }

    #[test]
    fn finds_a_rust_project_and_its_target() {
        let root = temp_root("rust");
        let project = root.join("svc");
        fs::create_dir_all(project.join("target/debug")).unwrap();
        fs::create_dir_all(project.join("src")).unwrap();
        File::create(project.join("Cargo.toml")).unwrap();
        File::create(project.join("src/main.rs")).unwrap();

        let found = collect(&options(&root));
        assert_eq!(found.len(), 1, "{found:#?}");
        let candidate = &found[0];
        assert_eq!(candidate.types, vec!["rust"]);
        assert_eq!(
            candidate
                .artifacts
                .iter()
                .map(|(_, p)| p)
                .collect::<Vec<_>>(),
            vec![&project.join("target")]
        );
        assert!(candidate.last_activity.is_some());
        assert_eq!(candidate.source_files, 2);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reports_both_types_of_a_polyglot_project() {
        let root = temp_root("poly");
        let project = root.join("app");
        fs::create_dir_all(project.join("target")).unwrap();
        fs::create_dir_all(project.join("node_modules")).unwrap();
        File::create(project.join("Cargo.toml")).unwrap();
        File::create(project.join("package.json")).unwrap();

        let found = collect(&options(&root));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].types, vec!["rust", "node"]);
        assert_eq!(found[0].artifacts.len(), 2);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn does_not_descend_into_a_project_by_default() {
        let root = temp_root("nested");
        let outer = root.join("workspace");
        let inner = outer.join("crates/inner");
        fs::create_dir_all(&inner).unwrap();
        File::create(outer.join("Cargo.toml")).unwrap();
        File::create(inner.join("Cargo.toml")).unwrap();

        let found = collect(&options(&root));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, outer);

        let mut nested = options(&root);
        nested.descend_into_projects = true;
        assert_eq!(collect(&nested).len(), 2);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn keeps_the_outermost_of_overlapping_artifacts() {
        let root = temp_root("overlap");
        let project = root.join("mono");
        fs::create_dir_all(project.join("node_modules/left/node_modules")).unwrap();
        File::create(project.join("package.json")).unwrap();

        let found = collect(&options(&root));
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0]
                .artifacts
                .iter()
                .map(|(_, p)| p)
                .collect::<Vec<_>>(),
            vec![&project.join("node_modules")]
        );

        fs::remove_dir_all(&root).unwrap();
    }
}
