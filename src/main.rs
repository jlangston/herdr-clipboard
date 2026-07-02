mod config;
mod filter;
mod herdr;
mod history;
mod img;
mod paths;
mod picker;
mod watcher;

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
        _ => {
            eprintln!("usage: herdr-clip <watch|pick|list>");
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
        match &e.content {
            history::Content::Text(t) => println!("{i}\t{}\t{}", e.ts, picker::format_preview(t, 100)),
            history::Content::Image { .. } => println!("{i}\t{}\t<image>", e.ts),
        }
    }
}
