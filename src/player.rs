use ratatui::crossterm::event::{self, Event, KeyCode as K, KeyEventKind::Press, KeyModifiers};
use ratatui::{prelude::*, widgets::*, DefaultTerminal};
use image::imageops::FilterType;
use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::{collections::BTreeMap as Map, thread, time::Duration};

pub struct Track {
    pub title: String,
    pub show: String,
    pub url: String,
    pub start: u64,
    pub art: Option<String>,
    pub chapters: Option<String>,
}

#[derive(Default)]
struct St { pos: f64, dur: f64, pause: bool, vol: f64, speed: f64, mute: bool, buffering: bool, eof: bool, closed: bool, mpv_chapters: Vec<Chap> }

#[derive(Clone)]
struct Chap { t: f64, title: String, link: Option<String> }

enum Msg { Chapters(Vec<Chap>), Img(String, image::DynamicImage) }

struct Mpv(Child, std::path::PathBuf);
impl Drop for Mpv {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_file(&self.1);
    }
}

fn bytes(url: &str, limit: u64) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    ureq::get(url).call().ok()?.into_body().into_reader().take(limit).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn decode(b: &[u8]) -> Option<image::DynamicImage> {
    image::load_from_memory(b).ok().map(|i| i.thumbnail(720, 720))
}

fn id3_tag(url: &str) -> Option<id3::Tag> {
    let h = bytes(url, 10).filter(|h| h.len() == 10 && &h[..3] == b"ID3")?;
    let size = h[6..10].iter().fold(0usize, |a, b| a << 7 | (*b as usize & 0x7f)) + 10 + if h[5] & 0x10 != 0 { 10 } else { 0 };
    id3::Tag::read_from2(io::Cursor::new(bytes(url, size.min(16 << 20) as u64)?)).ok()
}

fn load(tr: Track, tx: mpsc::Sender<Msg>) {
    let tag = id3_tag(&tr.url);
    let cover = tr.art.and_then(|u| bytes(&u, 8 << 20)).and_then(|b| decode(&b));
    let cover = cover.or_else(|| tag.as_ref().and_then(|t| t.pictures().next()).and_then(|p| decode(&p.data)));
    if let Some(c) = cover { let _ = tx.send(Msg::Img("art".into(), c)); }
    let mut chaps: Vec<(Chap, Option<Vec<u8>>, Option<String>)> = vec![];
    for f in tag.iter().flat_map(|t| t.frames()) {
        let id3::Content::Chapter(c) = f.content() else { continue };
        let title = c.frames.iter().find(|x| x.id() == "TIT2").and_then(|x| x.content().text()).unwrap_or("").to_string();
        let link = c.frames.iter().find_map(|x| match x.content() {
            id3::Content::ExtendedLink(l) => Some(l.link.clone()),
            id3::Content::Link(l) => Some(l.clone()),
            _ => None,
        });
        let img = c.frames.iter().find_map(|x| match x.content() { id3::Content::Picture(p) => Some(p.data.clone()), _ => None });
        chaps.push((Chap { t: c.start_time as f64 / 1000.0, title, link }, img, None));
    }
    if chaps.is_empty() {
        let json = tr.chapters.and_then(|u| bytes(&u, 2 << 20)).and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        for c in json.iter().flat_map(|j| j["chapters"].as_array().into_iter().flatten()) {
            let chap = Chap { t: c["startTime"].as_f64().unwrap_or(0.0), title: c["title"].as_str().unwrap_or("").into(), link: c["url"].as_str().map(String::from) };
            chaps.push((chap, None, c["img"].as_str().map(String::from)));
        }
    }
    chaps.sort_by(|a, b| a.0.t.total_cmp(&b.0.t));
    let _ = tx.send(Msg::Chapters(chaps.iter().map(|c| c.0.clone()).collect()));
    for (i, (_, embedded, url)) in chaps.into_iter().enumerate() {
        let img = embedded.or_else(|| url.and_then(|u| bytes(&u, 8 << 20))).and_then(|b| decode(&b));
        if let Some(img) = img { let _ = tx.send(Msg::Img(format!("ch{i}"), img)); }
    }
}

fn clock(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn watch(sock: UnixStream, st: Arc<Mutex<St>>) {
    for line in io::BufReader::new(sock).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let mut s = st.lock().unwrap();
        let d = &v["data"];
        match (v["event"].as_str(), v["name"].as_str()) {
            (Some("property-change"), Some("time-pos")) => s.pos = d.as_f64().unwrap_or(s.pos),
            (Some("property-change"), Some("duration")) => s.dur = d.as_f64().unwrap_or(0.0),
            (Some("property-change"), Some("pause")) => s.pause = d.as_bool().unwrap_or(false),
            (Some("property-change"), Some("volume")) => s.vol = d.as_f64().unwrap_or(s.vol),
            (Some("property-change"), Some("speed")) => s.speed = d.as_f64().unwrap_or(1.0),
            (Some("property-change"), Some("mute")) => s.mute = d.as_bool().unwrap_or(false),
            (Some("property-change"), Some("paused-for-cache")) => s.buffering = d.as_bool().unwrap_or(false),
            (Some("property-change"), Some("chapter-list")) => {
                s.mpv_chapters = d.as_array().into_iter().flatten()
                    .map(|c| Chap { t: c["time"].as_f64().unwrap_or(0.0), title: c["title"].as_str().unwrap_or("").into(), link: None }).collect();
            }
            (Some("end-file"), _) => s.eof = v["reason"].as_str() == Some("eof"),
            _ => {}
        }
    }
    st.lock().unwrap().closed = true;
}

fn send(w: &mut UnixStream, cmd: Value) {
    let _ = writeln!(w, "{}", json!({ "command": cmd }));
}

struct Art { img: image::DynamicImage, key: (u16, u16), w: u16, h: u16, cells: Vec<(char, [f32; 3], [f32; 3])> }

impl Art {
    fn new(img: image::DynamicImage) -> Self { Art { img, key: (0, 0), w: 0, h: 0, cells: vec![] } }
    fn draw(&mut self, slot: Rect, truecolor: bool, buf: &mut Buffer) {
        if self.key != (slot.width, slot.height) {
            let fit = self.img.resize(slot.width as u32 * 8, slot.height as u32 * 16, FilterType::Lanczos3);
            let (w, h) = ((fit.width() + 4) / 8, (fit.height() + 8) / 16);
            let im = self.img.resize_exact(w.max(1) * 8, h.max(1) * 16, FilterType::Lanczos3).to_rgb8();
            let sharp = image::imageops::blur(&im, 5.0);
            let mask = |f: &dyn Fn(u32, u32) -> bool| (0..128u32).fold(0u128, |a, i| a | (f(i % 8, i / 8) as u128) << i);
            let mut glyphs: Vec<(char, u128)> = " ▘▝▀▖▌▞▛".chars().enumerate().map(|(m, c)| (c, mask(&|x, y| m >> ((y >= 8) as usize * 2 + (x >= 4) as usize) & 1 == 1))).collect();
            glyphs.extend("▁▂▃".chars().zip(1..).chain("▅▆▇".chars().zip(5..)).map(|(c, k)| (c, mask(&|_, y| y >= 16 - 2 * k))));
            glyphs.extend("▏▎▍".chars().zip(1..).chain("▋▊▉".chars().zip(5..)).map(|(c, k)| (c, mask(&|x, _| x < k))));
            (self.key, self.w, self.h, self.cells) = ((slot.width, slot.height), w.max(1) as u16, h.max(1) as u16, vec![]);
            for cy in 0..self.h as u32 { for cx in 0..self.w as u32 {
                let px: Vec<[f32; 3]> = (0..128u32).map(|i| {
                    let (x, y) = (cx * 8 + i % 8, cy * 16 + i / 8);
                    let (o, b) = (im.get_pixel(x, y).0, sharp.get_pixel(x, y).0);
                    [0, 1, 2].map(|k| (o[k] as f32 + 0.45 * (o[k] as f32 - b[k] as f32)).clamp(0.0, 255.0))
                }).collect();
                let sums = |m: u128| { let (mut s, mut q, mut n) = ([0f32; 3], [0f32; 3], 0f32); for p in px.iter().enumerate().filter(|(i, _)| m >> i & 1 == 1).map(|x| x.1) { n += 1.0; for k in 0..3 { s[k] += p[k]; q[k] += p[k] * p[k]; } } (s, q, n) };
                let (ts, tq, tn) = sums(!0);
                self.cells.push(glyphs.iter().map(|&(c, m)| {
                    let ((is, iq, n), mean) = (sums(m), |s: [f32; 3], n: f32| s.map(|v| v / n.max(1.0)));
                    let (os, on) = ([0, 1, 2].map(|k| ts[k] - is[k]), tn - n);
                    let (fg, bg) = (mean(is, n), mean(os, on));
                    let err: f32 = (0..3).map(|k| iq[k] - is[k] * fg[k] + (tq[k] - iq[k]) - os[k] * bg[k]).sum();
                    (err, c, if n == 0.0 { bg } else { fg }, bg)
                }).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, c, f, b)| (c, f, b)).unwrap());
            } }
        }
        let col = |c: [f32; 3]| { let c = c.map(|v| v.round() as u8); if truecolor { Color::Rgb(c[0], c[1], c[2]) } else { Color::Indexed(16 + 36 * (c[0] as u16 * 5 / 255) as u8 + 6 * (c[1] as u16 * 5 / 255) as u8 + (c[2] as u16 * 5 / 255) as u8) } };
        let (x0, y0) = (slot.x + (slot.width - self.w.min(slot.width)) / 2, slot.y + (slot.height - self.h.min(slot.height)) / 2);
        for (i, (g, f, b)) in self.cells.iter().enumerate() {
            if let Some(c) = buf.cell_mut((x0 + i as u16 % self.w, y0 + i as u16 / self.w)) { c.set_char(*g).set_fg(col(*f)).set_bg(col(*b)); }
        }
    }
}

/// Plays a track full-screen. Returns where to resume, or None if it played to the end.
pub fn play(t: &mut DefaultTerminal, tr: Track) -> io::Result<Option<u64>> {
    let sock = std::env::temp_dir().join(format!("johncast-{}.sock", std::process::id()));
    let child = Command::new("mpv")
        .args(["--no-video", "--no-terminal", "--idle=no", "--audio-display=no"])
        .arg(format!("--input-ipc-server={}", sock.display()))
        .arg(format!("--start={}", tr.start))
        .arg(&tr.url)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn()?;
    let _mpv = Mpv(child, sock.clone());
    let mut w = (0..50).find_map(|_| UnixStream::connect(&sock).ok().or_else(|| { thread::sleep(Duration::from_millis(100)); None }))
        .ok_or_else(|| io::Error::other("mpv did not start"))?;
    for (i, p) in ["time-pos", "duration", "pause", "volume", "speed", "mute", "paused-for-cache", "chapter-list"].iter().enumerate() {
        send(&mut w, json!(["observe_property", i + 1, p]));
    }
    let st = Arc::new(Mutex::new(St { vol: 100.0, speed: 1.0, ..Default::default() }));
    let reader = w.try_clone()?;
    let st2 = st.clone();
    thread::spawn(move || watch(reader, st2));
    let (tx, rx) = mpsc::channel();
    let (title, show) = (tr.title.clone(), tr.show.clone());
    thread::spawn(move || load(tr, tx));

    let truecolor = std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"));
    let (mut chaps, mut imgs, mut sel, mut focus, mut help): (Vec<Chap>, Map<String, Art>, _, _, _) = (vec![], Map::new(), ListState::default(), false, false);
    loop {
        for m in rx.try_iter() {
            match m {
                Msg::Chapters(c) => chaps = c,
                Msg::Img(k, img) => { imgs.insert(k, Art::new(img)); }
            }
        }
        let (pos, dur, pause, vol, speed, mute, buffering, eof, closed) = {
            let s = st.lock().unwrap();
            if chaps.is_empty() && !s.mpv_chapters.is_empty() { chaps = s.mpv_chapters.clone(); }
            (s.pos, s.dur, s.pause, s.vol, s.speed, s.mute, s.buffering, s.eof, s.closed)
        };
        if eof || closed { return Ok(if eof { None } else { Some(pos as u64) }); }
        let cur = chaps.iter().rposition(|c| c.t <= pos + 0.5);
        if !focus { sel.select(cur); }

        t.draw(|f| {
            let hints = " space pause · ←→ 5s · ↑↓ 1m · [ ] speed · 9 0 volume · m mute · ? all keys · q back ";
            let outer = Block::bordered().border_type(BorderType::Rounded).border_style(Color::Cyan).title(" ♪ Now Playing ").title_bottom(hints);
            let inner = outer.inner(f.area());
            f.render_widget(outer, f.area());
            let art_h = inner.height.min(24).min(inner.width / 4);
            let art_w = if inner.width >= 64 { (art_h * 2).min(inner.width / 2) } else { 0 };
            let [left, right] = Layout::horizontal([Constraint::Length(if art_w > 0 { art_w + 2 } else { 0 }), Constraint::Min(10)]).areas(inner);
            if art_w > 0 {
                let slot = Rect::new(left.x + 1, left.y, art_w, art_h.min(left.height));
                let key = cur.map(|i| format!("ch{i}")).filter(|k| imgs.contains_key(k)).unwrap_or("art".into());
                match imgs.get_mut(&key) {
                    Some(p) => p.draw(slot, truecolor, f.buffer_mut()),
                    None => f.render_widget(Paragraph::new("\n♪").centered().style(Color::DarkGray).block(Block::bordered().border_style(Color::DarkGray)), slot),
                }
            }
            let [head, now, prog, stat, list] = Layout::vertical([Constraint::Length(4), Constraint::Length(2), Constraint::Length(1), Constraint::Length(2), Constraint::Min(0)]).areas(right.inner(Margin::new(1, 0)));
            let head_lines = vec![Line::from(title.as_str().bold()), Line::from(show.as_str().dim())];
            f.render_widget(Paragraph::new(head_lines).wrap(Wrap { trim: true }), head);
            if let Some(c) = cur.and_then(|i| chaps.get(i).map(|c| (i, c))) {
                let n = format!("§ {}/{}  ", c.0 + 1, chaps.len());
                let mut lines = vec![Line::from(vec![n.magenta(), c.1.title.as_str().magenta().bold()])];
                if let Some(l) = &c.1.link { lines.push(Line::from(format!("↗ {l}").cyan().underlined())); }
                f.render_widget(Paragraph::new(lines), now);
            }
            let (el, rem) = (clock(pos), format!("-{}", clock(dur - pos)));
            let w = (prog.width as usize).saturating_sub(el.len() + rem.len() + 2);
            let on = if dur > 0.0 { ((pos / dur).clamp(0.0, 1.0) * w as f64) as usize } else { 0 };
            f.render_widget(Paragraph::new(Line::from(vec![
                format!("{el} ").into(), "━".repeat(on).cyan(), "●".white(), "─".repeat(w.saturating_sub(on + 1)).dark_gray(), format!(" {rem}").into(),
            ])), prog);
            let n = if mute { 0 } else { ((vol / 10.0).round() as usize).min(10) };
            let state = if buffering { "… buffering".yellow() } else if pause { "⏸ paused".yellow() } else { "▶ playing".green() };
            f.render_widget(Paragraph::new(Line::from(vec![
                state, "   vol ".dark_gray(), "▰".repeat(n).cyan(), "▱".repeat(10 - n).dark_gray(),
                if mute { " muted".into() } else { format!(" {}%", vol as u32).into() }, format!("   speed {speed:.1}×").into(),
            ])), stat);
            if !chaps.is_empty() {
                let items: Vec<ListItem> = chaps.iter().enumerate().map(|(i, c)| {
                    let title = if c.title.is_empty() { format!("Chapter {}", i + 1) } else { c.title.clone() };
                    let line = format!("{:>2}  {:>7}  {title}", i + 1, clock(c.t));
                    ListItem::new(line).style(if Some(i) == cur { Style::new().cyan().bold() } else { Style::new().dark_gray() })
                }).collect();
                let block = Block::new().borders(Borders::TOP).border_style(Color::DarkGray).title(" Chapters ");
                let hl = if focus { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                f.render_stateful_widget(List::new(items).block(block).highlight_style(hl).highlight_symbol(if focus { "▸ " } else { "  " }), list, &mut sel);
            }
            if help {
                let rows = ["PLAYHEAD", "←  /  →|back / forward 5 seconds", "↓  /  ↑|back / forward 1 minute", "Shift+←  /  →|back / forward 1 second (exact)",
                    "Page Up  /  Down|next / previous chapter", "Tab|pick a chapter from the list", "SPEED", "[  /  ]|slower / faster by 10%", "{  /  }|half / double speed",
                    "Backspace|reset to normal speed", "VOLUME", "9  /  0|volume down / up", "m|mute / unmute", "PLAYBACK", "Space|pause / resume", "q|quit (saves your place)"];
                let lines: Vec<Line> = rows.iter().map(|r| match r.split_once('|') {
                    None => Line::from(format!(" {r}")).magenta().bold(),
                    Some((k, d)) => Line::from(vec![format!("   {k:<20}").yellow(), d.to_string().dark_gray()]),
                }).collect();
                let (w, h) = (66.min(f.area().width), (lines.len() as u16 + 2).min(f.area().height));
                let r = Rect::new(f.area().x + (f.area().width - w) / 2, f.area().y + (f.area().height - h) / 2, w, h);
                f.render_widget(Clear, r);
                f.render_widget(Paragraph::new(lines).block(Block::bordered().border_type(BorderType::Rounded).border_style(Color::Cyan).title(" Keys ")), r);
            }
        })?;

        if !event::poll(Duration::from_millis(100))? { continue; }
        let Event::Key(k) = event::read()? else { continue };
        if k.kind != Press { continue; }
        if help { help = false; continue; }
        let jump = |w: &mut UnixStream, to: f64| send(w, json!(["seek", to, "absolute"]));
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let chapter = |w: &mut UnixStream, step: i32| {
            let Some(i) = cur.or(Some(0)).filter(|_| !chaps.is_empty()) else { return };
            if step < 0 {
                jump(w, if pos - chaps[i].t > 3.0 || i == 0 { chaps[i].t } else { chaps[i - 1].t });
            } else if let Some(c) = chaps.get(cur.map_or(0, |i| i + 1)) {
                jump(w, c.t);
            }
        };
        match k.code {
            K::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break,
            K::Char('?') => help = true,
            K::Esc if focus => focus = false,
            K::Char('q') | K::Esc => break,
            K::Tab if !chaps.is_empty() => focus = !focus,
            K::Enter if focus => { if let Some(c) = sel.selected().and_then(|i| chaps.get(i)) { jump(&mut w, c.t); focus = false; } }
            K::Up | K::Down if focus => {
                let (n, i) = (chaps.len(), sel.selected().unwrap_or(0));
                sel.select(Some(if k.code == K::Up { (i + n - 1) % n } else { (i + 1) % n }));
            }
            K::Char(' ') => send(&mut w, json!(["set_property", "pause", !pause])),
            K::Left if shift => send(&mut w, json!(["seek", -1, "relative+exact"])),
            K::Right if shift => send(&mut w, json!(["seek", 1, "relative+exact"])),
            K::Left => send(&mut w, json!(["seek", -5, "relative"])),
            K::Right => send(&mut w, json!(["seek", 5, "relative"])),
            K::Up => send(&mut w, json!(["seek", 60, "relative"])),
            K::Down => send(&mut w, json!(["seek", -60, "relative"])),
            K::Char('[') => send(&mut w, json!(["multiply", "speed", 1.0 / 1.1])),
            K::Char(']') => send(&mut w, json!(["multiply", "speed", 1.1])),
            K::Char('{') => send(&mut w, json!(["multiply", "speed", 0.5])),
            K::Char('}') => send(&mut w, json!(["multiply", "speed", 2.0])),
            K::Backspace => send(&mut w, json!(["set_property", "speed", 1.0])),
            K::Char('9') | K::Char('/') => send(&mut w, json!(["add", "volume", -2])),
            K::Char('0') | K::Char('*') => send(&mut w, json!(["add", "volume", 2])),
            K::Char('m') => send(&mut w, json!(["cycle", "mute"])),
            K::PageUp => chapter(&mut w, 1),
            K::PageDown => chapter(&mut w, -1),
            _ => {}
        }
    }
    send(&mut w, json!(["quit"]));
    Ok(Some(st.lock().unwrap().pos as u64))
}
