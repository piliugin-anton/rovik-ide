//! The MCP servers' sign-ins on this computer, in the keychain (or the
//! encrypted file in its place): each server's grant, and the OAuth clients
//! Rovik registered, by issuer (mcp-plan-2.md, Q14). The core reads and
//! refreshes them through [`KeychainGrants`]; a sign-in made here registers
//! through [`KeychainRegistrations`].

use crate::secrets::{SecretError, SecretName, SecretStore};
use rovik_core::mcp::oauth::{Client, GrantStore, McpGrant, Registrations};
use rovik_protocol::Secret;
use std::sync::Arc;

/// The servers' grants, as JSON, each under its server's id.
pub struct KeychainGrants(pub Arc<SecretStore>);

impl GrantStore for KeychainGrants {
    fn get(&self, server: &str) -> Result<Option<McpGrant>, String> {
        let name = SecretName::McpGrant {
            server: server.to_string(),
        };
        match self.0.get(&name) {
            Ok(json) => serde_json::from_str(&json.0).map(Some).map_err(|e| {
                format!("The server's saved sign-in can't be read ({e}): sign in again.")
            }),
            Err(SecretError::Missing(_)) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn put(&self, server: &str, grant: &McpGrant) -> Result<(), String> {
        let name = SecretName::McpGrant {
            server: server.to_string(),
        };
        let json = serde_json::to_string(grant).map_err(|e| e.to_string())?;
        self.0.set(&name, &Secret(json)).map_err(|e| e.to_string())
    }

    fn forget(&self, server: &str) -> Result<(), String> {
        let name = SecretName::McpGrant {
            server: server.to_string(),
        };
        self.0.delete(&name).map_err(|e| e.to_string())
    }
}

/// The clients Rovik registered, as JSON, each under its issuer.
pub struct KeychainRegistrations(pub Arc<SecretStore>);

impl Registrations for KeychainRegistrations {
    fn get(&self, issuer: &str) -> Option<Client> {
        let name = SecretName::McpClient {
            issuer: issuer.to_string(),
        };
        match self.0.get(&name) {
            Ok(json) => serde_json::from_str(&json.0).ok(),
            Err(SecretError::Missing(_)) => None,
            Err(e) => {
                log::info!(
                    "Rovik's registration with an MCP authorization server wasn't read: {e}"
                );
                None
            }
        }
    }

    fn put(&self, issuer: &str, client: &Client) -> Result<(), String> {
        let name = SecretName::McpClient {
            issuer: issuer.to_string(),
        };
        let json = serde_json::to_string(client).map_err(|e| e.to_string())?;
        self.0.set(&name, &Secret(json)).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::Kdf;

    fn store(dir: &tempfile::TempDir) -> Arc<SecretStore> {
        let quick = Kdf {
            memory_kib: 256,
            iterations: 1,
            parallelism: 1,
        };
        let store = SecretStore::file(dir.path().join("secrets.json"), quick);
        store.unlock(&Secret("pass".into())).unwrap();
        Arc::new(store)
    }

    fn grant(access: &str) -> McpGrant {
        McpGrant {
            issuer: "https://auth.example.com".into(),
            token_endpoint: "https://auth.example.com/token".into(),
            client_id: "c1".into(),
            client_secret: Some(Secret("s3cret".into())),
            auth_method: "client_secret_basic".into(),
            resource: "https://mcp.example.com/mcp".into(),
            scope: "read".into(),
            access_token: Secret(access.into()),
            refresh_token: Some(Secret("rt".into())),
            expires_at: Some(1),
            wants: None,
        }
    }

    /// Q14: a server's sign-in is kept whole by its id, apart from the
    /// others', until it's forgotten; and a registration by its issuer.
    #[test]
    fn sign_ins_are_kept_by_server_and_registrations_by_issuer() {
        let dir = tempfile::tempdir().unwrap();
        let grants = KeychainGrants(store(&dir));
        assert_eq!(grants.get("s1"), Ok(None));
        grants.put("s1", &grant("at-1")).unwrap();
        grants.put("s2", &grant("at-2")).unwrap();
        assert_eq!(grants.get("s1"), Ok(Some(grant("at-1"))));
        grants.forget("s1").unwrap();
        assert_eq!(grants.get("s1"), Ok(None));
        assert_eq!(grants.get("s2"), Ok(Some(grant("at-2"))));
        grants.forget("s1").unwrap();

        let registrations = KeychainRegistrations(grants.0.clone());
        let client = Client {
            client_id: "registered-1".into(),
            client_secret: None,
            auth_method: "none".into(),
            redirect_uri: Some("http://127.0.0.1:4000/callback".into()),
        };
        registrations
            .put("https://auth.example.com", &client)
            .unwrap();
        assert_eq!(registrations.get("https://auth.example.com"), Some(client));
        assert_eq!(registrations.get("https://other.example.com"), None);
    }
}
