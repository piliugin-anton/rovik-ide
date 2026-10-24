//! Pages a remote chat's commands open on its team server (D20): a dev
//! server's own (`vite --open`), a command line tool's sign-in. Each comes
//! as `PageWanted`: a page of a server on a port of the user's private
//! network there. This computer forwards a port of its own loopback to
//! that one, each connection through the daemon (`Client::forward`): the
//! page's own port where it's free here, else one the system picks. It
//! tells the host which (`PageOpened`) and, once the host says the command
//! still waits for the page, opens it in the default browser at that port.
//! The port stays forwarded, shown above the chat (`CloudEvent::Ports`),
//! until nothing listens on it there any more (`PageClosed`), the chat
//! leaves the host, or the host goes or comes back having missed events.

use super::{CloudEvent, Remote};
use rovik_core::agent_run::events::AgentEvent;
use rovik_core::overlay::confine::page_url::local_page;
use rovik_protocol::Request;
use serde::Serialize;
use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

/// How long a port of this computer's loopback has to answer before it
/// counts as free: the system answers at once for whatever listens there,
/// while a closed port can take seconds to be refused (on Windows).
const ANSWER_WITHIN: Duration = Duration::from_millis(250);

/// How many ports the system picks are tried, when the page's own port is
/// taken here.
const PICKS: usize = 5;

/// A port a chat's server opened on its host, and the port of this
/// computer's loopback that leads to it.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct ForwardedPort {
    /// The server's port, on the host.
    pub port: u16,
    /// The port here that leads to it: the same one where it was free.
    pub local_port: u16,
}

/// Opens a page in this computer's default browser; why not, when it can't.
pub type OpenPage = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// The ports this computer forwards to its hosts, and what opens their
/// pages.
#[derive(Default)]
pub(super) struct Pages {
    /// By host and the port there.
    forwards: Mutex<HashMap<(String, u16), Forward>>,
    open: Mutex<Option<OpenPage>>,
}

impl Pages {
    fn forwards(&self) -> MutexGuard<'_, HashMap<(String, u16), Forward>> {
        self.forwards.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn opener(&self) -> Option<OpenPage> {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// A port of this computer forwarded to one of a host's.
struct Forward {
    local: u16,
    /// The chats whose pages it was opened for, as their host confirmed.
    chats: BTreeSet<String>,
    /// The pages on their way through it that their host hasn't confirmed
    /// yet: it stays while there are any.
    pending: usize,
    /// Its listeners, and the connections they carry: ended when it goes.
    serving: JoinHandle<()>,
}

impl Forward {
    /// Whether no page holds it any more.
    fn unused(&self) -> bool {
        self.chats.is_empty() && self.pending == 0
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

impl Remote {
    /// How a page opens in this computer's default browser (the app's):
    /// until it's set, pages are refused.
    pub fn on_open_page(&self, open: OpenPage) {
        *self.pages.open.lock().unwrap_or_else(|e| e.into_inner()) = Some(open);
    }

    /// The ports forwarded for the chat's pages, by the server's port.
    pub fn forwarded_ports(&self, dialog_id: &str) -> Vec<ForwardedPort> {
        let forwards = self.pages.forwards();
        let mut ports: Vec<ForwardedPort> = forwards
            .iter()
            .filter(|(_, forward)| forward.chats.contains(dialog_id))
            .map(|((_, port), forward)| ForwardedPort {
                port: *port,
                local_port: forward.local,
            })
            .collect();
        ports.sort();
        ports
    }

    /// Every chat's forwarded ports: for a window that starts listening
    /// after they were opened.
    pub fn all_forwarded_ports(&self) -> HashMap<String, Vec<ForwardedPort>> {
        let chats: BTreeSet<String> = self
            .pages
            .forwards()
            .values()
            .flat_map(|forward| forward.chats.iter().cloned())
            .collect();
        chats
            .into_iter()
            .map(|chat| {
                let ports = self.forwarded_ports(&chat);
                (chat, ports)
            })
            .collect()
    }

    /// `PageWanted` from `host`, in a task of its own: the host's other
    /// events don't wait for it.
    pub(super) fn page_wanted(
        &self,
        host: &str,
        dialog_id: String,
        id: String,
        url: String,
        port: u16,
    ) {
        let Some(me) = self.me.upgrade() else { return };
        let host = host.to_string();
        tokio::spawn(async move { me.open_page(&host, &dialog_id, id, &url, port).await });
    }

    /// Forwards a port here to `port` on `host` for `dialog_id`'s page at
    /// `url`, tells the host which (or why not), and opens the page once
    /// the host says its command still waits for it.
    async fn open_page(&self, host: &str, dialog_id: &str, id: String, url: &str, port: u16) {
        // Only a chat of this host's, in the window that shows its events:
        // a page opens once, on the computer its chat is from.
        if self.host_of(dialog_id).as_deref() != Some(host)
            && !self.placed_by_location(dialog_id, host).await
        {
            return;
        }
        if self.handled_elsewhere(dialog_id).await {
            return;
        }
        let forwarded = self.forward_page(host, url, port).await;
        let answer = match &forwarded {
            Ok((local, _)) => Request::PageOpened {
                id,
                local_port: Some(*local),
                error: None,
            },
            Err(why) => Request::PageOpened {
                id,
                local_port: None,
                error: Some(why.clone()),
            },
        };
        let wanted = match self.client(host) {
            Ok(client) => client.request::<bool>(answer).await.unwrap_or_else(|fail| {
                log::warn!("{host}: the page {url} wasn't confirmed: {}", fail.message);
                false
            }),
            Err(_) => false,
        };
        let Ok((_, address)) = forwarded else { return };
        if !self.settle_page(host, port, dialog_id, wanted) {
            return;
        }
        self.tell_ports(dialog_id);
        let opened = match self.pages.opener() {
            Some(open) => open(&address),
            None => Err("this app opens no pages".to_string()),
        };
        if let Err(why) = opened {
            log::warn!("{address} didn't open: {why}");
            self.core.emit(AgentEvent::Notice {
                dialog_id: Some(dialog_id.to_string()),
                kind: "warning".into(),
                title: "The page didn't open".into(),
                message: format!(
                    "{address} leads to {url} on {}, but the browser didn't open it: {why}",
                    self.host_name(host)
                ),
            });
        }
    }

    /// A port here that leads to `port` on `host`, for the page at `url`
    /// there: the one already forwarded to it, else a new one, held for
    /// the page until its host answers (`settle_page`). With the page's
    /// address at that port, to open. Why not, in words for the agent,
    /// when the page isn't one of the user's own servers or no port here
    /// can be had.
    async fn forward_page(
        &self,
        host: &str,
        url: &str,
        port: u16,
    ) -> Result<(u16, String), String> {
        if self.pages.opener().is_none() {
            return Err(format!(
                "Rovik couldn't open {url}: the app on the user's computer opens no pages. Tell \
                 the user the address instead."
            ));
        }
        // Checked here too: whatever the host asks, only a page of the
        // user's own servers opens on this computer.
        let mut page = page_at(url, port)?;
        let key = (host.to_string(), port);
        let held = self.pages.forwards().get_mut(&key).map(|forward| {
            forward.pending += 1;
            forward.local
        });
        let local = match held {
            Some(local) => local,
            None => {
                let listeners = listen(port).await.map_err(|e| {
                    format!(
                        "Rovik couldn't open {url} on the user's computer: no port of its \
                         loopback could be forwarded to it ({e})."
                    )
                })?;
                let local = listeners
                    .first()
                    .and_then(|l| l.local_addr().ok())
                    .map(|a| a.port())
                    .ok_or_else(|| format!("Rovik couldn't open {url}: its port here is gone."))?;
                let serving = tokio::spawn(serve(listeners, self.me.clone(), port, host.into()));
                let fresh = Forward {
                    local,
                    chats: BTreeSet::new(),
                    pending: 1,
                    serving,
                };
                match self.pages.forwards().entry(key) {
                    // Another page of the port forwarded it meanwhile: that
                    // forward serves both, and this one goes.
                    Entry::Occupied(mut made) => {
                        made.get_mut().pending += 1;
                        made.get().local
                    }
                    Entry::Vacant(place) => place.insert(fresh).local,
                }
            }
        };
        // It took a port in `page_at`: it takes this one too.
        let _ = page.set_port(Some(local));
        Ok((local, page.to_string()))
    }

    /// The host answered for a page held on the forward of `port`: the
    /// forward is `chat`'s from now on when its command still waits for the
    /// page, and goes once no page holds it. Whether the page is to open:
    /// it's wanted, and its forward is still there.
    fn settle_page(&self, host: &str, port: u16, chat: &str, wanted: bool) -> bool {
        let key = (host.to_string(), port);
        let mut forwards = self.pages.forwards();
        let Some(forward) = forwards.get_mut(&key) else {
            return false;
        };
        forward.pending = forward.pending.saturating_sub(1);
        if wanted {
            forward.chats.insert(chat.to_string());
        }
        if forward.unused() {
            forwards.remove(&key);
        }
        wanted
    }

    /// `PageClosed` from `host`: nothing listens there on `port` any more,
    /// which was forwarded for `chat`'s page.
    pub(super) fn page_closed(&self, host: &str, chat: &str, port: u16) {
        self.release(chat, |(at, forwarded)| at == host && *forwarded == port);
    }

    /// `chats` left their host, for this computer or for good: their
    /// pages' ports aren't forwarded for them any more.
    pub(super) fn release_pages(&self, chats: &[String]) {
        for chat in chats {
            self.release(chat, |_| true);
        }
    }

    /// `chat` lets go of the forwards `which` picks; one no page holds any
    /// more goes. The frontend hears of the chat's ports if they changed.
    fn release(&self, chat: &str, which: impl Fn(&(String, u16)) -> bool) {
        let mut changed = false;
        self.pages.forwards().retain(|key, forward| {
            if which(key) && forward.chats.remove(chat) {
                changed = true;
            }
            !forward.unused()
        });
        if changed {
            self.tell_ports(chat);
        }
    }

    /// Stops forwarding to `host`: it's gone from the settings, or came
    /// back having missed events, a `PageClosed` among them maybe.
    pub(super) fn stop_forwarding(&self, host: &str) {
        let mut chats = BTreeSet::new();
        self.pages.forwards().retain(|(at, _), forward| {
            if at != host {
                return true;
            }
            chats.extend(std::mem::take(&mut forward.chats));
            false
        });
        for chat in chats {
            self.tell_ports(&chat);
        }
    }

    /// The frontend hears of the chat's forwarded ports as they are now.
    fn tell_ports(&self, dialog_id: &str) {
        if let Some(cloud) = self.cloud() {
            cloud(CloudEvent::Ports {
                dialog_id: dialog_id.to_string(),
                ports: self.forwarded_ports(dialog_id),
            });
        }
    }
}

/// The page at `url`, served on `port`, when it's a page of the user's own
/// servers on that port (`local_page`); why not, in words for the agent.
fn page_at(url: &str, port: u16) -> Result<reqwest::Url, String> {
    let named = local_page(url)?;
    if named != port {
        return Err(format!(
            "Rovik didn't open {url}: it was asked to forward port {port}, and the page is on \
             port {named}."
        ));
    }
    let mut page =
        reqwest::Url::parse(url.trim()).map_err(|e| format!("Rovik didn't open {url}: {e}."))?;
    page.set_port(Some(port))
        .map_err(|()| format!("Rovik didn't open {url}: it can't name a port."))?;
    Ok(page)
}

/// Listeners on this computer's loopback for a forward of `wanted`: on
/// that port where it's free on 127.0.0.1 and ::1 alike, else on one the
/// system picks. Both, so that `localhost` leads there whichever of them
/// the browser tries first, and never to a server of this computer's own
/// that listens on the other; ::1 only where this computer has IPv6.
async fn listen(wanted: u16) -> io::Result<Vec<TcpListener>> {
    match listen_on(wanted).await {
        Ok(listeners) => return Ok(listeners),
        Err(e) => log::info!("port {wanted} is taken here ({e}): forwarding another"),
    }
    let mut failed = io::Error::from(io::ErrorKind::AddrInUse);
    for _ in 0..PICKS {
        match listen_on(0).await {
            Ok(listeners) => return Ok(listeners),
            Err(e) => failed = e,
        }
    }
    Err(failed)
}

/// Listeners on `port` of 127.0.0.1 and of ::1 (0: one the system picks);
/// AddrInUse when something listens there already.
async fn listen_on(port: u16) -> io::Result<Vec<TcpListener>> {
    if port != 0 && answers(port).await {
        return Err(io::ErrorKind::AddrInUse.into());
    }
    let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let picked = v4.local_addr()?.port();
    // The system picked a port free on 127.0.0.1: ::1 is asked now.
    if port == 0 && answers_at(Ipv6Addr::LOCALHOST.into(), picked).await {
        return Err(io::ErrorKind::AddrInUse.into());
    }
    match TcpListener::bind((Ipv6Addr::LOCALHOST, picked)).await {
        Ok(v6) => Ok(vec![v4, v6]),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => Err(e),
        // No IPv6 here: 127.0.0.1 alone.
        Err(_) => Ok(vec![v4]),
    }
}

/// Whether something answers on `port` of 127.0.0.1 or ::1: both asked at
/// once, so a closed port's wait comes once.
async fn answers(port: u16) -> bool {
    let (v4, v6) = tokio::join!(
        answers_at(Ipv4Addr::LOCALHOST.into(), port),
        answers_at(Ipv6Addr::LOCALHOST.into(), port)
    );
    v4 || v6
}

/// Whether something answers on `port` of `ip`: then it's taken, even where
/// the system would let another listener in beside it (one on a single
/// address beside one on every address, as macOS and Windows allow).
async fn answers_at(ip: IpAddr, port: u16) -> bool {
    let connecting = TcpStream::connect((ip, port));
    matches!(
        tokio::time::timeout(ANSWER_WITHIN, connecting).await,
        Ok(Ok(_))
    )
}

/// Carries each connection to `listeners` to `port` on `host`, until the
/// task is aborted: the connections it carries end with it.
async fn serve(listeners: Vec<TcpListener>, remote: Weak<Remote>, port: u16, host: String) {
    let mut carrying = JoinSet::new();
    loop {
        let accepted = tokio::select! {
            accepted = accept_any(&listeners) => accepted,
            Some(_) = carrying.join_next(), if !carrying.is_empty() => continue,
        };
        match accepted {
            Ok(local) => {
                carrying.spawn(carry(local, remote.clone(), port, host.clone()));
            }
            Err(e) => {
                log::warn!("a forwarded port to {host}: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// The next connection to any of `listeners`.
async fn accept_any(listeners: &[TcpListener]) -> io::Result<TcpStream> {
    let accepts = listeners.iter().map(|l| Box::pin(l.accept()));
    let (accepted, _, _) = futures_util::future::select_all(accepts).await;
    accepted.map(|(stream, _)| stream)
}

/// Carries `local`, a connection here, to `port` on `host`, through the
/// host's connection as it is now.
async fn carry(mut local: TcpStream, remote: Weak<Remote>, port: u16, host: String) {
    let Some(client) = remote.upgrade().and_then(|r| r.client(&host).ok()) else {
        return;
    };
    match client.forward(port).await {
        Ok(mut there) => {
            let _ = tokio::io::copy_bidirectional(&mut local, &mut there).await;
        }
        Err(fail) => log::info!("{host}: port {port} there: {}", fail.message),
    }
}
