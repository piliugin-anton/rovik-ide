//! What the crate's tests share: no daemon at all, and a daemon of the
//! tests' own, which answers every call and keeps the requests.

use crate::client::Identity;
use crate::connector::{Connecting, Connector, Stream};
use futures_util::{SinkExt, StreamExt};
use rovik_protocol::agent::AgentStatus;
use rovik_protocol::host::HostInfo;
use rovik_protocol::{Frame, Request, Response, Secret, PROTOCOL};
use std::sync::{Arc, Mutex};
use tokio_tungstenite::tungstenite::Message;

/// No daemon there.
pub struct Nowhere;

impl Connector for Nowhere {
    fn connect(&self) -> Connecting<'_> {
        Box::pin(async { Err(std::io::ErrorKind::ConnectionRefused.into()) })
    }
}

pub fn identity(host: &str) -> Identity {
    Identity {
        host: host.into(),
        computer: "laptop".into(),
        token: Secret("token".into()),
        client: "rovik-client tests".into(),
    }
}

/// A daemon on the other end of each connection: it welcomes, subscribes,
/// and answers each call as a daemon would when all goes well, keeping
/// the requests in order.
#[derive(Clone, Default)]
pub struct Fake {
    pub requests: Arc<Mutex<Vec<Request>>>,
    /// What it answers `HostInfo` with, once set.
    pub info: Arc<Mutex<Option<HostInfo>>>,
    /// What its welcome says it does: nothing unless told.
    pub features: Arc<Mutex<Vec<String>>>,
}

impl Connector for Fake {
    fn connect(&self) -> Connecting<'_> {
        let (requests, info) = (self.requests.clone(), self.info.clone());
        let features = self.features.lock().unwrap().clone();
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(1 << 16);
            tokio::spawn(serve(server, requests, info, features));
            Ok(Box::new(client) as Stream)
        })
    }
}

async fn serve(
    io: tokio::io::DuplexStream,
    requests: Arc<Mutex<Vec<Request>>>,
    info: Arc<Mutex<Option<HostInfo>>>,
    features: Vec<String>,
) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(io).await else {
        return;
    };
    let send = |frame: &Frame| Message::Text(serde_json::to_string(frame).unwrap().into());
    while let Some(Ok(Message::Text(text))) = socket.next().await {
        let answer = match serde_json::from_str::<Frame>(&text) {
            Ok(Frame::Hello { .. }) => Frame::Welcome {
                protocol: PROTOCOL,
                min_protocol: PROTOCOL,
                daemon: "fake".into(),
                boot: "boot".into(),
                features: features.clone(),
            },
            Ok(Frame::Subscribe { .. }) => Frame::Subscribed {
                replayed: 0,
                complete: true,
            },
            Ok(Frame::Call { id, req }) => {
                let info = info.lock().unwrap().clone();
                let res = Ok(match (&req, info) {
                    (Request::HostInfo, Some(info)) => Response::HostInfo(info),
                    _ => answer(&req),
                });
                requests.lock().unwrap().push(req);
                Frame::Reply { id, res }
            }
            _ => continue,
        };
        if socket.send(send(&answer)).await.is_err() {
            return;
        }
    }
}

fn answer(request: &Request) -> Response {
    match request {
        Request::AgentStart(_) => Response::RunId(Some("run-1".into())),
        Request::AgentStatus => Response::AgentStatus(AgentStatus::default()),
        Request::AgentStop { .. }
        | Request::AgentPause { .. }
        | Request::AgentResolveApproval { .. }
        | Request::AgentAnswerQuestions { .. } => Response::Bool(true),
        Request::DeliveryTest { remote } => {
            Response::DeliveryTested(rovik_protocol::autopilot::DeliveryTest {
                remote: remote.clone(),
                branch: "rovik/test-20261017-095301".into(),
                ..Default::default()
            })
        }
        _ => Response::Done,
    }
}
