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

/// Persistent memory record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MemoryRecord {
    /// Stable identifier.
    pub id: Uuid,
    /// Memory category.
    pub kind: MemoryKind,
    /// Trust state.
    pub verification: Verification,
    /// Repository identifier.
    pub repo: String,
    /// Short title.
    pub title: String,
    /// Reusable content.
    pub content: String,
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
    /// Last update time.
    pub updated_at: DateTime<Utc>,
    /// Observed uses.
    pub use_count: u32,
    /// Successful observed uses.
    pub success_count: u32,
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
    /// Confidence.
    pub confidence: f32,
}

impl NewMemory {
    /// Creates an episode.
    pub fn episode(repo: impl Into<String>, title: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(MemoryKind::Episode, repo, title, content)
    }

    /// Creates a project fact.
    pub fn project_fact(repo: impl Into<String>, title: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(MemoryKind::ProjectFact, repo, title, content)
    }

    /// Creates a procedure.
    pub fn procedure(repo: impl Into<String>, title: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(MemoryKind::Procedure, repo, title, content)
    }

    fn new(kind: MemoryKind, repo: impl Into<String>, title: impl Into<String>, content: impl Into<String>) -> Self {
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
}
