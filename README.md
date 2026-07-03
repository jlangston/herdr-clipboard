# herdr multi-clipboard

Clipboard history for the [herdr](https://herdr.dev) terminal multiplexer —
tmux's `prefix + =` buffer picker, plus images.

While a herdr session is running, a small background watcher records
everything that lands on the system clipboard (text and images) into a local
SQLite history. Press `prefix + +` to get an overlay picker: fuzzy-filter,
pick an entry, and it's pasted into the pane you were working in. Picking an
image puts it back on the system clipboard instead.

## Install

Requires herdr ≥ 0.7.0 and a Rust toolchain (the install step builds from
source with `cargo build --release`).

```bash
herdr plugin install jlangston/herdr-clipboard
```

For development, clone and link instead (linking skips the build step, so
build manually):

```bash
git clone https://github.com/jlangston/herdr-clipboard
cd herdr-clipboard
cargo build --release
herdr plugin link "$(pwd)"
```

### Keybinding (required)

herdr plugin manifests can't declare keybindings, so add one to your
`~/.config/herdr/config.toml` yourself:

```toml
[[keys.command]]
key = "prefix+plus"
type = "shell"
command = "herdr plugin pane open --plugin jlangston.multi-clipboard --entrypoint picker"
description = "Clipboard history"
```

Then reload (`herdr server reload-config`). `prefix+plus` is the same
physical key as tmux's `=` on most layouts; bind whatever you like.

## Using the picker

Entries are listed newest-first with one-line previews; multiline entries get
a `[3L]` badge, images show `[IMG 1920x1080 · 2.1 MB]`. A preview panel shows
the highlighted entry in full.

| Key | Action |
|---|---|
| type anything | fuzzy-filter the list (images match "img"/"image" and their dimensions) |
| `↑`/`↓`, `Ctrl+P`/`Ctrl+N` | move selection |
| `Enter` | paste text into the pane you came from / restore an image to the clipboard |
| `Enter` `Enter` | multiline text needs a confirming second press (see caveat below) |
| `Ctrl+D` / `Delete` | delete the selected entry |
| `Backspace` | edit the filter |
| `Esc` | clear the filter, then close; `Ctrl+C` closes immediately |

## Configuration

Optional `config.toml` in the plugin's config dir (find it with
`herdr plugin config-dir jlangston.multi-clipboard`). All keys optional;
zero/invalid values fall back to defaults:

```toml
max_entries = 50            # history length (oldest dropped)
max_entry_bytes = 262144    # per text entry (256 KiB); larger copies skipped
max_image_bytes = 5242880   # per image, PNG-encoded (5 MiB); larger skipped
poll_ms = 500               # clipboard poll interval
```

## How it works

- A watcher daemon starts automatically (workspace/pane-created hooks, or
  when the picker first opens) and exits when the herdr session does —
  history capture is scoped to "while herdr is running". Exactly one watcher
  runs per session.
- History lives in SQLite under the plugin's state dir. Re-copying an
  existing entry moves it to the front instead of duplicating; images are
  deduplicated by pixel content, not encoding. Deleted/expired entries'
  space is reclaimed automatically.
- Upgrading from the pre-SQLite format: an existing `history.jsonl` is
  imported once, atomically, then kept as `history.jsonl.bak`.
- Restoring an image spawns a tiny detached process that owns the clipboard
  until another application takes it (on Linux, clipboard contents die with
  their owning process). Restore failures are logged to
  `serve-clipboard.log` in the state dir.
- `herdr-clip list` (the plugin binary, in `target/release/`) dumps history
  as TSV for scripting.

## Caveats

- **Multiline paste really executes.** herdr's `pane.send_text` doesn't
  bracket-paste, so interior newlines hit the shell as Enter — that's why
  the picker demands a second `Enter` on multiline entries. A single
  trailing newline is always stripped.
- Anything that touches the system clipboard while herdr is running is
  captured, including copies made in other applications.
- Capture needs a display session (X11 or Wayland); on a headless host the
  watcher idles harmlessly.
- Developed and tested on Linux. `platforms` allows macOS but it is
  currently unverified there.

## License

[MIT](LICENSE)
