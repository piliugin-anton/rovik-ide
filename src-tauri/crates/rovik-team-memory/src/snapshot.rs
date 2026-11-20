//! Bounded reader stream, with a pinned epoch and a single buffered frame.
use crate::{auth, Hub};
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use rovik_team_memory::{
    bootstrap::{checksum, Frame, Request},
    Reply,
};
use sha2::{Digest, Sha256};
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;
use verified_code_memory::MemoryStore;

pub(super) async fn stream(
    State(hub): State<Arc<Hub>>,
    Path(project): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let permit = hub
        .snapshots
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    auth::authorize(&hub, project, &headers).await?;
    let request: Request = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if request.after < 0 || (request.replica.is_none() && request.after != 0) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (send, receive) = tokio::sync::mpsc::channel::<Result<Vec<u8>, io::Error>>(1);
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = (|| -> Result<(), io::Error> {
            let db = MemoryStore::open(hub.directory.join(format!("{project}.db")))
                .map_err(io::Error::other)?;
            let repo = project.to_string();
            let mut page = db
                .sync_export(&repo, request.after)
                .map_err(io::Error::other)?;
            let replica = page.replica.clone();
            if request.replica.as_ref().is_some_and(|r| r != &replica) {
                return Err(io::Error::other("hub identity changed"));
            }
            let epoch = page.epoch.clone();
            let mut hash = Sha256::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            let emit = |frame: Frame, hash: Option<&mut Sha256>| -> Result<(), io::Error> {
                let mut line = serde_json::to_vec(&frame)?;
                line.push(b'\n');
                if let Some(h) = hash {
                    h.update(&line);
                }
                runtime
                    .block_on(async {
                        tokio::time::timeout_at(deadline, send.send(Ok(line))).await
                    })
                    .map_err(|_| io::Error::other("snapshot timed out"))?
                    .map_err(|_| io::Error::other("client disconnected"))
            };
            emit(
                Frame::Header {
                    replica: replica.clone(),
                    epoch: epoch.clone(),
                    after: request.after,
                    format: 1,
                },
                Some(&mut hash),
            )?;
            let start = Instant::now();
            loop {
                if page.epoch != epoch
                    || page.replica != replica
                    || start.elapsed() > Duration::from_secs(30)
                {
                    return Err(io::Error::other("snapshot interrupted; resume"));
                }
                let through = page.through;
                let more = page.more;
                let mut attribution = Vec::new();
                for c in &page.changes {
                    if let Some((author, editor)) =
                        db.team_provenance(&repo, c).map_err(io::Error::other)?
                    {
                        attribution.push((c.id, author, editor));
                    }
                }
                emit(
                    Frame::Page {
                        reply: Reply {
                            page,
                            rejected: Vec::new(),
                            attribution,
                        },
                    },
                    Some(&mut hash),
                )?;
                if !more {
                    emit(
                        Frame::Trailer {
                            through,
                            digest: checksum(hash),
                        },
                        None,
                    )?;
                    break;
                }
                page = db.sync_export(&repo, through).map_err(io::Error::other)?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            let _ = send.try_send(Err(e));
        }
    });
    let stream =
        futures_util::stream::unfold(receive, |mut r| async move { Some((r.recv().await?, r)) });
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}
