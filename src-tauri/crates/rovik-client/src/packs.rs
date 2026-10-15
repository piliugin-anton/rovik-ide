//! Packs between a project's shadow repository and its host (6.4). A push
//! sends what a new commit reaches and the host's refs don't, and the host
//! moves its ref only if nobody moved it since the client looked. A fetch
//! takes what the host has of the commits wanted and the shadow lacks.
//! Both are built and taken in a piece at a time, so the first push of a
//! long history never sits in memory.

use crate::client::{unreachable, Identity};
use crate::connector::Connector;
use crate::http;
use git2::{Oid, Repository};
use http_body_util::BodyExt;
use hyper::body::Bytes;
use rovik_core::packs;
use rovik_protocol::packs::{Fetch, Pushed, Update};
use rovik_protocol::{Fail, FailCode};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;

/// A pack's pieces on their way between the repository and the socket.
const PIECES: usize = 16;

/// Sends `new` from the shadow repository at `shadow` to the project's ref
/// `name` on the host, telling `progress` the bytes sent so far.
pub async fn push(
    connector: &dyn Connector,
    identity: &Identity,
    shadow: &Path,
    project: &str,
    name: &str,
    new: Oid,
    progress: Box<dyn Fn(u64) + Send + Sync>,
) -> Result<Pushed, Fail> {
    let refs = http::pack_refs(connector, identity, project).await?.refs;
    let old = refs.get(name).cloned();
    if old == Some(new.to_string()) {
        return Ok(Pushed::default());
    }
    let haves: Vec<Oid> = refs
        .values()
        .filter_map(|id| Oid::from_str(id).ok())
        .collect();
    let (pieces, built) = mpsc::channel(PIECES);
    // Set when the request stops taking the pack: the host's answer, or a
    // lost link, says why.
    let stopped = Arc::new(AtomicBool::new(false));
    let building = {
        let (repo, stopped) = (shadow.to_path_buf(), stopped.clone());
        spawn_blocking(move || {
            let mut piece = |piece: &[u8]| {
                let taken = pieces.blocking_send(Bytes::copy_from_slice(piece)).is_ok();
                stopped.store(!taken, Ordering::Relaxed);
                taken
            };
            packs::build(&open(&repo)?, &[new], &haves, &mut piece)
        })
    };
    let update = Update {
        name: name.to_string(),
        old,
        new: new.to_string(),
    };
    let pushed = http::push_pack(connector, identity, project, &update, built, progress).await;
    match building.await.unwrap_or_else(|e| Err(e.to_string())) {
        // The pack couldn't be built: the host refused what came of it.
        Err(why) if !stopped.load(Ordering::Relaxed) => Err(internal(why)),
        _ => pushed,
    }
}

/// Takes what the host has of `wants` into the shadow repository at
/// `shadow`, apart from what its refs reach. Returns how many objects came.
pub async fn fetch(
    connector: &dyn Connector,
    identity: &Identity,
    shadow: &Path,
    project: &str,
    wants: &[Oid],
) -> Result<usize, Fail> {
    let repo = shadow.to_path_buf();
    let haves = spawn_blocking(move || tips(&open(&repo)?)).await;
    let haves = haves
        .unwrap_or_else(|e| Err(e.to_string()))
        .map_err(internal)?;
    let fetch = Fetch {
        wants: wants.iter().map(Oid::to_string).collect(),
        haves,
    };
    let mut body = http::fetch_pack(connector, identity, project, &fetch).await?;
    let (pieces, mut taken) = mpsc::channel::<Vec<u8>>(PIECES);
    let repo = shadow.to_path_buf();
    let receiving = spawn_blocking(move || {
        let pieces = std::iter::from_fn(|| taken.blocking_recv());
        packs::receive(&open(&repo)?, pieces)
    });
    let mut cut = false;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            cut = true;
            break;
        };
        let Ok(piece) = frame.into_data() else {
            continue;
        };
        if pieces.send(piece.to_vec()).await.is_err() {
            break;
        }
    }
    drop(pieces);
    match receiving.await.unwrap_or_else(|e| Err(e.to_string())) {
        Err(_) if cut => Err(unreachable(&identity.host)),
        received => received.map_err(internal),
    }
}

/// The commits the repository's refs point at.
fn tips(repo: &Repository) -> Result<Vec<String>, String> {
    let refs = repo.references().map_err(|e| e.message().to_string())?;
    Ok(refs
        .flatten()
        .filter_map(|r| r.target())
        .map(|id| id.to_string())
        .collect())
}

fn open(shadow: &Path) -> Result<Repository, String> {
    Repository::open(shadow).map_err(|e| e.message().to_string())
}

fn internal(why: String) -> Fail {
    Fail::new(FailCode::Internal, why)
}
