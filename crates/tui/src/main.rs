//! DirLinker terminal UI.
//!
//! Source screen:  ↑↓/jk move · Enter/→/l open · Backspace/←/h up · Space select · Enter (with selection) or Tab → destination
//! Dest screen:    navigate the same way · t cycle link type · m toggle move/link-only · Ctrl+Enter (or Ctrl+J / F2) link
//!
//! Run:  dirlinker-tui [start-folder]

use dirlinker_core::fsinfo::{self, DriveInfo};
use dirlinker_core::links::{self, Analysis, LinkKind, Mode};
use dirlinker_core::listing::{self, Entry};
use dirlinker_core::ops::{self, ItemResult, Progress};
use dirlinker_core::size::{self, SizeInfo};
use dirlinker_core::util::{display_path, fmt_bytes};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Gauge, Paragraph, Row, Table, TableState, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SIZE_SLOT: usize = 2;

const ACCENT: Color = Color::Rgb(79, 140, 255);
const MUTED: Color = Color::Rgb(138, 150, 171);
const FOLDER: Color = Color::Rgb(245, 196, 81);
const JUNCTION: Color = Color::Rgb(45, 212, 191);
const SYMLINK: Color = Color::Rgb(177, 140, 255);
const HARDLINK: Color = Color::Rgb(251, 191, 36);
const BROKEN: Color = Color::Rgb(248, 113, 113);
const OK: Color = Color::Rgb(52, 211, 153);

#[derive(Clone)]
enum Item {
    Drive(DriveInfo),
    Up,
    Entry(Entry),
}

#[derive(Clone, Copy, PartialEq)]
enum SortKey {
    Name,
    Size,
    Modified,
}

struct Browser {
    path: Option<PathBuf>,
    items: Vec<Item>,
    state: TableState,
    fs: String,
}

impl Browser {
    fn new() -> Self {
        Browser { path: None, items: vec![], state: TableState::default().with_selected(Some(0)), fs: String::new() }
    }
    fn current(&self) -> Option<&Item> {
        self.state.selected().and_then(|i| self.items.get(i))
    }
    fn move_by(&mut self, d: isize) {
        if self.items.is_empty() {
            return;
        }
        let i = self.state.selected().unwrap_or(0) as isize + d;
        self.state.select(Some(i.clamp(0, self.items.len() as isize - 1) as usize));
    }
}

#[derive(PartialEq)]
enum Screen {
    Source,
    Dest,
    Confirm,
    Running,
    Results,
}

enum OpMsg {
    Progress(Progress),
    Done(Vec<ItemResult>),
}

struct App {
    screen: Screen,
    src: Browser,
    dst: Browser,
    selected: BTreeSet<String>,
    sizes: HashMap<String, Option<SizeInfo>>,
    size_gen: Arc<AtomicU64>,
    size_tx: Sender<(u64, String)>,
    size_rx: Receiver<(String, SizeInfo)>,
    sort: SortKey,
    reverse: bool,
    show_hidden: bool,
    mode: Mode,
    kind: Option<LinkKind>,
    analysis: Option<Analysis>,
    progress: Option<Progress>,
    results: Vec<ItemResult>,
    op_rx: Option<Receiver<OpMsg>>,
    message: Option<(String, bool, Instant)>,
    quit: bool,
}

fn main() -> std::io::Result<()> {
    let start = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());

    // Background worker for folder sizes. Jobs from an old directory are skipped via the generation counter.
    let (job_tx, job_rx) = mpsc::channel::<(u64, String)>();
    let (res_tx, res_rx) = mpsc::channel::<(String, SizeInfo)>();
    let size_gen = Arc::new(AtomicU64::new(0));
    let worker_gen = size_gen.clone();
    std::thread::spawn(move || {
        while let Ok((g, path)) = job_rx.recv() {
            if g != worker_gen.load(Ordering::Relaxed) {
                continue;
            }
            if let Some(s) = size::dir_size(Path::new(&path), Some(SIZE_SLOT)) {
                if res_tx.send((path, s)).is_err() {
                    break;
                }
            }
        }
    });

    let mut app = App {
        screen: Screen::Source,
        src: Browser::new(),
        dst: Browser::new(),
        selected: BTreeSet::new(),
        sizes: HashMap::new(),
        size_gen,
        size_tx: job_tx,
        size_rx: res_rx,
        sort: SortKey::Name,
        reverse: false,
        show_hidden: false,
        mode: Mode::Move,
        kind: None,
        analysis: None,
        progress: None,
        results: vec![],
        op_rx: None,
        message: None,
        quit: false,
    };
    app.load(false, start, None);

    let mut terminal = ratatui::init();
    // Lets terminals that support it report Ctrl+Enter distinctly (Windows always does).
    let enhanced = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::event::PushKeyboardEnhancementFlags(
            ratatui::crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    )
    .is_ok()
        && !cfg!(windows);
    let res = run(&mut terminal, &mut app);
    if enhanced {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    res
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    while !app.quit {
        app.pump();
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    app.on_key(k);
                }
            }
        }
    }
    Ok(())
}

impl App {
    fn browser(&mut self, dest: bool) -> &mut Browser {
        if dest { &mut self.dst } else { &mut self.src }
    }

    fn flash(&mut self, msg: impl Into<String>, err: bool) {
        self.message = Some((msg.into(), err, Instant::now()));
    }

    /// Load a folder (None = drives) into the source or destination browser.
    fn load(&mut self, dest: bool, path: Option<PathBuf>, focus: Option<String>) {
        let show_hidden = self.show_hidden;
        let mut items: Vec<Item> = Vec::new();
        let mut fs = String::new();
        let path = match path {
            None => {
                items.extend(fsinfo::list_drives().into_iter().map(Item::Drive));
                None
            }
            Some(p) => match listing::list_dir(&display_path(&p)) {
                Ok(l) => {
                    fs = l.fs.clone();
                    items.push(Item::Up);
                    items.extend(l.entries.into_iter().filter(|e| show_hidden || !e.hidden).map(Item::Entry));
                    Some(PathBuf::from(l.path))
                }
                Err(e) => {
                    self.flash(e, true);
                    return;
                }
            },
        };
        {
            let b = self.browser(dest);
            b.path = path;
            b.items = items;
            b.fs = fs;
        }
        self.sort_items(dest);
        let b = self.browser(dest);
        let idx = focus
            .and_then(|f| b.items.iter().position(|it| matches!(it, Item::Entry(e) if e.path.eq_ignore_ascii_case(&f))))
            .unwrap_or(0);
        b.state.select(Some(idx));
        if !dest {
            self.queue_sizes();
        }
        if dest {
            self.analyze();
        }
    }

    fn reload(&mut self, dest: bool) {
        let b = self.browser(dest);
        let p = b.path.clone();
        let focus = match b.current() {
            Some(Item::Entry(e)) => Some(e.path.clone()),
            _ => None,
        };
        self.load(dest, p, focus);
    }

    fn sort_items(&mut self, dest: bool) {
        let (key, rev) = (self.sort, self.reverse);
        let sizes = self.sizes.clone();
        let b = self.browser(dest);
        let size_of = |e: &Entry| -> u64 {
            if e.is_dir {
                sizes.get(&e.path).copied().flatten().map(|s| s.bytes).unwrap_or(0)
            } else {
                e.size.unwrap_or(0)
            }
        };
        let cur = match b.current() {
            Some(Item::Entry(e)) => Some(e.path.clone()),
            _ => None,
        };
        b.items.sort_by(|a, b| match (a, b) {
            (Item::Up, _) => std::cmp::Ordering::Less,
            (_, Item::Up) => std::cmp::Ordering::Greater,
            (Item::Entry(x), Item::Entry(y)) => {
                if x.is_dir != y.is_dir {
                    return y.is_dir.cmp(&x.is_dir);
                }
                let o = match key {
                    SortKey::Name => x.name.to_lowercase().cmp(&y.name.to_lowercase()),
                    SortKey::Size => size_of(y).cmp(&size_of(x)),
                    SortKey::Modified => y.modified.cmp(&x.modified),
                };
                if rev { o.reverse() } else { o }
            }
            _ => std::cmp::Ordering::Equal,
        });
        if let Some(c) = cur {
            if let Some(i) = b.items.iter().position(|it| matches!(it, Item::Entry(e) if e.path == c)) {
                b.state.select(Some(i));
            }
        }
    }

    fn queue_sizes(&mut self) {
        let g = self.size_gen.fetch_add(1, Ordering::Relaxed) + 1;
        size::cancel(SIZE_SLOT);
        for it in &self.src.items {
            if let Item::Entry(e) = it {
                if e.is_dir && e.link.is_none() && !matches!(self.sizes.get(&e.path), Some(Some(_))) {
                    self.sizes.insert(e.path.clone(), None);
                    let _ = self.size_tx.send((g, e.path.clone()));
                }
            }
        }
    }

    fn analyze(&mut self) {
        let sources: Vec<String> = self.selected.iter().cloned().collect();
        let dest = self.dst.path.as_deref().map(display_path).unwrap_or_default();
        let a = links::analyze(&sources, &dest, self.mode);
        let keep = self.kind.filter(|k| a.options.iter().any(|o| o.kind == *k && o.available));
        self.kind = keep.or(a.recommended);
        self.analysis = Some(a);
    }

    fn pump(&mut self) {
        let mut got = false;
        while let Ok((p, s)) = self.size_rx.try_recv() {
            self.sizes.insert(p, Some(s));
            got = true;
        }
        if got && self.sort == SortKey::Size {
            self.sort_items(false);
        }
        if let Some(rx) = &self.op_rx {
            let mut done = None;
            while let Ok(m) = rx.try_recv() {
                match m {
                    OpMsg::Progress(p) => self.progress = Some(p),
                    OpMsg::Done(r) => done = Some(r),
                }
            }
            if let Some(r) = done {
                self.op_rx = None;
                self.results = r;
                self.screen = Screen::Results;
                self.sizes.clear();
            }
        }
        if let Some((_, _, t)) = &self.message {
            if t.elapsed() > Duration::from_secs(6) {
                self.message = None;
            }
        }
    }

    fn open_current(&mut self, dest: bool) {
        let target = match self.browser(dest).current() {
            Some(Item::Drive(d)) => Some(Some(PathBuf::from(&d.mount))),
            Some(Item::Up) => {
                self.go_up(dest);
                return;
            }
            Some(Item::Entry(e)) if e.is_dir => Some(Some(PathBuf::from(&e.path))),
            _ => None,
        };
        if let Some(t) = target {
            self.load(dest, t, None);
        }
    }

    fn go_up(&mut self, dest: bool) {
        let b = self.browser(dest);
        let Some(p) = b.path.clone() else { return };
        let parent = p.parent().map(Path::to_path_buf);
        self.load(dest, parent, Some(display_path(&p)));
    }

    fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.screen {
            Screen::Source => self.key_browse(k, false),
            Screen::Dest => {
                let link = (ctrl && matches!(k.code, KeyCode::Enter | KeyCode::Char('j') | KeyCode::Char('m')))
                    || k.code == KeyCode::F(2);
                if link {
                    self.try_confirm();
                } else {
                    match k.code {
                        KeyCode::Char('t') | KeyCode::Tab => self.cycle_kind(),
                        KeyCode::Char('m') => {
                            self.mode = if self.mode == Mode::Move { Mode::Link } else { Mode::Move };
                            self.analyze();
                        }
                        KeyCode::Esc => self.screen = Screen::Source,
                        _ => self.key_browse(k, true),
                    }
                }
            }
            Screen::Confirm => match k.code {
                KeyCode::Char('y') | KeyCode::Enter => self.start(),
                KeyCode::Char('n') | KeyCode::Esc => self.screen = Screen::Dest,
                _ => {}
            },
            Screen::Running => {}
            Screen::Results => {
                self.selected.clear();
                self.screen = Screen::Source;
                self.reload(false);
                self.reload(true);
            }
        }
    }

    fn key_browse(&mut self, k: KeyEvent, dest: bool) {
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.browser(dest).move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.browser(dest).move_by(1),
            KeyCode::PageUp => self.browser(dest).move_by(-15),
            KeyCode::PageDown => self.browser(dest).move_by(15),
            KeyCode::Home | KeyCode::Char('g') => self.browser(dest).state.select(Some(0)),
            KeyCode::End | KeyCode::Char('G') => {
                let n = self.browser(dest).items.len();
                self.browser(dest).state.select(Some(n.saturating_sub(1)));
            }
            KeyCode::Right | KeyCode::Char('l') => self.open_current(dest),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => self.go_up(dest),
            KeyCode::Char('.') => {
                self.show_hidden = !self.show_hidden;
                self.reload(dest);
            }
            KeyCode::Char('s') => {
                self.sort = match self.sort {
                    SortKey::Name => SortKey::Size,
                    SortKey::Size => SortKey::Modified,
                    SortKey::Modified => SortKey::Name,
                };
                self.sort_items(dest);
            }
            KeyCode::Char('r') => {
                self.reverse = !self.reverse;
                self.sort_items(dest);
            }
            KeyCode::Enter if dest => self.open_current(true),
            KeyCode::Enter | KeyCode::Tab if !dest => {
                if self.selected.is_empty() {
                    if k.code == KeyCode::Enter {
                        self.open_current(false);
                    } else {
                        self.flash("Select something with Space first.", true);
                    }
                } else {
                    self.screen = Screen::Dest;
                    if self.dst.items.is_empty() {
                        let start = self.src.path.clone();
                        self.load(true, start, None);
                    } else {
                        self.analyze();
                    }
                }
            }
            KeyCode::Char(' ') if !dest => {
                if let Some(Item::Entry(e)) = self.src.current() {
                    let p = e.path.clone();
                    if !self.selected.remove(&p) {
                        self.selected.insert(p);
                    }
                }
                self.src.move_by(1);
            }
            KeyCode::Char('a') if !dest => {
                let all: Vec<String> = self
                    .src
                    .items
                    .iter()
                    .filter_map(|i| if let Item::Entry(e) = i { Some(e.path.clone()) } else { None })
                    .collect();
                if all.iter().all(|p| self.selected.contains(p)) {
                    for p in &all {
                        self.selected.remove(p);
                    }
                } else {
                    self.selected.extend(all);
                }
            }
            KeyCode::Esc if !dest => {
                if self.selected.is_empty() {
                    self.quit = true;
                } else {
                    self.selected.clear();
                }
            }
            KeyCode::Char('q') if !dest => self.quit = true,
            _ => {}
        }
    }

    fn cycle_kind(&mut self) {
        let Some(a) = &self.analysis else { return };
        let avail: Vec<LinkKind> = a.options.iter().filter(|o| o.available).map(|o| o.kind).collect();
        if avail.is_empty() {
            return;
        }
        let i = self.kind.and_then(|k| avail.iter().position(|x| *x == k)).map(|i| (i + 1) % avail.len()).unwrap_or(0);
        self.kind = Some(avail[i]);
    }

    fn try_confirm(&mut self) {
        self.analyze();
        let Some(a) = &self.analysis else { return };
        if let Some(b) = a.blocking.first() {
            let b = b.clone();
            self.flash(b, true);
            return;
        }
        let ok = self.kind.is_some_and(|k| a.options.iter().any(|o| o.kind == k && o.available));
        if !ok {
            self.flash("No usable link type for this selection.", true);
            return;
        }
        self.screen = Screen::Confirm;
    }

    fn start(&mut self) {
        let Some(kind) = self.kind else { return };
        let req = ops::Request {
            sources: self.selected.iter().cloned().collect(),
            dest: self.dst.path.as_deref().map(display_path).unwrap_or_default(),
            kind,
            mode: self.mode,
        };
        let (tx, rx) = mpsc::channel();
        self.op_rx = Some(rx);
        self.progress = None;
        self.screen = Screen::Running;
        size::cancel(SIZE_SLOT);
        std::thread::spawn(move || {
            let ptx = tx.clone();
            let res = ops::execute(&req, &|p| {
                let _ = ptx.send(OpMsg::Progress(p.clone()));
            });
            let _ = tx.send(OpMsg::Done(res));
        });
    }
}

// ------------------------------------------------------------------ drawing

fn draw(f: &mut Frame, app: &mut App) {
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Min(5), Constraint::Length(2)]).areas(f.area());

    let title = match app.screen {
        Screen::Source => "Select what to move / link",
        _ => "Choose the destination",
    };
    f.render_widget(
        Line::from(vec![
            Span::styled(" ⛓ DirLinker ", Style::new().fg(Color::White).bg(ACCENT).bold()),
            Span::raw("  "),
            Span::styled(title, Style::new().bold()),
            Span::styled(
                format!(
                    "   sort: {}{}",
                    match app.sort {
                        SortKey::Name => "name",
                        SortKey::Size => "size",
                        SortKey::Modified => "modified",
                    },
                    if app.reverse { " ↓" } else { "" }
                ),
                Style::new().fg(MUTED),
            ),
        ]),
        head,
    );

    match app.screen {
        Screen::Source => draw_browser(f, body, app, false, true),
        _ => {
            let [left, right] = Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)]).areas(body);
            let [right_top, right_bottom] = Layout::vertical([Constraint::Min(6), Constraint::Length(9)]).areas(right);
            draw_selection(f, left, app);
            draw_browser(f, right_top, app, true, app.screen == Screen::Dest);
            draw_kinds(f, right_bottom, app);
        }
    }

    draw_footer(f, foot, app);

    match app.screen {
        Screen::Confirm => draw_confirm(f, app),
        Screen::Running => draw_running(f, app),
        Screen::Results => draw_results(f, app),
        _ => {}
    }
}

fn link_color(e: &Entry) -> Option<Color> {
    let l = e.link.as_ref()?;
    Some(match l.kind {
        LinkKind::Hardlink => HARDLINK,
        _ if !l.target_exists => BROKEN,
        LinkKind::Junction => JUNCTION,
        _ => SYMLINK,
    })
}

fn draw_browser(f: &mut Frame, area: Rect, app: &mut App, dest: bool, focused: bool) {
    let selected = app.selected.clone();
    let sizes = &app.sizes;
    let b = if dest { &mut app.dst } else { &mut app.src };
    let title = match &b.path {
        None => " This Computer ".to_string(),
        Some(p) => format!(" {} ", display_path(p)),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { ACCENT } else { MUTED }))
        .title(Line::from(vec![
            Span::styled(if dest { " DESTINATION " } else { " SOURCE " }, Style::new().bold().fg(if dest { JUNCTION } else { ACCENT })),
            Span::raw(title),
        ]))
        .title_bottom(Line::from(Span::styled(format!(" {} ", b.fs), Style::new().fg(MUTED))).right_aligned());

    let rows: Vec<Row> = b
        .items
        .iter()
        .map(|it| match it {
            Item::Drive(d) => {
                let used = if d.total > 0 { (d.total - d.free) as f64 / d.total as f64 } else { 0.0 };
                let bar_w = 12usize;
                let filled = (used * bar_w as f64).round() as usize;
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_w - filled));
                Row::new(vec![
                    Cell::from(" "),
                    Cell::from(Line::from(vec![
                        Span::styled("🖴 ", Style::new().fg(ACCENT)),
                        Span::styled(d.mount.clone(), Style::new().bold()),
                        Span::styled(format!("  {}", d.name), Style::new().fg(MUTED)),
                    ])),
                    Cell::from(Span::styled(bar, Style::new().fg(if used > 0.9 { BROKEN } else { ACCENT }))),
                    Cell::from(Line::from(format!("{} free / {}", fmt_bytes(d.free), fmt_bytes(d.total))).right_aligned()),
                    Cell::from(Span::styled(d.fs.clone(), Style::new().fg(MUTED))),
                ])
            }
            Item::Up => Row::new(vec![Cell::from(" "), Cell::from(Span::styled("↰ ..", Style::new().fg(MUTED)))]),
            Item::Entry(e) => {
                let sel = selected.contains(&e.path);
                let color = link_color(e).unwrap_or(if e.is_dir { FOLDER } else { Color::Reset });
                let icon = match (&e.link, e.is_dir) {
                    (Some(l), _) if l.kind != LinkKind::Hardlink => "↪ ",
                    (_, true) => "▸ ",
                    _ => "  ",
                };
                let mut name = vec![Span::styled(format!("{icon}{}", e.name), Style::new().fg(color))];
                if let Some(t) = e.link.as_ref().and_then(|l| l.target.as_ref()) {
                    name.push(Span::styled(format!("  → {t}"), Style::new().fg(MUTED)));
                }
                let size = if e.is_dir {
                    if e.link.is_some() {
                        "link".to_string()
                    } else {
                        match sizes.get(&e.path) {
                            Some(Some(s)) => fmt_bytes(s.bytes),
                            Some(None) => "…".into(),
                            None => String::new(),
                        }
                    }
                } else {
                    e.size.map(fmt_bytes).unwrap_or_default()
                };
                let kind = match &e.link {
                    Some(l) if l.kind == LinkKind::Hardlink => format!("hardlink×{}", l.link_count.unwrap_or(2)),
                    Some(l) if !l.target_exists => "broken".into(),
                    Some(l) => format!("{:?}", l.kind).to_lowercase(),
                    None if e.is_dir => "folder".into(),
                    None => String::new(),
                };
                let mut row = Row::new(vec![
                    Cell::from(if sel { "●" } else { " " }).style(Style::new().fg(ACCENT)),
                    Cell::from(Line::from(name)),
                    Cell::from(Line::from(size).right_aligned()),
                    Cell::from(modified(e.modified)).style(Style::new().fg(MUTED)),
                    Cell::from(kind).style(Style::new().fg(link_color(e).unwrap_or(MUTED))),
                ]);
                if sel {
                    row = row.style(Style::new().bg(Color::Rgb(28, 42, 70)));
                }
                row
            }
        })
        .collect();

    let widths = [Constraint::Length(2), Constraint::Min(20), Constraint::Length(12), Constraint::Length(17), Constraint::Length(12)];
    let table = Table::new(rows, widths)
        .block(block)
        .row_highlight_style(if focused {
            Style::new().bg(Color::Rgb(40, 60, 100)).add_modifier(Modifier::BOLD)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        })
        .highlight_symbol("▌");
    f.render_stateful_widget(table, area, &mut b.state);
}

fn modified(ms: Option<i64>) -> String {
    // Days-ago display keeps this dependency-free (no date library).
    let Some(ms) = ms else { return String::new() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let mins = (now - ms) / 60_000;
    match mins {
        m if m < 1 => "just now".into(),
        m if m < 60 => format!("{m} min ago"),
        m if m < 60 * 24 => format!("{} h ago", m / 60),
        m if m < 60 * 24 * 60 => format!("{} days ago", m / 1440),
        m if m < 60 * 24 * 730 => format!("{} months ago", m / 43_200),
        m => format!("{} years ago", m / 525_600),
    }
}

fn draw_selection(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        format!("{} item(s) selected", app.selected.len()),
        Style::new().bold(),
    ))];
    lines.push(Line::raw(""));
    for s in &app.selected {
        let size = app.sizes.get(s).copied().flatten().map(|z| format!("  {}", fmt_bytes(z.bytes))).unwrap_or_default();
        lines.push(Line::from(vec![Span::styled("● ", Style::new().fg(ACCENT)), Span::raw(s.clone()), Span::styled(size, Style::new().fg(MUTED))]));
    }
    if let Some(a) = &app.analysis {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!(
                "{} → {}{}",
                a.src_fs,
                a.dst_fs,
                if a.same_volume { " · same drive" } else { " · different drives" }
            ),
            Style::new().fg(MUTED),
        )));
        for b in &a.blocking {
            lines.push(Line::from(Span::styled(format!("✗ {b}"), Style::new().fg(BROKEN))));
        }
    }
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(MUTED))
                .title(Span::styled(" SOURCE ", Style::new().bold().fg(ACCENT))),
        ),
        area,
    );
}

fn draw_kinds(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled("Mode: ", Style::new().fg(MUTED)),
        Span::styled(
            if app.mode == Mode::Move { "Move & link back" } else { "Link only" },
            Style::new().bold(),
        ),
        Span::styled("   (m to toggle, t to change type)", Style::new().fg(MUTED)),
    ])];
    let mut kinds: Vec<Span> = vec![];
    let mut detail: Vec<Line> = vec![];
    if let Some(a) = &app.analysis {
        for o in &a.options {
            let on = Some(o.kind) == app.kind;
            let style = if !o.available {
                Style::new().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT)
            } else if on {
                Style::new().fg(Color::Black).bg(JUNCTION).bold()
            } else {
                Style::new().fg(Color::White)
            };
            kinds.push(Span::styled(format!(" {}{} ", o.title, if o.recommended { " ★" } else { "" }), style));
            kinds.push(Span::raw(" "));
            if on {
                detail.push(Line::from(Span::styled(o.short.clone(), Style::new().bold())));
                detail.push(Line::from(Span::styled(o.details.clone(), Style::new().fg(MUTED))));
                detail.push(Line::from(vec![Span::styled("Use for: ", Style::new().bold()), Span::raw(o.use_when.clone())]));
            }
        }
        if app.kind.is_none() {
            let reasons: Vec<String> = a.options.iter().filter_map(|o| o.reason.as_ref().map(|r| format!("{}: {r}", o.title))).collect();
            detail.push(Line::from(Span::styled(reasons.join("  ·  "), Style::new().fg(BROKEN))));
        }
    }
    lines.push(Line::from(kinds));
    lines.extend(detail);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(MUTED))
                .title(Span::styled(" LINK TYPE ", Style::new().bold())),
        ),
        area,
    );
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let keys = match app.screen {
        Screen::Source => "↑↓/jk move · Enter/l open · Backspace/h up · Space select · a all · Enter/Tab with selection → destination · s sort · r reverse · . hidden · q quit",
        Screen::Dest => "↑↓/jk move · Enter/l open · Backspace/h up · t link type · m move/link-only · Ctrl+Enter (Ctrl+J / F2) link · Esc back",
        Screen::Confirm => "y / Enter start · n / Esc cancel",
        Screen::Running => "working…",
        Screen::Results => "any key to continue",
    };
    let msg = match &app.message {
        Some((m, err, _)) => Line::from(Span::styled(m.clone(), Style::new().fg(if *err { BROKEN } else { OK }))),
        None => Line::from(vec![
            Span::styled("■ junction  ", Style::new().fg(JUNCTION)),
            Span::styled("■ symlink  ", Style::new().fg(SYMLINK)),
            Span::styled("■ hardlink  ", Style::new().fg(HARDLINK)),
            Span::styled("■ broken  ", Style::new().fg(BROKEN)),
            Span::styled(format!("· {} selected", app.selected.len()), Style::new().fg(MUTED)),
        ]),
    };
    f.render_widget(Paragraph::new(vec![msg, Line::from(Span::styled(keys, Style::new().fg(MUTED)))]), area);
}

fn popup(f: &mut Frame, w: u16, h: u16, title: &str, color: Color) -> Rect {
    let a = f.area();
    let w = w.min(a.width.saturating_sub(4));
    let h = h.min(a.height.saturating_sub(2));
    let r = Rect { x: a.x + (a.width - w) / 2, y: a.y + (a.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Span::styled(format!(" {title} "), Style::new().bold().fg(color)));
    let inner = block.inner(r);
    f.render_widget(block, r);
    inner
}

fn draw_confirm(f: &mut Frame, app: &App) {
    let Some(a) = &app.analysis else { return };
    let kind = app.kind.and_then(|k| a.options.iter().find(|o| o.kind == k));
    let title = kind.map(|o| o.title.clone()).unwrap_or_default();
    let moving = app.mode == Mode::Move && !kind.is_some_and(|o| o.keeps_source);
    let inner = popup(f, 100, 8 + app.selected.len().min(10) as u16 + a.warnings.len() as u16, "Confirm", ACCENT);
    let mut lines = vec![Line::from(Span::styled(
        if moving { format!("Move & leave a {title} at the original place:") } else { format!("Create {title}:") },
        Style::new().bold(),
    ))];
    lines.push(Line::raw(""));
    for (s, t) in app.selected.iter().zip(a.targets.iter()).take(10) {
        let (l, r) = if moving || kind.is_some_and(|o| o.keeps_source) { (s, t) } else { (t, s) };
        lines.push(Line::from(vec![Span::raw(l.clone()), Span::styled("  →  ", Style::new().fg(ACCENT)), Span::raw(r.clone())]));
    }
    if moving {
        lines.push(Line::raw(""));
        for w in &a.warnings {
            lines.push(Line::from(Span::styled(format!("⚠ {w}"), Style::new().fg(HARDLINK))));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("Press y / Enter to start, n / Esc to cancel", Style::new().fg(MUTED))));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_running(f: &mut Frame, app: &App) {
    let inner = popup(f, 90, 8, "Working", ACCENT);
    let [l1, l2, l3] = Layout::vertical([Constraint::Length(2), Constraint::Length(2), Constraint::Length(2)]).areas(inner);
    let (item, phase, ratio, file) = match &app.progress {
        Some(p) => (
            format!("({}/{}) {}", p.item_index + 1, p.item_count, p.item),
            p.phase.clone(),
            if p.bytes_total > 0 { (p.bytes_done as f64 / p.bytes_total as f64).min(1.0) } else { 0.0 },
            p.file.clone().unwrap_or_default(),
        ),
        None => ("starting…".into(), String::new(), 0.0, String::new()),
    };
    f.render_widget(Paragraph::new(item).bold(), l1);
    let label = match &app.progress {
        Some(p) if p.bytes_total > 0 => format!("{phase} {} / {}", fmt_bytes(p.bytes_done), fmt_bytes(p.bytes_total)),
        _ => phase,
    };
    f.render_widget(Gauge::default().gauge_style(Style::new().fg(ACCENT)).ratio(ratio).label(label), l2);
    f.render_widget(Paragraph::new(file).fg(MUTED), l3);
}

fn draw_results(f: &mut Frame, app: &App) {
    let bad = app.results.iter().filter(|r| !r.ok).count();
    let inner = popup(
        f,
        110,
        6 + app.results.len().min(12) as u16 * 2,
        if bad > 0 { "Finished with errors" } else { "Done" },
        if bad > 0 { BROKEN } else { OK },
    );
    let mut lines = vec![];
    for r in app.results.iter().take(12) {
        lines.push(Line::from(vec![
            Span::styled(if r.ok { "✔ " } else { "✘ " }, Style::new().fg(if r.ok { OK } else { BROKEN }).bold()),
            Span::styled(r.source.clone(), Style::new().bold()),
        ]));
        lines.push(Line::from(Span::styled(format!("  {}", r.message), Style::new().fg(MUTED))));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("Press any key to continue", Style::new().fg(MUTED))));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
