use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Directory for history + lock files. Herdr injects HERDR_PLUGIN_STATE_DIR
/// for plugin-launched processes; the fallback keeps `herdr-clip list`
/// usable from a plain shell during development.
pub fn state_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    dir_from(std::env::var_os("HERDR_PLUGIN_STATE_DIR"), &home, ".local/state/herdr-clip")
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
        let d = dir_from(Some("/var/lib/x".into()), Path::new("/home/u"), ".local/state/herdr-clip");
        assert_eq!(d, PathBuf::from("/var/lib/x"));
    }

    #[test]
    fn falls_back_to_home_relative_when_unset_or_empty() {
        let unset = dir_from(None, Path::new("/home/u"), ".local/state/herdr-clip");
        assert_eq!(unset, PathBuf::from("/home/u/.local/state/herdr-clip"));
        let empty = dir_from(Some("".into()), Path::new("/home/u"), ".local/state/herdr-clip");
        assert_eq!(empty, PathBuf::from("/home/u/.local/state/herdr-clip"));
    }
}
