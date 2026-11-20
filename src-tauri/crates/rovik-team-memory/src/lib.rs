//! Bounded HTTPS exchanges between a dedicated local replica and the team hub.

/// Streaming onboarding and resume.
pub mod bootstrap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use verified_code_memory::{
    sync::{Page, MAX_PAGE_BYTES},
    MemoryStore,
};

/// A retryable administrative clear tied to the observed epoch.
#[derive(Serialize, Deserialize)]
pub struct Clear {
    /// Unique request identity.
    pub operation: Uuid,
    /// Epoch displayed to the operator before confirmation.
    pub epoch: verified_code_memory::sync::Stamp,
}

/// Project-scoped protocol request.
#[derive(Serialize, Deserialize)]
pub struct Exchange {
    /// Expected server identity, learned through discovery.
    pub hub: Option<String>,
    /// Last server sequence committed by this client.
    pub after: i64,
    /// One bounded page. Readers can always omit it.
    pub push: Option<Page>,
    /// Administrative clear, mutually exclusive with publishing.
    #[serde(default)]
    pub clear: Option<Clear>,
}

/// Per-change dispositions travel with the pull page; the client saves them before acknowledging.
#[derive(Serialize, Deserialize)]
pub struct Reply {
    /// The next pull page.
    pub page: Page,
    /// IDs the publisher must review, with content-free reasons.
    pub rejected: Vec<(Uuid, String)>,
    /// Record ID, authenticated claim publisher and revision submitter.
    pub attribution: Vec<(Uuid, String, String)>,
}

/// One authenticated connection to a linked project. All requests use the same
/// HTTPS origin, device proof, connection pool and deadline.
pub struct HubClient<'a> {
    exchange: reqwest::Url,
    snapshot: reqwest::Url,
    http: reqwest::Client,
    session: &'a str,
    device: &'a str,
    retry_after: std::sync::atomic::AtomicU64,
}

impl<'a> HubClient<'a> {
    /// Opens a connection for the lifetime of one sync operation.
    pub fn new(
        hub: &str,
        project: Uuid,
        session: &'a str,
        device: &'a str,
    ) -> Result<Self, String> {
        Ok(Self {
            exchange: endpoint_url(hub, project, "exchange")?,
            snapshot: endpoint_url(hub, project, "snapshot")?,
            http: reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(35))
                .build()
                .map_err(|e| e.to_string())?,
            session,
            device,
            retry_after: Default::default(),
        })
    }

    /// Minimum delay requested by the service after a failed request, in seconds.
    pub fn retry_after(&self) -> u64 {
        self.retry_after.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn check_response(&self, response: reqwest::Response) -> Result<reqwest::Response, String> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        if let Some(seconds) = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            self.retry_after
                .store(seconds.min(86_400), std::sync::atomic::Ordering::Relaxed);
        }
        let message=match status.as_u16() {
            401 => "Sign in again in Settings → Repository accounts.",
            403 => "Check repository membership, write permission and the project’s personal-host policy.",
            409 => "Review the link and reconcile after a hub restore or device copy.",
            426 => "Update Rovik to the version required by this hub.",
            429 | 503 => "The service is busy. Automatic sync will retry.",
            _ => "Check the team hub configuration.",
        };
        Err(format!("Team hub returned HTTP {status}. {message}"))
    }

    fn request(&self, url: &reqwest::Url, replica: String) -> reqwest::RequestBuilder {
        self.http
            .post(url.clone())
            .bearer_auth(self.session)
            .header("x-rovik-device", self.device)
            .header("x-rovik-replica", replica)
    }
}

/// Exchanges one page in each direction. Only pass the isolated team replica.
/// Returns true when another exchange is needed.
pub async fn sync_once(
    store: &mut MemoryStore,
    scope: Uuid,
    client: &HubClient<'_>,
    publish: bool,
    check: impl Fn(&verified_code_memory::sync::Change) -> Result<bool, String>,
) -> Result<bool, String> {
    let peer = client.exchange.to_string();
    let local = scope.to_string();
    let progress = store
        .sync_progress(&local, &peer)
        .map_err(|e| e.to_string())?;
    let discovery = progress.replica.is_empty();
    let mut push = if publish && !discovery {
        Some(
            store
                .sync_export(&local, progress.sent)
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let sent = push.as_ref().map_or(progress.sent, |p| p.through);
    let more = push.as_ref().is_some_and(|p| p.more);
    let replica = store.sync_replica().map_err(|e| e.to_string())?;
    if let Some(outgoing) = &mut push {
        let mut allowed = Vec::new();
        for change in std::mem::take(&mut outgoing.changes) {
            if change.version.replica == replica && check(&change)? {
                allowed.push(change);
            }
        }
        outgoing.changes = allowed;
    }
    let push = push.filter(|p| !p.changes.is_empty());
    let request = Exchange {
        hub: (!discovery).then_some(progress.replica.clone()),
        after: progress.received,
        push,
        clear: None,
    };
    let response = client
        .request(&client.exchange, replica)
        .json(&request)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let mut response = client.check_response(response)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > MAX_PAGE_BYTES + 64 * 1024 {
            return Err("Oversized team response".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let reply: Reply = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let page = reply.page;
    if !discovery && page.replica != progress.replica || page.through < progress.received {
        return Err("Hub progress changed; reconcile this team link".into());
    }
    for (id, reason) in reply.rejected {
        store
            .team_block(&local, scope, id, &reason)
            .map_err(|e| e.to_string())?;
    }
    store
        .team_receive_attributed(&local, &peer, &progress, sent, &page, &reply.attribution)
        .map_err(|e| e.to_string())?;
    Ok(more || page.more || discovery && publish)
}

/// Constructs a project endpoint on the explicitly linked HTTPS hub.
pub fn endpoint_url(hub: &str, project: Uuid, endpoint: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(hub).map_err(|e| e.to_string())?;
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Enter an HTTPS team hub URL without credentials, query or fragment.".into());
    }
    url.path_segments_mut()
        .map_err(|_| "Invalid hub URL")?
        .pop_if_empty()
        .extend(["v1", "projects", &project.to_string()])
        .extend(endpoint.split('/'));
    Ok(url)
}
