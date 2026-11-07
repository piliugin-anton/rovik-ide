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

/// Offline sentence embedding using a cached local model directory. A Chroma
/// MiniLM ONNX export uses Python, onnxruntime, tokenizers and numpy; other
/// directories use sentence-transformers. Dependencies are operator-installed. No
/// network downloads or shell evaluation occur on a retrieval request.
#[derive(Debug, Clone)]
pub struct LocalSentenceEmbedder {
    python: std::path::PathBuf,
    model: std::path::PathBuf,
    dimensions: usize,
    onnx: bool,
    worker: std::sync::Arc<std::sync::Mutex<Option<Worker>>>,
}

impl LocalSentenceEmbedder {
    /// Selects an interpreter and local model; the revision is the caller's
    /// index identity, so a changed model must use a different revision.
    pub fn new(
        python: std::path::PathBuf,
        model: std::path::PathBuf,
        dimensions: usize,
    ) -> Result<Self> {
        if !python.is_absolute() || !model.is_absolute() || !model.is_dir() || dimensions == 0 {
            return Err(crate::VcmError::EmptyField {
                field: "absolute interpreter and local model directory",
            });
        }
        let onnx = model.join("model.onnx").is_file();
        if onnx && (!model.join("tokenizer.json").is_file() || dimensions != 384) {
            return Err(crate::VcmError::EmbeddingUnavailable(
                "MiniLM ONNX requires tokenizer.json and 384 dimensions; check the local export",
            ));
        }
        Ok(Self {
            python,
            model,
            dimensions,
            onnx,
            worker: Default::default(),
        })
    }
}

#[derive(Debug)]
struct Worker {
    child: std::process::Child,
    input: std::process::ChildStdin,
    replies: std::sync::mpsc::Receiver<Vec<u8>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl MemoryEmbedder for LocalSentenceEmbedder {
    fn dimensions(&self) -> usize {
        self.dimensions
    }
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        use std::io::{BufRead, Read, Write};
        use std::process::{Command, Stdio};
        let error = || crate::VcmError::EmbeddingUnavailable("local worker failed or timed out");
        let mut slot = self.worker.try_lock().map_err(|_| {
            crate::VcmError::EmbeddingUnavailable("local worker is busy or unavailable")
        })?;
        if slot.is_none() {
            let script = if self.onnx {
                include_str!("minilm.py")
            } else {
                "import json,sys\nfrom sentence_transformers import SentenceTransformer\nm=SentenceTransformer(sys.argv[1],local_files_only=True,device='cpu')\nfor line in sys.stdin:\n print(json.dumps(m.encode(json.loads(line),normalize_embeddings=True).tolist()),flush=True)"
            };
            let mut child = Command::new(&self.python)
                .args(["-u", "-c", script])
                .arg(&self.model)
                .env("HF_HUB_OFFLINE", "1")
                .env("TRANSFORMERS_OFFLINE", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| error())?;
            let input = child.stdin.take().ok_or_else(error)?;
            let output = child.stdout.take().ok_or_else(error)?;
            let (send, replies) = std::sync::mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(output);
                loop {
                    let mut bytes = Vec::new();
                    match reader
                        .by_ref()
                        .take(2_000_000)
                        .read_until(b'\n', &mut bytes)
                    {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if bytes.last() != Some(&b'\n') || send.send(bytes).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
            *slot = Some(Worker {
                child,
                input,
                replies,
            });
        }
        let worker = slot.as_mut().ok_or_else(error)?;
        // At most 4 KiB including JSON escaping: this fits an empty pipe while
        // the model loads, so writing cannot deadlock before the response timeout.
        let mut bounded = String::new();
        for c in text.chars().take(500) {
            bounded.push(c);
        }
        let mut request = serde_json::to_vec(&bounded)?;
        request.push(b'\n');
        let response = worker
            .input
            .write_all(&request)
            .map_err(|_| error())
            .and_then(|_| {
                worker
                    .replies
                    .recv_timeout(std::time::Duration::from_secs(15))
                    .map_err(|_| error())
            });
        let bytes = match response {
            Ok(bytes) => bytes,
            Err(error) => {
                *slot = None;
                return Err(error);
            }
        };
        let vector: Vec<f32> = serde_json::from_slice(&bytes)?;
        if vector.len() != self.dimensions {
            return Err(crate::VcmError::EmbeddingDimensionMismatch {
                expected: self.dimensions,
                actual: vector.len(),
            });
        }
        Ok(vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onnx_exports_require_a_tokenizer_and_minilm_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let python = dir.path().join("python");
        std::fs::write(dir.path().join("model.onnx"), []).unwrap();
        assert!(LocalSentenceEmbedder::new(python.clone(), dir.path().into(), 384).is_err());
        std::fs::write(dir.path().join("tokenizer.json"), "{}").unwrap();
        assert!(LocalSentenceEmbedder::new(python.clone(), dir.path().into(), 300).is_err());
        let backend = LocalSentenceEmbedder::new(python, dir.path().into(), 384).unwrap();
        assert!(backend.onnx);
    }

    #[test]
    fn existing_sentence_transformer_directories_keep_their_backend() {
        let dir = tempfile::tempdir().unwrap();
        let backend =
            LocalSentenceEmbedder::new(dir.path().join("python"), dir.path().into(), 300).unwrap();
        assert!(!backend.onnx);
        assert_eq!(backend.dimensions(), 300);
    }

    #[test]
    #[ignore = "requires ROVIK_MEMORY_EVAL_MODEL and ROVIK_MEMORY_EVAL_PYTHON with local MiniLM dependencies"]
    fn local_minilm_worker_reuses_normalized_vectors_and_recalls_paraphrases() {
        let model = std::env::var_os("ROVIK_MEMORY_EVAL_MODEL").unwrap();
        let python = std::env::var_os("ROVIK_MEMORY_EVAL_PYTHON").unwrap();
        let backend = LocalSentenceEmbedder::new(python.into(), model.into(), 384).unwrap();
        assert!(backend.onnx);
        let store = crate::SharedMemoryStore::open_in_memory().unwrap();
        let record = store
            .remember(crate::NewMemory::procedure(
                "a",
                "Concurrent state",
                "Use a mutex to protect shared data.",
            ))
            .unwrap();
        let index = crate::semantic::SemanticIndex::new(store, "minilm-test", 384).unwrap();
        assert_eq!(index.backfill("a", &backend).unwrap(), 1);
        let query = crate::RetrievalQuery::new("a", "Prevent simultaneous access with a lock");
        let vector = backend.embed(&query.task).unwrap();
        assert_eq!(vector.len(), 384);
        assert!(vector.iter().all(|v| v.is_finite()));
        assert!((vector.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-5);
        assert_eq!(backend.embed(&query.task).unwrap(), vector);
        let hits = index.search(&query, &vector, 0.3).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, record.id);
    }
}
