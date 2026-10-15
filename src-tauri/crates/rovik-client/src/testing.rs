//! What the crate's tests share.

use crate::client::Identity;
use crate::connector::{Connecting, Connector};
use rovik_protocol::Secret;

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
