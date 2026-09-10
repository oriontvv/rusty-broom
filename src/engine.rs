//! The background pipeline shared by both front-ends.
//!
//! One thread walks the roots and streams projects out as it finds them; a pool
//! of workers then resolves each project's artifacts against git, measures them,
//! and later performs the deletions. Everything reaches the caller as [`Event`]s
//! on a channel, so the TUI never blocks on disk I/O and the CLI can simply
//! drain the same stream to completion.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use crate::clean;
use crate::project::{Artifact, GitStatus, Project, ProjectId};
use crate::scan::{self, ScanOptions};
use crate::size::{self, DirStat};
use crate::{config::Settings, gitignore};

/// How often the scanner reports progress, in directories.
const PROGRESS_EVERY: u64 = 16;

#[derive(Debug)]
pub enum Event {
    /// A project was discovered. Its artifact list is not resolved yet.
    Found(Box<Project>),
    Progress {
        dirs: u64,
        current: PathBuf,
    },
    ScanFinished {
        dirs: u64,
    },
    /// The final artifact list for a project, after git filtering.
    Artifacts {
        id: ProjectId,
        artifacts: Vec<Artifact>,
        git: GitStatus,
        /// Paths dropped because git does not ignore them.
        skipped: Vec<PathBuf>,
    },
    Sized {
        id: ProjectId,
        index: usize,
        stat: DirStat,
    },
    CleanStarted {
        id: ProjectId,
    },
    CleanFinished {
        id: ProjectId,
        freed: u64,
        errors: Vec<String>,
    },
}

enum Job {
    Resolve {
        id: ProjectId,
        root: PathBuf,
        candidates: Vec<(String, PathBuf)>,
    },
    Size {
        id: ProjectId,
        index: usize,
        path: PathBuf,
    },
    Clean {
        id: ProjectId,
        root: PathBuf,
        paths: Vec<PathBuf>,
    },
}

/// A work queue that also tracks how much work is outstanding, so that a
/// consumer can wait for the pipeline to go idle.
struct Queue {
    state: Mutex<QueueState>,
    signal: Condvar,
}

struct QueueState {
    jobs: VecDeque<Job>,
    /// Jobs pushed but not yet finished.
    pending: usize,
    scanning: bool,
    closed: bool,
}

impl Queue {
    fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                jobs: VecDeque::new(),
                pending: 0,
                scanning: true,
                closed: false,
            }),
            signal: Condvar::new(),
        }
    }

    fn push(&self, job: Job, front: bool) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        state.pending += 1;
        if front {
            state.jobs.push_front(job);
        } else {
            state.jobs.push_back(job);
        }
        self.signal.notify_one();
    }

    /// Block until a job is available, or return `None` once the queue is closed.
    fn pop(&self) -> Option<Job> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(job) = state.jobs.pop_front() {
                return Some(job);
            }
            if state.closed {
                return None;
            }
            state = self.signal.wait(state).unwrap();
        }
    }

    fn finish_job(&self) {
        let mut state = self.state.lock().unwrap();
        state.pending = state.pending.saturating_sub(1);
        // Wake `wait_idle`, which may be watching for pending to reach zero.
        self.signal.notify_all();
    }

    fn scan_finished(&self) {
        let mut state = self.state.lock().unwrap();
        state.scanning = false;
        self.signal.notify_all();
    }

    /// Wait until the scan is over and no work is left.
    fn wait_idle(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.closed && (state.scanning || state.pending > 0) {
            state = self.signal.wait(state).unwrap();
        }
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.jobs.clear();
        self.signal.notify_all();
    }
}

pub struct Engine {
    events: Receiver<Event>,
    queue: Arc<Queue>,
    cancel: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Engine {
    /// Start scanning. Sizing begins as soon as the first project is found.
    pub fn start(options: ScanOptions, settings: &Settings) -> Self {
        let (tx, events) = channel();
        let queue = Arc::new(Queue::new());
        let cancel = Arc::new(AtomicBool::new(false));
        // Shared with the scanner only; project ids are handed out in order.
        let next_id = Arc::new(AtomicUsize::new(0));
        let require_git_ignored = settings.require_git_ignored;

        let mut threads = Vec::new();
        for _ in 0..worker_count(settings.workers) {
            let queue = Arc::clone(&queue);
            let cancel = Arc::clone(&cancel);
            let tx = tx.clone();
            threads.push(thread::spawn(move || {
                worker(&queue, &cancel, &tx, require_git_ignored)
            }));
        }

        {
            let queue = Arc::clone(&queue);
            let cancel = Arc::clone(&cancel);
            let tx = tx.clone();
            threads.push(thread::spawn(move || {
                scanner(options, &queue, &cancel, &next_id, &tx)
            }));
        }

        // The only remaining sender lives in the threads; dropping ours lets the
        // receiver observe a disconnect once they are all gone.
        drop(tx);

        Self {
            events,
            queue,
            cancel,
            threads,
        }
    }

    /// Events that arrived since the last call. Never blocks.
    pub fn drain(&self) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => out.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return out,
            }
        }
    }

    /// Block until scanning and sizing are done, feeding every event to `sink`.
    pub fn run_to_completion<F: FnMut(Event)>(&self, mut sink: F) {
        let queue = Arc::clone(&self.queue);
        // wait_idle blocks, so it gets its own thread while we keep draining the
        // channel; otherwise a full channel could stall the workers.
        let waiter = thread::spawn(move || queue.wait_idle());
        while !waiter.is_finished() {
            for event in self.drain() {
                sink(event);
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = waiter.join();
        for event in self.drain() {
            sink(event);
        }
    }

    /// Queue a deletion. Clean jobs jump the queue so the UI reacts at once.
    pub fn submit_clean(&self, id: ProjectId, root: PathBuf, paths: Vec<PathBuf>) {
        self.queue.push(Job::Clean { id, root, paths }, true);
    }

    /// Stop all background work. Safe to call more than once.
    pub fn shutdown(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.queue.close();
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_count(configured: usize) -> usize {
    if configured > 0 {
        return configured;
    }
    thread::available_parallelism()
        .map(|n| n.get().clamp(2, 8))
        .unwrap_or(4)
}

fn scanner(
    options: ScanOptions,
    queue: &Arc<Queue>,
    cancel: &Arc<AtomicBool>,
    next_id: &Arc<AtomicUsize>,
    tx: &Sender<Event>,
) {
    let mut dirs = 0u64;
    scan::walk(
        &options,
        cancel,
        |candidate| {
            let id = next_id.fetch_add(1, Ordering::SeqCst);
            let mut project = Project::new(
                id,
                candidate.root.clone(),
                candidate.scan_root,
                candidate.types,
            );
            project.last_activity = candidate.last_activity;
            project.source_files = candidate.source_files;
            if tx.send(Event::Found(Box::new(project))).is_err() {
                return;
            }
            queue.push(
                Job::Resolve {
                    id,
                    root: candidate.root,
                    candidates: candidate.artifacts,
                },
                false,
            );
        },
        |count, current| {
            dirs = count;
            // Progress is advisory and the walk is much faster than any redraw,
            // so report every so often instead of per directory.
            if count % PROGRESS_EVERY == 0 {
                let _ = tx.send(Event::Progress {
                    dirs: count,
                    current: current.to_path_buf(),
                });
            }
        },
    );
    let _ = tx.send(Event::ScanFinished { dirs });
    queue.scan_finished();
}

fn worker(
    queue: &Arc<Queue>,
    cancel: &Arc<AtomicBool>,
    tx: &Sender<Event>,
    require_git_ignored: bool,
) {
    while let Some(job) = queue.pop() {
        if cancel.load(Ordering::Relaxed) {
            queue.finish_job();
            break;
        }
        match job {
            Job::Resolve {
                id,
                root,
                candidates,
            } => {
                let (artifacts, git, skipped) = resolve(&root, candidates, require_git_ignored);
                let _ = tx.send(Event::Artifacts {
                    id,
                    artifacts: artifacts.clone(),
                    git,
                    skipped,
                });
                // Queue the measurements only after the consumer knows how many
                // artifacts to expect, so it can tell when a project is done.
                for (index, artifact) in artifacts.iter().enumerate() {
                    queue.push(
                        Job::Size {
                            id,
                            index,
                            path: artifact.path.clone(),
                        },
                        false,
                    );
                }
            }
            Job::Size { id, index, path } => {
                let stat = size::measure(&path, cancel);
                let _ = tx.send(Event::Sized { id, index, stat });
            }
            Job::Clean { id, root, paths } => {
                let _ = tx.send(Event::CleanStarted { id });
                let outcome = clean::remove_all(&root, &paths);
                let _ = tx.send(Event::CleanFinished {
                    id,
                    freed: outcome.freed,
                    errors: outcome.errors,
                });
            }
        }
        queue.finish_job();
    }
}

/// Turn candidate paths into the artifacts we are allowed to delete.
fn resolve(
    root: &Path,
    candidates: Vec<(String, PathBuf)>,
    require_git_ignored: bool,
) -> (Vec<Artifact>, GitStatus, Vec<PathBuf>) {
    let mut patterns: HashMap<PathBuf, String> = HashMap::new();
    let mut paths = Vec::with_capacity(candidates.len());
    for (pattern, path) in candidates {
        patterns.insert(path.clone(), pattern);
        paths.push(path);
    }

    let (kept, skipped, git) = if require_git_ignored {
        gitignore::partition_ignored(root, paths)
    } else {
        (paths, Vec::new(), GitStatus::Disabled)
    };

    let artifacts = kept
        .into_iter()
        .map(|path| Artifact {
            pattern: patterns.remove(&path).unwrap_or_default(),
            path,
            stat: None,
        })
        .collect();

    (artifacts, git, skipped)
}
