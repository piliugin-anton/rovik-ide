//! config.toml: the computers that may connect, by their token's hash
//! (D5), the log level, and the heavy-work slots its user wants, which a
//! team server's administrators may cap (5.5). It's read once, at start-up
//! (5.2), so a computer added later takes a restart:
//!
//!   log_level = "info"
//!   max_slots = 2
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
    /// Heavy-work slots at once; the machine's own rule when unset.
    #[serde(default)]
    pub max_slots: Option<usize>,
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

    /// Adds `computer`'s token hash to the file at `path`, or replaces the
    /// one it had (H05). A deploy runs this with the hash on stdin, so the
    /// token itself reaches neither the server's disk nor a command line.
    /// The file is rewritten for its user alone, keeping its settings;
    /// comments in it don't survive.
    pub fn add_token(path: &Path, computer: &str, hash: &str) -> Result<(), String> {
        let id_ok = computer.len() <= 64
            && !computer.is_empty()
            && computer
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !id_ok {
            return Err(format!("{computer:?} isn't a computer's id"));
        }
        let hash_ok = hash.len() == 64 && hash.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
        if !hash_ok {
            return Err("a token's hash is 64 hex digits".into());
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut table: toml::Table = text
            .parse()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let tokens = table
            .entry("tokens")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let Some(tokens) = tokens.as_table_mut() else {
            return Err(format!("{}: tokens isn't a table", path.display()));
        };
        // One token names one computer (`Tokens::new`).
        let same = tokens
            .iter()
            .find(|(other, value)| *other != computer && value.as_str() == Some(hash));
        if let Some((other, _)) = same {
            return Err(format!("{other} already connects with this token"));
        }
        tokens.insert(computer.into(), toml::Value::String(hash.into()));
        let text = toml::to_string(&table).map_err(|e| e.to_string())?;
        rovik_core::atomic_file::write_atomic_private(path, text.as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))
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
    fn a_token_is_added_keeping_the_rest() {
        let dir = rovik_core::testing::temp_dir();
        let path = dir.0.join("config.toml");
        let (one, two, three) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
        std::fs::write(&path, "log_level = \"debug\"\nmax_slots = 2\n").unwrap();
        Config::add_token(&path, "laptop", &one).unwrap();
        Config::add_token(&path, "desk", &two).unwrap();
        Config::add_token(&path, "laptop", &three).unwrap();
        // One token names one computer.
        let taken = Config::add_token(&path, "laptop", &two).unwrap_err();
        assert!(taken.contains("desk"), "{taken}");
        let config = Config::read(&path).unwrap();
        assert_eq!(config.tokens["laptop"], three, "replaced");
        assert_eq!(config.tokens["desk"], two);
        assert_eq!(config.level(), Ok(log::LevelFilter::Debug), "kept");
        assert_eq!(config.max_slots, Some(2));
        assert_eq!(config.tokens.len(), 2);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(Config::add_token(&path, "a b", &one).is_err());
        assert!(Config::add_token(&path, "laptop", "not a hash").is_err());
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
