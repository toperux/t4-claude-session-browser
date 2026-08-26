use anyhow::Result;
use chrono::Local;
use crossterm::cursor;
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use std::cmp::Reverse;
use std::collections::HashSet;

use crate::del::{self, human_bytes};
use crate::index::{truncate, Index, SessionMeta};
use crate::paths::ClaudeDir;
use crate::transcript::{self, Entry, Event, LoadOpts};

#[derive(PartialEq, Clone, Copy)]
enum Pane {
    Projects,
    Sessions,
    Preview,
}

#[derive(PartialEq, Clone, Copy)]
enum Sort {
    Date,
    Size,
    Msgs,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Sort::Date => Sort::Size,
            Sort::Size => Sort::Msgs,
            Sort::Msgs => Sort::Date,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Sort::Date => "date",
            Sort::Size => "size",
            Sort::Msgs => "msgs",
        }
    }
}

enum Mode {
    Browse,
    Filter,
    Confirm(Vec<del::DeletePlan>),
}

struct App {
    dir: ClaudeDir,
    index: Index,
    /// Project slugs; index 0 is the "all projects" pseudo-entry.
    project_rows: Vec<(Option<String>, String, usize, u64)>,
    project_sel: usize,
    visible: Vec<SessionMeta>,
    session_sel: usize,
    marked: HashSet<String>,
    /// Rendered once per selection, not once per frame.
    preview: Vec<Line<'static>>,
    preview_truncated: bool,
    preview_scroll: u16,
    focus: Pane,
    mode: Mode,
    filter: String,
    sort: Sort,
    status: String,
    quit: bool,
}

impl App {
    fn new(dir: ClaudeDir) -> Result<Self> {
        let index = Index::build(&dir)?;
        let mut app = Self {
            dir,
            index,
            project_rows: Vec::new(),
            project_sel: 0,
            visible: Vec::new(),
            session_sel: 0,
            marked: HashSet::new(),
            preview: Vec::new(),
            preview_truncated: false,
            preview_scroll: 0,
            focus: Pane::Sessions,
            mode: Mode::Browse,
            filter: String::new(),
            sort: Sort::Date,
            status: String::new(),
            quit: false,
        };
        app.status = app.index.warning_summary().unwrap_or_default();
        app.rebuild_projects();
        app.refilter();
        Ok(app)
    }

    fn rebuild_projects(&mut self) {
        let total_bytes = self.index.sessions.iter().map(|s| s.size_bytes).sum();
        let mut rows = vec![(
            None,
            format!("All projects ({})", self.index.sessions.len()),
            self.index.sessions.len(),
            total_bytes,
        )];
        for p in self.index.projects() {
            rows.push((
                Some(p.slug.clone()),
                p.label.clone(),
                p.sessions.len(),
                p.size_bytes(),
            ));
        }
        self.project_rows = rows;
        self.project_sel = self.project_sel.min(self.project_rows.len() - 1);
    }

    fn refilter(&mut self) {
        let slug = self
            .project_rows
            .get(self.project_sel)
            .and_then(|r| r.0.clone());
        let needle = self.filter.to_lowercase();

        let mut list: Vec<SessionMeta> = self
            .index
            .sessions
            .iter()
            .filter(|s| slug.as_ref().is_none_or(|sl| &s.project_slug == sl))
            .filter(|s| {
                needle.is_empty()
                    || s.title.to_lowercase().contains(&needle)
                    || s.location().to_lowercase().contains(&needle)
                    || s.id.starts_with(&needle)
            })
            .cloned()
            .collect();

        match self.sort {
            Sort::Date => list.sort_by_key(|s| Reverse(s.activity())),
            Sort::Size => list.sort_by_key(|s| Reverse(s.size_bytes)),
            Sort::Msgs => list.sort_by_key(|s| Reverse(s.user_msgs + s.assistant_msgs)),
        }

        self.visible = list;
        self.session_sel = self.session_sel.min(self.visible.len().saturating_sub(1));
        self.load_preview();
    }

    fn current(&self) -> Option<&SessionMeta> {
        self.visible.get(self.session_sel)
    }

    fn load_preview(&mut self) {
        self.preview.clear();
        self.preview_truncated = false;
        self.preview_scroll = 0;
        let Some(meta) = self.current().cloned() else {
            return;
        };

        let mut lines = vec![
            Line::from(Span::styled(
                meta.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("{} · {}", meta.id, meta.location()),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
        ];

        match transcript::load(&meta.path, &LoadOpts::default()) {
            Ok(t) => {
                lines.extend(t.entries.iter().flat_map(entry_lines));
                if t.truncated {
                    lines.push(Line::from(Span::styled(
                        "… preview truncated (use `csb show` for the full transcript)",
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                self.preview_truncated = t.truncated;
            }
            Err(e) => {
                let msg = format!("preview failed: {e}");
                lines.push(Line::from(Span::styled(
                    msg.clone(),
                    Style::default().fg(Color::Red),
                )));
                self.status = msg;
            }
        }
        self.preview = lines;
    }

    /// Highest scroll offset that still leaves a line on screen. Counting
    /// logical lines under-estimates wrapped ones, which errs toward showing
    /// too much rather than scrolling into an empty pane.
    fn max_scroll(&self) -> u16 {
        u16::try_from(self.preview.len().saturating_sub(1)).unwrap_or(u16::MAX)
    }

    /// Marked sessions, or the highlighted one when nothing is marked.
    fn delete_targets(&self) -> Vec<SessionMeta> {
        if self.marked.is_empty() {
            return self.current().cloned().into_iter().collect();
        }
        self.index
            .sessions
            .iter()
            .filter(|s| self.marked.contains(&s.id))
            .cloned()
            .collect()
    }

    fn reload_index(&mut self) -> Result<()> {
        self.index = Index::build(&self.dir)?;
        if let Some(summary) = self.index.warning_summary() {
            self.status = summary;
        }
        self.marked
            .retain(|id| self.index.sessions.iter().any(|s| &s.id == id));
        self.rebuild_projects();
        self.refilter();
        Ok(())
    }
}

/// Undo everything `run` set up. Safe to call twice and on any exit path, so
/// it never leaves the shell in raw mode on the alternate screen.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
}

/// A panic unwinds past the normal teardown, so restore the terminal first -
/// otherwise the backtrace is printed into a raw-mode alternate screen that
/// disappears, and the shell is left unusable.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

pub fn run(dir: ClaudeDir) -> Result<()> {
    let mut app = App::new(dir)?;

    enable_raw_mode()?;
    if let Err(e) = execute!(std::io::stdout(), EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e.into());
    }
    install_panic_hook();

    // Everything after setup runs inside this closure so a failure anywhere -
    // including `Terminal::new` - still reaches the restore below.
    let result = (|| {
        let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
        event_loop(&mut term, &mut app)
    })();

    restore_terminal();
    result
}

fn event_loop<B: Backend>(term: &mut Terminal<B>, app: &mut App) -> Result<()> {
    while !app.quit {
        term.draw(|f| draw(f, app))?;
        if let TermEvent::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                handle_key(app, key);
            }
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) {
    match std::mem::replace(&mut app.mode, Mode::Browse) {
        Mode::Filter => {
            match key.code {
                KeyCode::Esc => {
                    app.filter.clear();
                    app.refilter();
                }
                KeyCode::Enter => {}
                KeyCode::Backspace => {
                    app.filter.pop();
                    app.refilter();
                    app.mode = Mode::Filter;
                }
                KeyCode::Char(c) => {
                    app.filter.push(c);
                    app.refilter();
                    app.mode = Mode::Filter;
                }
                _ => app.mode = Mode::Filter,
            }
            return;
        }
        Mode::Confirm(plans) => {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let mut ok = 0;
                    let mut err = None;
                    for p in &plans {
                        match del::execute(&app.dir, p) {
                            Ok(()) => ok += 1,
                            Err(e) => {
                                err = Some(e.to_string());
                                break;
                            }
                        }
                    }
                    app.marked.clear();
                    app.status = match err {
                        Some(e) => format!("deleted {ok}, then failed: {e}"),
                        None => format!("deleted {ok} session(s) to the recycle bin"),
                    };
                    if let Err(e) = app.reload_index() {
                        app.status = format!("reindex failed: {e}");
                    }
                }
                _ => app.status = "delete cancelled".into(),
            }
            return;
        }
        Mode::Browse => {}
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
        KeyCode::Char('c') if ctrl => app.quit = true,
        KeyCode::Tab => {
            app.focus = match app.focus {
                Pane::Projects => Pane::Sessions,
                Pane::Sessions => Pane::Preview,
                Pane::Preview => Pane::Projects,
            }
        }
        KeyCode::BackTab => {
            app.focus = match app.focus {
                Pane::Projects => Pane::Preview,
                Pane::Sessions => Pane::Projects,
                Pane::Preview => Pane::Sessions,
            }
        }
        KeyCode::Char('/') => {
            app.mode = Mode::Filter;
            app.focus = Pane::Sessions;
        }
        KeyCode::Char('s') => {
            app.sort = app.sort.next();
            app.refilter();
        }
        KeyCode::Char('r') => {
            match app.reload_index() {
                Err(e) => app.status = format!("reindex failed: {e}"),
                // reload_index reports any unreadable files; don't overwrite it.
                Ok(()) if app.index.warnings.is_empty() => app.status = "reindexed".into(),
                Ok(()) => {}
            }
        }
        // Marking belongs to the session list; from another pane `move_sel`
        // would advance that pane's cursor instead.
        KeyCode::Char(' ') if app.focus == Pane::Sessions => {
            if let Some(meta) = app.current() {
                let id = meta.id.clone();
                if !app.marked.remove(&id) {
                    app.marked.insert(id);
                }
                if app.session_sel + 1 < app.visible.len() {
                    app.session_sel += 1;
                    app.load_preview();
                }
            }
        }
        KeyCode::Char(' ') => app.status = "tab to the sessions pane to mark".into(),
        KeyCode::Char('a') => {
            for s in &app.visible {
                app.marked.insert(s.id.clone());
            }
        }
        KeyCode::Char('A') => app.marked.clear(),
        KeyCode::Char('d') => {
            let targets = app.delete_targets();
            if targets.is_empty() {
                app.status = "nothing selected".into();
            } else {
                let plans = targets.iter().map(|m| del::plan(&app.dir, m)).collect();
                app.mode = Mode::Confirm(plans);
            }
        }
        KeyCode::Char('j') | KeyCode::Down => move_sel(app, 1),
        KeyCode::Char('k') | KeyCode::Up => move_sel(app, -1),
        KeyCode::PageDown => move_sel(app, 10),
        KeyCode::PageUp => move_sel(app, -10),
        KeyCode::Char('g') | KeyCode::Home => jump(app, true),
        KeyCode::Char('G') | KeyCode::End => jump(app, false),
        _ => {}
    }
}

fn move_sel(app: &mut App, delta: i32) {
    let step = |cur: usize, len: usize| -> usize {
        if len == 0 {
            return 0;
        }
        (cur as i32 + delta).clamp(0, len as i32 - 1) as usize
    };
    match app.focus {
        Pane::Projects => {
            let next = step(app.project_sel, app.project_rows.len());
            if next != app.project_sel {
                app.project_sel = next;
                app.session_sel = 0;
                app.refilter();
            }
        }
        Pane::Sessions => {
            let next = step(app.session_sel, app.visible.len());
            if next != app.session_sel {
                app.session_sel = next;
                app.load_preview();
            }
        }
        Pane::Preview => {
            let next = i64::from(app.preview_scroll) + i64::from(delta);
            app.preview_scroll = next.clamp(0, i64::from(app.max_scroll())) as u16;
        }
    }
}

fn jump(app: &mut App, top: bool) {
    match app.focus {
        Pane::Projects => {
            app.project_sel = if top { 0 } else { app.project_rows.len() - 1 };
            app.session_sel = 0;
            app.refilter();
        }
        Pane::Sessions => {
            app.session_sel = if top {
                0
            } else {
                app.visible.len().saturating_sub(1)
            };
            app.load_preview();
        }
        Pane::Preview => app.preview_scroll = if top { 0 } else { app.max_scroll() },
    }
}

// ---------------------------------------------------------------- drawing

fn draw(f: &mut Frame, app: &mut App) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(f.area());
    let cols = Layout::horizontal([
        Constraint::Percentage(22),
        Constraint::Percentage(33),
        Constraint::Percentage(45),
    ])
    .split(rows[0]);

    draw_projects(f, app, cols[0]);
    draw_sessions(f, app, cols[1]);
    draw_preview(f, app, cols[2]);
    draw_status(f, app, rows[1]);

    if let Mode::Confirm(plans) = &app.mode {
        draw_confirm(f, plans);
    }
}

fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(format!(" {title} "))
}

fn draw_projects(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .project_rows
        .iter()
        .map(|(_, label, count, bytes)| {
            ListItem::new(vec![
                Line::from(truncate(label, area.width.saturating_sub(4) as usize)),
                Line::from(Span::styled(
                    format!("  {count} sessions · {}", human_bytes(*bytes)),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect();

    let mut state = ListState::default().with_selected(Some(app.project_sel));
    f.render_stateful_widget(
        List::new(items)
            .block(pane_block("Projects", app.focus == Pane::Projects))
            .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        area,
        &mut state,
    );
}

fn draw_sessions(f: &mut Frame, app: &App, area: Rect) {
    let width = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|s| {
            let mark = if app.marked.contains(&s.id) {
                "[x] "
            } else {
                "[ ] "
            };
            let title = Line::from(vec![
                Span::styled(mark, Style::default().fg(Color::Yellow)),
                Span::raw(truncate(&s.title, width.saturating_sub(4))),
            ]);
            let meta = Line::from(Span::styled(
                format!(
                    "    {}  {} msgs  {}{}",
                    s.activity().with_timezone(&Local).format("%Y-%m-%d %H:%M"),
                    s.user_msgs + s.assistant_msgs,
                    human_bytes(s.size_bytes),
                    if s.is_recent() { "  ACTIVE?" } else { "" },
                ),
                Style::default().fg(Color::DarkGray),
            ));
            ListItem::new(vec![title, meta])
        })
        .collect();

    let title = format!(
        "Sessions ({}) · sort:{}{}",
        app.visible.len(),
        app.sort.label(),
        if app.filter.is_empty() {
            String::new()
        } else {
            format!(" · /{}", app.filter)
        }
    );
    let mut state = ListState::default().with_selected(Some(app.session_sel));
    f.render_stateful_widget(
        List::new(items)
            .block(pane_block(&title, app.focus == Pane::Sessions))
            .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        area,
        &mut state,
    );
}

fn draw_preview(f: &mut Frame, app: &mut App, area: Rect) {
    let block = pane_block("Preview", app.focus == Pane::Preview);
    if app.preview.is_empty() {
        f.render_widget(Paragraph::new("no session selected").block(block), area);
        return;
    }

    // Clamp here rather than at keypress time: this is where the content
    // length is known, so `G` and a held `j` can never land on a blank pane.
    app.preview_scroll = app.preview_scroll.min(app.max_scroll());

    // Only the lines that can appear are cloned; the rest stay cached. The
    // slack covers logical lines that wrap onto several rows.
    let start = app.preview_scroll as usize;
    let budget = usize::from(area.height).saturating_mul(3).max(16);
    let window: Vec<Line> = app
        .preview
        .iter()
        .skip(start)
        .take(budget)
        .cloned()
        .collect();

    f.render_widget(
        Paragraph::new(window)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn entry_lines(entry: &Entry) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    match &entry.event {
        Event::User(text) => vec![
            Line::from(Span::styled(
                "▌ user",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(text.clone()),
            Line::from(""),
        ],
        Event::Assistant(text) => vec![
            Line::from(Span::styled(
                "▌ claude",
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(text.clone()),
            Line::from(""),
        ],
        Event::Thinking(text) => vec![
            Line::from(Span::styled(
                format!("~ {}", truncate(text, 400)),
                Style::default().fg(Color::Magenta),
            )),
            Line::from(""),
        ],
        Event::ToolUse { name, headline, .. } => vec![Line::from(vec![
            Span::styled(format!("▸ {name}: "), Style::default().fg(Color::Yellow)),
            Span::styled(headline.clone(), dim),
        ])],
        Event::ToolResult {
            is_error, preview, ..
        } => {
            let style = if *is_error {
                Style::default().fg(Color::Red)
            } else {
                dim
            };
            vec![Line::from(Span::styled(
                format!("  ↳ {}", truncate(preview, 200)),
                style,
            ))]
        }
        Event::Meta(label) => vec![Line::from(Span::styled(format!("· {label}"), dim))],
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let marked_bytes: u64 = app
        .index
        .sessions
        .iter()
        .filter(|s| app.marked.contains(&s.id))
        .map(|s| s.size_bytes)
        .sum();

    let hint = match app.mode {
        Mode::Filter => "type to filter · Enter accept · Esc clear".to_string(),
        _ => "tab panes · j/k move · space mark · a/A all/none · d delete · / filter · s sort · r reload · q quit".to_string(),
    };
    let left = if app.marked.is_empty() {
        "nothing marked".to_string()
    } else {
        format!(
            "{} marked · {}",
            app.marked.len(),
            human_bytes(marked_bytes)
        )
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(left, Style::default().fg(Color::Yellow)),
            Span::raw("   "),
            Span::styled(app.status.clone(), Style::default().fg(Color::Green)),
        ]),
        Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_confirm(f: &mut Frame, plans: &[del::DeletePlan]) {
    let bytes: u64 = plans.iter().map(|p| p.bytes).sum();
    let files: usize = plans.iter().map(|p| p.paths.len()).sum();
    let live = plans.iter().filter(|p| p.recent).count();

    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                "Move {} session(s) — {files} paths, {} — to the recycle bin?",
                plans.len(),
                human_bytes(bytes)
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    if live > 0 {
        lines.push(Line::from(Span::styled(
            format!("WARNING: {live} of these were active in the last 5 minutes"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
    }
    for p in plans.iter().take(8) {
        lines.push(Line::from(format!(
            "  {}  {}",
            p.short_id(),
            truncate(&p.title, 50)
        )));
    }
    if plans.len() > 8 {
        lines.push(Line::from(format!("  … and {} more", plans.len() - 8)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "y confirm · any other key cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let area = centered(70, lines.len() as u16 + 2, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" Confirm delete "),
        ),
        area,
    );
}

fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}
