use serde::{Deserialize, Serialize};
use std::io::{self, Write};
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
    if let Err(e) = std::fs::write(&path, json) {
        eprintln!("could not write {}: {e}", path.display());
    }
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

fn prompt(msg: &str) -> Option<String> {
    print!("{msg}");
    io::stdout().flush().ok()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line).ok()? == 0 {
        return None; // EOF
    }
    Some(line.trim().to_string())
}

fn episode_id(item: &rss::Item) -> String {
    item.guid()
        .map(|g| g.value().to_string())
        .or_else(|| item.link().map(str::to_string))
        .or_else(|| item.title().map(str::to_string))
        .unwrap_or_default()
}

fn episodes(settings: &mut Settings, idx: usize) {
    let channel = match fetch(&settings.podcasts[idx].url) {
        Ok(c) => c,
        Err(e) => return println!("could not load feed: {e}"),
    };
    let items = channel.items();
    loop {
        println!("\n== {} ==", settings.podcasts[idx].title);
        for (i, item) in items.iter().enumerate() {
            let done = settings.podcasts[idx].listened.contains(&episode_id(item));
            println!(
                "{:>3}. [{}] {}",
                i + 1,
                if done { 'x' } else { ' ' },
                item.title().unwrap_or("(untitled)")
            );
        }
        let Some(cmd) = prompt("\n[number] toggle listened, [p number] play, [b] back: ") else { return };
        if cmd == "b" {
            return;
        }
        if let Some(n) = cmd.strip_prefix('p').and_then(|n| n.trim().parse::<usize>().ok()) {
            let Some(item) = items.get(n.wrapping_sub(1)) else { println!("?"); continue };
            let Some(url) = item.enclosure().map(|e| e.url()) else {
                println!("no audio in this episode");
                continue;
            };
            let id = episode_id(item);
            let listened = &mut settings.podcasts[idx].listened;
            if !listened.contains(&id) {
                listened.push(id);
                save(settings);
            }
            if let Err(e) = std::process::Command::new("mpv").args(["--no-video", url]).status() {
                println!("could not run mpv: {e}");
            }
            continue;
        }
        match cmd.parse::<usize>() {
            Ok(n) if (1..=items.len()).contains(&n) => {
                let id = episode_id(&items[n - 1]);
                let listened = &mut settings.podcasts[idx].listened;
                match listened.iter().position(|x| *x == id) {
                    Some(p) => {
                        listened.remove(p);
                    }
                    None => listened.push(id),
                }
                save(settings);
            }
            _ => println!("?"),
        }
    }
}

fn main() {
    let mut settings = load();
    loop {
        println!("\n== Johncast ==");
        for (i, p) in settings.podcasts.iter().enumerate() {
            println!("{:>3}. {}", i + 1, p.title);
        }
        let Some(cmd) = prompt("\n[number] open, [a] add feed, [r] remove, [q] quit: ") else { return };
        match cmd.as_str() {
            "q" => return,
            "a" => {
                let Some(url) = prompt("RSS feed URL: ") else { return };
                if url.is_empty() {
                    continue;
                }
                match fetch(&url) {
                    Ok(c) => {
                        settings.podcasts.push(Podcast {
                            url,
                            title: c.title().to_string(),
                            listened: vec![],
                        });
                        save(&settings);
                    }
                    Err(e) => println!("could not load feed: {e}"),
                }
            }
            "r" => {
                let Some(n) = prompt("remove which number? ") else { return };
                match n.parse::<usize>() {
                    Ok(n) if (1..=settings.podcasts.len()).contains(&n) => {
                        settings.podcasts.remove(n - 1);
                        save(&settings);
                    }
                    _ => println!("?"),
                }
            }
            other => match other.parse::<usize>() {
                Ok(n) if (1..=settings.podcasts.len()).contains(&n) => episodes(&mut settings, n - 1),
                _ => println!("?"),
            },
        }
    }
}
