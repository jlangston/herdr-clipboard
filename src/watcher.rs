use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::herdr::{self, HerdrClient};
use crate::history::HistoryStore;
use crate::paths;

const LOCK_FILE: &str = "watch.lock";
pub const FOCUS_FILE: &str = "last_focused_pane";

/// `herdr-clip watch`: make sure one watcher daemon is running, then return.
/// Called from manifest event hooks and by the picker on launch (covers
/// restored sessions where no create events fire). Never fails loudly —
/// a broken watcher must not break the hook that fired it.
pub fn ensure() {
    let state_dir = paths::state_dir();
    if fs::create_dir_all(&state_dir).is_err() {
        return;
    }
    match try_acquire_lock(&state_dir) {
        Ok(Some(_probe)) => {} // free (probe lock drops here) — spawn the daemon
        _ => return,           // held (daemon already running) or io error
    }
    spawn_detached(&["watch-foreground"], None);
}

/// Spawn the current executable with `args`, fully detached (own process
/// group, null stdio). Best-effort: failures are ignored. When `stderr_log`
/// is given, the child's stderr goes to that file (truncated each spawn so
/// it only ever describes the latest run); if the file can't be created the
/// spawn still proceeds with stderr nulled.
pub fn spawn_detached(args: &[&str], stderr_log: Option<&Path>) {
    let Ok(exe) = std::env::current_exe() else { return };
    let stderr = stderr_log
        .and_then(|p| File::create(p).ok())
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null);
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let _ = cmd.spawn();
}

/// `herdr-clip watch-foreground`: the actual daemon. Exits 0 immediately if
/// another instance holds the lock (two `ensure` calls can race — the losing
/// child lands here).
pub fn run() -> ! {
    let state_dir = paths::state_dir();
    fs::create_dir_all(&state_dir).expect("create state dir");
    // Held (not dropped) for the daemon's whole life; the underscore-prefixed
    // name keeps the File alive while silencing the unused warning.
    let _lock = match try_acquire_lock(&state_dir) {
        Ok(Some(f)) => f,
        _ => std::process::exit(0),
    };

    let cfg = Config::load(paths::config_dir().as_deref());
    let store = HistoryStore::new(&state_dir, cfg.max_entries, cfg.max_entry_bytes, cfg.max_image_bytes)
        .expect("open history store");
    let poll = Duration::from_millis(cfg.poll_ms.max(50));
    std::thread::spawn(move || poll_clipboard(store, poll));

    // Main thread: follow focus events so the picker knows its paste target;
    // the dropped socket doubles as our shutdown signal (session over).
    // NOTE: after subscribing, this connection must only be drained via
    // read_line — a request() here would silently eat pushed events.
    let Ok(mut client) = HerdrClient::connect() else { std::process::exit(0) };
    if client.subscribe_pane_focus().is_err() {
        std::process::exit(0);
    }
    let focus_path = state_dir.join(FOCUS_FILE);
    loop {
        match client.read_line() {
            Ok(event) => {
                if let Some(pane_id) = herdr::event_pane_id(&event) {
                    let _ = fs::write(&focus_path, pane_id);
                }
            }
            Err(_) => std::process::exit(0),
        }
    }
}

/// Ok(Some(file)) = lock acquired (held until the File drops).
/// Ok(None) = another process holds it.
fn try_acquire_lock(state_dir: &Path) -> std::io::Result<Option<File>> {
    let f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(state_dir.join(LOCK_FILE))?;
    match f.try_lock_exclusive() {
        Ok(()) => Ok(Some(f)),
        Err(_) => Ok(None),
    }
}

fn poll_clipboard(store: HistoryStore, poll: Duration) {
    let mut clipboard = loop {
        match arboard::Clipboard::new() {
            Ok(c) => break c,
            Err(_) => std::thread::sleep(Duration::from_secs(2)), // display not ready yet
        }
    };
    let mut last_text: Option<String> = None;
    let mut last_img: Option<Vec<u8>> = None;
    loop {
        // get_text errors (empty clipboard, non-text content, transient
        // Wayland quirks) just mean "nothing to record this tick".
        if let Ok(text) = clipboard.get_text() {
            if last_text.as_deref() != Some(text.as_str()) {
                let _ = store.append_text(&text, now_ms());
                last_text = Some(text);
            }
        }
        // get_image errors mean "clipboard isn't an image right now" — skip.
        if let Ok(image) = clipboard.get_image() {
            if last_img.as_deref() != Some(image.bytes.as_ref()) {
                let hash = crate::img::rgba_hash(&image.bytes);
                if let Ok(png) =
                    crate::img::encode_rgba_png(image.width as u32, image.height as u32, &image.bytes)
                {
                    let _ = store.append_image(
                        &png,
                        image.width as u32,
                        image.height as u32,
                        hash,
                        now_ms(),
                    );
                }
                last_img = Some(image.bytes.into_owned());
            }
        }
        std::thread::sleep(poll);
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_attempt_fails_while_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let first = try_acquire_lock(dir.path()).unwrap();
        assert!(first.is_some(), "fresh dir: lock must be free");
        assert!(try_acquire_lock(dir.path()).unwrap().is_none(), "held lock must not be re-acquirable");
        drop(first);
        assert!(try_acquire_lock(dir.path()).unwrap().is_some(), "dropped lock must be free again");
    }
}
