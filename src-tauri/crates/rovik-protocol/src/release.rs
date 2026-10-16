//! Rovik's releases (B02): for each version, the daemon built for each CPU
//! and the list of their SHA-256 (`SHA256SUMS`), each signed with the key
//! the app's updates are signed with. That's a minisign key, as `tauri
//! signer` makes it: the key and each signature are base64 of minisign's
//! text. The app checks a build it downloads before it goes to a server,
//! and the list its install check takes (D5). The daemon a server runs
//! checks a build before `admin upgrade` installs it, with the key it was
//! built with, so a new build can't vouch for itself.

use base64::Engine;
use minisign_verify::{PublicKey, Signature};

/// `src-tauri/release-key.pub`: the public key `tauri signer generate`
/// wrote, the updater's `pubkey` too. Lines that start with `#` are
/// comments. Without a key, nothing is taken as signed.
const KEY_FILE: &str = include_str!("../../../release-key.pub");

/// Where the app takes a release's files from:
/// `<RELEASES>/v<version>/<name>`.
pub const RELEASES: &str = "https://github.com/piliugin-anton/rovik-ide/releases/download";

/// The list of a release's builds, as `sha256sum` writes it.
pub const SUMS: &str = "SHA256SUMS";

/// What a file's signature is named after it: `<name>.sig`.
pub const SIGNATURE: &str = ".sig";

/// What a build is named in a release, gzipped: `<name>.gz`, a tenth of its
/// size, as it's mostly tables. Its signature is the build's own.
pub const PACKED: &str = ".gz";

/// The daemon's build for `cpu` (`x86_64`, `aarch64`) in `version`'s
/// release.
pub fn build_name(version: &str, cpu: &str) -> String {
    format!("rovik-agentd-{version}-{cpu}")
}

/// The CPU a build is made for, from what `uname -m` says.
pub fn cpu(machine: &str) -> Option<&'static str> {
    match machine {
        "x86_64" | "amd64" => Some("x86_64"),
        "aarch64" | "arm64" => Some("aarch64"),
        _ => None,
    }
}

/// The key this build of Rovik checks releases with, if it has one. A
/// debug build takes `ROVIK_RELEASE_KEY` instead, for its tests' own key.
pub fn key() -> Option<String> {
    #[cfg(debug_assertions)]
    if let Ok(key) = std::env::var("ROVIK_RELEASE_KEY") {
        return Some(key).filter(|k| !k.trim().is_empty());
    }
    key_in(KEY_FILE)
}

/// The key in a `release-key.pub`: its first line that isn't a comment,
/// ours or the one over minisign's own key.
fn key_in(file: &str) -> Option<String> {
    let mut lines = file.lines().map(str::trim);
    let key = lines.find(|line| {
        !line.is_empty() && !line.starts_with('#') && !line.starts_with("untrusted comment:")
    })?;
    Some(key.to_string())
}

/// Whether `bytes` are what `signature`, a `.sig` file's text, signed with
/// `key`. Both may be base64 of minisign's text, as `tauri signer` writes
/// them, or that text itself.
pub fn verify(key: &str, bytes: &[u8], signature: &str) -> Result<(), String> {
    let public = unwrapped(key)
        .and_then(|text| PublicKey::decode(&text).ok())
        .or_else(|| PublicKey::decode(key.trim()).ok())
        .or_else(|| PublicKey::from_base64(key.trim()).ok())
        .ok_or("Rovik's release key can't be read.")?;
    let signature = unwrapped(signature)
        .and_then(|text| Signature::decode(&text).ok())
        .or_else(|| Signature::decode(signature.trim()).ok())
        .ok_or("Its signature can't be read.")?;
    public
        .verify(bytes, &signature, false)
        .map_err(|_| "Its signature doesn't match: it isn't what the release signed.".into())
}

/// minisign's text, from the base64 `tauri signer` wraps it in.
fn unwrapped(given: &str) -> Option<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    String::from_utf8(engine.decode(given.trim()).ok()?).ok()
}

/// The SHA-256 a `SHA256SUMS` lists, in lowercase, each with its file's
/// name. Lines that aren't a hash and a name are skipped.
pub fn sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.trim().split_once(char::is_whitespace)?;
            let hex = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
            // `*name` is sha256sum's binary mode.
            let name = name.trim_start().trim_start_matches('*');
            (hex && !name.is_empty()).then(|| (hash.to_ascii_lowercase(), name.to_string()))
        })
        .collect()
}

/// What tests check releases with: a key `tauri signer generate` made, and
/// files `tauri signer sign` signed with it. Its private half is gone.
#[cfg(any(test, feature = "testing"))]
pub mod fixture {
    /// The key, as its `.pub` file has it.
    pub const KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEQ2MjhEMUM4NTUxOEU3MDIKUldRQzV4aFZ5TkVvMWp0dHhIRUk3YkxxZjkxU3djNUdnYWRleXRKaXJXUUZKdmhwVlBnV2lNL2gK";

    /// A stand-in for a build: a script that answers `version` as 9.9.8.
    pub const BUILD: &str = "#!/bin/sh\necho 'rovik-agentd 9.9.8 (protocol 1)'\n";
    pub const BUILD_SHA256: &str =
        "c86abcbdfe0bbb2f65a9083e03bbcbf26c0b26dd80a1fe06b1071045c3d7d9be";
    pub const BUILD_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRQzV4aFZ5TkVvMXMwcEdRTFNXZFY2QVhLMTliNlpXaHhUeWRBalRhaEFiZVpnY3FCTUl6M1hiTWx4SGtDUW5kOVk1M1ppeUZxdXU4OERlU3ZSVlBSUGtKREVBa0hxMVF3PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkyMTMwMjY2CWZpbGU6YnVpbGQKQTZIaGpXRHhJNnhoVUsvYUJkSlpUVGVhSGJaZTdHVS95dG5nbCtSbnFpRExkbFVZampING9IYXlIZExrQkd0aE5xZFo2VllQN2dTS3JPejFKdi81REE9PQo=";

    /// Release 9.9.8's list: that build, for both CPUs.
    pub const SHA256SUMS: &str = "c86abcbdfe0bbb2f65a9083e03bbcbf26c0b26dd80a1fe06b1071045c3d7d9be  rovik-agentd-9.9.8-x86_64\nc86abcbdfe0bbb2f65a9083e03bbcbf26c0b26dd80a1fe06b1071045c3d7d9be  rovik-agentd-9.9.8-aarch64\n";
    pub const SHA256SUMS_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRQzV4aFZ5TkVvMWoyS0N3ekloYnhCeTRITnJoVldKVDZXS0hTWnF6d1VYUy8vdE81OS9jTjkrcmNucW1HczNvWjdzZElVOHVtb05tem5Gb3BwUWJTMnpzcmZ0akhoNXdjPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkyMTMwMjY3CWZpbGU6U0hBMjU2U1VNUwpuWlViajRRWVQzdjdPZTdxWkN2blhJdG5UQitZMGkzenlBVHhwVHFXL09aRWw1RFBKWHVNRTdCRlM1c3EyY0hLM2Mzdk5xcFN3RzdXTlBIVDNpc1VCdz09Cg==";
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    #[test]
    fn what_tauri_signer_signed_is_checked_with_its_key() {
        assert_eq!(verify(KEY, BUILD.as_bytes(), BUILD_SIG), Ok(()));
        assert_eq!(verify(KEY, SHA256SUMS.as_bytes(), SHA256SUMS_SIG), Ok(()));
        let changed = BUILD.replace("9.9.8", "9.9.9");
        let refused = verify(KEY, changed.as_bytes(), BUILD_SIG).unwrap_err();
        assert!(refused.contains("doesn't match"), "{refused}");
        assert!(
            verify(KEY, BUILD.as_bytes(), SHA256SUMS_SIG).is_err(),
            "another file's"
        );
        // minisign's own text, unwrapped, reads the same.
        let text = |b64: &str| unwrapped(b64).unwrap();
        assert_eq!(
            verify(&text(KEY), BUILD.as_bytes(), &text(BUILD_SIG)),
            Ok(())
        );
        let line = text(KEY).lines().nth(1).unwrap().to_string();
        assert_eq!(verify(&line, BUILD.as_bytes(), BUILD_SIG), Ok(()));
        assert!(verify("not a key", BUILD.as_bytes(), BUILD_SIG)
            .unwrap_err()
            .contains("can't be read"));
        assert!(verify(KEY, BUILD.as_bytes(), "junk").is_err());
    }

    #[test]
    fn the_key_file_has_comments_and_maybe_a_key() {
        assert_eq!(key_in("# none yet\n\n# still none\n"), None);
        // minisign's own .pub: the key under its comment.
        let own = "untrusted comment: minisign public key: D628D1C85518E702\nRWQC5x==\n";
        assert_eq!(key_in(own).as_deref(), Some("RWQC5x=="));
        assert_eq!(
            key_in("# the key\n  abc==  \nnext\n").as_deref(),
            Some("abc==")
        );
        // The repository's file, as it is.
        let _ = key_in(KEY_FILE);
    }

    #[test]
    fn a_releases_list_names_each_builds_hash() {
        let listed = sums(SHA256SUMS);
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed[0],
            (BUILD_SHA256.into(), "rovik-agentd-9.9.8-x86_64".into())
        );
        let upper = format!("{} *rovik-agentd-1.0.0-aarch64\nnoise\n\n", "AB".repeat(32));
        assert_eq!(
            sums(&upper),
            [("ab".repeat(32), "rovik-agentd-1.0.0-aarch64".to_string())]
        );
        assert!(sums("abc  short-hash\n").is_empty());
    }

    #[test]
    fn builds_are_named_by_version_and_cpu() {
        assert_eq!(build_name("1.2.3", "x86_64"), "rovik-agentd-1.2.3-x86_64");
        assert_eq!(cpu("amd64"), Some("x86_64"));
        assert_eq!(cpu("arm64"), Some("aarch64"));
        assert_eq!(cpu("riscv64"), None);
    }
}
