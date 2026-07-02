use crate::filter::fuzzy_match;
use crate::history::{Content, Entry};

use crate::config::Config;
use crate::herdr::HerdrClient;
use crate::history::HistoryStore;
use crate::{paths, watcher};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use std::fs;
use std::io;

/// Terminal-agnostic key model so state transitions test without a TTY.
pub enum Key {
    Up,
    Down,
    Enter,
    Esc,
    Backspace,
    DeleteEntry,
    Char(char),
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Continue,
    Cancel,
    /// Final text to send (trailing newline already stripped).
    Paste(String),
    /// Image entry to restore to the system clipboard (by store id).
    RestoreImage(i64),
    /// Entry with this id was removed from the state; persist the delete.
    Delete(i64),
}

pub struct PickerState {
    entries: Vec<Entry>, // newest first
    pub filter: String,
    pub selected: usize, // index into visible()
    pub pending_confirm: bool,
    pub status: Option<String>,
}

impl PickerState {
    pub fn new(entries: Vec<Entry>) -> Self {
        Self { entries, filter: String::new(), selected: 0, pending_confirm: false, status: None }
    }

    /// Filtered view, history order preserved (newest first).
    pub fn visible(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| fuzzy_match(&self.filter, &searchable(e))).collect()
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.visible().get(self.selected).copied()
    }

    pub fn on_key(&mut self, key: Key) -> Outcome {
        if !matches!(key, Key::Enter) {
            self.pending_confirm = false;
            self.status = None;
        }
        match key {
            Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                Outcome::Continue
            }
            Key::Down => {
                let len = self.visible().len();
                if len > 0 && self.selected < len - 1 {
                    self.selected += 1;
                }
                Outcome::Continue
            }
            Key::Char(c) => {
                self.filter.push(c);
                self.selected = 0;
                Outcome::Continue
            }
            Key::Backspace => {
                self.filter.pop();
                self.selected = 0;
                Outcome::Continue
            }
            Key::Esc => {
                if self.filter.is_empty() {
                    Outcome::Cancel
                } else {
                    self.filter.clear();
                    self.selected = 0;
                    Outcome::Continue
                }
            }
            Key::DeleteEntry => {
                let id = self.visible().get(self.selected).map(|e| e.id);
                let Some(id) = id else { return Outcome::Continue };
                self.entries.retain(|e| e.id != id);
                let len = self.visible().len();
                if len > 0 && self.selected >= len {
                    self.selected = len - 1;
                }
                Outcome::Delete(id)
            }
            Key::Enter => {
                let Some(entry) = self.selected_entry() else { return Outcome::Continue };
                let id = entry.id;
                match &entry.content {
                    Content::Image { .. } => {
                        self.pending_confirm = false;
                        Outcome::RestoreImage(id)
                    }
                    Content::Text(t) => {
                        let text = paste_text(t);
                        if text.contains('\n') && !self.pending_confirm {
                            self.pending_confirm = true;
                            self.status = Some(format!(
                                "{}-line entry — press Enter again to paste",
                                text.lines().count()
                            ));
                            return Outcome::Continue;
                        }
                        self.pending_confirm = false;
                        Outcome::Paste(text)
                    }
                }
            }
        }
    }
}

/// What the fuzzy filter runs against: the text itself, or a stable
/// searchable label for images ("img"/"image" and the dimensions match).
fn searchable(e: &Entry) -> String {
    match &e.content {
        Content::Text(t) => t.clone(),
        Content::Image { w, h, .. } => format!("image img {w}x{h}"),
    }
}

/// Strip at most one trailing newline (LF or CRLF) so pasting a
/// shell-copied command doesn't immediately execute it.
pub fn paste_text(text: &str) -> String {
    let t = text.strip_suffix('\n').unwrap_or(text);
    let t = t.strip_suffix('\r').unwrap_or(t);
    t.to_string()
}

/// One-line list preview: whitespace runs flattened, char-boundary-safe
/// truncation, `[NL]` badge for multiline entries.
pub fn format_preview(text: &str, width: usize) -> String {
    let lines = paste_text(text).lines().count();
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = if lines > 1 { format!("[{lines}L] {flat}") } else { flat };
    if out.chars().count() > width {
        out = out.chars().take(width.saturating_sub(1)).collect::<String>() + "…";
    }
    if out.is_empty() {
        out = "(whitespace)".into();
    }
    out
}

/// One-line list label for any entry kind.
pub fn entry_label(e: &Entry, width: usize) -> String {
    match &e.content {
        Content::Text(t) => format_preview(t, width),
        Content::Image { w, h, bytes } => format!("[IMG {w}x{h} · {}]", human_size(*bytes)),
    }
}

fn human_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// `herdr-clip serve-clipboard <id>`: put an image entry back on the system
/// clipboard and keep serving it until another app takes ownership (on
/// Linux clipboard contents die with the owning process, so the short-lived
/// picker cannot do this itself). Starting a new server replaces — and so
/// terminates — the previous one.
pub fn serve_clipboard(id: i64) -> io::Result<()> {
    let cfg = Config::load(paths::config_dir().as_deref());
    let store = HistoryStore::new(
        &paths::state_dir(),
        cfg.max_entries,
        cfg.max_entry_bytes,
        cfg.max_image_bytes,
    )?;
    let png = store
        .get_image(id)?
        .ok_or_else(|| io::Error::other("image entry not found"))?;
    let (w, h, rgba) = crate::img::decode_png(&png)?;
    let image = arboard::ImageData {
        width: w as usize,
        height: h as usize,
        bytes: rgba.into(),
    };
    let mut clipboard = arboard::Clipboard::new().map_err(io::Error::other)?;
    #[cfg(target_os = "linux")]
    {
        use arboard::SetExtLinux;
        clipboard.set().wait().image(image).map_err(io::Error::other)?;
    }
    #[cfg(not(target_os = "linux"))]
    clipboard.set_image(image).map_err(io::Error::other)?;
    Ok(())
}

/// `herdr-clip pick`: overlay picker entrypoint.
pub fn run() -> io::Result<()> {
    watcher::ensure(); // covers restored sessions where no create events fired
    let cfg = Config::load(paths::config_dir().as_deref());
    let state_dir = paths::state_dir();
    let store = HistoryStore::new(&state_dir, cfg.max_entries, cfg.max_entry_bytes, cfg.max_image_bytes)?;

    // Paste target: the pane focused before this overlay took focus,
    // recorded by the watcher. Never target ourselves.
    let self_pane = std::env::var("HERDR_PANE_ID").ok();
    let target = fs::read_to_string(state_dir.join(watcher::FOCUS_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && Some(s) != self_pane.as_ref());

    let mut state = PickerState::new(store.load());
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut state, &store, target.as_deref(), self_pane.as_deref());
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    state: &mut PickerState,
    store: &HistoryStore,
    target: Option<&str>,
    self_pane: Option<&str>,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, state))?;
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(()),
            (KeyCode::Char('d'), KeyModifiers::CONTROL) | (KeyCode::Delete, _) => Key::DeleteEntry,
            (KeyCode::Char('p'), KeyModifiers::CONTROL) | (KeyCode::Up, _) => Key::Up,
            (KeyCode::Char('n'), KeyModifiers::CONTROL) | (KeyCode::Down, _) => Key::Down,
            (KeyCode::Enter, _) => Key::Enter,
            (KeyCode::Esc, _) => Key::Esc,
            (KeyCode::Backspace, _) => Key::Backspace,
            (KeyCode::Char(c), m) if m.is_empty() || m == KeyModifiers::SHIFT => Key::Char(c),
            _ => continue,
        };
        match state.on_key(mapped) {
            Outcome::Continue => {}
            Outcome::Cancel => return Ok(()),
            Outcome::Delete(id) => store.delete(id)?,
            Outcome::RestoreImage(id) => {
                watcher::spawn_detached(&["serve-clipboard", &id.to_string()]);
                return Ok(());
            }
            Outcome::Paste(text) => match paste(&text, target, self_pane) {
                Ok(()) => return Ok(()),
                Err(e) => state.status = Some(format!("paste failed: {e}")),
            },
        }
    }
}

/// Send to the recorded target pane; if that fails (pane closed) fall back
/// to whatever pane herdr says is focused, excluding the picker itself.
fn paste(text: &str, target: Option<&str>, self_pane: Option<&str>) -> io::Result<()> {
    let mut client = HerdrClient::connect()?;
    if let Some(t) = target {
        if client.send_text(t, text).is_ok() {
            return Ok(());
        }
    }
    let fallback = client
        .focused_pane_id()?
        .filter(|id| Some(id.as_str()) != self_pane)
        .ok_or_else(|| io::Error::other("no target pane"))?;
    client.send_text(&fallback, text)
}

fn draw(frame: &mut Frame, state: &PickerState) {
    let [list_area, preview_area, footer_area] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Percentage(40),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let visible = state.visible();
    let items: Vec<ListItem> = visible
        .iter()
        .map(|e| ListItem::new(entry_label(e, list_area.width.saturating_sub(3) as usize)))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" clipboard history "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut ls = ListState::default();
    ls.select((!visible.is_empty()).then_some(state.selected));
    frame.render_stateful_widget(list, list_area, &mut ls);

    let preview = state
        .selected_entry()
        .map(|e| match &e.content {
            Content::Text(t) => t.clone(),
            Content::Image { w, h, bytes } => format!(
                "PNG image {w}×{h}, {}.\n\nEnter restores it to the system clipboard.",
                human_size(*bytes)
            ),
        })
        .unwrap_or_else(|| "clipboard history is empty — copy something first".into());
    frame.render_widget(
        Paragraph::new(preview)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" preview ")),
        preview_area,
    );

    let footer = state.status.clone().unwrap_or_else(|| {
        format!(
            "filter: {}▏  ↑/↓ move · type to filter · Enter paste · Ctrl+D delete · Esc quit",
            state.filter
        )
    });
    frame.render_widget(Paragraph::new(footer), footer_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{Content, Entry};

    fn t(id: i64, ts: u64, text: &str) -> Entry {
        Entry { id, ts, content: Content::Text(text.into()) }
    }

    fn img(id: i64, ts: u64, w: u32, h: u32, bytes: usize) -> Entry {
        Entry { id, ts, content: Content::Image { w, h, bytes } }
    }

    fn entries() -> Vec<Entry> {
        vec![t(3, 3, "newest cargo build"), t(2, 2, "middle"), t(1, 1, "oldest line")]
    }

    #[test]
    fn typing_filters_and_resets_selection() {
        let mut s = PickerState::new(entries());
        s.on_key(Key::Down);
        for c in "cargo".chars() {
            assert_eq!(s.on_key(Key::Char(c)), Outcome::Continue);
        }
        let vis: Vec<_> = s.visible().iter().map(|e| e.id).collect();
        assert_eq!(vis, vec![3]);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn navigation_clamps_to_visible_range() {
        let mut s = PickerState::new(entries());
        s.on_key(Key::Up);
        assert_eq!(s.selected, 0);
        for _ in 0..10 {
            s.on_key(Key::Down);
        }
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn enter_pastes_single_line_entry() {
        let mut s = PickerState::new(entries());
        s.on_key(Key::Down);
        assert_eq!(s.on_key(Key::Enter), Outcome::Paste("middle".into()));
    }

    #[test]
    fn enter_on_multiline_requires_confirmation() {
        let mut s = PickerState::new(vec![t(1, 1, "a\nb\n")]);
        assert_eq!(s.on_key(Key::Enter), Outcome::Continue);
        assert!(s.pending_confirm);
        assert!(s.status.as_deref().unwrap_or("").contains("2-line"));
        assert_eq!(s.on_key(Key::Enter), Outcome::Paste("a\nb".into()));
    }

    #[test]
    fn any_other_key_disarms_multiline_confirmation() {
        let mut s = PickerState::new(vec![t(1, 1, "a\nb")]);
        s.on_key(Key::Enter);
        s.on_key(Key::Down);
        assert!(!s.pending_confirm);
        assert_eq!(s.on_key(Key::Enter), Outcome::Continue); // re-arms, doesn't paste
    }

    #[test]
    fn enter_on_image_restores_without_confirmation() {
        let mut s = PickerState::new(vec![img(7, 1, 640, 480, 1000)]);
        assert_eq!(s.on_key(Key::Enter), Outcome::RestoreImage(7));
        assert!(!s.pending_confirm);
    }

    #[test]
    fn images_match_filter_by_badge_text() {
        let mut s = PickerState::new(vec![t(2, 2, "hello"), img(1, 1, 640, 480, 1000)]);
        for c in "img".chars() {
            s.on_key(Key::Char(c));
        }
        let vis: Vec<_> = s.visible().iter().map(|e| e.id).collect();
        assert_eq!(vis, vec![1]);
    }

    #[test]
    fn delete_removes_selected_and_reports_id() {
        let mut s = PickerState::new(entries());
        s.on_key(Key::Down);
        assert_eq!(s.on_key(Key::DeleteEntry), Outcome::Delete(2));
        let vis: Vec<_> = s.visible().iter().map(|e| e.id).collect();
        assert_eq!(vis, vec![3, 1]);
        s.on_key(Key::Down);
        assert_eq!(s.on_key(Key::DeleteEntry), Outcome::Delete(1));
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn esc_clears_filter_then_cancels() {
        let mut s = PickerState::new(entries());
        s.on_key(Key::Char('m'));
        assert_eq!(s.on_key(Key::Esc), Outcome::Continue);
        assert!(s.filter.is_empty());
        assert_eq!(s.on_key(Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn enter_and_delete_on_empty_history_are_noops() {
        let mut s = PickerState::new(Vec::new());
        assert_eq!(s.on_key(Key::Enter), Outcome::Continue);
        assert_eq!(s.on_key(Key::DeleteEntry), Outcome::Continue);
    }

    #[test]
    fn keys_are_noops_when_filter_matches_nothing() {
        let mut s = PickerState::new(vec![t(1, 1, "hello")]);
        for c in "zzz".chars() {
            s.on_key(Key::Char(c));
        }
        assert!(s.visible().is_empty());
        s.on_key(Key::Up);
        s.on_key(Key::Down);
        assert_eq!(s.on_key(Key::Enter), Outcome::Continue);
        assert_eq!(s.on_key(Key::DeleteEntry), Outcome::Continue);
    }

    #[test]
    fn paste_text_strips_at_most_one_trailing_newline() {
        assert_eq!(paste_text("cmd\n"), "cmd");
        assert_eq!(paste_text("cmd\r\n"), "cmd");
        assert_eq!(paste_text("cmd\n\n"), "cmd\n");
        assert_eq!(paste_text("cmd"), "cmd");
    }

    #[test]
    fn format_preview_flattens_truncates_and_badges() {
        assert_eq!(format_preview("plain text", 80), "plain text");
        assert_eq!(format_preview("a\n  b\tc\n", 80), "[2L] a b c");
        assert_eq!(format_preview("abcdef", 5), "abcd…");
        assert_eq!(format_preview("   \n", 80), "(whitespace)");
    }

    #[test]
    fn preview_badge_matches_confirm_line_count() {
        assert_eq!(format_preview("a\nb\n\n\n", 80), "[3L] a b");
    }

    #[test]
    fn entry_labels_cover_both_kinds() {
        assert_eq!(entry_label(&t(1, 1, "some text"), 80), "some text");
        assert_eq!(entry_label(&img(1, 1, 1920, 1080, 2 * 1024 * 1024), 80), "[IMG 1920x1080 · 2.0 MB]");
        assert_eq!(entry_label(&img(1, 1, 8, 8, 300), 80), "[IMG 8x8 · 300 B]");
    }
}
