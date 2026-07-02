use crate::filter::fuzzy_match;
use crate::history::Entry;

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
    /// Entry with this ts was removed from the state; persist the delete.
    Delete(u64),
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
        self.entries.iter().filter(|e| fuzzy_match(&self.filter, &e.text)).collect()
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
                let ts = self.visible().get(self.selected).map(|e| e.ts);
                let Some(ts) = ts else { return Outcome::Continue };
                self.entries.retain(|e| e.ts != ts);
                let len = self.visible().len();
                if len > 0 && self.selected >= len {
                    self.selected = len - 1;
                }
                Outcome::Delete(ts)
            }
            Key::Enter => {
                let Some(entry) = self.selected_entry() else { return Outcome::Continue };
                let text = paste_text(&entry.text);
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

/// `herdr-clip pick`: overlay picker entrypoint.
pub fn run() -> io::Result<()> {
    watcher::ensure(); // covers restored sessions where no create events fired
    let cfg = Config::load(paths::config_dir().as_deref());
    let state_dir = paths::state_dir();
    let store = HistoryStore::new(&state_dir, cfg.max_entries, cfg.max_entry_bytes)?;

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
            Outcome::Delete(ts) => store.delete(ts)?,
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
        .map(|e| ListItem::new(format_preview(&e.text, list_area.width.saturating_sub(3) as usize)))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" clipboard history "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut ls = ListState::default();
    ls.select((!visible.is_empty()).then_some(state.selected));
    frame.render_stateful_widget(list, list_area, &mut ls);

    let preview = state
        .selected_entry()
        .map(|e| e.text.clone())
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
    use crate::history::Entry;

    fn entries() -> Vec<Entry> {
        vec![
            Entry { ts: 3, text: "newest cargo build".into() },
            Entry { ts: 2, text: "middle".into() },
            Entry { ts: 1, text: "oldest line".into() },
        ]
    }

    #[test]
    fn typing_filters_and_resets_selection() {
        let mut s = PickerState::new(entries());
        s.on_key(Key::Down);
        for c in "cargo".chars() {
            assert_eq!(s.on_key(Key::Char(c)), Outcome::Continue);
        }
        let vis: Vec<_> = s.visible().iter().map(|e| e.ts).collect();
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
        let mut s = PickerState::new(vec![Entry { ts: 1, text: "a\nb\n".into() }]);
        assert_eq!(s.on_key(Key::Enter), Outcome::Continue);
        assert!(s.pending_confirm);
        assert!(s.status.as_deref().unwrap_or("").contains("2-line"));
        assert_eq!(s.on_key(Key::Enter), Outcome::Paste("a\nb".into()));
    }

    #[test]
    fn any_other_key_disarms_multiline_confirmation() {
        let mut s = PickerState::new(vec![Entry { ts: 1, text: "a\nb".into() }]);
        s.on_key(Key::Enter);
        s.on_key(Key::Down);
        assert!(!s.pending_confirm);
        assert_eq!(s.on_key(Key::Enter), Outcome::Continue); // re-arms, doesn't paste
    }

    #[test]
    fn delete_removes_selected_and_reports_ts() {
        let mut s = PickerState::new(entries());
        s.on_key(Key::Down);
        assert_eq!(s.on_key(Key::DeleteEntry), Outcome::Delete(2));
        let vis: Vec<_> = s.visible().iter().map(|e| e.ts).collect();
        assert_eq!(vis, vec![3, 1]);
        // deleting the last entry pulls the selection back in range
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
        // paste_text("a\nb\n\n\n") == "a\nb\n\n" == 3 lines; badge must agree
        assert_eq!(format_preview("a\nb\n\n\n", 80), "[3L] a b");
    }

    #[test]
    fn keys_are_noops_when_filter_matches_nothing() {
        let mut s = PickerState::new(vec![Entry { ts: 1, text: "hello".into() }]);
        for c in "zzz".chars() {
            s.on_key(Key::Char(c));
        }
        assert!(s.visible().is_empty());
        s.on_key(Key::Up);
        s.on_key(Key::Down);
        assert_eq!(s.on_key(Key::Enter), Outcome::Continue);
        assert_eq!(s.on_key(Key::DeleteEntry), Outcome::Continue);
    }
}
