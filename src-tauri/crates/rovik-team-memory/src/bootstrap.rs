//! Resumable onboarding stream. The receiver verifies the digest before
//! marking completion; partial transfers resume from their last committed page.
use crate::{HubClient, Reply};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use verified_code_memory::{
    sync::{Progress, Stamp, MAX_PAGE_BYTES},
    MemoryStore,
};

/// First or resumed snapshot request.
#[derive(Deserialize, Serialize)]
pub struct Request {
    /// Expected replica identity from the last committed segment.
    pub replica: Option<String>,
    /// Durable sequence from which to resume.
    pub after: i64,
}
/// One newline-delimited record in a snapshot response.
#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Frame {
    /// Identifies the stream generation and starting cursor.
    Header {
        replica: String,
        epoch: Stamp,
        after: i64,
        format: u8,
    },
    /// Carries one ordinary replication page with attribution.
    Page {
        reply: Reply,
    },
    /// Commits the final cursor and digest of all preceding frames.
    Trailer {
        through: i64,
        digest: String,
    },
}

/// Encodes a snapshot digest in the canonical lower-case hexadecimal format.
pub fn checksum(hasher: Sha256) -> String {
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Initializes or resumes a dedicated link replica.
pub async fn receive(
    store: &MemoryStore,
    scope: Uuid,
    client: &HubClient<'_>,
) -> Result<(), String> {
    let peer = client.exchange.to_string();
    let local = scope.to_string();
    if store
        .team_bootstrapped(&local, &peer)
        .map_err(|e| e.to_string())?
    {
        return Ok(());
    }
    let mut progress = store
        .sync_progress(&local, &peer)
        .map_err(|e| e.to_string())?;
    let response = client
        .request(
            &client.snapshot,
            store.sync_replica().map_err(|e| e.to_string())?,
        )
        .json(&Request {
            replica: (!progress.replica.is_empty()).then(|| progress.replica.clone()),
            after: progress.received,
        })
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let mut response = client.check_response(response)?;
    let mut line = Vec::new();
    let mut hash = Sha256::new();
    let mut header = None;
    let mut final_page = false;
    let mut ended = false;
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        for part in chunk.split_inclusive(|b| *b == b'\n') {
            if ended || line.len() + part.len() > MAX_PAGE_BYTES + 64 * 1024 {
                return Err("Invalid team snapshot framing".into());
            }
            line.extend_from_slice(part);
            if !line.ends_with(b"\n") {
                continue;
            }
            let frame: Frame = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
            if !matches!(frame, Frame::Trailer { .. }) {
                hash.update(&line);
            }
            match frame {
                Frame::Header {
                    replica,
                    epoch,
                    after,
                    format,
                } if header.is_none()
                    && format == 1
                    && after == progress.received
                    && (progress.replica.is_empty() || progress.replica == replica) =>
                {
                    header = Some((replica, epoch));
                }
                Frame::Page { reply }
                    if !final_page
                        && header.as_ref()
                            == Some(&(reply.page.replica.clone(), reply.page.epoch.clone())) =>
                {
                    let page = reply.page;
                    if page.more && page.through <= progress.received {
                        return Err("Snapshot cursor did not advance".into());
                    }
                    store
                        .team_receive_attributed(
                            &local,
                            &peer,
                            &progress,
                            progress.sent,
                            &page,
                            &reply.attribution,
                        )
                        .map_err(|e| e.to_string())?;
                    progress = Progress {
                        replica: page.replica,
                        received: page.through,
                        sent: progress.sent,
                    };
                    final_page = !page.more;
                }
                Frame::Trailer { through, digest }
                    if final_page
                        && through == progress.received
                        && digest == checksum(hash.clone()) =>
                {
                    ended = true;
                }
                _ => return Err("Snapshot metadata changed; retry from saved progress".into()),
            }
            line.clear();
        }
    }
    if !ended || !line.is_empty() {
        return Err("Incomplete team snapshot; progress was saved".into());
    }
    store
        .team_finish_bootstrap(&local, &peer, &progress.replica, progress.received)
        .map_err(|e| e.to_string())
}
