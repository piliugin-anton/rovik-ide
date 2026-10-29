//! MCP servers on the loopback of the user's computers (mcp-plan-2.md,
//! Q11). A URL server at `localhost` is on the computer whose chat runs
//! here, and a host can't reach it: a run's POST to it goes through the
//! app there. That computer hears `McpRelayWanted`, takes the POST once
//! (`McpRelayTake`, so a replayed event makes none), makes it, and sends
//! back its answer as it comes (`McpRelayPart`), which the run reads as the
//! server's own. A computer that isn't connected has its servers
//! unavailable, saying it's away; one that leaves mid-answer ends it.

use crate::events::Events;
use futures_util::future::BoxFuture;
use rovik_protocol::mcp::{RelayPart, RelayedPost};
use rovik_protocol::{Event, Fail, FailCode};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// How long the computer has to take a POST and answer with its status.
#[cfg(not(test))]
const ANSWER_WITHIN: Duration = Duration::from_secs(20);
#[cfg(test)]
const ANSWER_WITHIN: Duration = Duration::from_millis(300);

/// Why a server on a computer that isn't connected is unavailable.
pub const AWAY: &str = "It runs on the user's computer, which is away: it's there only while \
                        that computer is on and Rovik runs there.";

/// Why an answer ended when its computer left.
const LEFT: &str = "The user's computer, which the server runs on, went away.";

/// Whether this daemon shares the loopback of the user's computer, and
/// posts to the servers there itself: WSL with mirrored networking. In
/// WSL's default NAT networking, Windows' loopback isn't WSL's.
pub fn loopback_shared() -> bool {
    if !crate::network::in_wsl() {
        return false;
    }
    let mut asked = rovik_core::process::command("wslinfo");
    let said = asked.arg("--networking-mode").output();
    said.is_ok_and(|said| said.status.success() && mirrored(&String::from_utf8_lossy(&said.stdout)))
}

/// Whether `wslinfo --networking-mode` said the networking is mirrored.
fn mirrored(said: &str) -> bool {
    said.trim().eq_ignore_ascii_case("mirrored")
}

/// What sends POSTs to the computers' apps and hears their answers.
#[derive(Clone)]
pub struct Relays {
    events: Arc<Events>,
    /// The POSTs whose answers are still wanted, by id.
    waiting: Arc<Mutex<HashMap<String, Waiting>>>,
}

/// A POST for a computer to make: until it's taken, the POST; then where
/// its answer's parts go.
struct Waiting {
    computer: String,
    post: Option<RelayedPost>,
    parts: mpsc::UnboundedSender<RelayPart>,
}

impl Relays {
    pub fn new(events: Arc<Events>) -> Relays {
        Relays {
            events,
            waiting: Arc::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Waiting>> {
        self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `McpRelayTake`: POST `id`, for its computer alone, once.
    pub fn take(&self, computer: &str, id: &str) -> Result<RelayedPost, Fail> {
        let mut waiting = self.lock();
        let Some(relay) = waiting.get_mut(id).filter(|w| w.computer == computer) else {
            return Err(Fail::new(
                FailCode::BadRequest,
                "No such POST waits for this computer: it was given up, or it's another's.",
            ));
        };
        relay
            .post
            .take()
            .ok_or_else(|| Fail::new(FailCode::BadRequest, "This POST was taken already."))
    }

    /// `McpRelayPart`: the next part of POST `id`'s answer, from its
    /// computer. Whether the answer is still wanted.
    pub fn part(&self, computer: &str, id: &str, part: RelayPart) -> bool {
        let waiting = self.lock();
        let relay = waiting.get(id).filter(|w| w.computer == computer);
        relay.is_some_and(|w| w.post.is_none() && w.parts.send(part).is_ok())
    }

    /// Has `computer`'s app make `post`: its answer, read as it comes.
    async fn relay(self, computer: String, post: RelayedPost) -> Result<reqwest::Response, String> {
        if !self.events.here(&computer) {
            return Err(AWAY.into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (parts, received) = mpsc::unbounded_channel();
        let left = self.events.leavings();
        let waiting = Waiting {
            computer: computer.clone(),
            post: Some(post),
            parts,
        };
        self.lock().insert(id.clone(), waiting);
        let mut answer = Answer {
            relays: self.clone(),
            id: id.clone(),
            computer: computer.clone(),
            received,
            left,
        };
        self.events
            .computer(&computer, Event::McpRelayWanted { id });
        let head = tokio::time::timeout(ANSWER_WITHIN, answer.next()).await;
        match head {
            Ok(Ok(Some(RelayPart::Head { status, headers }))) => {
                rovik_core::mcp::relayed_response(status, &headers, answer.body())
            }
            Ok(Ok(Some(RelayPart::Failed { error }))) => Err(error),
            Ok(Ok(_)) => Err("The user's computer sent no answer.".into()),
            Ok(Err(left)) => Err(left),
            Err(_) => Err(format!(
                "The user's computer, which the server runs on, didn't answer within {} s: it \
                 may be asleep, or run an older Rovik.",
                ANSWER_WITHIN.as_secs()
            )),
        }
    }
}

impl rovik_core::mcp::Relay for Relays {
    fn computer_of(&self, chat: &str) -> Option<String> {
        self.events.computer_of(chat)
    }

    fn post(
        &self,
        computer: &str,
        post: RelayedPost,
    ) -> BoxFuture<'static, Result<reqwest::Response, String>> {
        Box::pin(self.clone().relay(computer.to_string(), post))
    }
}

/// A POST's answer as its parts come, until its computer leaves. Once
/// dropped, read or given up, it isn't wanted any more.
struct Answer {
    relays: Relays,
    id: String,
    computer: String,
    received: mpsc::UnboundedReceiver<RelayPart>,
    left: watch::Receiver<u64>,
}

impl Answer {
    /// The next part; none once the computer's app has no more to send.
    async fn next(&mut self) -> Result<Option<RelayPart>, String> {
        loop {
            tokio::select! {
                part = self.received.recv() => return Ok(part),
                changed = self.left.changed() => {
                    if changed.is_err() || !self.relays.events.here(&self.computer) {
                        return Err(LEFT.into());
                    }
                }
            }
        }
    }

    /// The body's text, part by part, to its end; an error ends it.
    fn body(self) -> impl futures_util::Stream<Item = Result<String, String>> + Send + 'static {
        futures_util::stream::unfold(Some(self), |answer| async move {
            let mut answer = answer?;
            match answer.next().await {
                Ok(Some(RelayPart::Body { text })) => Some((Ok(text), Some(answer))),
                Ok(Some(RelayPart::End)) | Ok(None) => None,
                Ok(Some(RelayPart::Failed { error })) => Some((Err(error), None)),
                Ok(Some(RelayPart::Head { .. })) => {
                    Some((Err("The answer came with two heads.".into()), None))
                }
                Err(left) => Some((Err(left), None)),
            }
        })
    }
}

impl Drop for Answer {
    fn drop(&mut self) {
        self.relays.lock().remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovik_core::mcp::Relay;
    use rovik_protocol::Secret;

    fn relays() -> Relays {
        Relays::new(Events::new("boot".into(), Arc::new(|_| None)))
    }

    fn post() -> RelayedPost {
        RelayedPost {
            url: "http://127.0.0.1:3000/mcp".into(),
            headers: vec![("Authorization".into(), Secret("Bearer t0p".into()))],
            body: "{}".into(),
        }
    }

    /// The id of the next `McpRelayWanted` the subscription gets.
    async fn wanted(subscription: &mut crate::events::Subscription) -> String {
        loop {
            let published = subscription.live.recv().await.unwrap();
            let frame: serde_json::Value = serde_json::from_str(&published.text).unwrap();
            if frame["ev"]["t"] == "mcpRelayWanted" {
                return frame["ev"]["id"].as_str().unwrap().to_string();
            }
        }
    }

    /// Q11: a computer that isn't connected has its servers unavailable at
    /// once, saying it's away.
    #[tokio::test]
    async fn a_post_for_a_computer_that_isn_t_connected_fails_at_once() {
        let error = relays().post("laptop", post()).await.unwrap_err();
        assert_eq!(error, AWAY);
    }

    /// The computer takes the POST once, and its parts, in order, are the
    /// answer the run reads; another computer can neither take nor answer
    /// it, and once read to its end it isn't wanted any more.
    #[tokio::test]
    async fn the_computer_takes_the_post_once_and_its_parts_are_the_answer() {
        let relays = relays();
        let mut laptop = relays.events.subscribe("laptop", None);
        let _desk = relays.events.subscribe("desk", None);
        let asked = tokio::spawn(relays.post("laptop", post()));
        let id = wanted(&mut laptop).await;
        assert!(relays.take("desk", &id).is_err(), "another computer's");
        let taken = relays.take("laptop", &id).unwrap();
        assert_eq!(taken.headers[0].1 .0, "Bearer t0p");
        assert!(relays.take("laptop", &id).is_err(), "once");
        let head = RelayPart::Head {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
        };
        assert!(!relays.part("desk", &id, head.clone()));
        assert!(relays.part("laptop", &id, head));
        for text in ["{\"a\":", "1}"] {
            let body = RelayPart::Body { text: text.into() };
            assert!(relays.part("laptop", &id, body));
        }
        assert!(relays.part("laptop", &id, RelayPart::End));
        let response = asked.await.unwrap().unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), "{\"a\":1}");
        assert!(!relays.part("laptop", &id, RelayPart::End), "not wanted");
    }

    /// A POST no one takes is given up, saying why; one the app couldn't
    /// make fails with its error.
    #[tokio::test]
    async fn an_answer_that_doesn_t_come_is_given_up_saying_why() {
        let relays = relays();
        let mut laptop = relays.events.subscribe("laptop", None);
        let error = relays.post("laptop", post()).await.unwrap_err();
        assert!(error.contains("didn't answer"), "{error}");
        let id = wanted(&mut laptop).await;
        assert!(relays.take("laptop", &id).is_err(), "given up");

        let asked = tokio::spawn(relays.post("laptop", post()));
        let id = wanted(&mut laptop).await;
        relays.take("laptop", &id).unwrap();
        let failed = RelayPart::Failed {
            error: "Connection refused".into(),
        };
        assert!(relays.part("laptop", &id, failed));
        assert_eq!(asked.await.unwrap().unwrap_err(), "Connection refused");
    }

    /// WSL shares the computer's loopback only with mirrored networking.
    #[test]
    fn only_mirrored_networking_shares_the_loopback() {
        assert!(mirrored("mirrored\n"));
        assert!(!mirrored("nat\n"));
        assert!(!mirrored(""));
    }

    /// A computer that leaves mid-answer ends it, as a broken connection
    /// would.
    #[tokio::test]
    async fn a_computer_that_leaves_mid_answer_ends_it() {
        let relays = relays();
        let mut laptop = relays.events.subscribe("laptop", None);
        let asked = tokio::spawn(relays.post("laptop", post()));
        let id = wanted(&mut laptop).await;
        relays.take("laptop", &id).unwrap();
        let head = RelayPart::Head {
            status: 200,
            headers: Vec::new(),
        };
        relays.part("laptop", &id, head);
        let response = asked.await.unwrap().unwrap();
        drop(laptop);
        assert!(!relays.events.here("laptop"));
        assert!(response.text().await.is_err());
    }
}
