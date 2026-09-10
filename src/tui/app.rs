//! Interactive state: what is shown, what is selected, what the keys do.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use crate::config::Settings;
use crate::engine::Engine;
use crate::humantime;
use crate::project::{CleanState, Filter, Project, ProjectId, Sort};
use crate::scan::ScanOptions;
use crate::store::Store;

/// How long a status message stays on screen.
const STATUS_TTL: Duration = Duration::from_secs(4);

/// Thresholds cycled through with `o` / `O`.
const AGE_PRESETS: [&str; 8] = ["off", "7d", "14d", "30d", "90d", "6mo", "1y", "2y"];

pub enum Mode {
    Normal,
    Search { input: String },
    Confirm { targets: Vec<ProjectId>, bytes: u64 },
    Help,
}

pub struct App {
    pub store: Store,
    pub filter: Filter,
    pub sort: Sort,
    pub mode: Mode,
    pub table: TableState,
    pub marked: HashSet<ProjectId>,
    /// Ids currently displayed, in display order.
    pub view: Vec<ProjectId>,
    pub roots: Vec<PathBuf>,
    pub status: Option<(String, Instant)>,
    pub quit: bool,

    engine: Engine,
    options: ScanOptions,
    settings: Settings,
    /// `(label, threshold)` pairs, ascending.
    ages: Vec<(String, Duration)>,
    age_index: usize,
}

impl App {
    pub fn new(options: ScanOptions, settings: Settings, filter: Filter) -> Self {
        let ages = build_age_presets(filter.older_than);
        let age_index = ages
            .iter()
            .position(|(_, d)| *d == filter.older_than)
            .unwrap_or(0);
        let roots = options.roots.clone();
        let engine = Engine::start(options.clone(), &settings);

        Self {
            store: Store::new(),
            filter,
            sort: Sort::Size,
            mode: Mode::Normal,
            table: TableState::default().with_selected(Some(0)),
            marked: HashSet::new(),
            view: Vec::new(),
            roots,
            status: None,
            quit: false,
            engine,
            options,
            settings,
            ages,
            age_index,
        }
    }

    /// Take everything the pipeline produced and refresh the visible list.
    pub fn pump(&mut self) {
        let events = self.engine.drain();
        let mut cleaned = Vec::new();
        for event in events {
            if let crate::engine::Event::CleanFinished { id, .. } = &event {
                cleaned.push(*id);
            }
            self.store.apply(event);
        }
        for id in cleaned {
            self.marked.remove(&id);
            let failure = match self.store.get(id).map(|p| &p.state) {
                Some(CleanState::Failed { errors }) => errors.first().cloned(),
                _ => None,
            };
            if let Some(error) = failure {
                self.set_status(format!("clean failed: {error}"));
            }
        }
        self.rebuild_view();
        if self
            .status
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > STATUS_TTL)
        {
            self.status = None;
        }
    }

    fn rebuild_view(&mut self) {
        let anchor = self.selected_id();
        let previous = self.table.selected().unwrap_or(0);

        let mut visible: Vec<&Project> = self
            .store
            .all()
            .filter(|p| self.filter.accepts(p))
            .collect();
        self.sort.apply(&mut visible);
        self.view = visible.into_iter().map(|p| p.id).collect();

        let index = anchor
            .and_then(|id| self.view.iter().position(|v| *v == id))
            .unwrap_or_else(|| previous.min(self.view.len().saturating_sub(1)));
        self.table.select((!self.view.is_empty()).then_some(index));
    }

    pub fn selected_id(&self) -> Option<ProjectId> {
        let index = self.table.selected()?;
        self.view.get(index).copied()
    }

    pub fn selected(&self) -> Option<&Project> {
        self.store.get(self.selected_id()?)
    }

    pub fn visible(&self) -> impl Iterator<Item = &Project> {
        self.view.iter().filter_map(|id| self.store.get(*id))
    }

    /// Total reclaimable across the visible projects.
    pub fn visible_bytes(&self) -> u64 {
        self.visible().map(|p| p.size()).sum()
    }

    pub fn marked_bytes(&self) -> u64 {
        self.marked
            .iter()
            .filter_map(|id| self.store.get(*id))
            .map(|p| p.size())
            .sum()
    }

    pub fn age_label(&self) -> &str {
        &self.ages[self.age_index].0
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match &mut self.mode {
            Mode::Help => self.mode = Mode::Normal,
            Mode::Search { input } => match key.code {
                KeyCode::Esc => {
                    self.filter.query.clear();
                    self.mode = Mode::Normal;
                }
                KeyCode::Enter => self.mode = Mode::Normal,
                KeyCode::Backspace => {
                    input.pop();
                    self.filter.query = input.clone();
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.filter.query = input.clone();
                }
                _ => {}
            },
            Mode::Confirm { targets, .. } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    let targets = std::mem::take(targets);
                    self.mode = Mode::Normal;
                    self.start_clean(&targets);
                }
                _ => {
                    self.mode = Mode::Normal;
                    self.set_status("cancelled");
                }
            },
            Mode::Normal => self.on_normal_key(key),
        }
    }

    fn on_normal_key(&mut self, key: KeyEvent) {
        let page = 10usize;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(page as isize),
            KeyCode::PageUp => self.move_by(-(page as isize)),
            KeyCode::Char('g') | KeyCode::Home => self.select_index(0),
            KeyCode::Char('G') | KeyCode::End => {
                self.select_index(self.view.len().saturating_sub(1))
            }
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('a') => self.mark_all(true),
            KeyCode::Char('A') => self.mark_all(false),
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.set_status(format!("sorted by {}", self.sort.label()));
            }
            KeyCode::Char('o') => self.cycle_age(1),
            KeyCode::Char('O') => self.cycle_age(-1),
            KeyCode::Char('/') => {
                self.mode = Mode::Search {
                    input: self.filter.query.clone(),
                }
            }
            KeyCode::Char('v') => {
                self.filter.with_artifacts_only = !self.filter.with_artifacts_only;
                self.set_status(if self.filter.with_artifacts_only {
                    "showing projects with artifacts"
                } else {
                    "showing all projects"
                });
            }
            KeyCode::Char('r') => self.rescan(),
            KeyCode::Char('c') | KeyCode::Enter => self.request_clean(),
            KeyCode::Char('?') | KeyCode::Char('h') => self.mode = Mode::Help,
            _ => {}
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.view.is_empty() {
            return;
        }
        let current = self.table.selected().unwrap_or(0) as isize;
        let last = self.view.len() as isize - 1;
        self.select_index((current + delta).clamp(0, last) as usize);
    }

    fn select_index(&mut self, index: usize) {
        if self.view.is_empty() {
            self.table.select(None);
        } else {
            self.table.select(Some(index.min(self.view.len() - 1)));
        }
    }

    fn toggle_mark(&mut self) {
        let Some(id) = self.selected_id() else {
            return;
        };
        if !self.marked.remove(&id) {
            self.marked.insert(id);
        }
        self.move_by(1);
    }

    fn mark_all(&mut self, mark: bool) {
        if !mark {
            self.marked.clear();
            self.set_status("selection cleared");
            return;
        }
        let ids: Vec<ProjectId> = self
            .visible()
            .filter(|p| p.is_cleanable())
            .map(|p| p.id)
            .collect();
        let count = ids.len();
        self.marked.extend(ids);
        self.set_status(format!("{count} projects selected"));
    }

    fn cycle_age(&mut self, delta: isize) {
        let len = self.ages.len() as isize;
        self.age_index = ((self.age_index as isize + delta).rem_euclid(len)) as usize;
        self.filter.older_than = self.ages[self.age_index].1;
        let label = self.ages[self.age_index].0.clone();
        self.set_status(match self.filter.older_than.is_zero() {
            true => "age filter off".to_string(),
            false => format!("idle for at least {label}"),
        });
    }

    fn rescan(&mut self) {
        // Replacing the engine stops the old threads through its Drop impl.
        self.engine = Engine::start(self.options.clone(), &self.settings);
        self.store = Store::new();
        self.marked.clear();
        self.view.clear();
        self.table.select(Some(0));
        self.set_status("rescanning");
    }

    /// Ask for confirmation before deleting the marked (or selected) projects.
    fn request_clean(&mut self) {
        let mut targets: Vec<ProjectId> = if self.marked.is_empty() {
            self.selected_id().into_iter().collect()
        } else {
            // Keep display order so the confirmation reads like the table.
            self.view
                .iter()
                .copied()
                .filter(|id| self.marked.contains(id))
                .collect()
        };
        targets.retain(|id| self.store.get(*id).is_some_and(|p| p.is_cleanable()));

        if targets.is_empty() {
            self.set_status("nothing to clean here");
            return;
        }
        let bytes = targets
            .iter()
            .filter_map(|id| self.store.get(*id))
            .map(|p| p.size())
            .sum();
        self.mode = Mode::Confirm { targets, bytes };
    }

    fn start_clean(&mut self, targets: &[ProjectId]) {
        let mut queued = 0;
        for id in targets {
            let Some(project) = self.store.get(*id) else {
                continue;
            };
            let paths = project.artifact_paths();
            if paths.is_empty() {
                continue;
            }
            self.engine.submit_clean(*id, project.root.clone(), paths);
            queued += 1;
        }
        self.set_status(format!("cleaning {queued} project(s)"));
    }

    pub fn freed(&self) -> u64 {
        self.store.freed
    }
}

/// Presets plus, if it is not one of them, the threshold the CLI started with.
fn build_age_presets(initial: Duration) -> Vec<(String, Duration)> {
    let mut presets: Vec<(String, Duration)> = AGE_PRESETS
        .iter()
        .filter_map(|label| {
            humantime::parse_duration(label)
                .ok()
                .map(|d| ((*label).to_string(), d))
        })
        .collect();
    if !presets.iter().any(|(_, d)| *d == initial) {
        presets.push((humantime::format_duration(initial), initial));
    }
    presets.sort_by_key(|(_, d)| *d);
    presets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_include_the_initial_threshold() {
        let initial = humantime::parse_duration("45d").unwrap();
        let presets = build_age_presets(initial);
        assert!(
            presets
                .iter()
                .any(|(label, d)| label == "45d" && *d == initial)
        );
        // Ascending, starting with "off".
        assert_eq!(presets[0].1, Duration::ZERO);
        assert!(presets.windows(2).all(|w| w[0].1 <= w[1].1));
    }

    #[test]
    fn presets_are_not_duplicated() {
        let presets = build_age_presets(humantime::parse_duration("30d").unwrap());
        assert_eq!(presets.len(), AGE_PRESETS.len());
    }
}
