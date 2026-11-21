//! Exact byte identities. Hashes establish identity, not creator authority.

use crate::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest as _, Sha256};

/// A validated SHA-256 identity; cannot contain a filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    /// Hash and copy a bounded stream without retaining its contents in memory.
    pub fn copy(
        mut reader: impl std::io::Read,
        mut writer: impl std::io::Write,
        maximum_bytes: u64,
    ) -> Result<(Self, u64)> {
        let mut hash = Sha256::new();
        let mut count = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let length = reader.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            count = count
                .checked_add(length as u64)
                .ok_or_else(|| Error::new("stream length overflow".into()))?;
            crate::require(
                count <= maximum_bytes,
                format!("content exceeds the {maximum_bytes}-byte limit"),
            )?;
            hash.update(&buffer[..length]);
            writer.write_all(&buffer[..length])?;
        }
        let hex: String = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok((Self(format!("sha256:{hex}")), count))
    }
    /// Hash exact bytes without normalizing source text.
    pub fn of(bytes: &[u8]) -> Self {
        let hex: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self(format!("sha256:{hex}"))
    }
    /// Validate an externally supplied identity, without granting it authority.
    pub fn parse(text: &str) -> Result<Self> {
        crate::require(
            text.len() == 71
                && text.starts_with("sha256:")
                && text[7..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid SHA-256 digest",
        )?;
        Ok(Self(text.into()))
    }
    /// Digest text including the algorithm prefix.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Lowercase hexadecimal part used by the controller-owned store.
    pub fn hex(&self) -> &str {
        &self.0[7..]
    }
    /// Identity of canonical controller metadata.
    pub fn of_json(value: &impl Serialize) -> Result<Self> {
        Ok(Self::of(&crate::wire::canonical(value)?))
    }
}
impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::classified(
            crate::FailureCategory::Infrastructure,
            format!("filesystem operation failed: {e}"),
        )
    }
}

/// Every acceptance-sensitive input of one verification run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceKey {
    /// Controller-issued epoch for an explicitly requested fresh verification.
    /// Absent for the original pass; older runs keep their canonical identities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_nonce: Option<Digest>,
    /// Source tree and all declared inputs.
    pub candidate_digest: Digest,
    /// Approved domain meaning.
    pub contract_digest: Digest,
    /// Selected design and responsibility ownership.
    pub design_digest: Digest,
    /// Effective policy, registry and exceptions.
    pub policy_digest: Digest,
    /// Protected acceptance drivers and expectations.
    pub oracle_digest: Digest,
    /// Complete effective check matrix.
    pub plan_digest: Digest,
    /// Pinned tools, runtime and dependencies.
    pub environment_digest: Digest,
}

/// A typed reference to exact bytes, never to an arbitrary host file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// Content identity.
    pub digest: Digest,
    /// Declared format.
    pub media_type: String,
    /// Exact stored length.
    pub byte_count: u64,
    /// Registered artifact kind.
    pub kind: String,
}
