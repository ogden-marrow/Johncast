use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
struct Settings {
    podcasts: Vec<Podcast>,
}

#[derive(Serialize, Deserialize)]
struct Podcast {
    url: String,
    title: String,
    listened: Vec<String>,
    #[serde(default)]
    resume: BTreeMap<String, u64>, // episode id -> seconds
}

struct Episode {
    id: String,
    title: String,
    url: Option<String>,
}

enum Screen {
    Podcasts,
    Episodes { idx: usize, eps: Vec<Episode> },
}

struct App {
    settings: Settings,
    screen: Screen,
    podcasts: ListState,
    episodes: ListState,
    input: Option<String>, // Some while typing a feed URL
    confirm_remove: bool,
    msg: String,
    quit: bool,
}

fn settings_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("johncast").join("settings.json")
}

fn load() -> Settings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(s: &Settings) {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let json = serde_json::to_string_pretty(s).expect("serialize settings");
    let _ = std::fs::write(&path, json);
}

fn fetch(url: &str) -> Result<rss::Channel, String> {
    let bytes = ureq::get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    rss::Channel::read_from(&bytes[..]).map_err(|e| e.to_string())
}

fn episode_id(item: &rss::Item) -> String {
    item.guid()
        .map(|g| g.value().to_string())
        .or_else(|| item.link().map(str::to_string))
        .or_else(|| item.title().map(str::to_string))
        .unwrap_or_default()
}

fn clock(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// Fancy keyboard cheat sheet for mpv, printed on the normal screen before playback.
fn print_cheatsheet(title: &str) {
    const W: usize = 60; // inner width
    let (b, k, h, d, r) = ("\x1b[1;36m", "\x1b[1;33m", "\x1b[1;35m", "\x1b[2m", "\x1b[0m");
    let sections: [(&str, &[(&str, &str)]); 4] = [
        (
            "PLAYHEAD",
            &[
                ("←  /  →", "back / forward 5 seconds"),
                ("↓  /  ↑", "back / forward 1 minute"),
                ("Shift+←  /  →", "back / forward 1 second (exact)"),
            ],
        ),
        (
            "SPEED",
            &[
                ("[  /  ]", "slower / faster by 10%"),
                ("{  /  }", "half / double speed"),
                ("Backspace", "reset to normal speed"),
            ],
        ),
        (
            "VOLUME",
            &[("9  /  0", "volume down / up"), ("m", "mute / unmute")],
        ),
        (
            "PLAYBACK",
            &[("Space", "pause / resume"), ("q", "quit (saves your place)")],
        ),
    ];
    let line = "─".repeat(W);
    let t: String = title.chars().take(W - 6).collect();
    println!();
    println!("{b}╭{line}╮{r}");
    println!("{b}│{r}  {h}♪ NOW PLAYING{r}{pad}{b}│{r}", pad = " ".repeat(W - 15));
    println!("{b}│{r}  {t}{pad}{b}│{r}", pad = " ".repeat(W - 2 - t.chars().count()));
    for (name, rows) in sections {
        println!("{b}├{line}┤{r}");
        println!("{b}│{r}  {h}{name}{r}{pad}{b}│{r}", pad = " ".repeat(W - 2 - name.chars().count()));
        for (key, desc) in rows {
            let kp = " ".repeat(18usize.saturating_sub(key.chars().count()));
            let dp = " ".repeat(W - 4 - 18 - desc.chars().count());
            println!("{b}│{r}    {k}{key}{r}{kp}{d}{desc}{r}{dp}{b}│{r}");
        }
    }
    println!("{b}├{line}┤{r}");
    let note = "Quit with q and your place is saved for next time.";
    println!("{b}│{r}  {d}{note}{r}{pad}{b}│{r}", pad = " ".repeat(W - 2 - note.chars().count()));
    println!("{b}╰{line}╯{r}");
    println!();
}

/// Plays with mpv from `start` seconds. Returns the position to resume from,
/// or None if the episode finished (mpv saves no position then).
fn play(url: &str, start: u64) -> io::Result<Option<u64>> {
    let dir = std::env::temp_dir().join(format!("johncast-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let status = std::process::Command::new("mpv")
        .args(["--no-video", "--term-osd-bar", "--save-position-on-quit", "--watch-later-options=start"])
        .arg(format!("--watch-later-dir={}", dir.display()))
        .arg(format!("--start={start}"))
        .arg(url)
        .status();
    let pos = std::fs::read_dir(&dir)?
        .flatten()
        .filter_map(|f| std::fs::read_to_string(f.path()).ok())
        .find_map(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("start="))
                .and_then(|v| v.parse::<f64>().ok())
        })
        .map(|v| v as u64);
    let _ = std::fs::remove_dir_all(&dir);
    status.map(|_| pos)
}

/// Moves a list selection for navigation keys; returns false for other keys.
fn nav(state: &mut ListState, len: usize, code: KeyCode) -> bool {
    if len == 0 {
        return false;
    }
    let cur = state.selected().unwrap_or(0);
    let new = match code {
        KeyCode::Up | KeyCode::Char('k') => cur.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => (cur + 1).min(len - 1),
        KeyCode::PageUp => cur.saturating_sub(10),
        KeyCode::PageDown => (cur + 10).min(len - 1),
        KeyCode::Home | KeyCode::Char('g') => 0,
        KeyCode::End | KeyCode::Char('G') => len - 1,
        _ => return false,
    };
    state.select(Some(new));
    true
}

impl App {
    fn new() -> App {
        let settings = load();
        let mut podcasts = ListState::default();
        if !settings.podcasts.is_empty() {
            podcasts.select(Some(0));
        }
        App {
            settings,
            screen: Screen::Podcasts,
            podcasts,
            episodes: ListState::default(),
            input: None,
            confirm_remove: false,
            msg: String::new(),
            quit: false,
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let [main, help] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
        let accent = Style::default().fg(Color::Cyan);
        let dim = Style::default().fg(Color::DarkGray);
        let sel = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);

        let keys = match (&self.screen, self.input.is_some(), self.confirm_remove) {
            (_, true, _) => "Enter save   Esc cancel",
            (_, _, true) => "Remove this podcast?   y yes   any other key cancels",
            (Screen::Podcasts, ..) => "↑↓ move   Enter open   a add feed   d remove   q quit",
            (Screen::Episodes { .. }, ..) => "↑↓ move   Enter play   Space mark listened   Esc back",
        };
        let help_text = if self.msg.is_empty() { keys } else { &self.msg };
        let help_style = if self.msg.is_empty() { dim } else { Style::default().fg(Color::Yellow) };
        f.render_widget(Paragraph::new(help_text).style(help_style), help);

        match &self.screen {
            Screen::Podcasts => {
                let block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(accent)
                    .title(" ♪ Johncast ");
                if self.settings.podcasts.is_empty() {
                    let hint = "No podcasts yet — press a and paste an RSS feed URL.";
                    f.render_widget(Paragraph::new(hint).style(dim).block(block), main);
                } else {
                    let w = self.settings.podcasts.len().to_string().len();
                    let items: Vec<ListItem> = self
                        .settings
                        .podcasts
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            ListItem::new(Line::from(vec![
                                Span::styled(format!("{:>w$}  ", i + 1), dim),
                                Span::raw(p.title.clone()),
                                Span::styled(format!("   {} listened", p.listened.len()), dim),
                            ]))
                        })
                        .collect();
                    let list = List::new(items).block(block).highlight_style(sel).highlight_symbol("▸ ");
                    f.render_stateful_widget(list, main, &mut self.podcasts);
                }
            }
            Screen::Episodes { idx, eps } => {
                let p = &self.settings.podcasts[*idx];
                let block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(accent)
                    .title(format!(" ♪ {} ", p.title))
                    .title_bottom(format!(" {}/{} listened ", p.listened.len().min(eps.len()), eps.len()));
                let w = eps.len().to_string().len();
                let items: Vec<ListItem> = eps
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let done = p.listened.contains(&e.id);
                        let base = if done { dim } else { Style::default() };
                        let mut spans = vec![
                            Span::styled(format!("{:>w$}  ", i + 1), dim),
                            Span::styled(
                                if done { "✔ " } else { "  " },
                                Style::default().fg(Color::Green),
                            ),
                            Span::styled(e.title.clone(), base),
                        ];
                        if let Some(s) = p.resume.get(&e.id) {
                            spans.push(Span::styled(
                                format!("   ▶ {}", clock(*s)),
                                Style::default().fg(Color::Yellow),
                            ));
                        }
                        if e.url.is_none() {
                            spans.push(Span::styled("   (no audio)", dim));
                        }
                        ListItem::new(Line::from(spans))
                    })
                    .collect();
                let list = List::new(items).block(block).highlight_style(sel).highlight_symbol("▸ ");
                f.render_stateful_widget(list, main, &mut self.episodes);
            }
        }

        if let Some(input) = &self.input {
            let area = f.area();
            let w = area.width.saturating_sub(4).min(80);
            let popup = Rect::new(area.x + (area.width - w) / 2, area.y + area.height / 2 - 1, w, 3);
            f.render_widget(Clear, popup);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" Add podcast — paste its RSS feed URL ");
            // show the tail of long URLs so the cursor stays visible
            let room = w.saturating_sub(3) as usize;
            let chars: Vec<char> = input.chars().collect();
            let shown: String = chars[chars.len().saturating_sub(room)..].iter().collect();
            f.render_widget(Paragraph::new(shown.clone()).block(block), popup);
            f.set_cursor_position((popup.x + 1 + shown.chars().count() as u16, popup.y + 1));
        }
    }

    fn redraw(&mut self, terminal: &mut DefaultTerminal, msg: &str) -> io::Result<()> {
        self.msg = msg.to_string();
        terminal.draw(|f| self.draw(f))?;
        Ok(())
    }

    fn key(&mut self, terminal: &mut DefaultTerminal, k: KeyEvent) -> io::Result<()> {
        self.msg.clear();
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return Ok(());
        }
        if let Some(input) = &mut self.input {
            match k.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) => input.push(c),
                KeyCode::Enter => {
                    let url = input.trim().to_string();
                    self.input = None;
                    if !url.is_empty() {
                        self.add_feed(terminal, url)?;
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        if self.confirm_remove {
            self.confirm_remove = false;
            if k.code == KeyCode::Char('y') {
                if let Some(i) = self.podcasts.selected() {
                    self.settings.podcasts.remove(i);
                    save(&self.settings);
                    let n = self.settings.podcasts.len();
                    self.podcasts.select(if n == 0 { None } else { Some(i.min(n - 1)) });
                }
            }
            return Ok(());
        }
        match self.screen {
            Screen::Podcasts => self.key_podcasts(terminal, k.code),
            Screen::Episodes { .. } => self.key_episodes(terminal, k.code),
        }
    }

    fn add_feed(&mut self, terminal: &mut DefaultTerminal, url: String) -> io::Result<()> {
        self.redraw(terminal, "Fetching feed…")?;
        match fetch(&url) {
            Ok(c) => {
                self.settings.podcasts.push(Podcast {
                    url,
                    title: c.title().to_string(),
                    listened: vec![],
                    resume: BTreeMap::new(),
                });
                save(&self.settings);
                self.podcasts.select(Some(self.settings.podcasts.len() - 1));
            }
            Err(e) => self.msg = format!("Could not load feed: {e}"),
        }
        Ok(())
    }

    fn key_podcasts(&mut self, terminal: &mut DefaultTerminal, code: KeyCode) -> io::Result<()> {
        let len = self.settings.podcasts.len();
        if nav(&mut self.podcasts, len, code) {
            return Ok(());
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('a') => self.input = Some(String::new()),
            KeyCode::Char('d') if len > 0 => self.confirm_remove = true,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let Some(idx) = self.podcasts.selected() else { return Ok(()) };
                self.redraw(terminal, "Loading episodes…")?;
                match fetch(&self.settings.podcasts[idx].url) {
                    Ok(c) => {
                        // feeds list newest first; show oldest first so episode 1 is the first episode
                        let eps: Vec<Episode> = c
                            .items()
                            .iter()
                            .rev()
                            .map(|i| Episode {
                                id: episode_id(i),
                                title: i.title().unwrap_or("(untitled)").to_string(),
                                url: i.enclosure().map(|e| e.url().to_string()),
                            })
                            .collect();
                        let listened = &self.settings.podcasts[idx].listened;
                        let first_new = eps.iter().position(|e| !listened.contains(&e.id));
                        self.episodes.select(first_new.or(eps.len().checked_sub(1)));
                        self.screen = Screen::Episodes { idx, eps };
                        self.msg.clear();
                    }
                    Err(e) => self.msg = format!("Could not load feed: {e}"),
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn key_episodes(&mut self, terminal: &mut DefaultTerminal, code: KeyCode) -> io::Result<()> {
        let Screen::Episodes { idx, eps } = &self.screen else { return Ok(()) };
        let idx = *idx;
        if nav(&mut self.episodes, eps.len(), code) {
            return Ok(());
        }
        match code {
            KeyCode::Esc | KeyCode::Char('b') | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('q') => {
                self.screen = Screen::Podcasts;
            }
            KeyCode::Char(' ') => {
                let Some(e) = self.episodes.selected().and_then(|i| eps.get(i)) else { return Ok(()) };
                let listened = &mut self.settings.podcasts[idx].listened;
                match listened.iter().position(|x| *x == e.id) {
                    Some(p) => {
                        listened.remove(p);
                    }
                    None => listened.push(e.id.clone()),
                }
                save(&self.settings);
            }
            KeyCode::Enter | KeyCode::Char('p') => {
                let Some(e) = self.episodes.selected().and_then(|i| eps.get(i)) else { return Ok(()) };
                let Some(url) = e.url.clone() else {
                    self.msg = "This episode has no audio.".into();
                    return Ok(());
                };
                let (id, title) = (e.id.clone(), e.title.clone());
                let pod = &mut self.settings.podcasts[idx];
                if !pod.listened.contains(&id) {
                    pod.listened.push(id.clone());
                }
                let start = pod.resume.get(&id).copied().unwrap_or(0);
                save(&self.settings);

                ratatui::restore();
                print_cheatsheet(&title);
                let result = play(&url, start);
                *terminal = ratatui::init();

                let pod = &mut self.settings.podcasts[idx];
                match result {
                    Ok(Some(pos)) => {
                        pod.resume.insert(id, pos);
                        self.msg = format!("Resume saved at {}", clock(pos));
                    }
                    Ok(None) => {
                        pod.resume.remove(&id);
                    }
                    Err(e) => self.msg = format!("Could not run mpv: {e}"),
                }
                save(&self.settings);
            }
            _ => {}
        }
        Ok(())
    }
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut app = App::new();
    while !app.quit {
        terminal.draw(|f| app.draw(f))?;
        if let Event::Key(k) = event::read()? {
            if k.kind == KeyEventKind::Press {
                app.key(terminal, k)?;
            }
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}
