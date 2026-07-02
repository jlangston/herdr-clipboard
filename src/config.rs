use serde::Deserialize;
use std::path::Path;

#[derive(Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub max_entries: usize,
    pub max_entry_bytes: usize,
    pub poll_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self { max_entries: 50, max_entry_bytes: 256 * 1024, poll_ms: 500 }
    }
}

impl Config {
    /// Read `<config_dir>/config.toml`; any problem (no dir, no file,
    /// parse error) silently yields defaults — a broken config must not
    /// take the clipboard down.
    /// Zero values are treated as broken and fall back per-field.
    pub fn load(config_dir: Option<&Path>) -> Self {
        let mut c: Config = config_dir
            .map(|d| d.join("config.toml"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        let d = Config::default();
        if c.max_entries == 0 { c.max_entries = d.max_entries; }
        if c.max_entry_bytes == 0 { c.max_entry_bytes = d.max_entry_bytes; }
        if c.poll_ms == 0 { c.poll_ms = d.poll_ms; }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let c = Config::default();
        assert_eq!(c.max_entries, 50);
        assert_eq!(c.max_entry_bytes, 256 * 1024);
        assert_eq!(c.poll_ms, 500);
    }

    #[test]
    fn loads_partial_config_over_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "max_entries = 9\n").unwrap();
        let c = Config::load(Some(dir.path()));
        assert_eq!(c.max_entries, 9);
        assert_eq!(c.poll_ms, 500);
    }

    #[test]
    fn missing_dir_or_bad_toml_falls_back_to_defaults() {
        assert_eq!(Config::load(None), Config::default());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(Some(dir.path())), Config::default()); // no file
        std::fs::write(dir.path().join("config.toml"), "max_entries = \"lots\"").unwrap();
        assert_eq!(Config::load(Some(dir.path())), Config::default()); // bad type
    }

    #[test]
    fn zero_values_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "max_entries = 0\nmax_entry_bytes = 0\npoll_ms = 0\n",
        )
        .unwrap();
        assert_eq!(Config::load(Some(dir.path())), Config::default());
    }
}
