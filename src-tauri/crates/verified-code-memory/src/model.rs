use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Memory category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// A prior task, attempt, failure, or fix.
    Episode,
    /// A durable repository fact or convention.
    ProjectFact,
    /// A reusable workflow or procedure.
    Procedure,
}

impl fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Episode => write!(f, "episode"),
            Self::ProjectFact => write!(f, "project fact"),
            Self::Procedure => write!(f, "procedure"),
        }
    }
}

/// Trust state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// Usable as a hint only.
    Provisional,
    /// Supported by execution or review evidence.
    Verified,
    /// Excluded from retrieval.
    Deprecated,
}

impl fmt::Display for Verification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provisional => write!(f, "provisional"),
            Self::Verified => write!(f, "verified"),
            Self::Deprecated => write!(f, "deprecated"),
        }
    }
}

/// Whether a memory's claim is still expected to hold for the code as it is
/// now — a question apart from [`Verification`], which says what evidence once
/// supported the claim.
///
/// A record can be verified and no longer current (the code it is about
/// changed), or provisional and current (the user just said so). Neither
/// follows from the other, so neither is derived from the other.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Applicability {
    /// Nothing suggests the claim changed since someone checked it.
    Current,
    /// The claim's text, or the code it names, changed after the evidence was
    /// gathered: it may well still be true, and it is to be checked again
    /// before it is relied on.
    NeedsRevalidation,
    /// No one can say: a record without the metadata to tell, or one whose
    /// claim was never checked in the first place.
    #[default]
    Unknown,
}

impl fmt::Display for Applicability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Current => write!(f, "current"),
            Self::NeedsRevalidation => write!(f, "needs_revalidation"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Where a memory's claim came from: what kind of source asserted it, which is
/// what makes "the user said so" weigh differently from "an agent inferred it".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// The user said so: a correction of a memory, or a convention they stated.
    UserAssertion,
    /// An agent's reading of the code.
    CodeObservation,
    /// A check's result.
    TestResult,
    /// An agent's deduction from other evidence.
    GeneratedDeduction,
    /// An older record that did not say, or one written before this was kept.
    #[default]
    Unknown,
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UserAssertion => write!(f, "user assertion"),
            Self::CodeObservation => write!(f, "code observation"),
            Self::TestResult => write!(f, "test result"),
            Self::GeneratedDeduction => write!(f, "generated deduction"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// The identity of a claim's text: its title and content, each with its runs of
/// whitespace collapsed, hashed with FNV-1a.
///
/// Stored when a claim is validated, so that a later edit can tell that the
/// text the evidence was about is not the text that is stored now. Two records
/// with the same claim share it, which is what makes deduplication and
/// supersession decidable without another model call.
///
/// The two fields are hashed with a separator between them, because where the
/// title ends is part of what a claim says: a word moved from the title into
/// the body is a different claim, and hashing the words of both as one run of
/// text — as this once did — made `"Retry race"` over `"seed the queue"` and
/// `"Retry"` over `"race seed the queue"` the same claim, so an edit that moved
/// words across the boundary kept the trust earned by the old wording.
#[must_use]
pub fn claim_hash(title: &str, content: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    feed(&mut hash, title);
    // A newline, which the collapsed text of a field cannot contain.
    hash ^= u64::from(b'\n');
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    feed(&mut hash, content);
    format!("{hash:016x}")
}

/// Adds `text`'s words, whitespace collapsed, to the hash being built.
fn feed(hash: &mut u64, text: &str) {
    for byte in text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .bytes()
    {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// Persistent memory record.
///
/// Two questions are kept apart, because they have different answers: what
/// evidence once supported what it says ([`Verification`]), and whether that is
/// expected to hold for the code now ([`Applicability`]). A record can be
/// verified and out of date, or unproven and current.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MemoryRecord {
    /// Stable identifier.
    pub id: Uuid,
    /// Memory category.
    pub kind: MemoryKind,
    /// Trust state: what evidence once supported the claim.
    pub verification: Verification,
    /// Whether the claim is expected to hold for the code as it is now.
    pub applicability: Applicability,
    /// What kind of source asserted the claim.
    pub source_kind: SourceKind,
    /// Repository identifier.
    pub repo: String,
    /// Short title.
    pub title: String,
    /// Reusable content.
    pub content: String,
    /// The claim's identity as text ([`claim_hash`] of the title and content):
    /// recomputed when either changes.
    pub claim_hash: String,
    /// What this record *is*, within its project: the same identity means the
    /// same thing said again — a later run's fix for the same failure, a
    /// restatement of the same convention — so a newer one revises the older
    /// instead of piling up beside it.
    ///
    /// Empty when nobody can say (a record written before identities were kept),
    /// and such a record is never treated as a duplicate of anything.
    pub identity: String,
    /// The claim hash the evidence in [`Self::evidence`] was gathered about.
    /// Different from `claim_hash` means the text was edited afterwards, and
    /// the evidence no longer speaks about what is stored now.
    pub validated_claim_hash: Option<String>,
    /// Associated files.
    pub files: Vec<String>,
    /// Searchable tags.
    pub tags: Vec<String>,
    /// Repository revision.
    pub code_version: Option<String>,
    /// Source episode for distilled knowledge.
    pub source_episode: Option<Uuid>,
    /// Evidence supporting verification or deprecation.
    pub evidence: Option<String>,
    /// Confidence from 0 to 1.
    pub confidence: f32,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// When the title or text last changed. Recency is read from this: what a
    /// claim says is what goes stale, not how often it was looked at.
    pub content_updated_at: DateTime<Utc>,
    /// When anything about the record last changed.
    pub updated_at: DateTime<Utc>,
    /// When evidence last confirmed the claim; unset: never.
    pub last_validated_at: Option<DateTime<Utc>>,
    /// When a run last had it in hand; unset: never.
    pub last_retrieved_at: Option<DateTime<Utc>>,
    /// Observed uses.
    pub use_count: u32,
    /// Successful observed uses.
    pub success_count: u32,
}

impl MemoryRecord {
    /// Stable paging position, including the project and original update time.
    /// It remains valid if the anchor record is edited, touched, or deleted.
    pub fn page_cursor(&self) -> String {
        format!(
            "{}|{}|{}",
            claim_hash(&self.repo, "memory-page-v1"),
            self.updated_at.to_rfc3339(),
            self.id
        )
    }
    /// Whether the evidence a record carries speaks about the claim it says
    /// now: only when the claim was not edited after it was validated.
    ///
    /// A record never validated, or validated before the claim was edited, has
    /// no such evidence — which is why an edit cannot inherit trust.
    #[must_use]
    pub fn evidence_matches_claim(&self) -> bool {
        self.validated_claim_hash.as_deref() == Some(self.claim_hash.as_str())
    }

    /// Whether a run should rely on it as it is: verified, and about the claim
    /// and code as they are now.
    #[must_use]
    pub fn is_reliable(&self) -> bool {
        self.verification == Verification::Verified
            && self.applicability == Applicability::Current
            && self.evidence_matches_claim()
    }
}

/// How many memories one project has, by trust state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStats {
    /// Every memory of the project, whatever its state.
    pub total: u32,
    /// Memories promoted by evidence.
    pub verified: u32,
    /// Memories not yet confirmed.
    pub provisional: u32,
    /// Memories excluded from retrieval.
    pub deprecated: u32,
}

/// Input for a new provisional memory.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NewMemory {
    /// Memory category.
    pub kind: MemoryKind,
    /// Repository identifier.
    pub repo: String,
    /// Short title.
    pub title: String,
    /// Reusable content.
    pub content: String,
    /// Associated files.
    pub files: Vec<String>,
    /// Tags.
    pub tags: Vec<String>,
    /// Repository revision.
    pub code_version: Option<String>,
    /// Source episode.
    pub source_episode: Option<Uuid>,
    /// Evidence.
    pub evidence: Option<String>,
    /// What kind of source asserts the claim.
    pub source_kind: SourceKind,
    /// What the record is, within its project (see
    /// [`MemoryRecord::identity`]): what a newer record with the same value
    /// revises.
    pub identity: String,
    /// Confidence.
    pub confidence: f32,
}

impl NewMemory {
    /// Creates an episode.
    pub fn episode(
        repo: impl Into<String>,
        title: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self::new(MemoryKind::Episode, repo, title, content)
    }

    /// Creates a project fact.
    pub fn project_fact(
        repo: impl Into<String>,
        title: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self::new(MemoryKind::ProjectFact, repo, title, content)
    }

    /// Creates a procedure.
    pub fn procedure(
        repo: impl Into<String>,
        title: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self::new(MemoryKind::Procedure, repo, title, content)
    }

    fn new(
        kind: MemoryKind,
        repo: impl Into<String>,
        title: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            repo: repo.into(),
            title: title.into(),
            content: content.into(),
            files: Vec::new(),
            tags: Vec::new(),
            code_version: None,
            source_episode: None,
            evidence: None,
            source_kind: SourceKind::Unknown,
            identity: String::new(),
            confidence: 0.5,
        }
    }

    /// Adds files.
    #[must_use]
    pub fn with_files(mut self, files: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.files = files.into_iter().map(Into::into).collect();
        self
    }

    /// Adds tags.
    #[must_use]
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Adds a revision.
    #[must_use]
    pub fn with_code_version(mut self, version: impl Into<String>) -> Self {
        self.code_version = Some(version.into());
        self
    }

    /// Adds provenance.
    #[must_use]
    pub fn with_source_episode(mut self, source: Uuid) -> Self {
        self.source_episode = Some(source);
        self
    }

    /// Says what kind of source asserts the claim: the user, an observation of
    /// the code, a check's result, or the agent's own deduction.
    #[must_use]
    pub fn with_source_kind(mut self, source: SourceKind) -> Self {
        self.source_kind = source;
        self
    }

    /// Says what this record is within its project: a later record with the same
    /// identity revises it ([`MemoryRecord::identity`]).
    #[must_use]
    pub fn with_identity(mut self, identity: impl Into<String>) -> Self {
        self.identity = identity.into();
        self
    }

    /// Adds evidence.
    #[must_use]
    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = Some(evidence.into());
        self
    }

    /// Sets confidence, clamped to 0..1.
    #[must_use]
    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }
}

/// Retrieved memory and score explanation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RetrievedMemory {
    /// Underlying record.
    pub record: MemoryRecord,
    /// Total score.
    pub score: f32,
    /// Score explanation.
    pub reason: String,
    /// Component scores.
    pub components: crate::retrieval::MemoryScore,
}

/// Retrieval request.
///
/// The filters are part of the query, not of the ranking: a memory the caller
/// may not see (another chat's own work, a state it doesn't want) is never a
/// candidate, so it can't take a slot from one the caller may see.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RetrievalQuery {
    /// Repository.
    pub repo: String,
    /// Current task.
    pub task: String,
    /// Relevant files.
    pub files: Vec<String>,
    /// Current failure.
    pub failure: Option<String>,
    /// Maximum results.
    pub limit: usize,
    /// Trust states the caller accepts; empty means every usable state.
    /// [`Verification::Deprecated`] is never retrieved, whichever states are
    /// listed: a deprecated memory is history, and `list` is where it shows.
    pub verifications: Vec<Verification>,
    /// Tags a record must not carry: a chat's own work is in the chat
    /// already, so it isn't prior work to that chat.
    pub excluded_tags: Vec<String>,
}

impl RetrievalQuery {
    /// Creates a query with limit 8.
    pub fn new(repo: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            task: task.into(),
            files: Vec::new(),
            failure: None,
            limit: 8,
            verifications: Vec::new(),
            excluded_tags: Vec::new(),
        }
    }

    /// Adds files.
    #[must_use]
    pub fn with_files(mut self, files: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.files = files.into_iter().map(Into::into).collect();
        self
    }

    /// Adds a failure.
    #[must_use]
    pub fn with_failure(mut self, failure: impl Into<String>) -> Self {
        self.failure = Some(failure.into());
        self
    }

    /// Sets limit.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Accepts only these trust states.
    #[must_use]
    pub fn with_verifications(
        mut self,
        verifications: impl IntoIterator<Item = Verification>,
    ) -> Self {
        self.verifications = verifications.into_iter().collect();
        self
    }

    /// Leaves out the records carrying any of these tags.
    #[must_use]
    pub fn without_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.excluded_tags = tags.into_iter().map(Into::into).collect();
        self
    }
}
