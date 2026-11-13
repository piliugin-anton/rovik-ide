//! Opt-in reliable generation: shared Rovik networking with controller-owned retries.

use crate::{
    agent::ChatStreamEvent,
    chat_request::{self, Api, Clients, Limits, RequestSpec},
};
use rovik_harness::{
    provider::{ModelRequest, ModelResponse, Provider, Reservation, Usage},
    runner::Cancellation,
    wire, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Explicit billing rates in microunits per million tokens; zero is a deliberate local-model price.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rates {
    /// Uncached input price.
    pub input: u64,
    /// Cached input price. Missing cache counts conservatively use the input price.
    pub cached_input: u64,
    /// Output price.
    pub output: u64,
}
impl Rates {
    fn reservation(
        &self,
        request_bytes: usize,
        control_bytes: usize,
        output: u64,
    ) -> Result<Reservation> {
        // The serialized user message is bounded independently by the controller.
        // UTF-8 bytes conservatively bound input tokens; reserve additional framing
        // for the role and the chat-completions message envelope.
        let input = u64::try_from(request_bytes)
            .ok()
            .and_then(|n| n.checked_add(control_bytes as u64))
            .and_then(|n| n.checked_add(16 * 1024))
            .ok_or_else(|| Error::new("model input reservation overflow".into()))?;
        let tokens = input
            .checked_add(output)
            .ok_or_else(|| Error::new("model token reservation overflow".into()))?;
        let numerator = (u128::from(input) * u128::from(self.input.max(self.cached_input)))
            .checked_add(u128::from(output) * u128::from(self.output))
            .ok_or_else(|| Error::new("model cost reservation overflow".into()))?;
        let cost_microunits = u64::try_from(numerator.div_ceil(1_000_000))
            .map_err(|_| Error::new("model cost reservation overflow".into()))?;
        Ok(Reservation {
            tokens,
            cost_microunits,
        })
    }
    /// Price actual provider counts; missing or invalid usage remains unresolved.
    pub(crate) fn usage(&self, value: serde_json::Value) -> Result<Usage> {
        let input = value
            .get("prompt_tokens")
            .and_then(|n| n.as_u64())
            .ok_or_else(|| Error::new("provider omitted prompt token usage".into()))?;
        let output = value
            .get("completion_tokens")
            .and_then(|n| n.as_u64())
            .ok_or_else(|| Error::new("provider omitted completion token usage".into()))?;
        let cached = value
            .pointer("/prompt_tokens_details/cached_tokens")
            .map(|n| {
                n.as_u64().ok_or_else(|| {
                    Error::new("provider supplied invalid cached token usage".into())
                })
            })
            .transpose()?;
        let cached_count = cached.unwrap_or(0);
        let uncached = input
            .checked_sub(cached_count)
            .ok_or_else(|| Error::new("provider cache count exceeds input count".into()))?;
        let input_rate = if cached.is_some() {
            self.input
        } else {
            self.input.max(self.cached_input)
        };
        let numerator = (u128::from(uncached) * u128::from(input_rate))
            .checked_add(u128::from(cached_count) * u128::from(self.cached_input))
            .and_then(|n| n.checked_add(u128::from(output) * u128::from(self.output)))
            .ok_or_else(|| Error::new("model cost overflow".into()))?;
        let cost = u64::try_from(numerator.div_ceil(1_000_000))
            .map_err(|_| Error::new("model cost overflow".into()))?;
        Ok(Usage {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: cached,
            cost_microunits: Some(cost),
        })
    }
}

/// OpenAI-compatible provider using Rovik's shared HTTP client and stream decoder.
/// Construct on the trusted host and call from a blocking controller thread.
pub struct HarnessProvider {
    runtime: tokio::runtime::Handle,
    url: String,
    api_key: String,
    rates: Rates,
}
impl HarnessProvider {
    /// Credentials stay in this adapter; they never enter artifacts or sandbox mounts.
    pub fn new(
        runtime: tokio::runtime::Handle,
        url: String,
        api_key: String,
        rates: Rates,
    ) -> Result<Self> {
        let parsed = reqwest::Url::parse(&url)
            .map_err(|_| Error::new("invalid provider endpoint".into()))?;
        let local = parsed.host_str().is_some_and(|h| {
            h == "localhost"
                || h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(parsed.scheme() == "https" || (parsed.scheme() == "http" && local))
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(Error::new(
                "provider endpoint requires HTTPS or loopback HTTP, without URL credentials".into(),
            ));
        }
        Ok(Self {
            runtime,
            url,
            api_key,
            rates,
        })
    }
}
impl Provider for HarnessProvider {
    fn validate_reservation(
        &self,
        request: &ModelRequest,
        reservation: &Reservation,
    ) -> Result<()> {
        let required = self.rates.reservation(
            wire::canonical(request)?.len(),
            request.role.control().len(),
            request.maximum_output_tokens,
        )?;
        if reservation.tokens < required.tokens
            || reservation.cost_microunits < required.cost_microunits
        {
            return Err(Error::classified(rovik_harness::FailureCategory::Budget,
                format!("model reservation is below the complete request bound ({} tokens, {} cost microunits); review context, output and budget limits", required.tokens, required.cost_microunits)));
        }
        Ok(())
    }
    fn generate(
        &mut self,
        request: &ModelRequest,
        cancellation: &Cancellation,
    ) -> Result<ModelResponse> {
        let body = serde_json::json!({"model":request.provider_model_id,"messages":[{"role":"system","content":request.role.control()},{"role":"user","content":String::from_utf8(wire::canonical(request)?).map_err(|_|Error::new("invalid model context encoding".into()))?}],"stream":true,"stream_options":{"include_usage":true},"max_tokens":request.maximum_output_tokens,"response_format":{"type":"json_object"}});
        self.runtime.block_on(async {
            let (stop,mut cancel)=tokio::sync::watch::channel(cancellation.is_cancelled());
            let timeout=Duration::from_millis(request.timeout_ms);
            let spec=RequestSpec {client:Clients::Shared,url:&self.url,api_key:&self.api_key,body:&body,streaming:true,limits:Limits {first_byte:timeout,idle:timeout,overall:timeout},api:Api::ChatCompletions,chat_id:None,admission:None};
            let mut output=String::new();let mut usage=None;let mut finish_reason=None;let mut violation=None;
            let mut emit=|event|match event {
                ChatStreamEvent::Delta {content}=>{
                    if output.len().saturating_add(content.len())>wire::MAX_JSON_BYTES {violation=Some("model output exceeds the registered limit");let _=stop.send(true);}else{output.push_str(&content);}
                },
                ChatStreamEvent::Finished {finish_reason:reason,usage:counts}=>{if reason.is_some(){finish_reason=reason;}
                    if counts.is_some(){usage=counts;}},
                ChatStreamEvent::ToolCallStart {..}|ChatStreamEvent::ToolCallComplete {..}=>{violation=Some("provider attempted an unregistered native tool");let _=stop.send(true);},
                _=>{},
            };
            let monitor=async {loop {if cancellation.is_cancelled(){let _=stop.send(true);return;}tokio::time::sleep(Duration::from_millis(20)).await;}};
            let response=tokio::select! {result=chat_request::run_once(&spec,&mut cancel,&mut emit,wire::MAX_JSON_BYTES)=>result,_=monitor=>Err(chat_request::ChatError::stopped())};
            if let Some(reason)=violation{return Err(Error::new(reason.into()));}
            response.map_err(|e|Error::new(format!("model attempt {:?}: {}",e.kind,e.message)))?;
            Ok(ModelResponse {request_id:request.request_id.clone(),provider_response_id:None,output,finish_reason:finish_reason.unwrap_or_default(),usage:usage.map(|v|self.rates.usage(v)).transpose()?})
        })
    }
}

mod git_delivery;
pub mod ide;
pub mod ide_prepare;
pub mod ide_setup;
pub mod ide_source;
mod source;
pub use git_delivery::{DeliveryCandidate, DeliveryReceipt, MergeCandidate};

/// Integration observer for one explicitly selected Git repository and reference.
pub struct GitIntegration {
    repository_id: String,
    root: std::path::PathBuf,
    reference: String,
}
impl GitIntegration {
    /// Bind an operator-selected repository path to its registered identity.
    pub fn new(repository_id: String, root: &std::path::Path) -> Result<Self> {
        let root = root.canonicalize()?;
        git2::Repository::open(&root)
            .map_err(|e| Error::new(format!("open integration repository: {e}")))?;
        Ok(Self {
            repository_id,
            root,
            reference: "HEAD".into(),
        })
    }
    /// Observe a named local branch without changing the user's checkout.
    pub fn for_reference(
        repository_id: String,
        root: &std::path::Path,
        reference: &str,
    ) -> Result<Self> {
        if !reference.starts_with("refs/heads/") || !git2::Reference::is_valid_name(reference) {
            return Err(Error::new(
                "integration needs a valid refs/heads/ branch".into(),
            ));
        }
        let mut integration = Self::new(repository_id, root)?;
        integration.reference = reference.into();
        Ok(integration)
    }
    /// Import tracked HEAD bytes, modes and revision without changing the working tree.
    pub fn snapshot(
        &self,
        ledger: &rovik_harness::ledger::Ledger,
    ) -> Result<rovik_harness::snapshot::Snapshot> {
        let repository =
            git2::Repository::open(&self.root).map_err(|e| Error::new(e.to_string()))?;
        let commit = repository
            .find_reference(&self.reference)
            .and_then(|h| h.peel_to_commit())
            .map_err(|e| Error::new(e.to_string()))?;
        let tree = commit.tree().map_err(|e| Error::new(e.to_string()))?;
        Self::tree_snapshot(&repository, &tree, ledger, &commit.id().to_string())
    }
    fn tree_snapshot(
        repository: &git2::Repository,
        tree: &git2::Tree<'_>,
        ledger: &rovik_harness::ledger::Ledger,
        revision: &str,
    ) -> Result<rovik_harness::snapshot::Snapshot> {
        use rovik_harness::snapshot::{FileEntry, Snapshot};
        let mut files = std::collections::BTreeMap::new();
        let mut failure = None;
        let walked = tree.walk(git2::TreeWalkMode::PreOrder,|prefix,entry|{
            if entry.kind()==Some(git2::ObjectType::Tree){return git2::TreeWalkResult::Ok;}
            let result=(||->Result<()> {
                let name=entry.name().map_err(|_|Error::new("non-UTF-8 tracked path is unsupported".into()))?;
                let path=format!("{prefix}{name}");rovik_harness::scope::validate_path(&path)?;
                let mode=entry.filemode() as u32;
                if ![0o100644,0o100755].contains(&mode){return Err(Error::new(format!("unsupported tracked file mode for {path}; declare a supported dependency bundle")));}
                let content=source::blob(repository,ledger,entry.id(),&path)?;
                files.insert(path,FileEntry {mode,content});Ok(())
            })();
            match result {Ok(())=>git2::TreeWalkResult::Ok,Err(e)=>{failure=Some(e);git2::TreeWalkResult::Abort}}
        });
        if let Some(error) = failure {
            return Err(error);
        }
        walked.map_err(|error| Error::new(error.to_string()))?;
        let snapshot = Snapshot {
            schema_version: 1,
            base_revision: revision.into(),
            files,
            deletions: vec![],
            dependency_assets: vec![],
        };
        snapshot.validate(ledger, true)?;
        Ok(snapshot)
    }
    /// Capture tracked working-tree bytes plus explicitly selected untracked files.
    /// The index, HEAD and user's checkout are never changed; the returned digest must
    /// receive the same baseline/scope approval as any other immutable input.
    pub fn snapshot_worktree(
        &self,
        ledger: &rovik_harness::ledger::Ledger,
        untracked: &std::collections::BTreeSet<String>,
    ) -> Result<rovik_harness::snapshot::Snapshot> {
        use rovik_harness::snapshot::FileEntry;
        let mut snapshot = self.snapshot(ledger)?;
        let paths: std::collections::BTreeSet<_> = snapshot
            .files
            .keys()
            .cloned()
            .chain(untracked.iter().cloned())
            .collect();
        for path in paths {
            rovik_harness::scope::validate_path(&path)?;
            if path
                .split('/')
                .any(|part| part.eq_ignore_ascii_case(".git"))
            {
                return Err(Error::new(
                    "Git administrative files cannot enter a source snapshot".into(),
                ));
            }
            let file = self.root.join(&path);
            let meta = match std::fs::symlink_metadata(&file) {
                Ok(meta) => meta,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && snapshot.files.contains_key(&path) =>
                {
                    snapshot.files.remove(&path);
                    snapshot.deletions.push(path);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || !file.canonicalize()?.starts_with(&self.root)
            {
                return Err(Error::new(format!(
                    "dirty snapshot requires a regular in-repository file: {path}"
                )));
            }
            let mut parent = file.parent();
            while let Some(directory) = parent.filter(|p| *p != self.root) {
                if std::fs::symlink_metadata(directory)?
                    .file_type()
                    .is_symlink()
                {
                    return Err(Error::new(
                        "dirty snapshot cannot traverse a symlink".into(),
                    ));
                }
                parent = directory.parent();
            }
            source::require_hydrated(&file, &path, meta.len())?;
            let content = source::observe(&file, &path)?;
            source::publish(ledger, &file, &path, &content)?;
            let after = std::fs::symlink_metadata(&file)?;
            if after.len() != meta.len()
                || content.byte_count != meta.len()
                || after.modified()? != meta.modified()?
                || after.permissions() != meta.permissions()
                || !after.is_file()
                || after.file_type().is_symlink()
            {
                return Err(Error::new(
                    "working tree changed during capture; retry snapshot import".into(),
                ));
            }
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                }
            };
            #[cfg(not(unix))]
            let mode = 0o100644;
            snapshot.files.insert(path, FileEntry { mode, content });
        }
        use rovik_harness::controller::Integration;
        if self.current_revision(&self.repository_id)? != snapshot.base_revision {
            return Err(Error::new(
                "HEAD changed during working-tree capture; retry import".into(),
            ));
        }
        snapshot.deletions.sort();
        snapshot.validate(ledger, true)?;
        Ok(snapshot)
    }
}
impl rovik_harness::controller::Integration for GitIntegration {
    fn current_revision(&self, repository_id: &str) -> Result<String> {
        if repository_id != self.repository_id {
            return Err(Error::new("unregistered integration repository".into()));
        }
        let repository =
            git2::Repository::open(&self.root).map_err(|e| Error::new(e.to_string()))?;
        let commit = repository
            .find_reference(&self.reference)
            .and_then(|h| h.peel_to_commit())
            .map_err(|e| Error::new(e.to_string()))?;
        Ok(commit.id().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{self, Reply};
    use rovik_harness::{context::ContextBundle, identity::Digest};
    fn request() -> ModelRequest {
        ModelRequest {
            role: rovik_harness::provider::Role::Implementer,
            request_id: "request".into(),
            provider_model_id: "fixture".into(),
            context: ContextBundle {
                schema_version: 1,
                items: vec![],
                omitted: vec![],
            },
            snapshot_digest: Digest::of(b"source"),
            maximum_output_tokens: 100,
            timeout_ms: 2000,
            feedback: vec![],
        }
    }
    #[test]
    fn accounting_rejects_unknown_counts_invalid_cache_counts_and_overflow() {
        let rates = Rates {
            input: u64::MAX,
            cached_input: u64::MAX,
            output: u64::MAX,
        };
        assert!(rates.usage(serde_json::json!({"prompt_tokens":1})).is_err());
        assert!(rates.usage(serde_json::json!({"prompt_tokens":1,"completion_tokens":0,"prompt_tokens_details":{"cached_tokens":2}})).is_err());
        assert!(rates
            .usage(serde_json::json!({"prompt_tokens":u64::MAX,"completion_tokens":u64::MAX}))
            .is_err());
    }
    #[tokio::test]
    async fn underfunded_requests_never_claim_budget_or_call_the_model() {
        let (base, seen) =
            testing::model(vec![Reply::Usage(20, 10, Box::new(Reply::Text("{}")))]).await;
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            for (price, tokens) in [(1_000_000, 1_000_000), (0, 100)] {
                let directory = tempfile::tempdir().unwrap();
                let mut ledger =
                    rovik_harness::ledger::Ledger::open(&directory.path().join("ledger")).unwrap();
                let input = ledger
                    .store("context", "text/plain", b"reviewed task", 1)
                    .unwrap();
                ledger
                    .create_run(
                        "underfunded",
                        &input.digest,
                        &[
                            ("model_calls".into(), 1),
                            ("tokens".into(), 1_000_000),
                            ("cost_microunits".into(), 0),
                        ]
                        .into(),
                        1,
                    )
                    .unwrap();
                let before = ledger.budgets("underfunded").unwrap();
                let mut provider = HarnessProvider::new(
                    runtime.clone(),
                    format!("{base}/chat/completions"),
                    "synthetic".into(),
                    Rates {
                        input: price,
                        cached_input: price,
                        output: price,
                    },
                )
                .unwrap();
                let error = rovik_harness::provider::accounted_call(
                    &mut ledger,
                    "underfunded",
                    &request(),
                    Reservation {
                        tokens,
                        cost_microunits: 0,
                    },
                    &mut provider,
                    &Cancellation::default(),
                )
                .unwrap_err();
                assert!(error.message.contains("model reservation"));
                assert_eq!(
                    ledger.budgets("underfunded").unwrap(),
                    before,
                    "no claim may be created for an underfunded request"
                );
            }
        })
        .await
        .unwrap();
        assert!(seen.lock().unwrap().is_empty(), "positive prices with zero cost budget and incomplete input reservations must fail before HTTP");
    }
    #[test]
    fn importing_a_revision_ignores_dirty_worktree_bytes_and_never_changes_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repository");
        let repository = git2::Repository::init(&root).unwrap();
        std::fs::write(root.join("source.rs"), "committed source").unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(std::path::Path::new("source.rs")).unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
        let revision = repository
            .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        std::fs::write(root.join("source.rs"), "uncommitted user work").unwrap();
        let ledger = rovik_harness::ledger::Ledger::open(&dir.path().join("state")).unwrap();
        let integration = GitIntegration::new("fixture".into(), &root).unwrap();
        let snapshot = integration.snapshot(&ledger).unwrap();
        assert_eq!(snapshot.base_revision, revision.to_string());
        assert_eq!(
            ledger
                .read(&snapshot.files["source.rs"].content.digest)
                .unwrap(),
            b"committed source"
        );
        assert_eq!(
            std::fs::read(root.join("source.rs")).unwrap(),
            b"uncommitted user work"
        );
        std::fs::write(root.join("selected.rs"), "explicit new input").unwrap();
        std::fs::write(root.join("unselected.rs"), "unrelated user file").unwrap();
        let dirty = integration
            .snapshot_worktree(&ledger, &["selected.rs".into()].into())
            .unwrap();
        assert_eq!(
            ledger
                .read(&dirty.files["source.rs"].content.digest)
                .unwrap(),
            b"uncommitted user work"
        );
        assert!(dirty.files.contains_key("selected.rs"));
        assert!(!dirty.files.contains_key("unselected.rs"));
        assert_eq!(repository.index().unwrap().write_tree().unwrap(), tree_id);
        assert_eq!(repository.head().unwrap().target(), Some(revision));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("source.rs"), root.join("link.rs")).unwrap();
            assert!(integration
                .snapshot_worktree(&ledger, &["link.rs".into()].into())
                .is_err());
        }
    }
    #[tokio::test]
    async fn one_attempt_bounds_wire_bytes_before_decoding_a_stream_event() {
        let (base, seen) = testing::model(vec![Reply::Text("small content")]).await;
        let url = format!("{base}/chat/completions");
        let body = serde_json::json!({"model":"fixture","stream":true,"messages":[]});
        let spec = RequestSpec {
            client: Clients::Shared,
            url: &url,
            api_key: "synthetic",
            body: &body,
            streaming: true,
            limits: Limits::default(),
            api: Api::ChatCompletions,
            chat_id: None,
            admission: None,
        };
        let (_sender, mut cancel) = tokio::sync::watch::channel(false);
        let mut events = Vec::new();
        let error = chat_request::run_once(&spec, &mut cancel, &mut |event| events.push(event), 8)
            .await
            .unwrap_err();
        assert!(error.message.contains("byte limit"));
        assert_eq!(error.attempts, 1);
        assert!(events.is_empty());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn retryable_failure_is_one_billable_attempt() {
        let (base, seen) = testing::model(vec![
            Reply::Status(500, "failure"),
            Reply::Text("unexpected retry"),
        ])
        .await;
        let mut provider = HarnessProvider::new(
            tokio::runtime::Handle::current(),
            format!("{base}/chat/completions"),
            "synthetic".into(),
            Rates {
                input: 0,
                cached_input: 0,
                output: 0,
            },
        )
        .unwrap();
        assert!(tokio::task::spawn_blocking(
            move || provider.generate(&request(), &Cancellation::default())
        )
        .await
        .unwrap()
        .is_err());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn provider_preserves_usage_and_does_not_offer_host_tools() {
        let (base, seen) =
            testing::model(vec![Reply::Usage(123, 17, Box::new(Reply::Text("{}")))]).await;
        let mut provider = HarnessProvider::new(
            tokio::runtime::Handle::current(),
            format!("{base}/chat/completions"),
            "synthetic".into(),
            Rates {
                input: 1_000_000,
                cached_input: 1_000_000,
                output: 2_000_000,
            },
        )
        .unwrap();
        let result = tokio::task::spawn_blocking(move || {
            provider.generate(&request(), &Cancellation::default())
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.usage.unwrap().cost_microunits, Some(157));
        assert_eq!(result.output, "{}");
        assert_eq!(result.finish_reason, "stop");
        assert!(seen.lock().unwrap()[0].get("tools").is_none());
    }
}
