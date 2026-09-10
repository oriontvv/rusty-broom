//! The data model shared by the CLI and the TUI.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::humantime;
use crate::size::DirStat;

pub type ProjectId = usize;

/// A directory recognised as a project, together with the artifacts that may be
/// removed from it.
#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub id: ProjectId,
    pub root: PathBuf,
    /// Root this project was discovered under, used to shorten paths for display.
    pub scan_root: PathBuf,
    /// Every matching type name, most specific first.
    pub types: Vec<String>,
    pub artifacts: Vec<Artifact>,
    /// Newest modification time among the project's own (non-artifact) files.
    #[serde(serialize_with = "ser_time")]
    pub last_activity: Option<SystemTime>,
    pub source_files: u64,
    pub git: GitStatus,
    /// False until the artifact list has been resolved by a worker.
    pub resolved: bool,
    pub state: CleanState,
}

#[derive(Debug, Clone, Serialize)]
pub struct Artifact {
    pub path: PathBuf,
    /// Pattern from the configuration that produced this path.
    pub pattern: String,
    pub stat: Option<DirStat>,
}

/// Whether git confirmed that the artifacts are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitStatus {
    /// Paths were filtered through `git check-ignore`.
    Ignored,
    /// Not a git work tree — nothing was filtered.
    NoRepo,
    /// Filtering was switched off in the configuration.
    Disabled,
    /// `git` could not be run; nothing was filtered.
    Unavailable,
    /// Not determined yet.
    Pending,
}

impl GitStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ignored => "git-ignored",
            Self::NoRepo => "no-git",
            Self::Disabled => "unchecked",
            Self::Unavailable => "git-error",
            Self::Pending => "…",
        }
    }

    /// True when we could not prove the artifacts are ignored by git.
    pub fn is_unverified(self) -> bool {
        !matches!(self, Self::Ignored)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "state", content = "detail")]
pub enum CleanState {
    Idle,
    Running,
    Done { freed: u64 },
    Failed { errors: Vec<String> },
}

impl Project {
    pub fn new(id: ProjectId, root: PathBuf, scan_root: PathBuf, types: Vec<String>) -> Self {
        Self {
            id,
            root,
            scan_root,
            types,
            artifacts: Vec::new(),
            last_activity: None,
            source_files: 0,
            git: GitStatus::Pending,
            resolved: false,
            state: CleanState::Idle,
        }
    }

    /// Primary type label, e.g. `rust` or `node+rust` for a polyglot project.
    pub fn type_label(&self) -> String {
        match self.types.len() {
            0 => "unknown".to_string(),
            1 => self.types[0].clone(),
            _ => self.types.join("+"),
        }
    }

    /// Path relative to the scan root, which is what the user recognises.
    pub fn display_path(&self) -> String {
        self.root
            .strip_prefix(&self.scan_root)
            .unwrap_or(&self.root)
            .to_string_lossy()
            .into_owned()
    }

    pub fn age(&self) -> Option<Duration> {
        self.last_activity.map(humantime::age_of)
    }

    pub fn age_label(&self) -> String {
        match self.age() {
            Some(age) => humantime::format_age(age),
            None => "unknown".to_string(),
        }
    }

    /// Sum of the artifact sizes measured so far.
    pub fn size(&self) -> u64 {
        self.artifacts
            .iter()
            .filter_map(|a| a.stat.as_ref())
            .map(|s| s.bytes)
            .sum()
    }

    pub fn file_count(&self) -> u64 {
        self.artifacts
            .iter()
            .filter_map(|a| a.stat.as_ref())
            .map(|s| s.files)
            .sum()
    }

    /// True once every artifact has been measured.
    pub fn is_measured(&self) -> bool {
        self.resolved && self.artifacts.iter().all(|a| a.stat.is_some())
    }

    /// Newest modification time across all artifacts — effectively "last built".
    pub fn last_build(&self) -> Option<SystemTime> {
        self.artifacts
            .iter()
            .filter_map(|a| a.stat.as_ref())
            .filter_map(|s| s.newest)
            .max()
    }

    pub fn artifact_paths(&self) -> Vec<PathBuf> {
        self.artifacts.iter().map(|a| a.path.clone()).collect()
    }

    pub fn is_cleanable(&self) -> bool {
        !self.artifacts.is_empty() && self.state == CleanState::Idle
    }
}

/// Which projects to show or act on.
#[derive(Debug, Clone)]
pub struct Filter {
    /// Minimum time since the last source change. `ZERO` disables the check.
    pub older_than: Duration,
    pub min_size: u64,
    /// Case-insensitive substring of the project path.
    pub query: String,
    /// Only keep projects with at least one artifact.
    pub with_artifacts_only: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            older_than: Duration::ZERO,
            min_size: 0,
            query: String::new(),
            with_artifacts_only: true,
        }
    }
}

impl Filter {
    /// Age is known right after discovery, so this can run before sizing.
    pub fn accepts_age(&self, project: &Project) -> bool {
        if self.older_than.is_zero() {
            return true;
        }
        match project.age() {
            Some(age) => age >= self.older_than,
            // Without a timestamp we cannot prove the project is stale; keeping
            // it out of the list is the conservative choice.
            None => false,
        }
    }

    pub fn accepts(&self, project: &Project) -> bool {
        if !self.accepts_age(project) {
            return false;
        }
        if self.with_artifacts_only && project.resolved && project.artifacts.is_empty() {
            return false;
        }
        // An unmeasured project is kept: its size is not known yet and hiding
        // it would make the list jump around while sizing runs.
        if self.min_size > 0 && project.is_measured() && project.size() < self.min_size {
            return false;
        }
        if !self.query.is_empty() {
            let needle = self.query.to_lowercase();
            let haystack = project.root.to_string_lossy().to_lowercase();
            if !haystack.contains(&needle) && !project.type_label().contains(&needle) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Size,
    Age,
    Path,
    Type,
}

impl Sort {
    pub const ALL: [Sort; 4] = [Sort::Size, Sort::Age, Sort::Path, Sort::Type];

    pub fn label(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Age => "age",
            Self::Path => "path",
            Self::Type => "type",
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    /// Order `projects` in place. Largest / oldest first, since that is what the
    /// user is looking for.
    pub fn apply(self, projects: &mut [&Project]) {
        match self {
            Self::Size => {
                projects.sort_by(|a, b| b.size().cmp(&a.size()).then_with(|| a.root.cmp(&b.root)))
            }
            Self::Age => projects.sort_by(|a, b| {
                // Missing timestamps sort last.
                let key = |p: &Project| p.last_activity.unwrap_or(SystemTime::UNIX_EPOCH);
                key(a).cmp(&key(b)).then_with(|| a.root.cmp(&b.root))
            }),
            Self::Path => projects.sort_by(|a, b| a.root.cmp(&b.root)),
            Self::Type => projects.sort_by(|a, b| {
                a.type_label()
                    .cmp(&b.type_label())
                    .then_with(|| b.size().cmp(&a.size()))
            }),
        }
    }
}

fn ser_time<S: serde::Serializer>(
    value: &Option<SystemTime>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // Unix seconds keep the JSON output easy to post-process with jq.
    let secs = value.and_then(|t| {
        t.duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs())
    });
    match secs {
        Some(s) => serializer.serialize_some(&s),
        None => serializer.serialize_none(),
    }
}

/// Shorten a path for display by replacing the home prefix with `~`.
pub fn tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}
