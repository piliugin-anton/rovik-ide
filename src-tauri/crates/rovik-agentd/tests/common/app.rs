//! The app against the real daemon (T06): a core of its own, whose chat is
//! in the setup's project and on the daemon, with the project open, its
//! shadow where the app keeps it, and its remote chats' `Remote`.

use super::project::{Setup, CHAT, PROJECT};
use super::COMPUTER;
use rovik_client::{shadow, HostState, Identity, Remote, UnixConnector};
use rovik_core::chat_db::projects;
use rovik_core::chat_db::store::{self, NewDialog};
use rovik_core::overlay::chats;
use rovik_core::testing::TestCore;
use rovik_protocol::Secret;
use std::sync::Arc;
use std::time::Duration;

pub const HOST: &str = "server";

pub struct App {
    pub test: TestCore,
    pub remote: Arc<Remote>,
    /// The overlays' data folder is the process's: this app's while it
    /// lasts.
    _overlays: tokio::sync::OwnedMutexGuard<()>,
}

impl App {
    /// The app, not connected yet. The setup's snapshots go from its shadow.
    pub async fn new(setup: &mut Setup) -> App {
        let overlays = chats::TEST_DATA.clone().lock_owned().await;
        let test = TestCore::new();
        let data = test.core.paths.data_dir.clone();
        chats::init(&data);
        setup.shadow = shadow::dir(&data, PROJECT);
        let root = setup.project.clone();
        let dialog = NewDialog {
            id: CHAT.into(),
            title: "Chat".into(),
            created_at: 0,
            updated_at: 0,
            project_id: Some(PROJECT.into()),
        };
        let made = test.core.db.write(move |c| {
            projects::ensure(c, PROJECT, "app", &root)?;
            store::create_dialog(c, &dialog)
        });
        made.await.unwrap();
        let remote = Remote::new(test.core.clone());
        remote.place(CHAT, HOST);
        remote.set_open_project(Some((PROJECT.into(), setup.project.clone())));
        App {
            test,
            remote,
            _overlays: overlays,
        }
    }

    /// Connects to the daemon, once it's ready.
    pub async fn connect(&self, setup: &Setup) {
        let identity = Identity {
            host: HOST.into(),
            computer: COMPUTER.into(),
            token: Secret(setup.daemon.token.clone()),
            client: "app tests".into(),
        };
        let connector = UnixConnector(setup.daemon.socket.clone());
        self.remote.connect(HOST, connector, identity);
        for _ in 0..400 {
            if self.remote.states() == [(HOST.to_string(), HostState::Ready)] {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("never connected\n{}", setup.daemon.output());
    }
}
