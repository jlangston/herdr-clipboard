use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Fallback when HERDR_PLUGIN_STATE_DIR is absent (plain-shell invocations
/// like nvim's paste provider): herdr's real state dir for this plugin, so
/// `latest`/`list` read the same history the event hooks write.
const STATE_FALLBACK_REL: &str = ".local/state/herdr/plugins/jlangston.multi-clipboard";

/// Directory for history + lock files. Herdr injects HERDR_PLUGIN_STATE_DIR
/// for plugin-launched processes; plain shells fall back to the same dir.
pub fn state_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    dir_from(std::env::var_os("HERDR_PLUGIN_STATE_DIR"), &home, STATE_FALLBACK_REL)
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HERDR_PLUGIN_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn dir_from(var: Option<OsString>, home: &Path, fallback_rel: &str) -> PathBuf {
    match var {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(fallback_rel),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn uses_env_var_when_set() {
        let d = dir_from(Some("/var/lib/x".into()), Path::new("/home/u"), STATE_FALLBACK_REL);
        assert_eq!(d, PathBuf::from("/var/lib/x"));
    }

    #[test]
    fn falls_back_to_herdr_plugin_state_dir_when_unset_or_empty() {
        let expected =
            PathBuf::from("/home/u/.local/state/herdr/plugins/jlangston.multi-clipboard");
        let unset = dir_from(None, Path::new("/home/u"), STATE_FALLBACK_REL);
        assert_eq!(unset, expected);
        let empty = dir_from(Some("".into()), Path::new("/home/u"), STATE_FALLBACK_REL);
        assert_eq!(empty, expected);
    }
}
