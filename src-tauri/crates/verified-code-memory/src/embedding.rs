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

/// Offline sentence-transformers backend using a cached local model directory.
/// Python and sentence-transformers must be installed by the operator. No
/// network downloads or shell evaluation occur on a retrieval request.
#[derive(Debug, Clone)]
pub struct LocalSentenceEmbedder {
    python: std::path::PathBuf,
    model: std::path::PathBuf,
    dimensions: usize,
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
        Ok(Self {
            python,
            model,
            dimensions,
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
            let script = "import json,sys\nfrom sentence_transformers import SentenceTransformer\nm=SentenceTransformer(sys.argv[1],local_files_only=True,device='cpu')\nfor line in sys.stdin:\n print(json.dumps(m.encode(json.loads(line),normalize_embeddings=True).tolist()),flush=True)";
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
