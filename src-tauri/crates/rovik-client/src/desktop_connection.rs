//! A window's replaceable command session. Recovery never retries an operation.
mod session;

use crate::desktop_api::{DesktopRequest, DesktopResult};
use crate::desktop_backend::DesktopBackend;
use rovik_protocol::{Fail, Request, Response};
use session::Connection;
use std::sync::{Arc, Mutex, MutexGuard};

type Reporter = Arc<dyn Fn(String) + Send + Sync>;
struct State {
    connection: Option<Arc<Connection>>,
    closed: bool,
    report: Reporter,
}

/// A window's command boundary; each replacement owns a fresh resource session.
pub struct DesktopConnection {
    state: Mutex<State>,
    #[cfg(any(unix, windows, test))]
    replacement: tokio::sync::Mutex<()>,
    closing: tokio::sync::watch::Sender<bool>,
}

impl DesktopConnection {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn new(connection: Connection) -> Self {
        Self {
            state: Mutex::new(State {
                connection: Some(Arc::new(connection)),
                closed: false,
                report: Arc::new(|reason| log::warn!("Desktop events disconnected: {reason}")),
            }),
            #[cfg(any(unix, windows, test))]
            replacement: Default::default(),
            closing: tokio::sync::watch::channel(false).0,
        }
    }

    fn current(&self) -> Result<Arc<Connection>, String> {
        let state = self.lock();
        if state.closed {
            return Err("Desktop client detached".into());
        }
        state
            .connection
            .clone()
            .ok_or_else(|| "Desktop session unavailable; reconnect this window".into())
    }

    /// Whether commands and private callbacks still have a usable session.
    pub fn usable(&self) -> bool {
        self.current().is_ok_and(|connection| connection.usable())
    }

    /// Releases this window only, including a replacement that is still opening.
    pub async fn close(&self) -> Result<(), String> {
        self.closing.send_replace(true);
        let connection = {
            let mut state = self.lock();
            state.closed = true;
            state.connection.clone()
        };
        match connection {
            Some(connection) => connection.close().await,
            None => Ok(()),
        }
    }

    /// Reports private callback loss for this and subsequent sessions.
    pub fn on_event_error(&self, report: impl Fn(String) + Send + Sync + 'static) {
        let report: Reporter = Arc::new(report);
        let mut state = self.lock();
        state.report = report.clone();
        if let Some(connection) = &state.connection {
            connection.on_event_error(move |reason| report(reason));
        }
    }

    /// Uses an embedded backend on platforms awaiting local IPC.
    pub fn embedded(backend: Arc<DesktopBackend>) -> Self {
        Self::new(Connection::embedded(backend))
    }

    /// Attaches to an authenticated local host without an embedded fallback.
    #[cfg(any(unix, windows))]
    pub async fn connect(directory: &std::path::Path) -> std::io::Result<Self> {
        Ok(Self::new(Connection::connect(directory).await?))
    }

    /// Launches the host only when discovery is absent, then attaches this window.
    #[cfg(any(unix, windows))]
    pub async fn attach(
        command: std::process::Command,
        data: &std::path::Path,
        config: &std::path::Path,
    ) -> std::io::Result<Self> {
        Ok(Self::new(Connection::attach(command, data, config).await?))
    }

    /// Attaches to a standalone executable, retaining its Windows image across updates.
    #[cfg(any(unix, windows))]
    pub async fn attach_backend(
        executable: &std::path::Path,
        data: &std::path::Path,
        config: &std::path::Path,
    ) -> std::io::Result<Self> {
        Ok(Self::new(
            Connection::attach_backend(executable, data, config).await?,
        ))
    }

    /// Recovers through the standalone launcher without replaying accepted commands.
    #[cfg(any(unix, windows))]
    pub async fn reconnect_backend(
        &self,
        executable: &std::path::Path,
        data: &std::path::Path,
        config: &std::path::Path,
        project: Option<(String, String)>,
    ) -> Result<(), String> {
        self.reconnect_with(
            Connection::attach_backend(executable, data, config),
            project,
        )
        .await
    }

    /// Replaces a failed session explicitly. Only validated project registration
    /// is restored; callers recreate subscriptions and refresh authoritative state.
    #[cfg(any(unix, windows))]
    pub async fn reconnect(
        &self,
        command: std::process::Command,
        data: &std::path::Path,
        config: &std::path::Path,
        project: Option<(String, String)>,
    ) -> Result<(), String> {
        self.reconnect_with(Connection::attach(command, data, config), project)
            .await
    }

    #[cfg(any(unix, windows))]
    async fn reconnect_with(
        &self,
        open: impl std::future::Future<Output = std::io::Result<Connection>>,
        project: Option<(String, String)>,
    ) -> Result<(), String> {
        self.replace(async {
            let connection = open.await.map_err(|e| e.to_string())?;
            if let Some((root, project_id)) = project {
                if let Err(error) = connection
                    .desktop(
                        crate::desktop_api::projects::ProjectRequest::Restore { root, project_id }
                            .into(),
                    )
                    .await
                {
                    if let Err(cleanup) = connection.close().await {
                        log::warn!("Could not detach rejected recovery session: {cleanup}");
                    }
                    return Err(error);
                }
            }
            Ok(connection)
        })
        .await
    }

    #[cfg(any(unix, windows, test))]
    async fn replace(
        &self,
        open: impl std::future::Future<Output = Result<Connection, String>>,
    ) -> Result<(), String> {
        let mut closing = self.closing.subscribe();
        let recover = async {
            let _turn = self.replacement.lock().await;
            let old = {
                let mut state = self.lock();
                if state.closed {
                    return Err("Desktop client detached".into());
                }
                state.connection.take()
            };
            if let Some(old) = old {
                // A dead host cannot acknowledge detach. Its socket is abandoned;
                // the new host's lease still arbitrates execution ownership.
                if let Err(error) = old.close().await {
                    log::warn!("Old desktop session could not acknowledge detach: {error}");
                }
            }
            let connection = Arc::new(open.await?);
            {
                let mut state = self.lock();
                if !state.closed {
                    let report = state.report.clone();
                    connection.on_event_error(move |reason| report(reason));
                    state.connection = Some(connection.clone());
                    return Ok(());
                }
            }
            connection.close().await?;
            Err("Desktop client detached".into())
        };
        if *closing.borrow() {
            return Err("Desktop client detached".into());
        }
        tokio::select! {
            biased;
            _ = closing.changed() => Err("Desktop client detached".into()),
            result = recover => result,
        }
    }

    /// Reads shared replay independently of command transport failures.
    pub async fn poll_events(
        &self,
        scope: crate::desktop_journal::Scope,
        cursor: Option<crate::desktop_journal::Cursor>,
    ) -> Result<serde_json::Value, String> {
        self.current()?.poll_events(scope, cursor).await
    }
    /// Reads authoritative run state for the selected scope.
    pub async fn event_status(
        &self,
        scope: crate::desktop_journal::Scope,
    ) -> Result<serde_json::Value, String> {
        self.current()?.event_status(scope).await
    }
    /// Starts one private stream, pinned to its original session throughout startup.
    pub async fn desktop_stream(
        &self,
        request: impl FnOnce(String) -> DesktopRequest,
        callback: impl Fn(serde_json::Value) -> bool + Send + Sync + 'static,
    ) -> DesktopResult {
        self.current()
            .map_err(serde_json::Value::String)?
            .desktop_stream(request, callback)
            .await
    }
    /// Waits for a finite task; replacement cancels its callback, never replays it.
    pub async fn desktop_task(
        &self,
        request: impl FnOnce(String) -> DesktopRequest,
        callback: impl Fn(serde_json::Value) -> Result<(), String> + Send + Sync + 'static,
    ) -> DesktopResult {
        self.current()
            .map_err(serde_json::Value::String)?
            .desktop_task(request, callback)
            .await
    }
    /// Runs one desktop operation without automatic retries.
    pub async fn desktop(&self, request: DesktopRequest) -> Result<serde_json::Value, String> {
        self.current()?.desktop(request).await
    }
    /// Preserves structured service errors.
    pub async fn desktop_raw(&self, request: DesktopRequest) -> DesktopResult {
        self.current()
            .map_err(serde_json::Value::String)?
            .desktop_raw(request)
            .await
    }
    /// Decodes a compact binary response.
    pub async fn desktop_bytes(&self, request: DesktopRequest) -> Result<Vec<u8>, String> {
        self.current()?.desktop_bytes(request).await
    }
    /// Routes one typed operation through the current session.
    pub async fn request(&self, request: Request) -> Result<Response, Fail> {
        self.current().map_err(unavailable)?.request(request).await
    }
    /// Routes one operation to its explicitly selected host.
    pub async fn host_request(&self, host: &str, request: Request) -> Result<Response, Fail> {
        self.current()
            .map_err(unavailable)?
            .host_request(host, request)
            .await
    }
    /// Converts a reply to the command's established return type.
    pub async fn call<T>(&self, request: Request) -> Result<T, String>
    where
        T: TryFrom<Response, Error = Fail>,
    {
        self.request(request)
            .await
            .and_then(T::try_from)
            .map_err(|e| e.message)
    }

    /// The typed reply of an operation on a selected host, or this backend
    /// when no host was selected.
    pub async fn call_on<T>(&self, host: Option<&str>, request: Request) -> Result<T, String>
    where
        T: TryFrom<Response, Error = Fail>,
    {
        let reply = match host.filter(|id| !id.is_empty()) {
            Some(host) => self.host_request(host, request).await,
            None => self.request(request).await,
        };
        reply.and_then(T::try_from).map_err(|e| e.message)
    }
}

fn unavailable(message: String) -> Fail {
    Fail::new(rovik_protocol::FailCode::Unreachable, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unavailable_session() -> DesktopConnection {
        DesktopConnection {
            state: Mutex::new(State {
                connection: None,
                closed: false,
                report: Arc::new(|_| {}),
            }),
            replacement: Default::default(),
            closing: tokio::sync::watch::channel(false).0,
        }
    }

    #[tokio::test]
    async fn closing_interrupts_recovery_and_prevents_late_publication() {
        let connection = unavailable_session();
        let (started, opening) = tokio::sync::oneshot::channel();
        let recover = connection.replace(async {
            started.send(()).unwrap();
            std::future::pending::<Result<Connection, String>>().await
        });
        tokio::pin!(recover);
        tokio::select! { _ = opening => {}, result = &mut recover => panic!("finished early: {result:?}") }
        connection.close().await.unwrap();
        assert!(recover.await.unwrap_err().contains("detached"));
        assert!(connection
            .replace(async { panic!("closed session opened") })
            .await
            .unwrap_err()
            .contains("detached"));
        assert!(!connection.usable());
    }

    #[tokio::test]
    async fn failed_recovery_does_not_publish_a_session_or_replay_a_command() {
        let connection = unavailable_session();
        assert_eq!(
            connection
                .replace(async { Err("incompatible host".into()) })
                .await
                .unwrap_err(),
            "incompatible host"
        );
        assert!(connection.request(Request::AgentStatus).await.is_err());
        assert!(connection.lock().connection.is_none());
        assert_eq!(
            connection
                .replace(async { Err("still unavailable".into()) })
                .await
                .unwrap_err(),
            "still unavailable"
        );
    }
}
