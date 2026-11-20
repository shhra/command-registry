//! Two-pane TUI: commands on the left, details + last output on the right.

use crate::runner::{self, fmt_duration};
use crate::store::{Command, Run, Session, Store};
use anyhow::Result;
use chrono::{DateTime, Local};
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::{
        event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::io::{self, Write};

#[derive(PartialEq)]
enum Mode {
    Normal,
    Filter,
    ConfirmRun,
    ConfirmDelete,
}

struct Detail {
    id: i64,
    runs: Vec<Run>,
    last_output: String,
}

struct App {
    store: Store,
    db: std::path::PathBuf,
    sessions: Vec<Session>,
    /// None: every command; Some(i): sessions[i]'s commands, in order
    scope: Option<usize>,
    all: Vec<Command>,
    visible: Vec<usize>,
    list: ListState,
    filter: String,
    mode: Mode,
    scroll: u16,
    status: String,
    detail: Option<Detail>,
    quit: bool,
}

pub fn run(store: Store, session: Option<String>, db: std::path::PathBuf) -> Result<()> {
    let mut app = App {
        store,
        db,
        sessions: vec![],
        scope: None,
        all: vec![],
        visible: vec![],
        list: ListState::default(),
        filter: String::new(),
        mode: Mode::Normal,
        scroll: 0,
        status: String::new(),
        detail: None,
        quit: false,
    };
    app.sessions = app.store.sessions()?;
    app.scope = session.and_then(|n| app.sessions.iter().position(|s| s.name == n));
    app.reload(None)?;
    let mut terminal = ratatui::init();
    let res = app.main_loop(&mut terminal);
    ratatui::restore();
    res
}

impl App {
    fn selected(&self) -> Option<&Command> {
        self.list.selected().and_then(|i| self.visible.get(i)).map(|&i| &self.all[i])
    }

    fn reload(&mut self, keep: Option<i64>) -> Result<()> {
        let keep = keep.or_else(|| self.selected().map(|c| c.id));
        let scoped = self.scope.and_then(|i| self.sessions.get(i)).map(|s| s.name.clone());
        self.sessions = self.store.sessions()?;
        self.scope = scoped.and_then(|n| self.sessions.iter().position(|s| s.name == n));
        self.all = match self.scope {
            Some(i) => self.store.session_commands(self.sessions[i].id)?,
            None => self.store.list()?,
        };
        self.apply_filter(keep);
        Ok(())
    }

    fn apply_filter(&mut self, keep: Option<i64>) {
        let f = self.filter.to_lowercase();
        self.visible = (0..self.all.len())
            .filter(|&i| {
                let c = &self.all[i];
                f.is_empty()
                    || c.name.to_lowercase().contains(&f)
                    || c.description.to_lowercase().contains(&f)
                    || c.command.to_lowercase().contains(&f)
                    || c.tags.iter().any(|t| t.to_lowercase().contains(&f))
            })
            .collect();
        let pos = keep.and_then(|id| self.visible.iter().position(|&i| self.all[i].id == id));
        self.list.select(match (pos, self.visible.is_empty()) {
            (_, true) => None,
            (Some(p), _) => Some(p),
            (None, _) => Some(0),
        });
        self.detail = None;
        self.scroll = 0;
    }

    fn ensure_detail(&mut self) {
        let Some(id) = self.selected().map(|c| c.id) else {
            self.detail = None;
            return;
        };
        if self.detail.as_ref().is_some_and(|d| d.id == id) {
            return;
        }
        let runs = self.store.runs(id, 20, false).unwrap_or_default();
        let last_output = runs.first().and_then(|r| self.store.run_output(r.id).ok()).unwrap_or_default();
        self.detail = Some(Detail { id, runs, last_output });
    }

    fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let cur = self.list.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, self.visible.len() as isize - 1) as usize;
        if Some(next) != self.list.selected() {
            self.list.select(Some(next));
            self.scroll = 0;
        }
    }

    fn main_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.ensure_detail();
            terminal.draw(|f| self.draw(f))?;
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    self.on_key(key, terminal)?;
                }
            }
        }
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return Ok(());
        }
        match self.mode {
            Mode::Filter => match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.mode = Mode::Normal;
                    let keep = self.selected().map(|c| c.id);
                    self.apply_filter(keep);
                }
                KeyCode::Enter => self.mode = Mode::Normal,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.apply_filter(None);
                }
                KeyCode::Up => self.move_by(-1),
                KeyCode::Down => self.move_by(1),
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.apply_filter(None);
                }
                _ => {}
            },
            Mode::ConfirmRun => {
                self.mode = Mode::Normal;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                    self.run_selected(terminal)?;
                }
            }
            Mode::ConfirmDelete => {
                self.mode = Mode::Normal;
                if key.code == KeyCode::Char('y') {
                    if let Some(name) = self.selected().map(|c| c.name.clone()) {
                        self.store.remove(&name)?;
                        self.status = format!("removed {name}");
                        self.reload(None)?;
                    }
                }
            }
            Mode::Normal => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    if !self.filter.is_empty() {
                        self.filter.clear();
                        let keep = self.selected().map(|c| c.id);
                        self.apply_filter(keep);
                    } else {
                        self.quit = true;
                    }
                }
                KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
                KeyCode::Char('g') | KeyCode::Home => self.move_by(-(self.visible.len() as isize)),
                KeyCode::Char('G') | KeyCode::End => self.move_by(self.visible.len() as isize),
                KeyCode::Char('J') | KeyCode::PageDown => self.scroll = self.scroll.saturating_add(5),
                KeyCode::Char('K') | KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
                KeyCode::Char('/') => {
                    self.mode = Mode::Filter;
                    self.status.clear();
                }
                KeyCode::Enter | KeyCode::Char('x') if self.selected().is_some() => {
                    self.mode = Mode::ConfirmRun;
                    self.status.clear();
                }
                KeyCode::Char('d') if self.selected().is_some() => self.mode = Mode::ConfirmDelete,
                KeyCode::Char('o') => self.open_log_in_pager(terminal)?,
                KeyCode::Char('s') => {
                    self.scope = match self.scope {
                        None if !self.sessions.is_empty() => Some(0),
                        Some(i) if i + 1 < self.sessions.len() => Some(i + 1),
                        _ => None,
                    };
                    self.filter.clear();
                    self.reload(None)?;
                    self.status = match self.scope {
                        Some(i) => format!("session {}", self.sessions[i].name),
                        None => "all commands".into(),
                    };
                }
                KeyCode::Char('H') => self.open_session_log(terminal)?,
                KeyCode::Char('r') => {
                    self.reload(None)?;
                    self.status = "reloaded".into();
                }
                _ => {}
            },
        }
        Ok(())
    }

    fn run_selected(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let Some(cmd) = self.selected().cloned() else { return Ok(()) };
        let store = &self.store;
        let db = self.db.clone();
        let result = suspended(terminal, || {
            let r = runner::run(store, &cmd, &[], "user", &db, false);
            if let Err(e) = &r {
                eprintln!("\x1b[31mcr: {e:#}\x1b[0m");
            }
            eprint!("\x1b[2m── press Enter to return to cr ──\x1b[0m");
            let _ = io::stderr().flush();
            let mut s = String::new();
            let _ = io::stdin().read_line(&mut s);
            r
        })?;
        self.status = match result {
            Ok(o) => format!(
                "{} exited {} in {}",
                cmd.name,
                o.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "by signal".into()),
                fmt_duration(o.duration_ms)
            ),
            Err(e) => format!("{e:#}").lines().next().unwrap_or_default().to_string(),
        };
        self.reload(Some(cmd.id))
    }

    fn open_log_in_pager(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        self.ensure_detail();
        let Some(d) = &self.detail else { return Ok(()) };
        let Some(run) = d.runs.first() else {
            self.status = "no runs yet".into();
            return Ok(());
        };
        let text = format!(
            "# {} · exit {} · {}\n$ {}\n\n{}",
            run.started_at.format("%Y-%m-%d %H:%M:%S"),
            run.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
            fmt_duration(run.duration_ms),
            run.command_text,
            d.last_output
        );
        page(terminal, &text)
    }

    /// The session in scope (else the selected command's) as a transcript in the pager.
    fn open_session_log(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let name = match self.scope {
            Some(i) => Some(self.sessions[i].name.clone()),
            None => self.selected().and_then(|c| c.session.clone()),
        };
        let Some(name) = name else {
            self.status = "no session here (s picks one)".into();
            return Ok(());
        };
        let s = self.store.session_require(&name)?;
        let text = crate::session_transcript(&self.store, &s, true)?;
        page(terminal, &text)
    }


    // ───────────────────────────── drawing ─────────────────────────────

    fn draw(&mut self, f: &mut Frame) {
        let [main, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)]).areas(main);

        self.draw_list(f, left);
        self.draw_detail(f, right);
        self.draw_footer(f, footer);

        match self.mode {
            Mode::ConfirmRun => self.draw_confirm_run(f),
            Mode::ConfirmDelete => {
                if let Some(c) = self.selected() {
                    let lines = vec![
                        Line::from(vec!["Delete ".into(), c.name.clone().bold(), " and its run history?".into()]),
                        Line::from(""),
                        Line::from("[y] delete   [any] cancel".dim()),
                    ];
                    popup(f, " Delete ", lines, Color::Red);
                }
            }
            _ => {}
        }
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        let title = if self.mode == Mode::Filter || !self.filter.is_empty() {
            format!(" Commands /{}{} ", self.filter, if self.mode == Mode::Filter { "▏" } else { "" })
        } else {
            match self.scope {
                Some(i) => format!(" Session {} ({}) ", self.sessions[i].name, self.all.len()),
                None => format!(" Commands ({}) ", self.all.len()),
            }
        };
        let numbered = self.scope.is_some();
        let items: Vec<ListItem> = self
            .visible
            .iter()
            .map(|&i| {
                let c = &self.all[i];
                let (mark, color) = match &c.last_run {
                    Some(r) if !r.finished => ("●", Color::Yellow),
                    Some(r) if r.exit_code == Some(0) => ("✓", Color::Green),
                    Some(_) => ("✗", Color::Red),
                    None => ("·", Color::DarkGray),
                };
                let step = if numbered { format!("{:>2}. ", i + 1) } else { String::new() };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::styled(step, Style::default().fg(Color::DarkGray)),
                    Span::raw(c.name.clone()),
                ]))
            })
            .collect();
        let empty = items.is_empty();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
            .highlight_symbol("▌");
        f.render_stateful_widget(list, area, &mut self.list);
        if empty {
            let msg = if self.all.is_empty() {
                "No commands yet.\n\nRegister one with\n  cr add NAME --cmd '...'\nor /cr-register in Claude."
            } else {
                "No matches."
            };
            let inner = area.inner(ratatui::layout::Margin { horizontal: 2, vertical: 2 });
            f.render_widget(Paragraph::new(msg).dim().wrap(Wrap { trim: false }), inner);
        }
    }

    fn draw_detail(&self, f: &mut Frame, area: Rect) {
        let Some(c) = self.selected() else {
            f.render_widget(Block::default().borders(Borders::ALL).title(" Description "), area);
            return;
        };
        let detail = self.detail.as_ref().filter(|d| d.id == c.id);
        let has_output = detail.is_some_and(|d| !d.runs.is_empty());
        let [top, bottom] = if has_output {
            Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area)
        } else {
            [area, Rect::default()]
        };

        let label = |s: &str| Span::styled(format!("{s:<10}"), Style::default().fg(Color::DarkGray));
        let mut lines: Vec<Line> = vec![
            Line::from(c.name.clone().bold().fg(Color::Cyan)),
            Line::from(if c.description.is_empty() { "(no description)".dim() } else { c.description.clone().into() }),
            Line::from(""),
        ];
        for (i, cl) in c.command.lines().enumerate() {
            let prefix = if i == 0 { "$ " } else { "  " };
            lines.push(Line::from(vec![prefix.dim(), cl.to_string().fg(Color::Yellow)]));
        }
        if c.redacted {
            lines.push(Line::from("  ⚠ contains a redacted secret — not replayable".fg(Color::Red)));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(vec![label("cwd"), c.cwd.clone().unwrap_or_else(|| "(where cr runs)".into()).into()]));
        if let Some(s) = c.session.as_ref().and_then(|n| self.sessions.iter().find(|s| &s.name == n)) {
            lines.push(Line::from(vec![label("session"), s.name.clone().fg(Color::Cyan)]));
            for (k, v) in &s.env {
                lines.push(Line::from(vec![label(""), format!("{k}=").fg(Color::Magenta), v.clone().into()]));
            }
        }
        if !c.tags.is_empty() {
            lines.push(Line::from(vec![label("tags"), c.tags.join(", ").into()]));
        }
        lines.push(Line::from(vec![
            label("added"),
            format!("{} by {}", fmt_time(&c.created_at), c.source).into(),
        ]));
        let last = match &c.last_run {
            Some(r) => {
                let code = r.exit_code.map(|c| format!("exit {c}")).unwrap_or_else(|| "killed".into());
                let color = if r.exit_code == Some(0) { Color::Green } else { Color::Red };
                Line::from(vec![
                    label("last run"),
                    format!("{} ({}) · ", fmt_time(&r.started_at), ago(&r.started_at)).into(),
                    code.fg(color),
                    format!(" · {} · by {} · {} run(s)", fmt_duration(r.duration_ms), r.by, c.run_count).into(),
                ])
            }
            None => Line::from(vec![label("last run"), "never".dim()]),
        };
        lines.push(last);

        if !c.args.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("Arguments".bold()));
            let w = c.args.iter().map(|a| a.name.len()).max().unwrap_or(0);
            for a in &c.args {
                lines.push(Line::from(vec![
                    format!("  {:<w$}  ", a.name).fg(Color::Magenta),
                    a.description.clone().into(),
                ]));
            }
        }
        if !c.instructions.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("Instructions".bold()));
            for l in c.instructions.lines() {
                lines.push(Line::from(format!("  {l}")));
            }
        }
        if let Some(d) = detail.filter(|d| d.runs.len() > 1) {
            lines.push(Line::from(""));
            lines.push(Line::from("History".bold()));
            for r in &d.runs {
                let (mark, color) =
                    if r.exit_code == Some(0) { ("✓", Color::Green) } else { ("✗", Color::Red) };
                lines.push(Line::from(vec![
                    format!("  {mark} ").fg(color),
                    format!(
                        "{}  {:>7}  by {}",
                        r.started_at.format("%Y-%m-%d %H:%M"),
                        fmt_duration(r.duration_ms),
                        r.by
                    )
                    .into(),
                ]));
            }
        }

        let para = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Description "))
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        f.render_widget(para, top);

        if let (true, Some(d)) = (has_output, detail) {
            let r = &d.runs[0];
            let title = format!(
                " Last output · {} · exit {}{} ",
                ago(&r.started_at),
                r.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
                if r.truncated { " · truncated" } else { "" }
            );
            let h = bottom.height.saturating_sub(2) as usize;
            let all: Vec<&str> = d.last_output.lines().collect();
            let tail: Vec<Line> = all[all.len().saturating_sub(h)..]
                .iter()
                .map(|l| Line::from(l.replace('\t', "    ")))
                .collect();
            let body = if tail.is_empty() { vec![Line::from("(no output)".dim())] } else { tail };
            f.render_widget(
                Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(title)),
                bottom,
            );
        }
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let line = if !self.status.is_empty() && self.mode == Mode::Normal {
            Line::from(format!(" {}", self.status)).fg(Color::Yellow)
        } else {
            let keys: &[(&str, &str)] = match self.mode {
                Mode::Filter => &[("type", "filter"), ("Enter", "keep"), ("Esc", "clear")],
                _ => &[
                    ("j/k", "move"),
                    ("Enter", "run"),
                    ("/", "filter"),
                    ("J/K", "scroll"),
                    ("o", "full log"),
                    ("s", "session"),
                    ("H", "session log"),
                    ("d", "delete"),
                    ("r", "reload"),
                    ("q", "quit"),
                ],
            };
            let mut spans = vec![Span::raw(" ")];
            for (k, v) in keys {
                spans.push(k.to_string().bold().fg(Color::Cyan));
                spans.push(format!(" {v}  ").dim());
            }
            Line::from(spans)
        };
        f.render_widget(Paragraph::new(line), area);
    }

    fn draw_confirm_run(&self, f: &mut Frame) {
        let Some(c) = self.selected() else { return };
        let mut lines = vec![Line::from(vec!["Run ".into(), c.name.clone().bold(), "?".into()]), Line::from("")];
        for l in c.command.lines() {
            lines.push(Line::from(format!("$ {l}")).fg(Color::Yellow));
        }
        lines.push(Line::from(format!("in {}", c.cwd.as_deref().unwrap_or("(current directory)"))).dim());
        if let Some(s) = c.session.as_ref().and_then(|n| self.sessions.iter().find(|s| &s.name == n)) {
            for (k, v) in &s.env {
                lines.push(Line::from(format!("env {k}={v}")).dim());
            }
        }
        lines.push(Line::from(""));
        if c.redacted {
            lines.push(Line::from("⚠ has a redacted secret; this will be refused".fg(Color::Red)));
        }
        lines.push(Line::from("[y/Enter] run   [any] cancel".dim()));
        popup(f, " Confirm ", lines, Color::Cyan);
    }
}

/// `text` in $PAGER (default `less -R`), the TUI suspended meanwhile.
fn page(terminal: &mut DefaultTerminal, text: &str) -> Result<()> {
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less -R".into());
    suspended(terminal, || {
        let mut parts = pager.split_whitespace();
        let Some(bin) = parts.next() else { return };
        if let Ok(mut child) = std::process::Command::new(bin).args(parts).stdin(std::process::Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    })
}

/// Leave the TUI, hand the real terminal to `f`, then come back.
fn suspended<T>(terminal: &mut DefaultTerminal, f: impl FnOnce() -> T) -> Result<T> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    let out = f();
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(out)
}

fn popup(f: &mut Frame, title: &str, lines: Vec<Line>, color: Color) {
    let area = f.area();
    let w = area.width.saturating_sub(8).min(90);
    let h = (lines.len() as u16 + 4).min(area.height);
    let rect = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height.saturating_sub(h)) / 2, width: w, height: h };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title.to_string()).border_style(Style::default().fg(color)))
            .style(Style::default()),
        rect,
    );
}

fn fmt_time(dt: &DateTime<Local>) -> String {
    dt.format("%Y-%m-%d %H:%M").to_string()
}

fn ago(dt: &DateTime<Local>) -> String {
    let s = (Local::now() - *dt).num_seconds().max(0);
    match s {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}
