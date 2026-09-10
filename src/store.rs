//! Accumulated pipeline state, shared by the CLI and the TUI.

use std::path::PathBuf;

use crate::engine::Event;
use crate::project::{CleanState, Project, ProjectId};

#[derive(Debug, Default)]
pub struct Store {
    /// Projects indexed by id; the scanner hands out ids in order.
    pub projects: Vec<Project>,
    pub dirs_scanned: u64,
    pub scanning: bool,
    pub current_dir: Option<PathBuf>,
    /// Artifact paths left alone because git does not ignore them.
    pub skipped_paths: usize,
    pub freed: u64,
}

impl Store {
    pub fn new() -> Self {
        Self {
            scanning: true,
            ..Self::default()
        }
    }

    pub fn get(&self, id: ProjectId) -> Option<&Project> {
        self.projects.get(id)
    }

    /// Fold one event in. Returns the project it touched, if any.
    pub fn apply(&mut self, event: Event) -> Option<ProjectId> {
        match event {
            Event::Found(project) => {
                let id = project.id;
                // Ids are sequential, but stay robust if one ever goes missing.
                if id >= self.projects.len() {
                    self.projects.resize_with(id + 1, || {
                        Project::new(usize::MAX, PathBuf::new(), PathBuf::new(), Vec::new())
                    });
                }
                self.projects[id] = *project;
                Some(id)
            }
            Event::Progress { dirs, current } => {
                self.dirs_scanned = dirs;
                self.current_dir = Some(current);
                None
            }
            Event::ScanFinished { dirs } => {
                self.dirs_scanned = self.dirs_scanned.max(dirs);
                self.scanning = false;
                self.current_dir = None;
                None
            }
            Event::Artifacts {
                id,
                artifacts,
                git,
                skipped,
            } => {
                self.skipped_paths += skipped.len();
                let project = self.projects.get_mut(id)?;
                project.artifacts = artifacts;
                project.git = git;
                project.resolved = true;
                Some(id)
            }
            Event::Sized { id, index, stat } => {
                let project = self.projects.get_mut(id)?;
                project.artifacts.get_mut(index)?.stat = Some(stat);
                Some(id)
            }
            Event::CleanStarted { id } => {
                self.projects.get_mut(id)?.state = CleanState::Running;
                Some(id)
            }
            Event::CleanFinished { id, freed, errors } => {
                self.freed += freed;
                let project = self.projects.get_mut(id)?;
                project.state = if errors.is_empty() {
                    CleanState::Done { freed }
                } else {
                    CleanState::Failed { errors }
                };
                // The artifacts are gone; keep the row but show nothing to reclaim.
                if matches!(project.state, CleanState::Done { .. }) {
                    project.artifacts.clear();
                }
                Some(id)
            }
        }
    }

    /// Projects that were actually discovered, in id order.
    pub fn all(&self) -> impl Iterator<Item = &Project> {
        self.projects.iter().filter(|p| p.id != usize::MAX)
    }
}
