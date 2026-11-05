use crate::error::Result;
use uuid::Uuid;

/// Pluggable embedding backend.
pub trait MemoryEmbedder: Send + Sync {
    /// Embeds text.
    ///
    /// # Errors
    /// Implementations should return an error for model or provider failures.
    fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Embedding dimension.
    fn dimensions(&self) -> usize;
}

/// Pluggable vector-search backend.
pub trait VectorIndex: Send + Sync {
    /// Searches one repository scope.
    ///
    /// # Errors
    /// Implementations should return an error for index or provider failures.
    fn search(&self, repo: &str, query: &[f32], limit: usize) -> Result<Vec<(Uuid, f32)>>;
}
