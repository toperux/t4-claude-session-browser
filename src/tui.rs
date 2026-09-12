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
use std::collections::HashSet;
use std::time::Duration;

use crate::del::{self, human_bytes, PlanSummary};
use crate::index::{truncate, Index, Project, SessionMeta, Sort};
use crate::paths::ClaudeDir;
use crate::transcript::{self, Entry, Event, LoadOpts};

#[derive(PartialEq, Clone, Copy)]
enum Pane {
    Projects,
    Sessions,
    Preview,
}

enum Mode {
    Browse,
    Filter,
    Confirm(Vec<del::DeletePlan>),
}

struct App {
    dir: ClaudeDir,
    index: Index,
    project_rows: Vec<Project>,
    /// 0 is the "all projects" row; `n` is `project_rows[n - 1]`.
    project_sel: usize,
    visible: Vec<SessionMeta>,
    session_sel: usize,
    marked: HashSet<String>,
    /// Rendered once per selection, not once per frame.
    preview: Vec<Line<'static>>,
    preview_truncated: bool,
    preview_scroll: usize,
    /// Session `preview` was built for. Filter, sort and reload rebuild the
    /// list without necessarily moving the highlight, and re-parsing a large
    /// transcript per keystroke is what this remembers enough to skip.
    preview_for: Option<String>,
    #[cfg(test)]
    preview_loads: usize,
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
            preview_for: None,
            #[cfg(test)]
            preview_loads: 0,
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
        let slug = self.selected_slug().map(str::to_owned);
        self.project_rows = self.index.projects();
        // `projects()` orders by newest session, so a rebuild can put a
        // different project on the row the cursor sits on. Re-resolve it from
        // the slug; a project that is gone falls back to row 0, "all projects".
        self.project_sel = slug
            .and_then(|slug| self.project_rows.iter().position(|p| p.slug == slug))
            .map_or(0, |i| i + 1);
    }

    fn selected_slug(&self) -> Option<&str> {
        let p = self.project_rows.get(self.project_sel.checked_sub(1)?)?;
        Some(&p.slug)
    }

    /// Point the list at another project, always landing on its top row.
    /// `refilter` would re-resolve the highlighted id instead, and on a project
    /// change that id belongs to the list being left.
    fn select_project(&mut self, sel: usize) {
        self.project_sel = sel;
        self.visible = self
            .index
            .filter(self.selected_slug(), &self.filter, self.sort);
        self.session_sel = 0;
        self.sync_preview();
    }

    fn refilter(&mut self) {
        // The sort key and the index both reorder the list, so the row number
        // alone would silently re-point the cursor at another session. Keep the
        // highlight on the same id; one the new list does not hold leaves the
        // clamped row in place.
        let id = self.current().map(|s| s.id.clone());
        self.visible = self
            .index
            .filter(self.selected_slug(), &self.filter, self.sort);
        self.session_sel = self.session_sel.min(self.visible.len().saturating_sub(1));
        if let Some(i) = id.and_then(|id| self.visible.iter().position(|s| s.id == id)) {
            self.session_sel = i;
        }
        self.sync_preview();
    }

    fn current(&self) -> Option<&SessionMeta> {
        self.visible.get(self.session_sel)
    }

    /// Reload the preview only when the highlight moved to another session.
    /// Filter typing, sorting and a drained run of `j` repeats all resolve back
    /// to an id that is already rendered. `reload_index` clears `preview_for` to
    /// force the re-read a growing live transcript needs.
    fn sync_preview(&mut self) {
        let id = self.current().map(|s| s.id.clone());
        if id.is_some() && id == self.preview_for {
            return;
        }
        self.load_preview();
    }

    fn load_preview(&mut self) {
        #[cfg(test)]
        {
            self.preview_loads += 1;
        }
        self.preview.clear();
        self.preview_truncated = false;
        self.preview_scroll = 0;
        self.preview_for = self.current().map(|s| s.id.clone());
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

    /// Highest scroll offset that still leaves a line on screen. `preview` holds
    /// one entry per source line, so the only remaining under-estimate is a
    /// single source line long enough to wrap onto several rows.
    fn max_scroll(&self) -> usize {
        self.preview.len().saturating_sub(1)
    }

    fn reload_index(&mut self) -> Result<()> {
        self.index = Index::build(&self.dir)?;
        if let Some(summary) = self.index.warning_summary() {
            self.status = summary;
        }
        self.marked
            .retain(|id| self.index.sessions.iter().any(|s| &s.id == id));
        self.rebuild_projects();
        // A live session's file has grown since it was rendered, so this is the
        // one path that must re-read the transcript for the same id.
        self.preview_for = None;
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
        feed(app, event::read()?);
        // A held j/k queues repeats faster than a transcript parses. Handle the
        // whole burst, then load the preview once for wherever the cursor came
        // to rest, instead of parsing every session it passed over.
        // A `q` in the middle of the burst ends the session: the rest of the
        // queued type-ahead is not for this program, and the preview for a
        // cursor nobody will see is a transcript parse on the way out.
        while !app.quit && event::poll(Duration::ZERO)? {
            feed(app, event::read()?);
        }
        if !app.quit {
            app.sync_preview();
        }
    }
    Ok(())
}

fn feed(app: &mut App, event: TermEvent) {
    if let TermEvent::Key(key) = event {
        if key.kind == KeyEventKind::Press {
            handle_key(app, key);
        }
    }
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
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.quit = true;
                }
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
                    let outcome = del::execute_all(&app.dir, &plans);
                    app.marked.clear();
                    app.status = outcome.summary(del::summarize(&plans).bytes);
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
        KeyCode::Char('q') => app.quit = true,
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
                    app.sync_preview();
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
            let targets = app.index.marked_or(&app.marked, app.current());
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
            let next = step(app.project_sel, app.project_rows.len() + 1);
            if next != app.project_sel {
                app.select_project(next);
            }
        }
        // Moving the cursor does not load the preview; `event_loop` does that
        // once the whole burst of queued repeats has been handled.
        Pane::Sessions => app.session_sel = step(app.session_sel, app.visible.len()),
        Pane::Preview => {
            app.preview_scroll = if delta < 0 {
                app.preview_scroll
                    .saturating_sub(delta.unsigned_abs() as usize)
            } else {
                app.preview_scroll
                    .saturating_add(delta as usize)
                    .min(app.max_scroll())
            };
        }
    }
}

fn jump(app: &mut App, top: bool) {
    match app.focus {
        Pane::Projects => {
            app.select_project(if top { 0 } else { app.project_rows.len() });
        }
        Pane::Sessions => {
            app.session_sel = if top {
                0
            } else {
                app.visible.len().saturating_sub(1)
            };
            app.sync_preview();
        }
        Pane::Preview => app.preview_scroll = if top { 0 } else { app.max_scroll() },
    }
}

// ---------------------------------------------------------------- drawing

fn draw(f: &mut Frame, app: &mut App) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(f.area());
    // Minimums, not percentages: at 80 columns a 33% sessions pane has 24 inner
    // cells and cuts the metadata line off after the date. The sessions pane
    // keeps enough room for a short date plus msgs and size; the preview takes
    // what is left over.
    let cols = Layout::horizontal([
        Constraint::Length(22),
        Constraint::Min(34),
        Constraint::Min(30),
    ])
    .split(rows[0]);

    draw_projects(f, app, cols[0]);
    draw_sessions(f, app, cols[1]);
    draw_preview(f, app, cols[2]);
    draw_status(f, app, rows[1]);

    if let Mode::Confirm(plans) = &app.mode {
        draw_confirm(f, plans, &app.visible);
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
    let all_label = format!("All projects ({})", app.index.sessions.len());
    let all = (
        all_label.as_str(),
        app.index.sessions.len(),
        app.index.sessions.iter().map(|s| s.size_bytes).sum::<u64>(),
    );
    let rows = app
        .project_rows
        .iter()
        .map(|p| (p.label.as_str(), p.count, p.bytes));
    let items: Vec<ListItem> = std::iter::once(all)
        .chain(rows)
        .map(|(label, count, bytes)| {
            ListItem::new(vec![
                Line::from(truncate(label, area.width.saturating_sub(4) as usize)),
                Line::from(Span::styled(
                    format!("  {count} sessions · {}", human_bytes(bytes)),
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
            // `ACTIVE?` leads: it is the one part of this line that must survive
            // a narrow pane, and the year is what a narrow pane can spare.
            let stamp = if width < 40 {
                "%m-%d %H:%M"
            } else {
                "%Y-%m-%d %H:%M"
            };
            let meta = Line::from(Span::styled(
                format!(
                    "{}{}  {} msgs  {}",
                    if s.is_recent() { "  ACTIVE? " } else { "  " },
                    s.activity().with_timezone(&Local).format(stamp),
                    s.user_msgs + s.assistant_msgs,
                    human_bytes(s.size_bytes),
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
    // slack covers source lines that wrap onto several rows.
    let start = app.preview_scroll;
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

/// One `Line` per source line. ratatui treats a `\n` inside a `Line` as
/// zero-width whitespace, so a whole message packed into one `Line` is one
/// scroll step no matter how many rows it renders as - and `max_scroll` counts
/// lines, so its tail would be unreachable.
fn styled_lines(text: &str, style: Style) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|l| Line::from(Span::styled(l.to_owned(), style)))
        .collect()
}

fn entry_lines(entry: &Entry) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let body = |header: Span<'static>, text: &str| {
        let mut lines = vec![Line::from(header)];
        lines.extend(styled_lines(text, Style::default()));
        lines.push(Line::from(""));
        lines
    };
    match &entry.event {
        Event::User(text) => body(
            Span::styled(
                "▌ user",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            text,
        ),
        Event::Assistant(text) => body(
            Span::styled(
                "▌ claude",
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
            text,
        ),
        Event::Thinking(text) => {
            let mut lines = styled_lines(
                &format!("~ {}", truncate(text, 400)),
                Style::default().fg(Color::Magenta),
            );
            lines.push(Line::from(""));
            lines
        }
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
            styled_lines(&format!("  ↳ {}", truncate(preview, 200)), style)
        }
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

fn draw_confirm(f: &mut Frame, plans: &[del::DeletePlan], visible: &[SessionMeta]) {
    let PlanSummary { bytes, files, live } = del::summarize(plans);
    let warn = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);

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
    // Marks are global, so a project or filter change leaves sessions marked
    // that the list no longer shows. Say how many are being taken on trust.
    let off_view = plans
        .iter()
        .filter(|p| !visible.iter().any(|s| s.id == p.id))
        .count();
    if off_view > 0 {
        lines.push(Line::from(Span::styled(
            format!("{off_view} of these are outside the current view"),
            warn,
        )));
        lines.push(Line::from(""));
    }
    if live > 0 {
        lines.push(Line::from(Span::styled(
            format!("WARNING: {live} of these were active in the last 5 minutes"),
            warn,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Two projects of two sessions each, every session a different size and a
    /// different age, so `s` and `r` really do reorder the lists under the
    /// cursor. Date order is a1, a2, b1, b2; size and msgs order is the reverse.
    fn tree() -> (tempfile::TempDir, ClaudeDir) {
        let tmp = tempfile::tempdir().unwrap();
        // (slug, id, records, day) - more records is a bigger file.
        let sessions = [
            ("-src-alpha", "a1", 1, 4),
            ("-src-alpha", "a2", 2, 3),
            ("-src-beta", "b1", 3, 2),
            ("-src-beta", "b2", 4, 1),
        ];
        for (slug, id, records, day) in sessions {
            let dir = tmp.path().join("projects").join(slug);
            std::fs::create_dir_all(&dir).unwrap();
            let body: String = (0..records)
                .map(|_| record(slug, id, &format!("2026-01-0{day}T00:00:00Z")))
                .collect();
            std::fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
        }
        let dir = ClaudeDir::resolve(Some(tmp.path())).unwrap();
        (tmp, dir)
    }

    fn record(slug: &str, id: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{ts}","cwd":"/p/{slug}","message":{{"content":"hello {id}"}}}}
"#
        )
    }

    /// Make one session the newest in the tree, so `projects()` reorders.
    fn touch_newest(tmp: &tempfile::TempDir, slug: &str, id: &str) {
        let path = tmp
            .path()
            .join("projects")
            .join(slug)
            .join(format!("{id}.jsonl"));
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str(&record(slug, id, "2026-02-01T00:00:00Z"));
        std::fs::write(&path, body).unwrap();
    }

    fn press(app: &mut App, code: KeyCode) {
        handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn sort_keeps_the_same_session_selected() {
        let (_tmp, dir) = tree();
        let mut app = App::new(dir).unwrap();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().id, "a2");

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.current().unwrap().id, "a2");
        // Size order is the reverse of date order, so the row moved and only
        // re-resolving the id can have kept the highlight.
        assert_eq!(app.session_sel, 2);

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.current().unwrap().id, "a2");
    }

    #[test]
    fn project_switch_lands_on_the_top_session() {
        let (_tmp, dir) = tree();
        let mut app = App::new(dir).unwrap();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().id, "a2");

        app.focus = Pane::Projects;
        // Row 1 is alpha, row 2 is beta.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_slug(), Some("-src-beta"));
        assert_eq!(app.session_sel, 0);
        assert_eq!(app.current().unwrap().id, "b1");

        // Back to "all projects": beta's top session sits at row 2 of the full
        // list, so re-resolving the id would move the cursor off the top.
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.project_sel, 0);
        assert_eq!(app.session_sel, 0);
        assert_eq!(app.current().unwrap().id, "a1");
    }

    #[test]
    fn reload_keeps_the_same_project_selected() {
        let (tmp, dir) = tree();
        let mut app = App::new(dir).unwrap();
        app.focus = Pane::Projects;
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_slug(), Some("-src-beta"));

        touch_newest(&tmp, "-src-beta", "b1");
        press(&mut app, KeyCode::Char('r'));

        assert_eq!(app.selected_slug(), Some("-src-beta"));
        // beta is now the newest project, so it moved to the first row.
        assert_eq!(app.project_sel, 1);
    }

    #[test]
    fn reload_falls_back_to_all_when_the_project_is_gone() {
        let (tmp, dir) = tree();
        let mut app = App::new(dir).unwrap();
        app.focus = Pane::Projects;
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_slug(), Some("-src-beta"));

        std::fs::remove_dir_all(tmp.path().join("projects").join("-src-beta")).unwrap();
        press(&mut app, KeyCode::Char('r'));

        assert_eq!(app.project_sel, 0);
        assert_eq!(app.selected_slug(), None);
    }

    #[test]
    fn filter_typing_does_not_reload_the_preview() {
        let (_tmp, dir) = tree();
        let mut app = App::new(dir).unwrap();
        assert!(!app.preview.is_empty());
        let loads = app.preview_loads;
        let id = app.current().unwrap().id.clone();

        press(&mut app, KeyCode::Char('/'));
        // Every title matches, so the same session stays highlighted.
        press(&mut app, KeyCode::Char('h'));

        assert_eq!(app.current().unwrap().id, id);
        assert_eq!(app.preview_loads, loads);
    }

    #[test]
    fn confirm_returns_to_browse_on_any_key() {
        let (_tmp, dir) = tree();
        let mut app = App::new(dir).unwrap();
        let path = app.current().unwrap().path.clone();

        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(app.mode, Mode::Confirm(_)));

        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Browse));
        assert_eq!(app.status, "delete cancelled");
        assert!(path.exists());
    }

    #[test]
    fn entry_lines_splits_on_newlines() {
        let entry = Entry {
            ts: None,
            sidechain: false,
            event: Event::User("a\n\nb".into()),
        };
        let text: Vec<String> = entry_lines(&entry)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(text, ["▌ user", "a", "", "b", ""]);
    }
}
