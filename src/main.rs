mod config;
mod filter;
mod herdr;
mod history;
mod paths;
mod watcher;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("watch") => watcher::ensure(),
        Some("watch-foreground") => watcher::run(),
        Some("pick") => todo!("Task 12"),
        Some("list") => todo!("Task 12"),
        _ => {
            eprintln!("usage: herdr-clip <watch|pick|list>");
            std::process::exit(2);
        }
    }
}
