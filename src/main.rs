mod config;
mod filter;
mod herdr;
mod history;
mod img;
mod paths;
mod picker;
mod watcher;

use watcher::now_ms;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("watch") => watcher::ensure(),
        Some("watch-foreground") => watcher::run(),
        Some("pick") => {
            if let Err(e) = picker::run() {
                eprintln!("herdr-clip pick: {e}");
                std::process::exit(1);
            }
        }
        Some("list") => cmd_list(),
        Some("latest") => cmd_latest(),
        Some("save-copied") => cmd_save_copied(),
        Some("serve-clipboard") => {
            let id = args.get(1).and_then(|s| s.parse::<i64>().ok());
            match id {
                Some(id) => {
                    if let Err(e) = picker::serve_clipboard(id) {
                        eprintln!("herdr-clip serve-clipboard: {e}");
                        std::process::exit(1);
                    }
                }
                None => {
                    eprintln!("usage: herdr-clip serve-clipboard <id>");
                    std::process::exit(2);
                }
            }
        }
        _ => {
            eprintln!("usage: herdr-clip <watch|pick|list|latest>");
            std::process::exit(2);
        }
    }
}

fn cmd_list() {
    let cfg = config::Config::load(paths::config_dir().as_deref());
    let store = match history::HistoryStore::new(
        &paths::state_dir(),
        cfg.max_entries,
        cfg.max_entry_bytes,
        cfg.max_image_bytes,
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("herdr-clip list: {e}");
            std::process::exit(1);
        }
    };
    for (i, e) in store.load().iter().enumerate() {
        println!("{i}\t{}\t{}", e.ts, picker::entry_label(e, 100));
    }
}

/// `herdr-clip latest`: newest text entry raw on stdout — the paste backend
/// for editors inside herdr panes (herdr drops OSC 52 queries, so paste
/// cannot go through the terminal). Empty history or store errors produce
/// empty output with exit 0: paste must never error into the caller.
fn cmd_latest() {
    // A read-only paste must not create the state dir / an empty db as a
    // side effect, which opening the store would do.
    let state_dir = paths::state_dir();
    if !state_dir.join("history.db").exists() {
        return;
    }
    let cfg = config::Config::load(paths::config_dir().as_deref());
    let store = match history::HistoryStore::new(
        &state_dir,
        cfg.max_entries,
        cfg.max_entry_bytes,
        cfg.max_image_bytes,
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("herdr-clip latest: {e}");
            return;
        }
    };
    if let Some(text) = store.newest_text() {
        use std::io::Write;
        let _ = std::io::stdout().write_all(text.as_bytes());
    }
}

/// `herdr-clip save-copied`: hidden subcommand invoked by the
/// `clipboard.copied` event hook. Hooks must never fail loudly, so every
/// error path here is a silent `exit(0)` rather than a panic or nonzero
/// status — a broken or unrecognized envelope just means nothing is saved.
fn cmd_save_copied() {
    let Some(raw) = std::env::var_os("HERDR_PLUGIN_EVENT_JSON") else { return };
    let Some(raw) = raw.to_str() else { return };
    let Ok(event) = serde_json::from_str::<serde_json::Value>(raw) else { return };
    let Some(text) = herdr::event_copied_text(&event) else { return };

    let cfg = config::Config::load(paths::config_dir().as_deref());
    let store = match history::HistoryStore::new(
        &paths::state_dir(),
        cfg.max_entries,
        cfg.max_entry_bytes,
        cfg.max_image_bytes,
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("herdr-clip save-copied: {e}");
            std::process::exit(1);
        }
    };
    let _ = store.append_text(&text, now_ms());
}
