//! Secrets the app keeps for its hosts and its providers (design §6, H01):
//! a host's SSH password and key passphrase, the token that names this
//! computer to its daemon, and the providers' API keys (H06). They live in
//! the OS keychain (`keyring`: the macOS Keychain, the Windows Credential
//! Manager, Secret Service on Linux), never in a settings file, which only
//! names what to look up. Where no keychain answers, a Linux desktop
//! without Secret Service, they go in a file encrypted with a key derived
//! from a passphrase (argon2id, XChaCha20-Poly1305). The passphrase is
//! asked once a session, and `SecretStore::kind` lets the interface say
//! where secrets are. Nothing here logs a secret (plan rule 9).

use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rovik_protocol::Secret;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use zeroize::{Zeroize, Zeroizing};

/// The keychain's service for every secret of the app.
const SERVICE: &str = "rovik-ide";

/// The encrypted file's format.
const FILE_VERSION: u32 = 1;

/// What a secret is, and whose: a host by its id in `cloud-hosts.json`, a
/// provider by its id in the AI settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretName {
    /// A host's SSH password.
    Password { host: String },
    /// The passphrase of a host's SSH key file.
    KeyPassphrase { host: String },
    /// The token that names this computer to a host's daemon (D5).
    Token { host: String },
    /// A provider's API key (H06).
    ApiKey { provider: String },
    /// The password of a proxy saved in Settings → Network, by its id.
    ProxyPassword { proxy: String },
}

impl SecretName {
    /// Its account in the keychain, and its key in the encrypted file.
    fn account(&self) -> String {
        match self {
            SecretName::Password { host } => format!("host/{host}/password"),
            SecretName::KeyPassphrase { host } => format!("host/{host}/passphrase"),
            SecretName::Token { host } => format!("host/{host}/token"),
            SecretName::ApiKey { provider } => format!("provider/{provider}/api-key"),
            SecretName::ProxyPassword { proxy } => format!("network/proxy/{proxy}/password"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SecretError {
    /// It isn't saved.
    Missing(SecretName),
    /// The encrypted file waits for its passphrase.
    Locked,
    /// The passphrase doesn't open the encrypted file.
    WrongPassphrase,
    /// The keychain or the file failed, and why.
    Failed(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretError::Missing(SecretName::Password { .. }) => {
                f.write_str("No SSH password is saved for this host: enter it again.")
            }
            SecretError::Missing(SecretName::KeyPassphrase { .. }) => {
                f.write_str("No passphrase is saved for this host's SSH key: enter it again.")
            }
            SecretError::Missing(SecretName::Token { .. }) => {
                f.write_str("This computer has no token for this host: deploy Rovik to it again.")
            }
            SecretError::Missing(SecretName::ApiKey { provider }) => write!(
                f,
                "No API key is saved for {provider}: enter it again in the settings."
            ),
            SecretError::Missing(SecretName::ProxyPassword { .. }) => f.write_str(
                "No password is saved for the proxy: enter it again in Settings → Network.",
            ),
            SecretError::Locked => f.write_str(
                "The saved passwords and keys are locked: enter their passphrase first.",
            ),
            SecretError::WrongPassphrase => {
                f.write_str("That passphrase doesn't open the saved passwords and keys.")
            }
            SecretError::Failed(why) => write!(f, "The saved passwords and keys: {why}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// Where the secrets are, for the interface to say (design §6).
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StoreKind {
    /// The OS keychain.
    Keychain,
    /// The encrypted file in its place, and whether its passphrase was
    /// given this session.
    File { unlocked: bool },
}

/// How the encrypted file's key comes from its passphrase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kdf {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Kdf {
    /// What a new file takes: 64 MiB and three passes, a fraction of a
    /// second on a desktop, once a session.
    pub const DEFAULT: Kdf = Kdf {
        memory_kib: 64 * 1024,
        iterations: 3,
        parallelism: 1,
    };
}

pub struct SecretStore {
    /// Chosen on first use (`backend`): asking the keychain may take a
    /// moment, or ask the user, which the app's start mustn't wait for.
    backend: std::sync::OnceLock<Backend>,
    /// The encrypted file in the keychain's place, when none answers.
    fallback: PathBuf,
}

enum Backend {
    Keychain,
    File(FileStore),
}

impl SecretStore {
    /// The keychain if it answers, else the encrypted file at `fallback`;
    /// asked on first use.
    pub fn open(fallback: PathBuf) -> SecretStore {
        SecretStore {
            backend: std::sync::OnceLock::new(),
            fallback,
        }
    }

    /// The encrypted file at `path`, whatever the keychain: a new one
    /// derives its key as `kdf` says, an existing one as it says.
    pub fn file(path: PathBuf, kdf: Kdf) -> SecretStore {
        SecretStore {
            backend: std::sync::OnceLock::from(Backend::File(FileStore {
                path: path.clone(),
                kdf,
                opened: Mutex::new(None),
            })),
            fallback: path,
        }
    }

    /// Where the secrets are. An encrypted file once in use stays in use:
    /// its secrets would be out of reach if the keychain answered on a
    /// later start. The keychain is asked again when it doesn't answer at
    /// first: a Secret Service that is just starting takes a moment, and
    /// falling back for one start would put that start's secrets in a
    /// file the next start never reads.
    fn backend(&self) -> &Backend {
        self.backend.get_or_init(|| {
            if !self.fallback.is_file() {
                for attempt in 0..3 {
                    if keychain_answers() {
                        return Backend::Keychain;
                    }
                    if attempt < 2 {
                        std::thread::sleep(std::time::Duration::from_millis(400));
                    }
                }
            }
            Backend::File(FileStore {
                path: self.fallback.clone(),
                kdf: Kdf::DEFAULT,
                opened: Mutex::new(None),
            })
        })
    }

    pub fn kind(&self) -> StoreKind {
        match self.backend() {
            Backend::Keychain => StoreKind::Keychain,
            Backend::File(file) => StoreKind::File {
                unlocked: file.lock().is_some(),
            },
        }
    }

    /// The encrypted file's passphrase, for this session: a new file is
    /// made with it, an existing one must open with it. The keychain needs
    /// none.
    pub fn unlock(&self, passphrase: &Secret) -> Result<(), SecretError> {
        match self.backend() {
            Backend::Keychain => Ok(()),
            Backend::File(file) => file.unlock(passphrase),
        }
    }

    pub fn get(&self, name: &SecretName) -> Result<Secret, SecretError> {
        match self.backend() {
            Backend::Keychain => {
                let entry = keychain_entry(name)?;
                entry
                    .get_password()
                    .map(Secret)
                    .map_err(|e| keychain_error(e, name))
            }
            Backend::File(file) => {
                let secrets = file.read()?;
                let value = secrets.0.get(&name.account()).cloned();
                value.map(Secret).ok_or(SecretError::Missing(name.clone()))
            }
        }
    }

    pub fn set(&self, name: &SecretName, value: &Secret) -> Result<(), SecretError> {
        match self.backend() {
            Backend::Keychain => {
                let entry = keychain_entry(name)?;
                entry
                    .set_password(&value.0)
                    .map_err(|e| keychain_error(e, name))
            }
            Backend::File(file) => file.change(|secrets| {
                secrets.0.insert(name.account(), value.0.clone());
            }),
        }
    }

    /// Forgets it: done already when it wasn't saved.
    pub fn delete(&self, name: &SecretName) -> Result<(), SecretError> {
        match self.backend() {
            Backend::Keychain => {
                let entry = keychain_entry(name)?;
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(e) => Err(keychain_error(e, name)),
                }
            }
            Backend::File(file) => file.change(|secrets| {
                secrets.0.remove(&name.account());
            }),
        }
    }
}

/// Whether the OS keychain answers: a missing entry is an answer, a
/// platform that has no store isn't.
fn keychain_answers() -> bool {
    let Ok(entry) = keyring::Entry::new(SERVICE, "probe") else {
        return false;
    };
    !matches!(
        entry.get_password(),
        Err(keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_))
    )
}

fn keychain_entry(name: &SecretName) -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(SERVICE, &name.account()).map_err(|e| keychain_error(e, name))
}

fn keychain_error(e: keyring::Error, name: &SecretName) -> SecretError {
    match e {
        keyring::Error::NoEntry => SecretError::Missing(name.clone()),
        e => SecretError::Failed(format!("the keychain: {e}")),
    }
}

/// The encrypted file, and its key once its passphrase was given.
struct FileStore {
    path: PathBuf,
    kdf: Kdf,
    opened: Mutex<Option<Opened>>,
}

/// What the passphrase opened: the key, with the salt and the settings it
/// came from, which every write keeps.
struct Opened {
    key: Zeroizing<[u8; 32]>,
    salt: Vec<u8>,
    kdf: Kdf,
}

/// The file as it's written: its key's salt and settings in the clear, the
/// secrets, a JSON map by account, sealed under a new nonce at each write.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sealed {
    version: u32,
    salt: String,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
    nonce: String,
    data: String,
}

/// The secrets by account, wiped from memory when dropped.
#[derive(Default, Serialize, Deserialize)]
#[serde(transparent)]
struct Secrets(BTreeMap<String, String>);

impl Drop for Secrets {
    fn drop(&mut self) {
        self.0.values_mut().for_each(Zeroize::zeroize);
    }
}

impl FileStore {
    fn lock(&self) -> MutexGuard<'_, Option<Opened>> {
        self.opened.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn unlock(&self, passphrase: &Secret) -> Result<(), SecretError> {
        let mut opened = self.lock();
        match self.sealed()? {
            Some(sealed) => {
                let kdf = Kdf {
                    memory_kib: sealed.memory_kib,
                    iterations: sealed.iterations,
                    parallelism: sealed.parallelism,
                };
                let salt = decode(&sealed.salt)?;
                let key = derive(passphrase, &salt, kdf)?;
                let tried = Opened { key, salt, kdf };
                open(&tried, &sealed)?;
                *opened = Some(tried);
            }
            None => {
                let mut salt = vec![0; 16];
                random(&mut salt)?;
                let key = derive(passphrase, &salt, self.kdf)?;
                let made = Opened {
                    key,
                    salt,
                    kdf: self.kdf,
                };
                self.write(&made, &Secrets::default())?;
                *opened = Some(made);
            }
        }
        Ok(())
    }

    fn read(&self) -> Result<Secrets, SecretError> {
        let opened = self.lock();
        let opened = opened.as_ref().ok_or(SecretError::Locked)?;
        match self.sealed()? {
            Some(sealed) => open(opened, &sealed),
            None => Ok(Secrets::default()),
        }
    }

    fn change(&self, edit: impl FnOnce(&mut Secrets)) -> Result<(), SecretError> {
        let opened = self.lock();
        let opened = opened.as_ref().ok_or(SecretError::Locked)?;
        let mut secrets = match self.sealed()? {
            Some(sealed) => open(opened, &sealed)?,
            None => Secrets::default(),
        };
        edit(&mut secrets);
        self.write(opened, &secrets)
    }

    fn sealed(&self) -> Result<Option<Sealed>, SecretError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(failed(&self.path, e)),
        };
        let sealed: Sealed = serde_json::from_slice(&bytes)
            .map_err(|e| SecretError::Failed(format!("{}: {e}", self.path.display())))?;
        if sealed.version > FILE_VERSION {
            return Err(SecretError::Failed(format!(
                "{} was written by a newer Rovik.",
                self.path.display()
            )));
        }
        Ok(Some(sealed))
    }

    fn write(&self, opened: &Opened, secrets: &Secrets) -> Result<(), SecretError> {
        let plain = Zeroizing::new(
            serde_json::to_vec(secrets).map_err(|e| SecretError::Failed(e.to_string()))?,
        );
        let mut nonce = [0; 24];
        random(&mut nonce)?;
        let cipher = XChaCha20Poly1305::new(&Key::from(*opened.key));
        let data = cipher
            .encrypt(&XNonce::from(nonce), plain.as_slice())
            .map_err(|_| SecretError::Failed("they couldn't be encrypted".into()))?;
        let sealed = Sealed {
            version: FILE_VERSION,
            salt: BASE64.encode(&opened.salt),
            memory_kib: opened.kdf.memory_kib,
            iterations: opened.kdf.iterations,
            parallelism: opened.kdf.parallelism,
            nonce: BASE64.encode(nonce),
            data: BASE64.encode(data),
        };
        let bytes =
            serde_json::to_vec_pretty(&sealed).map_err(|e| SecretError::Failed(e.to_string()))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| failed(dir, e))?;
        }
        rovik_core::atomic_file::write_atomic_private(&self.path, &bytes)
            .map_err(|e| failed(&self.path, e))
    }
}

/// The secrets `sealed` holds, if `opened`'s key is its.
fn open(opened: &Opened, sealed: &Sealed) -> Result<Secrets, SecretError> {
    let nonce: [u8; 24] = decode(&sealed.nonce)?
        .try_into()
        .map_err(|_| SecretError::Failed("the file's nonce is damaged".into()))?;
    let data = decode(&sealed.data)?;
    let cipher = XChaCha20Poly1305::new(&Key::from(*opened.key));
    let plain = cipher
        .decrypt(&XNonce::from(nonce), data.as_slice())
        .map(Zeroizing::new)
        .map_err(|_| SecretError::WrongPassphrase)?;
    serde_json::from_slice(&plain).map_err(|e| SecretError::Failed(e.to_string()))
}

fn derive(passphrase: &Secret, salt: &[u8], kdf: Kdf) -> Result<Zeroizing<[u8; 32]>, SecretError> {
    let params = Params::new(kdf.memory_kib, kdf.iterations, kdf.parallelism, Some(32))
        .map_err(|e| SecretError::Failed(format!("the key's settings: {e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0; 32]);
    argon2
        .hash_password_into(passphrase.0.as_bytes(), salt, &mut key[..])
        .map_err(|e| SecretError::Failed(format!("the key: {e}")))?;
    Ok(key)
}

fn random(bytes: &mut [u8]) -> Result<(), SecretError> {
    getrandom::fill(bytes).map_err(|e| SecretError::Failed(format!("no random bytes: {e}")))
}

fn decode(text: &str) -> Result<Vec<u8>, SecretError> {
    BASE64
        .decode(text)
        .map_err(|_| SecretError::Failed("the file is damaged".into()))
}

fn failed(path: &std::path::Path, e: std::io::Error) -> SecretError {
    SecretError::Failed(format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap enough for tests.
    const QUICK: Kdf = Kdf {
        memory_kib: 256,
        iterations: 1,
        parallelism: 1,
    };

    fn token() -> SecretName {
        SecretName::Token {
            host: "host-1".into(),
        }
    }

    fn secret(text: &str) -> Secret {
        Secret(text.into())
    }

    #[test]
    fn the_encrypted_file_round_trips_under_its_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let store = SecretStore::file(path.clone(), QUICK);
        assert_eq!(store.kind(), StoreKind::File { unlocked: false });
        assert_eq!(store.get(&token()), Err(SecretError::Locked));
        store.unlock(&secret("correct horse")).unwrap();
        assert_eq!(store.kind(), StoreKind::File { unlocked: true });
        store.set(&token(), &secret("tok-123")).unwrap();
        let key = SecretName::ApiKey {
            provider: "openai".into(),
        };
        store.set(&key, &secret("sk-456")).unwrap();

        // The next session asks again; a wrong passphrase opens nothing.
        let again = SecretStore::file(path.clone(), Kdf::DEFAULT);
        assert_eq!(
            again.unlock(&secret("wrong")),
            Err(SecretError::WrongPassphrase)
        );
        assert_eq!(again.get(&token()), Err(SecretError::Locked));
        again.unlock(&secret("correct horse")).unwrap();
        assert_eq!(again.get(&token()), Ok(secret("tok-123")));
        assert_eq!(again.get(&key), Ok(secret("sk-456")));
        again.delete(&key).unwrap();
        again.delete(&key).unwrap();
        assert_eq!(again.get(&key), Err(SecretError::Missing(key.clone())));

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("tok-123") && !written.contains("sk-456"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "only the user reads it");
        }
    }

    #[test]
    fn a_missing_secret_says_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::file(dir.path().join("secrets.json"), QUICK);
        store.unlock(&secret("pass")).unwrap();
        let missing = store.get(&token()).unwrap_err();
        assert_eq!(missing, SecretError::Missing(token()));
        assert_eq!(
            missing.to_string(),
            "This computer has no token for this host: deploy Rovik to it again."
        );
        // The keychain's "no entry" is the same error.
        let password = SecretName::Password {
            host: "host-1".into(),
        };
        let from_keychain = keychain_error(keyring::Error::NoEntry, &password);
        assert_eq!(
            from_keychain.to_string(),
            "No SSH password is saved for this host: enter it again."
        );
    }

    #[test]
    fn a_damaged_or_newer_file_is_refused_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(&path, r#"{"version":9,"salt":"","memoryKib":8,"iterations":1,"parallelism":1,"nonce":"","data":""}"#).unwrap();
        let store = SecretStore::file(path.clone(), QUICK);
        let refused = store.unlock(&secret("pass")).unwrap_err();
        assert!(refused.to_string().contains("newer Rovik"), "{refused}");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("\"version\":9"));
    }
}
