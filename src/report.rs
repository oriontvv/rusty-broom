//! Non-interactive output.

use std::io::{self, Write};

use anyhow::Result;

use crate::config::Config;
use crate::fmt;
use crate::project::{GitStatus, Project, tilde};
use crate::store::Store;

/// Print the project table. `long` adds one line per artifact path.
pub fn table(projects: &[&Project], long: bool) -> Result<()> {
    let out = io::stdout();
    let mut out = out.lock();

    if projects.is_empty() {
        writeln!(out, "No matching projects.")?;
        return Ok(());
    }

    let type_width = projects
        .iter()
        .map(|p| p.type_label().chars().count())
        .chain(std::iter::once(4))
        .max()
        .unwrap_or(4)
        .min(20);
    let age_width = projects
        .iter()
        .map(|p| p.age_label().chars().count())
        .chain(std::iter::once(3))
        .max()
        .unwrap_or(9);

    writeln!(
        out,
        "{:<type_width$}  {:>9}  {:<age_width$}  PROJECT",
        "TYPE", "SIZE", "IDLE"
    )?;

    for project in projects {
        let unverified = if project.git.is_unverified() {
            format!("  [{}]", project.git.label())
        } else {
            String::new()
        };
        writeln!(
            out,
            "{:<type_width$}  {:>9}  {:<age_width$}  {}{}",
            truncate(&project.type_label(), type_width),
            fmt::bytes(project.size()),
            project.age_label(),
            tilde(&project.root),
            unverified
        )?;
        if long {
            for artifact in &project.artifacts {
                writeln!(
                    out,
                    "{:<type_width$}  {:>9}  {:<age_width$}    {}",
                    "",
                    fmt::maybe_bytes(artifact.stat.as_ref().map(|s| s.bytes)),
                    "",
                    artifact
                        .path
                        .strip_prefix(&project.root)
                        .unwrap_or(&artifact.path)
                        .display()
                )?;
            }
        }
    }
    Ok(())
}

/// One-line totals plus any caveats worth surfacing.
pub fn summary(store: &Store, shown: &[&Project]) -> Result<()> {
    let out = io::stdout();
    let mut out = out.lock();

    let total: u64 = shown.iter().map(|p| p.size()).sum();
    let files: u64 = shown.iter().map(|p| p.file_count()).sum();
    let hidden = store.all().count() - shown.len();

    writeln!(out)?;
    write!(
        out,
        "{} · {} reclaimable · {}",
        plural(shown.len() as u64, "project"),
        fmt::bytes(total),
        plural(files, "file"),
    )?;
    if hidden > 0 {
        write!(out, " · {hidden} filtered out")?;
    }
    writeln!(out)?;

    if store.skipped_paths > 0 {
        writeln!(
            out,
            "{} kept because git does not ignore {}.",
            plural(store.skipped_paths as u64, "path"),
            if store.skipped_paths == 1 {
                "it"
            } else {
                "them"
            }
        )?;
    }
    let unverified = shown
        .iter()
        .filter(|p| p.git.is_unverified() && p.git != GitStatus::Pending)
        .count();
    if unverified > 0 {
        writeln!(
            out,
            "{} could not be verified against git — check {} before cleaning.",
            plural(unverified as u64, "project"),
            if unverified == 1 { "it" } else { "them" }
        )?;
    }
    Ok(())
}

fn plural(count: u64, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

pub fn json(projects: &[&Project], store: &Store) -> Result<()> {
    let payload = serde_json::json!({
        "projects": projects,
        "totals": {
            "projects": projects.len(),
            "bytes": projects.iter().map(|p| p.size()).sum::<u64>(),
            "files": projects.iter().map(|p| p.file_count()).sum::<u64>(),
        },
        "scan": {
            "directories": store.dirs_scanned,
            "skipped_paths": store.skipped_paths,
        },
    });
    let mut out = io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, &payload)?;
    writeln!(out)?;
    Ok(())
}

/// `rusty-broom types`
pub fn types(config: &Config) -> Result<()> {
    let mut out = io::stdout().lock();
    for ty in &config.project_types {
        let state = if ty.enabled { "" } else { "  (disabled)" };
        writeln!(out, "{}{}", ty.name, state)?;
        let mode = if ty.require_all_markers {
            "all of"
        } else {
            "any of"
        };
        writeln!(out, "  markers   {mode} {}", ty.markers.join(", "))?;
        writeln!(out, "  artifacts {}", ty.artifacts.join(", "))?;
        writeln!(out)?;
    }
    Ok(())
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let keep = width.saturating_sub(1);
    format!("{}…", text.chars().take(keep).collect::<String>())
}
