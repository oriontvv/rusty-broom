//! Configuration: global settings and the project-type catalogue.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::humantime;

/// The catalogue shipped with the binary.
pub const DEFAULT_CONFIG: &str = include_str!("../assets/default_config.toml");

/// File name looked up in the current directory before falling back to the
/// user-wide config.
pub const PROJECT_CONFIG_NAME: &str = ".rusty-broom.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub project_types: Vec<ProjectType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Default age threshold, e.g. `"30d"`. `"0"` disables the filter.
    #[serde(default = "default_older_than")]
    pub older_than: String,
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    #[serde(default)]
    pub descend_into_projects: bool,
    #[serde(default = "default_true")]
    pub skip_hidden: bool,
    #[serde(default)]
    pub prune_dirs: Vec<String>,
    #[serde(default = "default_true")]
    pub require_git_ignored: bool,
    #[serde(default)]
    pub follow_links: bool,
    #[serde(default)]
    pub workers: usize,
    /// Whether user-defined types are merged with the built-in catalogue.
    #[serde(default = "default_true")]
    pub extend_default_types: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectType {
    pub name: String,
    /// Files or directories that identify the project. Globs and `a/b` paths
    /// are allowed.
    #[serde(default)]
    pub markers: Vec<String>,
    /// Deletable paths relative to the project root. Supports `*`, `?`, `**`.
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Require every marker to be present instead of just one.
    #[serde(default)]
    pub require_all_markers: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}
fn default_older_than() -> String {
    "30d".to_string()
}
fn default_max_depth() -> usize {
    8
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            older_than: default_older_than(),
            max_depth: default_max_depth(),
            descend_into_projects: false,
            skip_hidden: true,
            prune_dirs: Vec::new(),
            require_git_ignored: true,
            follow_links: false,
            workers: 0,
            extend_default_types: true,
        }
    }
}

impl Config {
    /// The built-in configuration. Panics only if the embedded asset is broken,
    /// which is a build-time error.
    pub fn embedded() -> Self {
        toml::from_str(DEFAULT_CONFIG).expect("embedded default config must parse")
    }

    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("invalid configuration")
    }

    /// Load configuration from `explicit`, or the first of
    /// `./.rusty-broom.toml` and the user config, or the built-in defaults.
    pub fn load(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        let path = match explicit {
            Some(p) => {
                if !p.exists() {
                    bail!("config file not found: {}", p.display());
                }
                Some(p.to_path_buf())
            }
            None => discover_config_path(),
        };

        let Some(path) = path else {
            return Ok((Self::embedded(), None));
        };

        let text =
            fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let user = Self::parse(&text).with_context(|| format!("in {}", path.display()))?;
        Ok((user.merged_with_defaults(), Some(path)))
    }

    /// Merge a user configuration on top of the built-in catalogue: types are
    /// matched by name, user entries win, unknown names are appended.
    fn merged_with_defaults(mut self) -> Self {
        if !self.settings.extend_default_types {
            return self;
        }
        let defaults = Self::embedded();
        if self.settings.prune_dirs.is_empty() {
            self.settings.prune_dirs = defaults.settings.prune_dirs.clone();
        }
        let mut types = defaults.project_types;
        for user_type in self.project_types {
            match types.iter_mut().find(|t| t.name == user_type.name) {
                Some(slot) => *slot = user_type,
                None => types.push(user_type),
            }
        }
        self.project_types = types;
        self
    }

    /// The types to detect: everything enabled, or exactly `only` when the user
    /// asked for specific types (an explicit request overrides `enabled`).
    pub fn active_types(&self, only: &[String]) -> Vec<ProjectType> {
        self.project_types
            .iter()
            .filter(|t| {
                if only.is_empty() {
                    t.enabled
                } else {
                    only.iter().any(|n| n == &t.name)
                }
            })
            .cloned()
            .collect()
    }

    pub fn default_older_than(&self) -> Result<Duration> {
        humantime::parse_duration(&self.settings.older_than)
            .with_context(|| format!("settings.older_than = {:?}", self.settings.older_than))
    }

    /// Fail early if the configuration references unknown type names.
    pub fn validate_type_names(&self, names: &[String]) -> Result<()> {
        for name in names {
            if !self.project_types.iter().any(|t| &t.name == name) {
                let known: Vec<&str> = self.project_types.iter().map(|t| t.name.as_str()).collect();
                bail!(
                    "unknown project type {name:?}; known types: {}",
                    known.join(", ")
                );
            }
        }
        Ok(())
    }
}

/// `./.rusty-broom.toml`, then `<config dir>/rusty-broom/config.toml`.
pub fn discover_config_path() -> Option<PathBuf> {
    let local = PathBuf::from(PROJECT_CONFIG_NAME);
    if local.is_file() {
        return Some(local);
    }
    let user = user_config_path()?;
    user.is_file().then_some(user)
}

pub fn user_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rusty-broom").join("config.toml"))
}
