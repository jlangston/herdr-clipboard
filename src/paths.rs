use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Fallback when HERDR_PLUGIN_STATE_DIR is absent (plain-shell invocations
/// like nvim's paste provider): herdr's real state dir for this plugin, so
/// `latest`/`list` read the same history the event hooks write. Mirrors
/// herdr's own resolution: non-empty XDG_STATE_HOME first, then
/// ~/.local/state. Keep the plugin id in sync with `id` in herdr-plugin.toml.
const STATE_FALLBACK_REL: &str = "herdr/plugins/jlangston.multi-clipboard";

/// Directory for history + lock files. Herdr injects HERDR_PLUGIN_STATE_DIR
/// for plugin-launched processes; plain shells fall back to the same dir.
pub fn state_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    state_dir_from(
        std::env::var_os("HERDR_PLUGIN_STATE_DIR"),
        std::env::var_os("XDG_STATE_HOME"),
        &home,
    )
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HERDR_PLUGIN_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn state_dir_from(plugin: Option<OsString>, xdg: Option<OsString>, home: &Path) -> PathBuf {
    match plugin {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => match xdg {
            Some(v) if !v.is_empty() => PathBuf::from(v).join(STATE_FALLBACK_REL),
            _ => home.join(".local/state").join(STATE_FALLBACK_REL),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn uses_env_var_when_set() {
        let d = state_dir_from(Some("/var/lib/x".into()), Some("/xdg".into()), Path::new("/home/u"));
        assert_eq!(d, PathBuf::from("/var/lib/x"));
    }

    #[test]
    fn falls_back_to_xdg_state_home_when_set() {
        let expected = PathBuf::from("/xdg/herdr/plugins/jlangston.multi-clipboard");
        assert_eq!(state_dir_from(None, Some("/xdg".into()), Path::new("/home/u")), expected);
        assert_eq!(state_dir_from(Some("".into()), Some("/xdg".into()), Path::new("/home/u")), expected);
    }

    #[test]
    fn falls_back_to_herdr_plugin_state_dir_when_unset_or_empty() {
        let expected = PathBuf::from("/home/u/.local/state/herdr/plugins/jlangston.multi-clipboard");
        assert_eq!(state_dir_from(None, None, Path::new("/home/u")), expected);
        assert_eq!(state_dir_from(Some("".into()), Some("".into()), Path::new("/home/u")), expected);
    }
}
