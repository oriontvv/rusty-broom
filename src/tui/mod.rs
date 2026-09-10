//! The interactive front-end.

mod app;
mod ui;

use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event};

use crate::config::Settings;
use crate::project::Filter;
use crate::scan::ScanOptions;

pub use app::App;

/// Redraw at most this often while waiting for input; sizes stream in between
/// key presses, so the interval doubles as the refresh rate.
const TICK: Duration = Duration::from_millis(120);

pub fn run(options: ScanOptions, settings: Settings, filter: Filter) -> Result<()> {
    let mut app = App::new(options, settings, filter);
    // Installs a panic hook that leaves the terminal usable if we crash.
    let mut terminal = ratatui::try_init()
        .context("cannot start the interactive UI — stdout is not a terminal")?;

    let result = event_loop(&mut terminal, &mut app);
    ratatui::try_restore()?;

    let freed = app.freed();
    if freed > 0 {
        println!("Freed {}.", crate::fmt::bytes(freed));
    }
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.quit {
        app.pump();
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) => app.on_key(key),
                // A resize needs no state change; the next draw handles it.
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::app::Mode;
    use super::*;
    use crate::config::Config;
    use crate::project::{Artifact, GitStatus, Project};
    use crate::size::DirStat;

    /// An app over an empty directory, pre-loaded with two synthetic projects.
    fn app_with_projects() -> App {
        let root = std::env::temp_dir().join(format!("rb-ui-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let config = Config::embedded();
        let options = ScanOptions::from_config(&config, vec![root.clone()], &[], Some(1));
        let filter = Filter {
            older_than: Duration::ZERO,
            ..Filter::default()
        };
        let mut app = App::new(options, config.settings.clone(), filter);

        for (id, name, bytes, git) in [
            (0usize, "old-service", 4 << 30, GitStatus::Ignored),
            (1, "web-frontend", 300 << 20, GitStatus::NoRepo),
        ] {
            let mut project =
                Project::new(id, root.join(name), root.clone(), vec!["rust".to_string()]);
            project.resolved = true;
            project.git = git;
            project.source_files = 42;
            project.last_activity = Some(SystemTime::now() - Duration::from_secs(200 * 86400));
            project.artifacts = vec![Artifact {
                path: root.join(name).join("target"),
                pattern: "target".to_string(),
                stat: Some(DirStat {
                    bytes,
                    files: 1000,
                    newest: Some(SystemTime::now()),
                }),
            }];
            if id >= app.store.projects.len() {
                app.store.projects.resize_with(id + 1, || {
                    Project::new(usize::MAX, PathBuf::new(), PathBuf::new(), Vec::new())
                });
            }
            app.store.projects[id] = project;
        }
        app.store.scanning = false;
        app.pump();
        app
    }

    fn render(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        let completed = terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        flatten(completed.buffer)
    }

    fn flatten(buffer: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if let Some(cell) = buffer.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn renders_the_project_list() {
        let mut app = app_with_projects();
        let screen = render(&mut app);

        assert!(screen.contains("rusty-broom"), "{screen}");
        assert!(screen.contains("old-service"), "{screen}");
        assert!(screen.contains("web-frontend"), "{screen}");
        // Largest first, and the total is the sum of both.
        assert!(screen.contains("4.00 GiB"), "{screen}");
        assert!(screen.contains("4.29 GiB reclaimable"), "{screen}");
        // The project outside a repo is flagged rather than silently trusted.
        assert!(screen.contains("no-git"), "{screen}");
        assert!(screen.contains("details"), "{screen}");
    }

    #[test]
    fn marking_then_cleaning_asks_for_confirmation() {
        let mut app = app_with_projects();
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.marked.len(), 1);

        press(&mut app, KeyCode::Char('c'));
        let screen = render(&mut app);
        assert!(screen.contains("confirm"), "{screen}");
        assert!(screen.contains("y delete"), "{screen}");

        // Anything other than `y` backs out without touching the disk.
        press(&mut app, KeyCode::Char('n'));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(!render(&mut app).contains("confirm"));
    }

    #[test]
    fn search_filters_the_list() {
        let mut app = app_with_projects();
        press(&mut app, KeyCode::Char('/'));
        for ch in "web".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Enter);
        app.pump();

        assert_eq!(app.view.len(), 1);
        let screen = render(&mut app);
        assert!(screen.contains("web-frontend"), "{screen}");
        assert!(!screen.contains("old-service"), "{screen}");
    }

    #[test]
    fn age_threshold_can_hide_everything() {
        let mut app = app_with_projects();
        // The synthetic projects are ~200 days old; 1y hides them, off shows them.
        while app.age_label() != "1y" {
            press(&mut app, KeyCode::Char('o'));
        }
        app.pump();
        assert!(app.view.is_empty());

        while app.age_label() != "off" {
            press(&mut app, KeyCode::Char('O'));
        }
        app.pump();
        assert_eq!(app.view.len(), 2);
    }

    #[test]
    fn help_overlay_opens_and_closes() {
        let mut app = app_with_projects();
        press(&mut app, KeyCode::Char('?'));
        assert!(render(&mut app).contains("keys"));
        press(&mut app, KeyCode::Char('x'));
        assert!(!render(&mut app).contains("any key to close"));
    }

    #[test]
    fn q_quits() {
        let mut app = app_with_projects();
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }
}
