//! The desktop's API keys, in the keychain (H06). `ai-settings.json` keeps
//! the rest of the AI settings, and for each provider whose key is saved,
//! where it is (`apiKeyIn`), its `apiKey` left empty. A version 2 file,
//! keys in it, is moved over at the first load. The frontend gets the keys
//! filled in and saves them as before, so it doesn't change.
//!
//! Where no keychain answers, the store is the encrypted file in its place
//! (H01). While that file is locked, before its passphrase was given this
//! session, keys can't be read or saved there: those already in the
//! settings file stay in it, as before H06, until a load with the store
//! unlocked moves them over.

use crate::secrets::{SecretName, SecretStore, StoreKind};
use rovik_protocol::Secret;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The settings file's format: version 3 keeps keys out of it.
pub const VERSION: u64 = 3;

/// What a missing settings file reads as.
const EMPTY: &str = r#"{"profiles":[],"activeProfileId":null}"#;

/// The AI settings file, and the store its keys are in.
pub struct ProviderKeys {
    path: PathBuf,
    store: Arc<SecretStore>,
    /// The providers whose saved key couldn't be read at the last load (a
    /// locked file, a keychain that said no): an empty key the frontend
    /// saves for one means "unknown", not "cleared", and the key is kept.
    /// Held through each load and save, so a load that moves keys can't
    /// write over a save.
    unread: Mutex<Vec<String>>,
}

impl ProviderKeys {
    pub fn new(path: PathBuf, store: Arc<SecretStore>) -> ProviderKeys {
        ProviderKeys {
            path,
            store,
            unread: Mutex::new(Vec::new()),
        }
    }

    /// The settings as the frontend reads them, each saved key filled in.
    /// Keys still in the file move to the store first, when it can take
    /// them.
    pub fn load(&self) -> Result<String, String> {
        let mut unread = self.unread.lock().unwrap_or_else(|e| e.into_inner());
        if !self.path.exists() {
            return Ok(EMPTY.into());
        }
        let text = std::fs::read_to_string(&self.path).map_err(|e| e.to_string())?;
        // The frontend says it can't read it, and doesn't save over it.
        let Ok(mut settings) = serde_json::from_str::<Value>(&text) else {
            return Ok(text);
        };
        if self.usable() && self.move_keys(&mut settings) {
            self.write(&mut settings)?;
        }
        unread.clear();
        for provider in providers(&mut settings) {
            let (Some(id), Some(_)) = (id(provider), provider.remove("apiKeyIn")) else {
                continue;
            };
            match self.store.get(&name(&id)) {
                Ok(key) => {
                    provider.insert("apiKey".into(), Value::String(key.0));
                }
                Err(e) => {
                    eprintln!("ai settings: the API key of {id} couldn't be read: {e}");
                    unread.push(id);
                }
            }
        }
        Ok(settings.to_string())
    }

    /// Saves the settings the frontend sends, keys and all: each key goes
    /// to the store, and the file keeps where it is. A provider that's gone
    /// or whose key was cleared has its key forgotten.
    pub fn save(&self, json: &str) -> Result<(), String> {
        let unread = self.unread.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let saved = self.saved_keys();
        let usable = self.usable();
        let mut kept = Vec::new();
        for provider in providers(&mut settings) {
            let Some(id) = id(provider) else {
                continue;
            };
            provider.remove("apiKeyIn");
            let key = match provider.get("apiKey") {
                Some(Value::String(key)) => key.clone(),
                _ => String::new(),
            };
            match (key.is_empty(), saved.get(&id)) {
                // A key that couldn't be read at the load stays where it is.
                (true, Some(place)) if unread.contains(&id) || !usable => {
                    provider.insert("apiKeyIn".into(), place.clone());
                    kept.push(id);
                }
                (true, _) => {}
                (false, _) if usable => match self.store.set(&name(&id), &Secret(key)) {
                    Ok(()) => {
                        provider.insert("apiKey".into(), Value::String(String::new()));
                        provider.insert("apiKeyIn".into(), self.place());
                        kept.push(id);
                    }
                    // It stays in the file, as before H06.
                    Err(e) => eprintln!("ai settings: the API key of {id} couldn't be saved: {e}"),
                },
                // A locked store: in the file, as before H06.
                (false, _) => {}
            }
        }
        if usable {
            for id in saved.keys().filter(|id| !kept.contains(id)) {
                if let Err(e) = self.store.delete(&name(id)) {
                    eprintln!("ai settings: the API key of {id} couldn't be forgotten: {e}");
                }
            }
        }
        self.write(&mut settings)
    }

    /// Whether keys can be read and saved now.
    fn usable(&self) -> bool {
        match self.store.kind() {
            StoreKind::Keychain => true,
            StoreKind::File { unlocked } => unlocked,
        }
    }

    /// Where a key saved now is, as the file says.
    fn place(&self) -> Value {
        Value::String(
            match self.store.kind() {
                StoreKind::Keychain => "keychain",
                StoreKind::File { .. } => "secrets-file",
            }
            .into(),
        )
    }

    /// Moves the keys still in `settings` to the store. Whether any moved.
    fn move_keys(&self, settings: &mut Value) -> bool {
        let place = self.place();
        let mut moved = false;
        for provider in providers(settings) {
            let (Some(id), Some(Value::String(key))) = (id(provider), provider.get("apiKey"))
            else {
                continue;
            };
            if key.is_empty() {
                continue;
            }
            match self.store.set(&name(&id), &Secret(key.clone())) {
                Ok(()) => {
                    provider.insert("apiKey".into(), Value::String(String::new()));
                    provider.insert("apiKeyIn".into(), place.clone());
                    moved = true;
                }
                Err(e) => eprintln!("ai settings: the API key of {id} couldn't be moved: {e}"),
            }
        }
        moved
    }

    /// The providers whose key the file says is saved, and where.
    fn saved_keys(&self) -> BTreeMap<String, Value> {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let Ok(mut settings) = serde_json::from_str::<Value>(&text) else {
            return BTreeMap::new();
        };
        let mut saved = BTreeMap::new();
        for provider in providers(&mut settings) {
            if let (Some(id), Some(place)) = (id(provider), provider.get("apiKeyIn")) {
                saved.insert(id, place.clone());
            }
        }
        saved
    }

    fn write(&self, settings: &mut Value) -> Result<(), String> {
        if let Some(object) = settings.as_object_mut() {
            object.insert("version".into(), VERSION.into());
        }
        rovik_core::atomic_file::write_atomic_private(&self.path, settings.to_string().as_bytes())
            .map_err(|e| e.to_string())
    }
}

fn providers(settings: &mut Value) -> impl Iterator<Item = &mut Map<String, Value>> {
    settings
        .get_mut("providers")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
}

fn id(provider: &Map<String, Value>) -> Option<String> {
    provider
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn name(provider: &str) -> SecretName {
    SecretName::ApiKey {
        provider: provider.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{Kdf, SecretError};
    use serde_json::json;

    /// Cheap enough for tests.
    const QUICK: Kdf = Kdf {
        memory_kib: 256,
        iterations: 1,
        parallelism: 1,
    };

    /// A version 2 file as the app wrote it: two providers with keys, one
    /// without.
    fn version_2() -> Value {
        json!({
            "version": 2,
            "providers": [
                {
                    "id": "openai",
                    "name": "OpenAI",
                    "baseUrl": "https://api.openai.com/v1",
                    "apiKey": "sk-openai-secret",
                    "stream": true,
                    "models": [{ "id": "gpt-5", "enabled": true }]
                },
                {
                    "id": "local",
                    "name": "Ollama",
                    "baseUrl": "http://127.0.0.1:11434/v1",
                    "apiKey": "",
                    "stream": true,
                    "models": []
                },
                {
                    "id": "router",
                    "name": "OpenRouter",
                    "baseUrl": "https://openrouter.ai/api/v1",
                    "apiKey": "sk-or-secret",
                    "stream": false,
                    "models": []
                }
            ],
            "active": { "providerId": "openai", "modelId": "gpt-5" },
            "permissions": { "mode": "ask" },
            "subagents": { "enabled": false }
        })
    }

    struct Setup {
        _dir: tempfile::TempDir,
        settings: PathBuf,
        secrets: PathBuf,
        keys: ProviderKeys,
        store: Arc<SecretStore>,
    }

    /// The encrypted file in place of the keychain, unlocked or not.
    fn setup(unlocked: bool) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("ai-settings.json");
        let secrets = dir.path().join("secrets.json");
        std::fs::write(&settings, version_2().to_string()).unwrap();
        let store = Arc::new(SecretStore::file(secrets.clone(), QUICK));
        if unlocked {
            store.unlock(&Secret("passphrase".into())).unwrap();
        }
        let keys = ProviderKeys::new(settings.clone(), Arc::clone(&store));
        Setup {
            _dir: dir,
            settings,
            secrets,
            keys,
            store,
        }
    }

    fn file(setup: &Setup) -> (String, Value) {
        let text = std::fs::read_to_string(&setup.settings).unwrap();
        let value = serde_json::from_str(&text).unwrap();
        (text, value)
    }

    fn key_of(setup: &Setup, provider: &str) -> Result<String, SecretError> {
        setup.store.get(&name(provider)).map(|s| s.0)
    }

    #[test]
    fn a_version_2_file_moves_its_keys_out_at_the_first_load() {
        let setup = setup(true);
        let loaded: Value = serde_json::from_str(&setup.keys.load().unwrap()).unwrap();
        let mut expected = version_2();
        expected["version"] = json!(VERSION);
        assert_eq!(loaded, expected, "the frontend sees what it saw");

        let (text, saved) = file(&setup);
        assert!(!text.contains("sk-openai-secret"), "{text}");
        assert!(!text.contains("sk-or-secret"), "{text}");
        assert_eq!(saved["version"], json!(3));
        assert_eq!(saved["providers"][0]["apiKey"], json!(""));
        assert_eq!(saved["providers"][0]["apiKeyIn"], json!("secrets-file"));
        assert_eq!(
            saved["providers"][1].get("apiKeyIn"),
            None,
            "no key, nothing saved"
        );
        assert_eq!(
            saved["providers"][0]["models"],
            version_2()["providers"][0]["models"]
        );
        assert_eq!(saved["permissions"], json!({ "mode": "ask" }));
        assert_eq!(key_of(&setup, "openai").unwrap(), "sk-openai-secret");
        assert_eq!(key_of(&setup, "router").unwrap(), "sk-or-secret");
        let sealed = std::fs::read_to_string(&setup.secrets).unwrap();
        assert!(
            !sealed.contains("sk-openai-secret"),
            "the fallback is encrypted"
        );

        let again: Value = serde_json::from_str(&setup.keys.load().unwrap()).unwrap();
        assert_eq!(again, loaded, "a second load reads them back");
    }

    #[test]
    fn saving_puts_keys_in_the_store_and_forgets_removed_or_cleared_ones() {
        let setup = setup(true);
        let mut settings: Value = serde_json::from_str(&setup.keys.load().unwrap()).unwrap();
        settings["providers"][0]["apiKey"] = json!("sk-openai-new");
        settings["providers"][1]["apiKey"] = json!("sk-local-new");
        let providers = settings["providers"].as_array_mut().unwrap();
        providers.remove(2);
        setup.keys.save(&settings.to_string()).unwrap();

        let (text, saved) = file(&setup);
        assert!(!text.contains("-new"), "{text}");
        assert_eq!(saved["providers"].as_array().unwrap().len(), 2);
        assert_eq!(saved["providers"][1]["apiKeyIn"], json!("secrets-file"));
        assert_eq!(key_of(&setup, "openai").unwrap(), "sk-openai-new");
        assert_eq!(key_of(&setup, "local").unwrap(), "sk-local-new");
        assert!(
            matches!(key_of(&setup, "router"), Err(SecretError::Missing(_))),
            "a removed provider's key is forgotten"
        );

        settings["providers"][0]["apiKey"] = json!("");
        setup.keys.save(&settings.to_string()).unwrap();
        assert!(matches!(
            key_of(&setup, "openai"),
            Err(SecretError::Missing(_))
        ));
        let (_, saved) = file(&setup);
        assert_eq!(saved["providers"][0].get("apiKeyIn"), None);
        let loaded: Value = serde_json::from_str(&setup.keys.load().unwrap()).unwrap();
        assert_eq!(loaded["providers"][0]["apiKey"], json!(""));
        assert_eq!(loaded["providers"][1]["apiKey"], json!("sk-local-new"));
    }

    #[test]
    fn a_locked_fallback_leaves_keys_where_they_are_and_loses_none() {
        // Moved to the encrypted file in an earlier session...
        let setup = setup(true);
        setup.keys.load().unwrap();
        // ...and now its passphrase isn't given yet.
        let locked = Arc::new(SecretStore::file(setup.secrets.clone(), QUICK));
        let keys = ProviderKeys::new(setup.settings.clone(), Arc::clone(&locked));
        let mut settings: Value = serde_json::from_str(&keys.load().unwrap()).unwrap();
        assert_eq!(
            settings["providers"][0]["apiKey"],
            json!(""),
            "can't be read"
        );
        assert_eq!(settings["providers"][0].get("apiKeyIn"), None);

        // A save meanwhile keeps the unread keys, and a new key goes in the
        // file, as before H06.
        settings["providers"][1]["apiKey"] = json!("sk-local-typed");
        keys.save(&settings.to_string()).unwrap();
        let (text, saved) = file(&setup);
        assert_eq!(saved["providers"][0]["apiKeyIn"], json!("secrets-file"));
        assert_eq!(saved["providers"][2]["apiKeyIn"], json!("secrets-file"));
        assert!(text.contains("sk-local-typed"), "no store to take it");

        // Once unlocked, the next load moves the new key and reads them all.
        locked.unlock(&Secret("passphrase".into())).unwrap();
        let loaded: Value = serde_json::from_str(&keys.load().unwrap()).unwrap();
        assert_eq!(loaded["providers"][0]["apiKey"], json!("sk-openai-secret"));
        assert_eq!(loaded["providers"][1]["apiKey"], json!("sk-local-typed"));
        assert_eq!(loaded["providers"][2]["apiKey"], json!("sk-or-secret"));
        let (text, _) = file(&setup);
        assert!(!text.contains("sk-"), "{text}");
    }

    #[test]
    fn a_version_2_file_with_a_locked_fallback_is_read_as_it_is() {
        let setup = setup(false);
        let loaded: Value = serde_json::from_str(&setup.keys.load().unwrap()).unwrap();
        assert_eq!(loaded, version_2());
        let (_, saved) = file(&setup);
        assert_eq!(saved, version_2(), "not written");
    }

    #[test]
    fn a_missing_or_unreadable_file_reads_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai-settings.json");
        let store = Arc::new(SecretStore::file(dir.path().join("secrets.json"), QUICK));
        let keys = ProviderKeys::new(path.clone(), store);
        assert_eq!(keys.load().unwrap(), EMPTY);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(
            keys.load().unwrap(),
            "{ not json",
            "the frontend reports it"
        );
    }
}
