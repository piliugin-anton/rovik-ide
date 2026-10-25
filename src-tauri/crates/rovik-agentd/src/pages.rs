//! Pages a command asks to open on a team server (D20,
//! rovik_core::overlay::confine::pages): each goes to the computer its chat
//! belongs to as `PageWanted`. That computer's Rovik forwards its own port
//! on the loopback to the same port of the user's private network here
//! (`/forward/{port}`, server.rs), opens the page (in a browser tab of its
//! editor, or its default browser), and says how it went (`PageOpened`). The command waits for that, and is
//! told. Once nothing listens on a forwarded port any more, its server
//! gone, the computer hears of it (`PageClosed`) and stops forwarding.

use crate::events::Events;
use futures_util::future::BoxFuture;
use rovik_core::overlay::confine::pages;
use rovik_protocol::Event;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// How long a command waits for the user's computer to open its page.
const ANSWER_WITHIN: Duration = Duration::from_secs(20);

/// How often what listens in the network is looked at, while ports are
/// forwarded.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// How long a page's port may stay unheard after it was opened, its server
/// still starting, before it's taken as closed.
const START_WITHIN: Duration = Duration::from_secs(60);

/// A computer's answer for a page: the port on it that leads there, or why
/// it wasn't forwarded.
type Answer = Result<u16, String>;

/// What sends pages to their chats' computers and hears back.
#[derive(Clone)]
pub struct Opener {
    events: Arc<Events>,
    /// The pages asked for whose computer hasn't answered yet, by id.
    waiting: Arc<Mutex<HashMap<String, oneshot::Sender<Answer>>>>,
    /// The ports computers forward for chats' pages, watched until nothing
    /// listens on them.
    forwarded: Arc<Mutex<Forwarded>>,
}

/// The forwarded ports, and whether a watcher looks at them.
#[derive(Default)]
struct Forwarded {
    ports: HashMap<u16, Port>,
    watched: bool,
}

/// A forwarded port: the chats it was opened for, when, and whether
/// something has listened on it since.
struct Port {
    chats: BTreeSet<String>,
    opened: Instant,
    heard: bool,
}

impl Opener {
    pub fn new(events: Arc<Events>) -> Opener {
        Opener {
            events,
            waiting: Arc::default(),
            forwarded: Arc::default(),
        }
    }

    /// `port` is forwarded for `chat`'s page, a server listening on it now
    /// or not (`listens`): watched until nothing does, then its computer
    /// hears so.
    fn forwarding(&self, chat: &str, port: u16, listens: bool) {
        let mut forwarded = self.forwarded.lock().unwrap_or_else(|e| e.into_inner());
        let entry = forwarded.ports.entry(port).or_insert_with(|| Port {
            chats: BTreeSet::new(),
            opened: Instant::now(),
            heard: false,
        });
        entry.chats.insert(chat.to_string());
        // Heard now, it closes as soon as its server goes, however soon.
        entry.heard |= listens;
        if !forwarded.watched {
            forwarded.watched = true;
            tokio::spawn(self.clone().watch());
        }
    }

    /// Looks at what listens in the network every few seconds, while any
    /// port is forwarded, and tells each closed port's chats.
    async fn watch(self) {
        loop {
            tokio::time::sleep(WATCH_EVERY).await;
            let listening = listening_now().await;
            let (closed, done) = closed_ports(
                &mut self.forwarded.lock().unwrap_or_else(|e| e.into_inner()),
                &listening,
                Instant::now(),
            );
            for (port, chats) in closed {
                for chat in chats {
                    let ev = Event::PageClosed {
                        dialog_id: chat.clone(),
                        port,
                    };
                    self.events.chat(&chat, ev);
                }
            }
            if done {
                return;
            }
        }
    }

    /// `PageOpened`: page `id` was forwarded from `outcome`'s port on the
    /// computer and is ready to open, or why not. Whether its command still
    /// waits for it: the computer opens the page only then.
    pub fn answered(&self, id: &str, outcome: Answer) -> bool {
        let waiting = self
            .waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        match waiting {
            Some(waiting) => waiting.send(outcome).is_ok(),
            None => false,
        }
    }
}

impl pages::Pages for Opener {
    fn open(
        &self,
        chat: String,
        url: String,
        port: u16,
    ) -> BoxFuture<'static, Result<String, String>> {
        let this = self.clone();
        Box::pin(async move {
            if this.events.computer_of(&chat).is_none() {
                return Err(format!(
                    "Rovik couldn't open {url}: this command's chat belongs to no computer of the \
                     user's on this server. Tell the user the address instead."
                ));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let (answer, answered) = oneshot::channel();
            this.waiting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.clone(), answer);
            this.events.chat(
                &chat,
                Event::PageWanted {
                    dialog_id: chat.clone(),
                    id: id.clone(),
                    url: url.clone(),
                    port,
                },
            );
            let heard = tokio::time::timeout(ANSWER_WITHIN, answered).await;
            this.waiting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            if matches!(heard, Ok(Ok(Ok(_)))) {
                let listens = listening_now().await.contains(&port);
                this.forwarding(&chat, port, listens);
            }
            match heard {
                Ok(Ok(Ok(local))) if local == port => Ok(format!(
                    "Rovik opened {url} on the user's computer: its own port {port} leads to \
                     port {port} here, for as long as a server listens on it."
                )),
                Ok(Ok(Ok(local))) => Ok(format!(
                    "Rovik opened {url} on the user's computer, at port {local} there: its port \
                     {port} was taken, so its port {local} leads to port {port} here, for as long \
                     as a server listens on it."
                )),
                Ok(Ok(Err(why))) => Err(why),
                _ => Err(format!(
                    "Rovik couldn't open {url}: the user's computer didn't answer within {} s (it \
                     may be asleep, not connected to this server, or running an older Rovik). The \
                     page is still at that address for commands here; tell the user the address \
                     if they need it.",
                    ANSWER_WITHIN.as_secs()
                )),
            }
        })
    }
}

/// The ports something listens on in the user's private network now: none
/// without one.
async fn listening_now() -> Vec<u16> {
    match rovik_core::overlay::confine::network::current() {
        Some(network) => rovik_core::rt::spawn_blocking(move || network.listening_ports())
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

/// The ports of `forwarded` that are closed, given what's `listening` at
/// `now`, each with its chats, taken out; and whether none is left to
/// watch, the watcher then ending. A port is closed once something has
/// listened on it and nothing does now, or nothing has since it was opened
/// [`START_WITHIN`] ago.
fn closed_ports(
    forwarded: &mut Forwarded,
    listening: &[u16],
    now: Instant,
) -> (Vec<(u16, BTreeSet<String>)>, bool) {
    let mut closed = Vec::new();
    forwarded.ports.retain(|port, forward| {
        if listening.contains(port) {
            forward.heard = true;
            return true;
        }
        let gone = forward.heard || now.duration_since(forward.opened) > START_WITHIN;
        if gone {
            closed.push((*port, std::mem::take(&mut forward.chats)));
        }
        !gone
    });
    let done = forwarded.ports.is_empty();
    if done {
        forwarded.watched = false;
    }
    (closed, done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forwarded(ports: &[(u16, &str)], opened: Instant) -> Forwarded {
        let mut forwarded = Forwarded {
            watched: true,
            ..Default::default()
        };
        for (port, chat) in ports {
            let entry = forwarded.ports.entry(*port).or_insert_with(|| Port {
                chats: BTreeSet::new(),
                opened,
                heard: false,
            });
            entry.chats.insert(chat.to_string());
        }
        forwarded
    }

    #[test]
    fn a_port_closes_once_its_server_was_heard_and_is_gone() {
        let start = Instant::now();
        let mut ports = forwarded(&[(3000, "a"), (3000, "b"), (5173, "a")], start);
        // Both listen: nothing closes, and both are heard.
        assert_eq!(
            closed_ports(&mut ports, &[3000, 5173], start),
            (vec![], false)
        );
        // 3000 is gone: both its chats hear so; 5173 stays watched.
        let (closed, done) = closed_ports(&mut ports, &[5173], start);
        let chats: BTreeSet<String> = ["a".to_string(), "b".to_string()].into();
        assert_eq!(closed, vec![(3000, chats)]);
        assert!(!done);
        // The last one gone: the watcher ends.
        let (closed, done) = closed_ports(&mut ports, &[], start);
        assert_eq!(closed.len(), 1);
        assert!(done && !ports.watched);
    }

    #[test]
    fn a_server_still_starting_has_a_minute_to_listen() {
        let start = Instant::now();
        let mut ports = forwarded(&[(8080, "a")], start);
        let soon = start + Duration::from_secs(5);
        assert_eq!(closed_ports(&mut ports, &[], soon), (vec![], false));
        let late = start + START_WITHIN + Duration::from_secs(1);
        let (closed, done) = closed_ports(&mut ports, &[], late);
        assert_eq!(closed.len(), 1);
        assert!(done);
    }

    #[tokio::test]
    async fn an_answer_reaches_the_page_waiting_for_it_once() {
        let opener = Opener::new(Events::new("boot".into(), Arc::new(|_: &str| None)));
        let (answer, answered) = oneshot::channel();
        opener
            .waiting
            .lock()
            .unwrap()
            .insert("page-1".into(), answer);
        assert!(opener.answered("page-1", Ok(3001)));
        assert_eq!(answered.await, Ok(Ok(3001)));
        assert!(!opener.answered("page-1", Ok(3001)), "not wanted twice");
        assert!(
            !opener.answered("page-2", Err("no".into())),
            "never asked for"
        );
    }

    #[tokio::test]
    async fn a_page_for_a_chat_no_computer_started_is_refused_at_once() {
        let opener = Opener::new(Events::new("boot".into(), Arc::new(|_: &str| None)));
        let refused = pages::Pages::open(&opener, "chat-x".into(), "http://localhost:1/".into(), 1)
            .await
            .unwrap_err();
        assert!(refused.contains("belongs to no computer"), "{refused}");
    }
}
