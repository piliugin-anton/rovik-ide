//! keys.json: the providers' API keys, the one file the daemon keeps them
//! in (D11, plan rule 9). Each start brings its keys and refreshes them, by
//! provider; a configuration that comes without them, as the app sends it,
//! gets them from here. A key goes only with the base URL it came with, so
//! a model pointed at another server never gets this one's key.

use rovik_protocol::agent::ModelProfile;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Keys {
    /// By provider: the part of a model's id before `::`.
    #[serde(default)]
    providers: BTreeMap<String, Key>,
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
    /// older one. True when one was new or changed.
    pub fn take<'a>(&mut self, models: impl IntoIterator<Item = &'a ModelProfile>) -> bool {
        let mut changed = false;
        for model in models.into_iter().filter(|m| !m.api_key.is_empty()) {
            let key = Key {
                base_url: model.base_url.clone(),
                api_key: model.api_key.clone(),
            };
            let old = self
                .providers
                .insert(provider(model).to_string(), key.clone());
            changed |= old.as_ref() != Some(&key);
        }
        changed
    }

    /// The providers with a key, and the base URL each key goes with.
    pub fn providers(&self) -> Vec<(String, String)> {
        let keys = self.providers.iter();
        keys.map(|(provider, key)| (provider.clone(), key.base_url.clone()))
            .collect()
    }

    /// Forgets `provider`'s key, or every key. How many went.
    pub fn remove(&mut self, provider: Option<&str>) -> usize {
        match provider {
            Some(provider) => usize::from(self.providers.remove(provider).is_some()),
            None => std::mem::take(&mut self.providers).len(),
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
