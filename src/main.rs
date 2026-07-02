mod paths;
mod history;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("watch") => todo!("Task 10"),
        Some("watch-foreground") => todo!("Task 10"),
        Some("pick") => todo!("Task 12"),
        Some("list") => todo!("Task 12"),
        _ => {
            eprintln!("usage: herdr-clip <watch|pick|list>");
            std::process::exit(2);
        }
    }
}
