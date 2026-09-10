//! Command-line surface.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::config::Config;
use crate::fmt;
use crate::humantime;
use crate::project::{Filter, Sort};
use crate::scan::ScanOptions;

#[derive(Debug, Parser)]
#[command(
    name = "rusty-broom",
    version,
    about = "Reclaim disk space from build artifacts of projects you stopped working on",
    long_about = "rusty-broom finds projects under one or more roots, works out how long \
                  ago each was touched, and removes the build artifacts and dependency \
                  directories of the stale ones.\n\n\
                  Only paths that git itself ignores are ever deleted, so tracked files \
                  are never at risk. Run without a subcommand to open the interactive UI.",
    after_help = "Examples:\n  \
                  rusty-broom ~/dev                      interactive UI over ~/dev\n  \
                  rusty-broom scan ~/dev -o 6mo          list projects idle for half a year\n  \
                  rusty-broom clean ~/dev -o 1y -m 500M  delete the big, year-old ones\n  \
                  rusty-broom types                      show the project catalogue"
)]
pub struct Cli {
    /// Configuration file to use instead of the discovered one.
    #[arg(short, long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,

    /// Roots for the interactive UI when no subcommand is given.
    #[arg(value_name = "ROOT")]
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List matching projects and what could be reclaimed.
    Scan(ScanArgs),
    /// Delete the artifacts of matching projects.
    Clean(CleanArgs),
    /// Open the interactive interface.
    Tui(TuiArgs),
    /// Show the project types the configuration knows about.
    Types,
    /// Inspect or create the configuration file.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Write the default configuration to the user config path.
    Init {
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Print the path of the configuration in use.
    Path,
    /// Print the effective configuration.
    Show,
}

/// Options shared by every command that walks the disk.
#[derive(Debug, Args, Clone)]
pub struct Selection {
    /// Directories to search (default: the current directory).
    #[arg(value_name = "ROOT")]
    pub roots: Vec<PathBuf>,

    /// Only projects untouched for at least this long: 30d, 6mo, 1y, 0 for any.
    #[arg(short = 'o', long, value_name = "DURATION")]
    pub older_than: Option<String>,

    /// Only projects with at least this much to reclaim: 500M, 2G.
    #[arg(short = 'm', long, value_name = "SIZE")]
    pub min_size: Option<String>,

    /// Restrict detection to these project types (repeatable).
    #[arg(short = 't', long = "type", value_name = "NAME")]
    pub types: Vec<String>,

    /// Only projects whose path contains this text.
    #[arg(short = 'q', long, value_name = "TEXT")]
    pub query: Option<String>,

    /// How deep to descend from each root.
    #[arg(long, value_name = "N")]
    pub max_depth: Option<usize>,

    /// Ignore the age threshold entirely.
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Also report nested projects, such as workspace members.
    #[arg(long)]
    pub nested: bool,

    /// Delete artifacts even when git does not confirm they are ignored.
    #[arg(long)]
    pub no_git_check: bool,
}

#[derive(Debug, Args)]
pub struct ScanArgs {
    #[command(flatten)]
    pub selection: Selection,

    /// Order of the report.
    #[arg(short = 's', long, value_enum, default_value_t = SortArg::Size)]
    pub sort: SortArg,

    /// Show at most this many projects.
    #[arg(short = 'n', long, value_name = "N")]
    pub limit: Option<usize>,

    /// Print the artifact paths of every project.
    #[arg(short = 'l', long)]
    pub long: bool,

    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    #[command(flatten)]
    pub selection: Selection,

    /// Delete without asking.
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Only report what would be deleted. Implied unless --yes is given.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct TuiArgs {
    #[command(flatten)]
    pub selection: Selection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortArg {
    Size,
    Age,
    Path,
    Type,
}

impl From<SortArg> for Sort {
    fn from(value: SortArg) -> Self {
        match value {
            SortArg::Size => Sort::Size,
            SortArg::Age => Sort::Age,
            SortArg::Path => Sort::Path,
            SortArg::Type => Sort::Type,
        }
    }
}

impl Selection {
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            older_than: None,
            min_size: None,
            types: Vec::new(),
            query: None,
            max_depth: None,
            all: false,
            nested: false,
            no_git_check: false,
        }
    }

    pub fn roots_or_cwd(&self) -> Result<Vec<PathBuf>> {
        if self.roots.is_empty() {
            Ok(vec![std::env::current_dir()?])
        } else {
            Ok(self.roots.clone())
        }
    }

    /// Age threshold: `--all` wins, then `--older-than`, then the config value.
    pub fn older_than(&self, config: &Config) -> Result<Duration> {
        if self.all {
            return Ok(Duration::ZERO);
        }
        match &self.older_than {
            Some(text) => humantime::parse_duration(text),
            None => config.default_older_than(),
        }
    }

    pub fn filter(&self, config: &Config) -> Result<Filter> {
        Ok(Filter {
            older_than: self.older_than(config)?,
            min_size: match &self.min_size {
                Some(text) => fmt::parse_bytes(text)?,
                None => 0,
            },
            query: self.query.clone().unwrap_or_default(),
            with_artifacts_only: true,
        })
    }

    pub fn scan_options(&self, config: &Config) -> Result<ScanOptions> {
        config.validate_type_names(&self.types)?;
        let mut options =
            ScanOptions::from_config(config, self.roots_or_cwd()?, &self.types, self.max_depth);
        if self.nested {
            options.descend_into_projects = true;
        }
        Ok(options)
    }
}
