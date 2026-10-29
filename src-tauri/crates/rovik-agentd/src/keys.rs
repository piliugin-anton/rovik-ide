//! keys.json: the providers' API keys, the one file the daemon keeps them
//! in (D11, plan rule 9). Each start brings its keys and refreshes them, by
//! provider; a configuration that comes without them, as the app sends it,
//! gets them from here. A key goes only with the base URL it came with, so
//! a model pointed at another server never gets this one's key.

use rovik_protocol::agent::ModelProfile;
use rovik_protocol::Secret;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Keys {
    /// By provider: the part of a model's id before `::`.
    #[serde(default)]
    providers: BTreeMap<String, Key>,
    /// The MCP servers' saved secrets: by server id, then by key
    /// (`env/NAME`, `header/Name`). Brought by a start, like the keys.
    #[serde(default)]
    mcp: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Key {
    base_url: String,
    api_key: String,
}

impl Keys {
    /// The file at `path`; none yet is no keys.
    pub fn read(path: &Path) -> Result<Keys, String> {
        match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Keys::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Written whole, readable by this user alone.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        rovik_core::atomic_file::write_atomic_private(path, &bytes)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Keeps the keys these models carry, each replacing its provider's
    /// older one.
    pub fn take<'a>(&mut self, models: impl IntoIterator<Item = &'a ModelProfile>) {
        for model in models.into_iter().filter(|m| !m.api_key.is_empty()) {
            let key = Key {
                base_url: model.base_url.clone(),
                api_key: model.api_key.clone(),
            };
            self.providers.insert(provider(model).to_string(), key);
        }
    }

    /// The providers with a key, and the base URL each key goes with.
    pub fn providers(&self) -> Vec<(String, String)> {
        let keys = self.providers.iter();
        keys.map(|(provider, key)| (provider.clone(), key.base_url.clone()))
            .collect()
    }

    /// Keeps the MCP secrets a start brought, each server's replacing all
    /// its older ones; a server brought with none loses its.
    pub fn take_mcp(&mut self, secrets: &BTreeMap<String, BTreeMap<String, Secret>>) {
        for (id, values) in secrets {
            let values: BTreeMap<String, String> = values
                .iter()
                .map(|(key, value)| (key.clone(), value.0.clone()))
                .collect();
            if values.is_empty() {
                self.mcp.remove(id);
            } else {
                self.mcp.insert(id.clone(), values);
            }
        }
    }

    /// Forgets the MCP secrets `wanted` doesn't name, which has the secret
    /// keys of each server some computer has on (Q11): a server removed or
    /// turned off loses all its secrets, and one without a secret field
    /// any more loses that field's value. A project's variables stay.
    pub fn keep_mcp(&mut self, wanted: &BTreeMap<String, BTreeSet<String>>) {
        self.mcp.retain(|id, values| {
            // A project's variables are no server's: each start of its
            // chats brings them all, or none to clear them (Q12).
            if id.starts_with(rovik_protocol::mcp::PROJECT_SERVERS) {
                return true;
            }
            let Some(keys) = wanted.get(id) else {
                return false;
            };
            values.retain(|key, _| keys.contains(key));
            !values.is_empty()
        });
    }

    /// The ids of the MCP servers whose saved secrets `other` has otherwise.
    pub fn mcp_changes(&self, other: &Keys) -> Vec<String> {
        let ids: BTreeSet<&String> = self.mcp.keys().chain(other.mcp.keys()).collect();
        ids.into_iter()
            .filter(|id| self.mcp.get(*id) != other.mcp.get(*id))
            .cloned()
            .collect()
    }

    /// The saved secret `key` of MCP server `server`.
    pub fn mcp_value(&self, server: &str, key: &str) -> Option<String> {
        self.mcp.get(server)?.get(key).cloned()
    }

    /// How many MCP servers have saved secrets here.
    pub fn mcp_servers(&self) -> usize {
        self.mcp.len()
    }

    /// Forgets `provider`'s key, or every key and every MCP secret. How
    /// many went: each MCP server counts as one.
    pub fn remove(&mut self, provider: Option<&str>) -> usize {
        match provider {
            Some(provider) => usize::from(self.providers.remove(provider).is_some()),
            None => std::mem::take(&mut self.providers).len() + std::mem::take(&mut self.mcp).len(),
        }
    }

    /// Gives each model without a key its provider's, when it came with
    /// the same base URL.
    pub fn give<'a>(&self, models: impl IntoIterator<Item = &'a mut ModelProfile>) {
        for model in models.into_iter().filter(|m| m.api_key.is_empty()) {
            if let Some(key) = self.providers.get(provider(model)) {
                if key.base_url == model.base_url {
                    model.api_key = key.api_key.clone();
                }
            }
        }
    }
}

/// A model's provider: the part of its id before `::`.
pub fn provider(model: &ModelProfile) -> &str {
    model.id.split("::").next().unwrap_or(&model.id)
}
