use uuid::Uuid;

/// Errors returned by VerifiedCodeMemory.
#[derive(Debug, thiserror::Error)]
pub enum VcmError {
    /// A replication page is malformed, too large or crosses a scope boundary.
    #[error("invalid memory synchronization: {0}")]
    InvalidSync(&'static str),
    /// Paging state is invalid or belongs to a different project.
    #[error("invalid memory page cursor; refresh the list")]
    InvalidCursor,
    /// An optional embedding backend cannot serve the request.
    #[error("embedding unavailable: {0}")]
    EmbeddingUnavailable(&'static str),
    /// Loading or running the local ONNX model failed.
    #[error("local ONNX embedding failed: {0}")]
    Onnx(String),
    /// A newer application owns this database; an older writer must not alter it.
    #[error(
        "memory schema {actual} is newer than supported schema {supported}; update the application"
    )]
    UnsupportedSchema {
        /// Version found in the database.
        actual: i64,
        /// Newest version this library can write.
        supported: i64,
    },
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
