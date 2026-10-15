//! config.toml: the computers that may connect, by their token's hash
//! (D5), and the log level. It's read once, at start-up (5.2), so a
//! computer added later takes a restart:
//!
//!   log_level = "info"
//!
//!   [tokens]
//!   laptop-4f2a = "<the SHA-256 of laptop-4f2a's token, in hex>"

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Deserialize, Default, Debug)]
pub struct Config {
    /// "error", "warn", "info" (the default) or "debug".
    #[serde(default)]
    pub log_level: Option<String>,
    /// Each computer's id and its token's SHA-256.
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

impl Config {
    /// The file at `path`; none yet is a daemon no computer can reach.
    pub fn read(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn level(&self) -> Result<log::LevelFilter, String> {
        match self.log_level.as_deref().unwrap_or("info") {
            "error" => Ok(log::LevelFilter::Error),
            "warn" => Ok(log::LevelFilter::Warn),
            "info" => Ok(log::LevelFilter::Info),
            "debug" => Ok(log::LevelFilter::Debug),
            other => Err(format!(
                "config.toml: log_level is error, warn, info or debug, not {other}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_the_level_read_from_toml() {
        let config: Config =
            toml::from_str("log_level = \"debug\"\n[tokens]\nlaptop = \"00ff\"\ndesk = \"ab12\"\n")
                .unwrap();
        assert_eq!(config.level(), Ok(log::LevelFilter::Debug));
        assert_eq!(config.tokens["laptop"], "00ff");
        assert_eq!(config.tokens.len(), 2);
    }

    #[test]
    fn a_missing_file_is_no_computers_and_a_bad_level_is_refused() {
        let config = Config::read(Path::new("/nonexistent/config.toml")).unwrap();
        assert!(config.tokens.is_empty());
        assert_eq!(config.level(), Ok(log::LevelFilter::Info));
        let loud = Config {
            log_level: Some("loud".into()),
            ..Default::default()
        };
        assert!(loud.level().is_err());
    }
}
