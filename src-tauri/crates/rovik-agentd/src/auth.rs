//! The computers that may connect, by the SHA-256 of their token (D5). The
//! server keeps only the hashes, so the token never touches its disk and the
//! agent's own commands, which run as the same user, can't read it and
//! connect.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

pub struct Tokens(HashMap<[u8; 32], String>);

impl Tokens {
    /// From config.toml's `[tokens]`: each computer's id and its token's
    /// hash, in hex.
    pub fn new(hashes: &BTreeMap<String, String>) -> Result<Tokens, String> {
        let mut by_hash = HashMap::new();
        for (computer, hex) in hashes {
            let hash = parse_hex(hex).ok_or_else(|| {
                format!("config.toml: the token hash of {computer} isn't 64 hex digits")
            })?;
            by_hash.insert(hash, computer.clone());
        }
        Ok(Tokens(by_hash))
    }

    /// The computer whose token this is.
    pub fn computer(&self, token: &str) -> Option<&str> {
        self.0.get(&hash(token)).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

pub fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn parse_hex(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn a_token_names_its_computer_by_its_hash() {
        let mut hashes = BTreeMap::new();
        hashes.insert("laptop".to_string(), hex(&hash("secret-1")));
        hashes.insert("desk".to_string(), hex(&hash("secret-2")).to_uppercase());
        let tokens = Tokens::new(&hashes).unwrap();
        assert_eq!(tokens.computer("secret-1"), Some("laptop"));
        assert_eq!(tokens.computer("secret-2"), Some("desk"));
        assert_eq!(tokens.computer("secret-3"), None);
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn a_malformed_hash_names_its_computer() {
        let mut hashes = BTreeMap::new();
        hashes.insert("laptop".to_string(), "abc".to_string());
        let err = Tokens::new(&hashes).err().unwrap();
        assert!(err.contains("laptop"), "{err}");
    }
}
