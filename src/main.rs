//! rusty-broom — reclaim disk space from stale projects.

mod clean;
mod cli;
mod config;
mod engine;
mod fmt;
mod gitignore;
mod glob;
mod humantime;
mod project;
mod report;
mod scan;
mod size;
mod store;
mod tui;

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::cli::{CleanArgs, Cli, Command, ConfigAction, ScanArgs, Selection};
use crate::config::{Config, Settings};
use crate::engine::{Engine, Event};
use crate::project::{Project, ProjectId, Sort};
use crate::store::Store;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let (config, config_path) = Config::load(cli.config.as_deref())?;

    match cli.command {
        None => run_tui(Selection::with_roots(cli.roots), &config),
        Some(Command::Tui(args)) => run_tui(args.selection, &config),
        Some(Command::Scan(args)) => run_scan(args, &config),
        Some(Command::Clean(args)) => run_clean(args, &config),
        Some(Command::Types) => report::types(&config),
        Some(Command::Config { action }) => run_config(action, &config, config_path),
    }
}

/// Settings with the command-line overrides applied.
fn settings_for(config: &Config, selection: &Selection) -> Settings {
    let mut settings = config.settings.clone();
    if selection.no_git_check {
        settings.require_git_ignored = false;
    }
    settings
}

fn run_tui(selection: Selection, config: &Config) -> Result<()> {
    let options = selection.scan_options(config)?;
    let settings = settings_for(config, &selection);
    let filter = selection.filter(config)?;
    tui::run(options, settings, filter)
}

fn run_scan(args: ScanArgs, config: &Config) -> Result<()> {
    let filter = args.selection.filter(config)?;
    let store = collect(&args.selection, config, !args.json)?;

    let mut shown: Vec<&Project> = store.all().filter(|p| filter.accepts(p)).collect();
    Sort::from(args.sort).apply(&mut shown);
    if let Some(limit) = args.limit {
        shown.truncate(limit);
    }

    if args.json {
        report::json(&shown, &store)
    } else {
        report::table(&shown, args.long)?;
        report::summary(&store, &shown)
    }
}

fn run_clean(args: CleanArgs, config: &Config) -> Result<()> {
    let filter = args.selection.filter(config)?;
    let options = args.selection.scan_options(config)?;
    let settings = settings_for(config, &args.selection);

    let mut store = Store::new();
    let engine = Engine::start(options, &settings);
    let progress = io::stderr().is_terminal();
    engine.run_to_completion(|event| apply_with_progress(&mut store, event, progress));
    clear_progress(progress);

    let mut targets: Vec<&Project> = store
        .all()
        .filter(|p| filter.accepts(p) && p.is_cleanable())
        .collect();
    Sort::Size.apply(&mut targets);

    if targets.is_empty() {
        println!("Nothing to clean.");
        return Ok(());
    }

    report::table(&targets, true)?;
    report::summary(&store, &targets)?;
    println!();

    let total: u64 = targets.iter().map(|p| p.size()).sum();
    let plan: Vec<(ProjectId, PathBuf, Vec<PathBuf>)> = targets
        .iter()
        .map(|p| (p.id, p.root.clone(), p.artifact_paths()))
        .collect();
    drop(targets);

    if args.dry_run || !confirm(&args, plan.len(), total)? {
        println!("Nothing was deleted.");
        return Ok(());
    }

    for (id, root, paths) in plan {
        engine.submit_clean(id, root, paths);
    }
    let mut failures = 0usize;
    engine.run_to_completion(|event| {
        if let Event::CleanFinished { errors, .. } = &event {
            for error in errors {
                eprintln!("{error}");
            }
            failures += usize::from(!errors.is_empty());
        }
        store.apply(event);
    });

    println!("Freed {}.", fmt::bytes(store.freed));
    if failures > 0 {
        bail!("{failures} project(s) could not be fully cleaned");
    }
    Ok(())
}

/// `--dry-run` and `--yes` short-circuit; otherwise ask, and refuse to guess
/// when there is no terminal to ask on.
fn confirm(args: &CleanArgs, projects: usize, bytes: u64) -> Result<bool> {
    if args.yes {
        return Ok(true);
    }
    if !io::stdin().is_terminal() {
        bail!("refusing to delete without confirmation; pass --yes or use --dry-run");
    }
    print!(
        "Delete artifacts of {projects} project(s), freeing about {}? [y/N] ",
        fmt::bytes(bytes)
    );
    io::stdout().flush()?;

    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("cannot read confirmation")?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// Run a full scan and return the accumulated state.
fn collect(selection: &Selection, config: &Config, progress: bool) -> Result<Store> {
    let options = selection.scan_options(config)?;
    let settings = settings_for(config, selection);
    let progress = progress && io::stderr().is_terminal();

    let mut store = Store::new();
    let engine = Engine::start(options, &settings);
    engine.run_to_completion(|event| apply_with_progress(&mut store, event, progress));
    clear_progress(progress);
    Ok(store)
}

fn apply_with_progress(store: &mut Store, event: Event, progress: bool) {
    let show = progress && matches!(event, Event::Progress { .. });
    store.apply(event);
    if show {
        // Single rewritten line so the report is not buried in scroll-back.
        eprint!(
            "\r\x1b[2Kscanning… {} dirs, {} projects",
            store.dirs_scanned,
            store.all().count()
        );
        let _ = io::stderr().flush();
    }
}

fn clear_progress(progress: bool) {
    if progress {
        eprint!("\r\x1b[2K");
        let _ = io::stderr().flush();
    }
}

fn run_config(action: ConfigAction, config: &Config, path: Option<PathBuf>) -> Result<()> {
    match action {
        ConfigAction::Path => match path {
            Some(path) => println!("{}", path.display()),
            None => {
                println!("(built-in defaults)");
                if let Some(target) = config::user_config_path() {
                    println!(
                        "run `rusty-broom config init` to create {}",
                        target.display()
                    );
                }
            }
        },
        ConfigAction::Show => print!("{}", toml::to_string_pretty(config)?),
        ConfigAction::Init { force } => {
            let target = config::user_config_path()
                .context("cannot determine the user configuration directory")?;
            if target.exists() && !force {
                bail!(
                    "{} already exists; pass --force to overwrite",
                    target.display()
                );
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            std::fs::write(&target, config::DEFAULT_CONFIG)
                .with_context(|| format!("cannot write {}", target.display()))?;
            println!("Wrote {}", target.display());
        }
    }
    Ok(())
}
