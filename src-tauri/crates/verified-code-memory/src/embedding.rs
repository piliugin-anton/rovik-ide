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

/// Native, offline Chroma all-MiniLM-L6-v2 inference. The caller provisions
/// the local model and tokenizer; embedding never downloads or starts a process.
pub struct OnnxMiniLm {
    tokenizer: tokenizers::Tokenizer,
    model:
        std::sync::Mutex<tract_onnx::prelude::TypedRunnableModel<tract_onnx::prelude::TypedModel>>,
}

/// The number of dimensions produced by the pinned MiniLM model.
pub const MINILM_DIMENSIONS: usize = 384;
/// Model identity, including the preprocessing policy, for derived SQLite vectors.
pub const MINILM_REVISION: &str = "chroma-minilm-l6-v2-913d7300-title-claim-native-v1";
const TOKENS: usize = 256;

fn onnx_error(error: impl std::fmt::Display) -> crate::VcmError {
    crate::VcmError::Onnx(error.to_string())
}

impl OnnxMiniLm {
    /// Loads an ONNX model and tokenizer from `directory`. CPU inference is
    /// compiled once and reused. Missing or unsupported model files return errors.
    pub fn new(directory: &std::path::Path) -> Result<Self> {
        use tract_onnx::prelude::*;
        let mut tokenizer = tokenizers::Tokenizer::from_file(directory.join("tokenizer.json"))
            .map_err(onnx_error)?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: TOKENS,
                ..Default::default()
            }))
            .map_err(onnx_error)?;
        tokenizer.with_padding(Some(tokenizers::PaddingParams {
            strategy: tokenizers::PaddingStrategy::Fixed(TOKENS),
            ..Default::default()
        }));
        let mut model = tract_onnx::onnx()
            .model_for_path(directory.join("model.onnx"))
            .map_err(onnx_error)?;
        for input in 0..3 {
            model = model
                .with_input_fact(input, i64::fact([1, TOKENS]).into())
                .map_err(onnx_error)?;
        }
        let model = model
            .into_optimized()
            .map_err(onnx_error)?
            .into_runnable()
            .map_err(onnx_error)?;
        Ok(Self {
            tokenizer,
            model: std::sync::Mutex::new(model),
        })
    }
}

impl MemoryEmbedder for OnnxMiniLm {
    fn dimensions(&self) -> usize {
        MINILM_DIMENSIONS
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        use tract_onnx::prelude::*;
        let model = self.model.try_lock().map_err(|_| {
            crate::VcmError::EmbeddingUnavailable("ONNX model is busy or unavailable")
        })?;
        let text: String = text.chars().take(500).collect();
        let tokens = self.tokenizer.encode(text, true).map_err(onnx_error)?;
        let tensor = |values: &[u32]| {
            Tensor::from_shape(
                &[1, TOKENS],
                &values.iter().map(|v| i64::from(*v)).collect::<Vec<_>>(),
            )
            .map(TValue::from)
            .map_err(onnx_error)
        };
        let outputs = model
            .run(tvec![
                tensor(tokens.get_ids())?,
                tensor(tokens.get_attention_mask())?,
                tensor(tokens.get_type_ids())?,
            ])
            .map_err(onnx_error)?;
        let hidden = outputs
            .first()
            .ok_or_else(|| onnx_error("model returned no output"))?
            .to_array_view::<f32>()
            .map_err(onnx_error)?;
        if hidden.shape() != [1, TOKENS, MINILM_DIMENSIONS] {
            return Err(onnx_error(
                "MiniLM must return [1, 256, 384] token embeddings",
            ));
        }
        let values: Vec<f32> = hidden.iter().copied().collect();
        pool(&values, tokens.get_attention_mask())
    }
}

fn pool(hidden: &[f32], mask: &[u32]) -> Result<Vec<f32>> {
    if hidden.len() != mask.len() * MINILM_DIMENSIONS {
        return Err(onnx_error(
            "token embeddings do not match the attention mask",
        ));
    }
    let mut vector = vec![0.0f64; MINILM_DIMENSIONS];
    for (token, &include) in hidden.chunks_exact(MINILM_DIMENSIONS).zip(mask) {
        if include != 0 {
            for (sum, &value) in vector.iter_mut().zip(token) {
                *sum += f64::from(value);
            }
        }
    }
    // The masked mean and masked sum have the same L2-normalized direction.
    let norm = vector.iter().map(|value| value * value).sum::<f64>().sqrt();
    if !norm.is_finite() || norm <= 0.0 {
        return Err(onnx_error("MiniLM returned a zero or non-finite embedding"));
    }
    Ok(vector
        .into_iter()
        .map(|value| (value / norm) as f32)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_does_not_contribute_to_the_normalized_embedding() {
        let mut hidden = vec![0.0; 3 * MINILM_DIMENSIONS];
        hidden[0] = 6.0;
        hidden[MINILM_DIMENSIONS + 1] = 8.0;
        hidden[2 * MINILM_DIMENSIONS..].fill(1000.0);
        let vector = pool(&hidden, &[1, 1, 0]).unwrap();
        assert!((vector[0] - 0.6).abs() < 1e-6);
        assert!((vector[1] - 0.8).abs() < 1e-6);
        assert!(vector[2..].iter().all(|value| *value == 0.0));
    }

    #[test]
    fn invalid_outputs_cannot_enter_the_semantic_index() {
        for hidden in [
            vec![0.0; MINILM_DIMENSIONS],
            vec![f32::NAN; MINILM_DIMENSIONS],
            vec![1.0; 300],
        ] {
            assert!(pool(&hidden, &[1]).is_err());
        }
        assert!(pool(&[], &[]).is_err());
    }

    #[test]
    fn a_missing_model_returns_an_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            OnnxMiniLm::new(dir.path()),
            Err(crate::VcmError::Onnx(_))
        ));
    }

    #[test]
    #[ignore = "requires ROVIK_MEMORY_EVAL_MODEL pointing to the pinned local ONNX files"]
    fn native_minilm_matches_the_reference_and_recalls_paraphrases() {
        let model = std::env::var_os("ROVIK_MEMORY_EVAL_MODEL").unwrap();
        let backend = OnnxMiniLm::new(std::path::Path::new(&model)).unwrap();
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/minilm-reference.json")).unwrap();
        let vector = backend.embed(reference["text"].as_str().unwrap()).unwrap();
        let expected: Vec<f32> = serde_json::from_value(reference["embedding"].clone()).unwrap();
        assert_eq!(vector.len(), MINILM_DIMENSIONS);
        for (actual, expected) in vector.iter().zip(&expected) {
            assert!(
                (actual - expected).abs() < 1e-4,
                "native {actual}, reference {expected}"
            );
        }
        assert_eq!(
            backend.embed(reference["text"].as_str().unwrap()).unwrap(),
            vector
        );
        let store = crate::SharedMemoryStore::open_in_memory().unwrap();
        let wanted = store
            .remember(crate::NewMemory::procedure(
                "a",
                "Concurrent state",
                "Use a mutex to protect shared data.",
            ))
            .unwrap();
        let index =
            crate::semantic::SemanticIndex::new(store, MINILM_REVISION, MINILM_DIMENSIONS).unwrap();
        index.backfill("a", &backend).unwrap();
        let hits = index
            .retrieve(
                &crate::RetrievalQuery::new("a", "Prevent simultaneous access with a lock"),
                &backend,
                0.3,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.id, wanted.id);
    }
}
