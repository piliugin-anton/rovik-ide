//! Packs between a project's shadow repository and the real daemon (T02,
//! 6.4): the first push carries the history, the next only what changed, a
//! pack cut short or pushed against a moved ref leaves the ref where it
//! was, and a fetch takes only what the shadow lacks.

mod common;

use common::{commit_all, write, Daemon, COMPUTER};
use git2::{ObjectType, Oid, Repository, Signature};
use rovik_client::shadow::Shadow;
use rovik_client::{Client, CloudEvent, HostState, Identity, Remote, UnixConnector};
use rovik_core::packs;
use rovik_core::testing::{temp_dir, TestCore};
use rovik_protocol::packs::{snapshot_ref, Pushed, Refs, Update};
use rovik_protocol::{Fail, FailCode, Secret};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PROJECT: &str = "5b1d3c9e-7f2a-4e61-9c3b-0d8e4f6a2b17";
const CHAT: &str = "chat-1";

/// A project with three commits, its shadow, and a client of the daemon.
struct Setup {
    daemon: Daemon,
    project: PathBuf,
    shadow: PathBuf,
    client: Client,
}

impl Setup {
    async fn new() -> Setup {
        let daemon = Daemon::start(&[]).await;
        let project = daemon.dir.0.join("project");
        for n in 1..=3 {
            write(
                &project.join(format!("src/lib/file{n}.rs")),
                &format!("// {n}\n"),
            );
            commit_all(&project);
        }
        let shadow = daemon.dir.0.join("shadow.git");
        let client = Client::connect(UnixConnector(daemon.socket.clone()), identity(&daemon), {
            Arc::new(|_| {})
        });
        ready(|| client.state() == HostState::Ready).await;
        Setup {
            daemon,
            project,
            shadow,
            client,
        }
    }

    fn snapshot(&self) -> Oid {
        let shadow = Shadow::open(&self.shadow, &self.project).unwrap();
        shadow.snapshot(CHAT).unwrap().commit
    }

    async fn push(&self, snapshot: Oid) -> Result<Pushed, Fail> {
        let name = snapshot_ref(CHAT);
        (self.client)
            .push(PROJECT, &self.shadow, &name, snapshot, |_| {})
            .await
    }

    /// The daemon's repository of the project.
    fn there(&self) -> Repository {
        let path = self
            .daemon
            .data
            .join("projects")
            .join(format!("{PROJECT}.git"));
        Repository::open_bare(path).unwrap()
    }

    async fn refs(&self) -> Refs {
        let path = format!("/packs/{PROJECT}/refs");
        let (status, body) = self
            .daemon
            .http("GET", &path, Some(&self.daemon.token), vec![])
            .await;
        assert!(status.is_success(), "{}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }

    /// The daemon's pack files, by name.
    fn pack_files(&self) -> Vec<PathBuf> {
        let dir = self.there().path().join("objects/pack");
        let mut packs: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        packs.retain(|path| path.extension().is_some_and(|e| e == "pack"));
        packs.sort();
        packs
    }
}

fn identity(daemon: &Daemon) -> Identity {
    Identity {
        host: "server".into(),
        computer: COMPUTER.into(),
        token: Secret(daemon.token.clone()),
        client: "pack tests".into(),
    }
}

async fn ready(check: impl Fn() -> bool) {
    for _ in 0..400 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never ready");
}

/// How many objects of each kind a pack file holds.
fn kinds(pack: &Path) -> BTreeMap<String, usize> {
    let dir = temp_dir();
    let repo = Repository::init_bare(dir.0.join("count.git")).unwrap();
    let packs = repo.path().join("objects/pack");
    for file in [pack.to_path_buf(), pack.with_extension("idx")] {
        std::fs::copy(&file, packs.join(file.file_name().unwrap())).unwrap();
    }
    let odb = repo.odb().unwrap();
    let mut kinds = BTreeMap::new();
    odb.foreach(|id| {
        let (_, kind) = odb.read_header(*id).unwrap();
        *kinds.entry(kind.to_string()).or_default() += 1;
        true
    })
    .unwrap();
    kinds
}

fn counted(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
    pairs
        .iter()
        .map(|(kind, n)| (kind.to_string(), *n))
        .collect()
}

#[tokio::test]
async fn the_first_push_carries_the_history_and_the_next_only_the_change() {
    let setup = Setup::new().await;
    let first = setup.snapshot();
    let pushed = setup.push(first).await.unwrap();

    let there = setup.there();
    let mut walk = there.revwalk().unwrap();
    walk.push(first).unwrap();
    assert_eq!(
        walk.count(),
        4,
        "the snapshot and the project's three commits"
    );
    // The snapshot's tree is HEAD's: nothing in the project changed yet.
    let objects = 4 + 3 * 3 + 3;
    assert_eq!(
        pushed.objects, objects,
        "four commits, three trees each for three, three files"
    );
    assert_eq!(
        setup.refs().await.refs[&snapshot_ref(CHAT)],
        first.to_string()
    );

    write(&setup.project.join("src/lib/file2.rs"), "// two, edited\n");
    let second = setup.snapshot();
    let before = setup.pack_files();
    setup.push(second).await.unwrap();
    let new: Vec<_> = setup
        .pack_files()
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    assert_eq!(new.len(), 1);
    let expected = counted(&[("blob", 1), ("commit", 1), ("tree", 3)]);
    assert_eq!(kinds(&new[0]), expected, "the root, src and src/lib trees");
    assert_eq!(
        setup.refs().await.refs[&snapshot_ref(CHAT)],
        second.to_string()
    );
    assert!(packs::complete(&setup.there(), second));
}

#[tokio::test]
async fn a_cut_pack_leaves_the_ref_where_it_was() {
    let setup = Setup::new().await;
    let first = setup.snapshot();
    setup.push(first).await.unwrap();
    write(&setup.project.join("new.txt"), "new\n");
    let second = setup.snapshot();
    let mut pack = Vec::new();
    let shadow = Repository::open(&setup.shadow).unwrap();
    packs::build(&shadow, &[second], &[first], &mut |piece| {
        pack.extend_from_slice(piece);
        true
    })
    .unwrap();
    pack.truncate(pack.len() / 2);

    let update = Update {
        name: snapshot_ref(CHAT),
        old: Some(first.to_string()),
        new: second.to_string(),
    };
    let path = format!("/packs/{PROJECT}?{}", update.query());
    let (status, body) = setup
        .daemon
        .http("POST", &path, Some(&setup.daemon.token), pack)
        .await;

    assert_eq!(status.as_u16(), 400, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        setup.refs().await.refs[&snapshot_ref(CHAT)],
        first.to_string()
    );
    assert!(!setup.there().find_commit(second).is_ok());
}

#[tokio::test]
async fn a_push_against_a_moved_ref_is_refused() {
    let setup = Setup::new().await;
    let first = setup.snapshot();
    setup.push(first).await.unwrap();
    write(&setup.project.join("new.txt"), "new\n");
    let second = setup.snapshot();
    // Someone else's push: the ref is new to them, but not to the daemon.
    let update = Update {
        name: snapshot_ref(CHAT),
        old: None,
        new: second.to_string(),
    };
    let mut pack = Vec::new();
    let shadow = Repository::open(&setup.shadow).unwrap();
    packs::build(&shadow, &[second], &[first], &mut |piece| {
        pack.extend_from_slice(piece);
        true
    })
    .unwrap();
    let path = format!("/packs/{PROJECT}?{}", update.query());
    let (status, body) = setup
        .daemon
        .http("POST", &path, Some(&setup.daemon.token), pack)
        .await;

    assert_eq!(status.as_u16(), 409, "{}", String::from_utf8_lossy(&body));
    let fail: Fail = serde_json::from_slice(&body).unwrap();
    assert_eq!(fail.code, FailCode::Busy);
    assert_eq!(
        setup.refs().await.refs[&snapshot_ref(CHAT)],
        first.to_string()
    );
    // The client itself reads the refs first, and gets there.
    setup.push(second).await.unwrap();
    assert_eq!(
        setup.refs().await.refs[&snapshot_ref(CHAT)],
        second.to_string()
    );
}

#[tokio::test]
async fn a_fetch_takes_only_what_the_shadow_lacks() {
    let setup = Setup::new().await;
    let snapshot = setup.snapshot();
    setup.push(snapshot).await.unwrap();
    // A result the daemon made on top of the snapshot: one file changed.
    let there = setup.there();
    let base = there.find_commit(snapshot).unwrap();
    let blob = there.blob(b"from the server\n").unwrap();
    let mut tree = there.treebuilder(Some(&base.tree().unwrap())).unwrap();
    tree.insert("result.txt", blob, 0o100644).unwrap();
    let tree = there.find_tree(tree.write().unwrap()).unwrap();
    let me = Signature::now("Rovik", "server@rovik.invalid").unwrap();
    let result = there
        .commit(
            Some("refs/rovik/chats/chat-1/result"),
            &me,
            &me,
            "result",
            &tree,
            &[&base],
        )
        .unwrap();

    let fetched = setup
        .client
        .fetch(PROJECT, &setup.shadow, &[result])
        .await
        .unwrap();

    assert_eq!(fetched, 3, "the result's commit, its tree and the new file");
    let shadow = Repository::open(&setup.shadow).unwrap();
    let tree = shadow.find_commit(result).unwrap().tree().unwrap();
    let file = tree
        .get_name("result.txt")
        .unwrap()
        .to_object(&shadow)
        .unwrap();
    assert_eq!(file.kind(), Some(ObjectType::Blob));

    let stranger = Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap();
    let refused = setup
        .client
        .fetch(PROJECT, &setup.shadow, &[stranger])
        .await;
    assert_eq!(refused.unwrap_err().code, FailCode::BadRequest);
}

#[tokio::test]
async fn a_push_tells_its_progress_on_cloud_event() {
    let setup = Setup::new().await;
    let app = TestCore::new();
    let remote = Remote::new(app.core.clone());
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = heard.clone();
    remote.on_cloud(Arc::new(move |event| sink.lock().unwrap().push(event)));
    remote.connect(
        "server",
        UnixConnector(setup.daemon.socket.clone()),
        identity(&setup.daemon),
    );
    remote.place(CHAT, "server");
    ready(|| remote.states() == [("server".to_string(), HostState::Ready)]).await;

    let snapshot = setup.snapshot();
    remote
        .push_snapshot(CHAT, PROJECT, &setup.shadow, snapshot)
        .await
        .unwrap();

    let heard = heard.lock().unwrap();
    let sent: Vec<u64> = heard
        .iter()
        .map(|event| match event {
            CloudEvent::Pushing {
                dialog_id,
                project,
                sent,
            } => {
                assert_eq!((dialog_id.as_str(), project.as_str()), (CHAT, PROJECT));
                *sent
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(!sent.is_empty());
    assert!(sent.windows(2).all(|pair| pair[0] < pair[1]), "{sent:?}");
    let pack: u64 = setup
        .pack_files()
        .iter()
        .map(|p| std::fs::metadata(p).unwrap().len())
        .sum();
    assert_eq!(*sent.last().unwrap(), pack, "every byte of the pack");
}
