//! `rovik-agentd attach --stdio`: a byte bridge between stdin and stdout and
//! the daemon's socket, for a server that doesn't forward Unix sockets over
//! SSH: the client runs it on an exec channel instead (D5). It knows nothing
//! of HTTP or WebSockets, so both go through it as through the socket.

use crate::home::Home;
use std::path::PathBuf;
use tokio::io::{self, AsyncWriteExt};
use tokio::net::UnixStream;

pub fn attach(socket: Option<PathBuf>) -> Result<(), String> {
    let socket = match socket {
        Some(socket) => socket,
        None => Home::locate(None)?.socket(),
    };
    rovik_core::rt::block_on(async move {
        let daemon = UnixStream::connect(&socket)
            .await
            .map_err(|e| format!("{}: {e}", socket.display()))?;
        bridge(daemon).await.map_err(|e| e.to_string())
    })
}

/// Copies both ways until the daemon closes. When stdin ends first, the
/// daemon hears it, and whatever it still sends comes out; a client keeps
/// its side open until it has its answer, though, as the daemon may drop a
/// request whose client closed its half first.
async fn bridge(daemon: UnixStream) -> io::Result<()> {
    let (mut from_daemon, mut to_daemon) = daemon.into_split();
    let (mut stdin, mut stdout) = (io::stdin(), io::stdout());
    let up = async {
        io::copy(&mut stdin, &mut to_daemon).await?;
        to_daemon.shutdown().await
    };
    let down = async {
        io::copy(&mut from_daemon, &mut stdout).await?;
        stdout.flush().await
    };
    tokio::pin!(up, down);
    tokio::select! {
        done = &mut down => done,
        sent = &mut up => {
            sent?;
            down.await
        }
    }
}
