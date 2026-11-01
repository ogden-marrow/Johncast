use ratatui::crossterm::event::{self, Event, KeyCode as K, KeyEventKind::Press, KeyModifiers};
use ratatui::{prelude::*, widgets::*};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap as Map, process::Command};

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Settings { podcasts: Vec<Podcast>, sync: bool, cookie: String }
#[derive(Serialize, Deserialize, Default)]
struct Podcast { url: String, title: String, listened: Vec<String>, #[serde(default)] resume: Map<String, u64> }
type Ep = (String, String, Option<String>);
struct Oc { id: String, url: String, played: bool, progress: u64 }

fn file() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap_or(format!("{home}/.config"))).join("johncast/settings.json")
}
fn load() -> Settings {
    std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn save(s: &Settings) {
    let f = file();
    let _ = std::fs::create_dir_all(f.parent().unwrap());
    let _ = std::fs::write(&f, serde_json::to_string_pretty(s).unwrap());
    let _ = std::fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o600));
}
fn fetch(url: &str) -> Result<rss::Channel, Box<dyn std::error::Error>> {
    Ok(rss::Channel::read_from(&ureq::get(url).call()?.body_mut().read_to_vec()?[..])?)
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> &'a str {
    s.split_once(a).and_then(|(_, r)| r.split_once(b)).map_or("", |(x, _)| x)
}

fn keep(jar: &mut Map<String, String>, r: &ureq::http::Response<ureq::Body>) {
    for h in r.headers().get_all("set-cookie") {
        if let Some((k, v)) = h.to_str().ok().and_then(|h| h.split(';').next()).and_then(|c| c.split_once('=')) {
            jar.insert(k.into(), v.into());
        }
    }
}

fn login() -> Option<String> {
    use qrcode::render::unicode::Dense1x2::{Dark, Light};
    let (mut jar, cookie) = (Map::new(), |j: &Map<String, String>| j.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "));
    let mut r = ureq::get("https://overcast.fm/login").call().ok()?;
    keep(&mut jar, &r);
    let html = r.body_mut().read_to_string().ok()?;
    let token = between(&html, "data-token=\"", "\"").to_string();
    let qr = qrcode::QrCode::new(format!("overcast:///auth?t={token}&l=browser")).ok()?;
    println!("\nScan with your iPhone camera to log in with the Overcast app (Ctrl-C cancels):\n");
    println!("{}", qr.render::<qrcode::render::unicode::Dense1x2>().dark_color(Light).light_color(Dark).build());
    println!("No camera? Open https://overcast.fm/main/login_app?t={token}&l=browser on your phone.");
    for _ in 0..300 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let sent = ureq::post("https://overcast.fm/main/login_qr_verify").header("Cookie", cookie(&jar)).send_form([("token", token.as_str()), ("then", "podcasts")]);
        let Ok(mut r) = sent else { continue };
        keep(&mut jar, &r);
        if r.body_mut().read_to_string().ok()?.len() > 1 { return Some(cookie(&jar)).filter(|c| !c.is_empty()) }
    }
    None
}

type Feeds = Vec<(String, String)>;

fn pull(cookie: &str) -> Result<(Map<String, Oc>, Feeds), Box<dyn std::error::Error>> {
    let xml = ureq::get("https://overcast.fm/account/export_opml/extended").header("Cookie", cookie).call()?.body_mut().read_to_string()?;
    let doc = roxmltree::Document::parse(&xml)?;
    let feeds = doc.descendants().filter(|n| n.attribute("type") == Some("rss") && n.attribute("subscribed") == Some("1"))
        .filter_map(|n| Some((n.attribute("xmlUrl")?.to_string(), n.attribute("title").or(n.attribute("text"))?.to_string()))).collect();
    let eps = doc.descendants().filter(|n| n.attribute("type") == Some("podcast-episode")).filter_map(|n| {
        let oc = Oc { id: n.attribute("overcastId")?.into(), url: n.attribute("overcastUrl")?.into(), played: n.attribute("played") == Some("1"),
            progress: n.attribute("progress").and_then(|p| p.parse().ok()).unwrap_or(0) };
        Some((n.attribute("enclosureUrl")?.to_string(), oc))
    }).collect();
    Ok((eps, feeds))
}

fn import(s: &mut Settings, feeds: Feeds) -> usize {
    let key = |u: &str| u.to_lowercase().trim_end_matches('/').split_once("://").map_or(u.to_string(), |x| x.1.to_string());
    let (mut old, mut added) = (std::mem::take(&mut s.podcasts), 0);
    for (url, title) in feeds {
        match old.iter().position(|p| key(&p.url) == key(&url)) {
            Some(i) => s.podcasts.push(old.remove(i)),
            None => { added += 1; s.podcasts.push(Podcast { url, title, ..Default::default() }) }
        }
    }
    s.podcasts.extend(old);
    added
}

fn push(cookie: &str, o: &Oc, pos: Option<u64>) -> bool {
    let Some(page) = ureq::get(&o.url).header("Cookie", cookie).call().ok().and_then(|mut r| r.body_mut().read_to_string().ok()) else { return false };
    if between(&page, "data-saved-for-user=\"", "\"") != "1" { return false }
    let p = pos.unwrap_or(0x7FFFFFFF).to_string();
    let form = [("p", p.as_str()), ("speed", between(&page, "data-speed-id=\"", "\"")), ("v", between(&page, "data-sync-version=\"", "\""))];
    ureq::post(&format!("https://overcast.fm/podcasts/set_progress/{}", o.id)).header("Cookie", cookie).send_form(form).is_ok()
}

fn cheatsheet(title: &str) {
    let rows = ["PLAYHEAD", "←  /  →|back / forward 5 seconds", "↓  /  ↑|back / forward 1 minute",
        "Shift+←  /  →|back / forward 1 second", "SPEED", "[  /  ]|slower / faster by 10%",
        "{  /  }|half / double speed", "Backspace|reset speed", "VOLUME", "9  /  0|volume down / up",
        "m|mute", "PLAYBACK", "Space|pause / resume", "q|quit (saves your place)"];
    let line = "─".repeat(60);
    println!("\n\x1b[1;36m╭{line}╮\n│\x1b[35m  ♪ {title:<56.56}\x1b[36m│");
    for r in rows {
        match r.split_once('|') {
            None => println!("\x1b[36m├{line}┤\n│\x1b[35m  {r:<58}\x1b[36m│"),
            Some((k, d)) => println!("\x1b[36m│\x1b[33m    {k:<18}\x1b[0;2m{d:<38}\x1b[0;1;36m│"),
        }
    }
    println!("\x1b[36m╰{line}╯\x1b[0m\n");
}

fn play(url: &str, start: u64) -> Option<u64> {
    let dir = std::env::temp_dir().join(format!("johncast-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let wl = format!("--watch-later-dir={}", dir.display());
    let args = ["--no-video", "--term-osd-bar", "--save-position-on-quit", "--watch-later-options=start", &wl];
    if let Err(e) = Command::new("mpv").args(args).arg(format!("--start={start}")).arg(url).status() {
        println!("could not run mpv: {e}");
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    let pos = std::fs::read_dir(&dir).into_iter().flatten().flatten().find_map(|f| {
        std::fs::read_to_string(f.path()).ok()?.lines().find_map(|l| l.strip_prefix("start=")?.parse::<f64>().ok())
    });
    let _ = std::fs::remove_dir_all(&dir);
    pos.map(|p| p as u64)
}

fn nav(st: &mut ListState, typed: &mut String, len: usize, c: K) -> bool {
    let cur = st.selected().unwrap_or(0);
    match c {
        K::Char(d) if d.is_ascii_digit() && len > 0 => {
            typed.push(d);
            match typed.parse::<usize>() {
                Ok(n) if (1..=len).contains(&n) => st.select(Some(n - 1)),
                _ => { typed.pop(); }
            }
        }
        K::Up | K::Down if len > 0 => {
            typed.clear();
            st.select(Some(if c == K::Up { (cur + len - 1) % len } else { (cur + 1) % len }));
        }
        _ => { typed.clear(); return false; }
    }
    true
}

fn main() -> std::io::Result<()> {
    let (mut s, mut st, mut typed, mut msg) = (load(), ListState::default(), String::new(), String::new());
    let mut oc: Map<String, Oc> = Map::new();
    let (mut eps, mut input, mut job, mut ask): (Option<(usize, Vec<Ep>)>, Option<String>, Option<Result<usize, String>>, bool) = (None, None, None, false);
    if s.sync {
        println!("Syncing with Overcast…");
        match pull(&s.cookie) {
            Ok((m, f)) => { oc = m; msg = format!("Overcast: added {} podcasts", import(&mut s, f)); save(&s); }
            Err(_) => { s.sync = false; msg = "Overcast session expired. Press s to log in again.".into(); }
        }
    }
    st.select(if s.podcasts.is_empty() { None } else { Some(0) });
    let mut t = ratatui::init();
    loop {
        t.draw(|f| {
            let [main, help] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
            let row = |i: usize, n: usize, text: String, dim: bool| {
                ListItem::new(format!("{:>w$}  {text}", i + 1, w = n.to_string().len())).style(if dim { Color::DarkGray } else { Color::Reset })
            };
            let (title, foot, items): (String, String, Vec<ListItem>) = match &eps {
                None => (if s.sync { " ♪ Johncast · Overcast sync on " } else { " ♪ Johncast " }.into(), "".into(), s.podcasts.iter().enumerate()
                    .map(|(i, p)| row(i, s.podcasts.len(), format!("{}   {} listened", p.title, p.listened.len()), false)).collect()),
                Some((pi, v)) => {
                    let p = &s.podcasts[*pi];
                    (format!(" ♪ {} ", p.title), format!(" {} listened ", p.listened.len()), v.iter().enumerate().map(|(i, e)| {
                        let done = p.listened.contains(&e.0);
                        let at = p.resume.get(&e.0).map_or(String::new(), |r| format!("   ▶ {}:{:02}", r / 60, r % 60));
                        row(i, v.len(), format!("{}{}{at}", if done { "✔ " } else { "  " }, e.1), done)
                    }).collect())
                }
            };
            let block = Block::bordered().border_type(BorderType::Rounded).border_style(Color::Cyan).title(title).title_bottom(foot);
            let list = List::new(items).block(block).highlight_symbol("▸ ")
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD));
            f.render_stateful_widget(list, main, &mut st);
            let hint = if !msg.is_empty() { msg.clone() }
                else if !typed.is_empty() { format!("Go to: {typed}▏ (Enter to select)") }
                else if let Some(u) = &input { format!("Feed URL: {u}▏  (Enter to add, Esc to cancel)") }
                else if ask { "Remove this podcast? y = yes, any other key cancels".into() }
                else if eps.is_some() { "↑↓ or type a number   Enter play   Space mark listened   Esc back".into() }
                else { "↑↓ or type a number   Enter open   a add feed   d remove   s Overcast sync   q quit".into() };
            f.render_widget(Paragraph::new(hint).style(if input.is_some() { Color::Yellow } else { Color::DarkGray }), help);
        })?;
        if let Some(j) = job.take() {
            msg.clear();
            let url = match &j { Ok(i) => s.podcasts[*i].url.clone(), Err(u) => u.clone() };
            match (fetch(&url), j) {
                (Err(e), _) => msg = format!("Could not load feed: {e}"),
                (Ok(c), Err(url)) => {
                    s.podcasts.push(Podcast { url, title: c.title().into(), ..Default::default() });
                    save(&s);
                    st.select(Some(s.podcasts.len() - 1));
                }
                (Ok(c), Ok(i)) => {
                    let v: Vec<Ep> = c.items().iter().rev().map(|x| {
                        let id = x.guid().map(|g| g.value()).or(x.link()).or(x.title()).unwrap_or_default();
                        (id.into(), x.title().unwrap_or("(untitled)").into(), x.enclosure().map(|e| e.url().into()))
                    }).collect();
                    if s.sync {
                        match pull(&s.cookie) {
                            Ok((m, _)) => oc = m,
                            Err(_) => { s.sync = false; msg = "Overcast session expired. Press s to log in again.".into(); }
                        }
                        let mut hit = 0;
                        for e in &v {
                            let Some(o) = e.2.as_ref().and_then(|u| oc.get(u)) else { continue };
                            let p = &mut s.podcasts[i];
                            hit += 1;
                            if o.played && !p.listened.contains(&e.0) { p.listened.push(e.0.clone()) }
                            if o.played { p.resume.remove(&e.0); } else if o.progress > 0 { p.resume.insert(e.0.clone(), o.progress); }
                        }
                        if s.sync { msg = format!("Overcast: {hit} of {} episodes matched", v.len()); }
                        save(&s);
                    }
                    st.select(v.iter().position(|e| !s.podcasts[i].listened.contains(&e.0)).or(v.len().checked_sub(1)));
                    eps = Some((i, v));
                }
            }
            continue;
        }
        let Event::Key(k) = event::read()? else { continue };
        if k.kind != Press { continue }
        msg.clear();
        let code = k.code;
        if k.modifiers.contains(KeyModifiers::CONTROL) && code == K::Char('c') { break }
        if let Some(u) = &mut input {
            match code {
                K::Esc => input = None,
                K::Backspace => { u.pop(); }
                K::Char(c) => u.push(c),
                K::Enter => {
                    job = Some(Err(u.trim().to_string()));
                    msg = "Fetching feed…".into();
                    input = None;
                }
                _ => {}
            }
            continue;
        }
        if ask {
            ask = false;
            if let (K::Char('y'), Some(i)) = (code, st.selected()) {
                s.podcasts.remove(i);
                save(&s);
                st.select(s.podcasts.len().checked_sub(1).map(|n| i.min(n)));
            }
        } else if let Some((pi, list)) = &eps {
            if nav(&mut st, &mut typed, list.len(), code) { continue }
            match code {
                K::Esc | K::Left | K::Char('q') => { st.select(Some(*pi)); eps = None; }
                K::Char(' ') | K::Enter => if let Some((id, title, url)) = st.selected().and_then(|i| list.get(i)) {
                    let (play_it, p) = (code == K::Enter, &mut s.podcasts[*pi]);
                    if play_it && url.is_none() { msg = "No audio in this episode.".into(); continue }
                    match p.listened.iter().position(|x| x == id) {
                        Some(i) if !play_it => { p.listened.remove(i); }
                        None => p.listened.push(id.clone()),
                        _ => {}
                    }
                    let start = p.resume.get(id).copied().unwrap_or(0);
                    save(&s);
                    if let (true, Some(url)) = (play_it, url) {
                        ratatui::restore();
                        cheatsheet(title);
                        let pos = play(url, start);
                        t = ratatui::init();
                        match pos {
                            Some(x) => { s.podcasts[*pi].resume.insert(id.clone(), x); }
                            None => { s.podcasts[*pi].resume.remove(id); }
                        }
                        save(&s);
                        if s.sync && let Some(o) = oc.get(url) && !push(&s.cookie, o, pos) { msg = "Overcast not updated (is the episode in your library?)".into(); }
                    }
                },
                _ => {}
            }
        } else if !nav(&mut st, &mut typed, s.podcasts.len(), code) {
            match code {
                K::Char('q') | K::Esc => break,
                K::Char('a') => input = Some(String::new()),
                K::Char('d') if !s.podcasts.is_empty() => ask = true,
                K::Char('s') => {
                    if s.sync { s.sync = false; s.cookie.clear(); msg = "Overcast sync off".into(); } else {
                        ratatui::restore();
                        let c = login();
                        t = ratatui::init();
                        msg = if c.is_some() { "Overcast sync on".into() } else { "Overcast login failed or timed out".into() };
                        s.sync = c.is_some();
                        s.cookie = c.unwrap_or_default();
                        if s.sync && let Ok((m, f)) = pull(&s.cookie) {
                            oc = m;
                            msg = format!("Overcast sync on. Added {} podcasts.", import(&mut s, f));
                            st.select(Some(0));
                        }
                    }
                    save(&s);
                }
                K::Enter | K::Right => if let Some(i) = st.selected() { msg = "Loading episodes…".into(); job = Some(Ok(i)); },
                _ => {}
            }
        }
    }
    ratatui::restore();
    Ok(())
}
