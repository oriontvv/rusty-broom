//! Rendering. Every widget is built from owned data so that the immutable view
//! of the app is finished before `TableState` is borrowed mutably.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table};

use crate::fmt;
use crate::humantime;
use crate::project::{CleanState, GitStatus, Project, tilde};
use crate::tui::app::{App, Mode};

const KEYS: &str = "j/k move · space mark · a all · c clean · o age · s sort · / find · v show · r rescan · ? help · q quit";

/// A project idle for longer than this is highlighted as clearly abandoned.
const VERY_OLD: Duration = Duration::from_secs(365 * 24 * 3600);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, table, details, footer] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(5),
        Constraint::Length(8),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, app);
    let rows = build_rows(app);
    draw_table(frame, table, app, rows);
    draw_details(frame, details, app);
    draw_footer(frame, footer, app);

    match &app.mode {
        Mode::Help => draw_help(frame),
        Mode::Confirm { targets, bytes } => draw_confirm(frame, app, targets, *bytes),
        _ => {}
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let roots = app
        .roots
        .iter()
        .map(|r| tilde(r))
        .collect::<Vec<_>>()
        .join(", ");
    // Leave room for the scan status that follows on the same line.
    let roots = ellipsize_start(&roots, usize::from(area.width).saturating_sub(30));

    let scan = if app.store.scanning {
        Span::styled(
            format!("scanning… {} dirs", app.store.dirs_scanned),
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::styled(
            format!("{} dirs scanned", app.store.dirs_scanned),
            Style::default().fg(Color::DarkGray),
        )
    };

    let mut stats = vec![
        Span::styled("idle ≥ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.age_label().to_string(),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("   "),
        Span::styled("sort ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.sort.label().to_string(),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("   "),
        Span::raw(format!(
            "{}/{} projects",
            app.view.len(),
            app.store.all().count()
        )),
        Span::raw("   "),
        Span::styled(
            format!("{} reclaimable", fmt::bytes(app.visible_bytes())),
            Style::default().fg(Color::Yellow).bold(),
        ),
    ];
    if !app.marked.is_empty() {
        stats.push(Span::raw("   "));
        stats.push(Span::styled(
            format!(
                "{} marked ({})",
                app.marked.len(),
                fmt::bytes(app.marked_bytes())
            ),
            Style::default().fg(Color::Green),
        ));
    }
    if app.freed() > 0 {
        stats.push(Span::raw("   "));
        stats.push(Span::styled(
            format!("freed {}", fmt::bytes(app.freed())),
            Style::default().fg(Color::Green).bold(),
        ));
    }

    let block = Block::bordered()
        .title(Span::styled(
            " rusty-broom ",
            Style::default().fg(Color::Cyan).bold(),
        ))
        .border_style(Style::default().fg(Color::DarkGray));

    let text = vec![
        Line::from(vec![
            Span::styled("root ", Style::default().fg(Color::DarkGray)),
            Span::raw(roots),
            Span::raw("   "),
            scan,
        ]),
        Line::from(stats),
    ];
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn build_rows(app: &App) -> Vec<Row<'static>> {
    app.visible()
        .map(|project| {
            let marked = app.marked.contains(&project.id);
            let mark = if marked {
                Span::styled("✓", Style::default().fg(Color::Green).bold())
            } else {
                Span::raw(" ")
            };

            let (size_text, size_style) = size_cell(project);
            let age_style = match project.age() {
                Some(age) if age >= VERY_OLD => Style::default().fg(Color::Red),
                Some(_) => Style::default(),
                None => Style::default().fg(Color::DarkGray),
            };

            let row = Row::new(vec![
                Cell::from(Line::from(mark)),
                Cell::from(project.type_label()),
                Cell::from(Line::from(Span::styled(size_text, size_style)).right_aligned()),
                Cell::from(Line::from(Span::styled(project.age_label(), age_style))),
                Cell::from(Line::from(git_span(project.git))),
                Cell::from(path_line(project)),
            ]);
            if marked {
                row.style(Style::default().add_modifier(Modifier::BOLD))
            } else {
                row
            }
        })
        .collect()
}

/// Size plus a hint that measuring is still in progress.
fn size_cell(project: &Project) -> (String, Style) {
    match &project.state {
        CleanState::Running => ("cleaning".to_string(), Style::default().fg(Color::Yellow)),
        CleanState::Done { freed } => (
            format!("-{}", fmt::bytes(*freed)),
            Style::default().fg(Color::Green),
        ),
        CleanState::Failed { .. } => ("failed".to_string(), Style::default().fg(Color::Red)),
        CleanState::Idle if !project.is_measured() => (
            format!("~{}", fmt::bytes(project.size())),
            Style::default().fg(Color::DarkGray),
        ),
        CleanState::Idle => (
            fmt::bytes(project.size()),
            Style::default().fg(Color::Yellow),
        ),
    }
}

fn git_span(status: GitStatus) -> Span<'static> {
    let colour = match status {
        GitStatus::Ignored => Color::Green,
        GitStatus::NoRepo | GitStatus::Disabled => Color::Yellow,
        GitStatus::Unavailable => Color::Red,
        GitStatus::Pending => Color::DarkGray,
    };
    Span::styled(status.label(), Style::default().fg(colour))
}

fn path_line(project: &Project) -> Line<'static> {
    let display = project.display_path();
    let (parent, name) = match display.rfind('/') {
        Some(index) => (
            display[..=index].to_string(),
            display[index + 1..].to_string(),
        ),
        None => (String::new(), display),
    };
    Line::from(vec![
        Span::styled(parent, Style::default().fg(Color::DarkGray)),
        Span::raw(name),
    ])
}

fn draw_table(frame: &mut Frame, area: Rect, app: &mut App, rows: Vec<Row<'static>>) {
    let widths = [
        Constraint::Length(1),
        Constraint::Length(12),
        Constraint::Length(11),
        Constraint::Length(11),
        Constraint::Length(11),
        Constraint::Fill(1),
    ];
    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("TYPE"),
        Cell::from(Line::from("SIZE").right_aligned()),
        Cell::from("IDLE"),
        Cell::from("GIT"),
        Cell::from("PROJECT"),
    ])
    .style(Style::default().fg(Color::DarkGray).bold());

    let empty = app.view.is_empty();
    let title = if empty && app.store.scanning {
        " searching… "
    } else if empty {
        " nothing matches the current filter "
    } else {
        " projects "
    };

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::default().bg(Color::Rgb(40, 44, 52)).bold())
        .block(
            Block::bordered()
                .title(title)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
    frame.render_stateful_widget(table, area, &mut app.table);
}

fn draw_details(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered()
        .title(" details ")
        .border_style(Style::default().fg(Color::DarkGray));

    let Some(project) = app.selected() else {
        frame.render_widget(
            Paragraph::new("Select a project to see what would be removed.")
                .style(Style::default().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    };

    let inner_width = usize::from(area.width).saturating_sub(2);
    let mut lines = vec![
        headline(project, inner_width),
        Line::from(vec![
            Span::styled("idle ", Style::default().fg(Color::DarkGray)),
            Span::raw(project.age_label()),
            Span::styled("   built ", Style::default().fg(Color::DarkGray)),
            Span::raw(match project.last_build() {
                Some(at) => humantime::format_age(humantime::age_of(at)),
                None => "unknown".to_string(),
            }),
            Span::styled("   sources ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{} files", project.source_files)),
            Span::styled("   git ", Style::default().fg(Color::DarkGray)),
            git_span(project.git),
        ]),
    ];

    if let CleanState::Failed { errors } = &project.state {
        for error in errors.iter().take(3) {
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().fg(Color::Red),
            )));
        }
    } else if project.artifacts.is_empty() {
        lines.push(Line::from(Span::styled(
            if project.resolved {
                "nothing to remove"
            } else {
                "resolving artifacts…"
            },
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for artifact in project.artifacts.iter().take(4) {
            let relative = artifact
                .path
                .strip_prefix(&project.root)
                .unwrap_or(&artifact.path)
                .display()
                .to_string();
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>10}  ", fmt::maybe_bytes(artifact.stat.map(|s| s.bytes))),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(relative),
                Span::styled(
                    format!("   ({})", artifact.pattern),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        if project.artifacts.len() > 4 {
            lines.push(Line::from(Span::styled(
                format!("… and {} more", project.artifacts.len() - 4),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn headline(project: &Project, width: usize) -> Line<'static> {
    let types = project.type_label();
    let path = ellipsize_start(
        &tilde(&project.root),
        width.saturating_sub(types.chars().count() + 3),
    );
    Line::from(vec![
        Span::styled(path, Style::default().bold()),
        Span::styled(format!("   {types}"), Style::default().fg(Color::Cyan)),
    ])
}

/// Shorten to `max` characters, keeping the end — the interesting part of a
/// path is its last few components.
fn ellipsize_start(text: &str, max: usize) -> String {
    let length = text.chars().count();
    if length <= max || max == 0 {
        return text.to_string();
    }
    let tail: String = text.chars().skip(length - max.saturating_sub(1)).collect();
    format!("…{tail}")
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = match &app.mode {
        Mode::Search { input } => Line::from(vec![
            Span::styled("find ", Style::default().fg(Color::Cyan).bold()),
            Span::raw(input.clone()),
            Span::styled("█", Style::default().fg(Color::Cyan)),
            Span::styled(
                "   enter to keep · esc to clear",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        _ => match &app.status {
            Some((message, _)) => Line::from(Span::styled(
                message.clone(),
                Style::default().fg(Color::Cyan),
            )),
            None => Line::from(Span::styled(KEYS, Style::default().fg(Color::DarkGray))),
        },
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_help(frame: &mut Frame) {
    let rows = [
        ("j / k, ↓ / ↑", "move"),
        ("g / G, PgUp / PgDn", "jump"),
        ("space", "mark or unmark the project"),
        ("a / A", "mark everything visible / clear marks"),
        (
            "c or enter",
            "clean the marked projects (or the selected one)",
        ),
        ("o / O", "raise or lower the idle threshold"),
        ("s", "change the sort order"),
        ("/", "filter by path or type"),
        ("v", "show projects with nothing to reclaim as well"),
        ("r", "rescan from scratch"),
        ("q, esc", "quit"),
    ];
    let mut lines = vec![
        Line::from(Span::styled(
            "Only paths git reports as ignored are ever deleted.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::raw(""),
    ];
    lines.extend(rows.iter().map(|(keys, what)| {
        Line::from(vec![
            Span::styled(format!("{keys:<20}"), Style::default().fg(Color::Cyan)),
            Span::raw(*what),
        ])
    }));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "any key to close",
        Style::default().fg(Color::DarkGray),
    )));

    let area = popup(frame.area(), 68, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" keys ")
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn draw_confirm(frame: &mut Frame, app: &App, targets: &[usize], bytes: u64) {
    const WIDTH: u16 = 76;
    // Paths are shortened to the popup's inner width so that nothing wraps and
    // the line count below is the exact height the box needs.
    let inner = usize::from(WIDTH.min(frame.area().width)).saturating_sub(2);

    let mut lines = vec![
        Line::from(vec![
            Span::raw("Delete artifacts of "),
            Span::styled(
                format!("{} project(s)", targets.len()),
                Style::default().bold(),
            ),
            Span::raw(", freeing about "),
            Span::styled(fmt::bytes(bytes), Style::default().fg(Color::Yellow).bold()),
            Span::raw("."),
        ]),
        Line::raw(""),
    ];

    for id in targets.iter().take(6) {
        if let Some(project) = app.store.get(*id) {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>10}  ", fmt::bytes(project.size())),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(ellipsize_start(
                    &tilde(&project.root),
                    inner.saturating_sub(12),
                )),
            ]));
        }
    }
    if targets.len() > 6 {
        lines.push(Line::from(Span::styled(
            format!("… and {} more", targets.len() - 6),
            Style::default().fg(Color::DarkGray),
        )));
    }

    let unverified = targets
        .iter()
        .filter_map(|id| app.store.get(*id))
        .filter(|p| p.git.is_unverified())
        .count();
    if unverified > 0 {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("{unverified} of them could not be verified against git."),
            Style::default().fg(Color::Red).bold(),
        )));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("y", Style::default().fg(Color::Green).bold()),
        Span::raw(" delete    "),
        Span::styled("any other key", Style::default().fg(Color::Cyan)),
        Span::raw(" cancel"),
    ]));

    let area = popup(frame.area(), WIDTH, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" confirm ")
                .border_style(Style::default().fg(Color::Red)),
        ),
        area,
    );
}

/// A centred box of at most `width` x `height`, clamped to the screen.
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    cell
}
