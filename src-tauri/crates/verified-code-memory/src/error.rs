use uuid::Uuid;

/// Errors returned by VerifiedCodeMemory.
#[derive(Debug, thiserror::Error)]
pub enum VcmError {
    /// SQLite operation failed.
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    /// JSON serialization failed.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// A required field was empty.
    #[error("{field} must not be empty")]
    EmptyField {
        /// Invalid field name.
        field: &'static str,
    },

    /// A numeric value was outside its permitted range.
    #[error("{field} must be between {minimum} and {maximum}, got {actual}")]
    OutOfRange {
        /// Invalid field name.
        field: &'static str,
        /// Minimum accepted value.
        minimum: f64,
        /// Maximum accepted value.
        maximum: f64,
        /// Supplied value.
        actual: f64,
    },

    /// A deprecated memory cannot be verified.
    #[error("memory {0} is deprecated and cannot be verified")]
    Deprecated(Uuid),

    /// The requested memory was not found.
    #[error("memory {0} was not found")]
    NotFound(Uuid),

    /// Retrieval limit must be between 1 and 32.
    #[error("invalid retrieval limit: {0}; must be between 1 and 32")]
    InvalidLimit(usize),

    /// The shared-store mutex was poisoned.
    #[error("memory-store lock poisoned")]
    LockPoisoned,

    /// An embedding dimension did not match the index configuration.
    #[error("embedding dimension mismatch: expected {expected}, got {actual}")]
    EmbeddingDimensionMismatch {
        /// Expected vector dimension.
        expected: usize,
        /// Actual vector dimension.
        actual: usize,
    },
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, VcmError>;
