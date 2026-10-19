//! This app's releases on the server's side (B02): the daemon's build for
//! a server's CPU, which a deploy and an upgrade take unless
//! `ROVIK_AGENTD_BINARY` names a build of one's own (B03), and a release's
//! `SHA256SUMS`, which the install check takes as Rovik's own builds (D5).
//! Each file is downloaded once, with its signature, into the app's data
//! folder, and checked against the release key whenever it's read: a file
//! the key didn't sign is never used. A release has its builds gzipped;
//! the app keeps them unpacked, the current version's alone.

use rovik_protocol::release;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a build's download may take.
const BUILD_WAIT: Duration = Duration::from_secs(300);
/// How long a list's may: the install check waits on it while connecting.
const LIST_WAIT: Duration = Duration::from_secs(20);
/// More than any build unpacks to: a file that would is no build.
const MAX_BUILD: u64 = 1 << 30;

/// Where a release's files come from, and are kept.
#[derive(Clone, Debug)]
pub struct Releases {
    /// `<cache>/<version>/<name>`, each beside its `.sig`.
    cache: PathBuf,
    /// `release::RELEASES`, or `ROVIK_RELEASES_URL`.
    base: String,
    /// The key they're checked with: without one, nothing is taken.
    key: Option<String>,
}

/// Why a release's file wasn't had.
#[derive(Clone, Debug, PartialEq)]
pub enum Unfetched {
    /// The release's answer: it hasn't the file, what came isn't what it
    /// signed, or this build has no key to check it with.
    Refused(String),
    /// It couldn't be had now (the network, the server's answer, the
    /// disk): asking again later may bring it.
    Failed(String),
}

impl std::fmt::Display for Unfetched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unfetched::Refused(why) | Unfetched::Failed(why) => f.write_str(why),
        }
    }
}

impl From<Unfetched> for String {
    fn from(e: Unfetched) -> String {
        e.to_string()
    }
}

/// A release's file, checked.
#[derive(Clone, Debug)]
pub struct Signed {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    /// Its signature, as its `.sig` file has it.
    pub signature: String,
}

impl Releases {
    /// This build of Rovik's releases, kept under `data_dir`.
    pub fn new(data_dir: &Path) -> Releases {
        let base = std::env::var("ROVIK_RELEASES_URL").unwrap_or_else(|_| release::RELEASES.into());
        Releases::at(data_dir.join("releases"), &base, release::key())
    }

    /// Releases at `base`, kept in `cache`, checked with `key`.
    pub fn at(cache: PathBuf, base: &str, key: Option<String>) -> Releases {
        Releases {
            cache,
            base: base.trim_end_matches('/').to_string(),
            key,
        }
    }

    /// Whether this build of Rovik can check what it downloads.
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    /// The SHA-256 of `version`'s builds, as its signed list says.
    pub async fn sums(&self, version: &str) -> Result<Vec<String>, Unfetched> {
        let list = self.fetch(version, release::SUMS, false).await?;
        let text = String::from_utf8_lossy(&list.bytes);
        Ok(release::sums(&text)
            .into_iter()
            .map(|(hash, _)| hash)
            .collect())
    }

    /// `version`'s build of the daemon for `cpu`, with its signature. Once
    /// it's kept, other versions' builds go: each is a server's worth of
    /// disk, and the next deploy takes this one.
    pub async fn build(&self, version: &str, cpu: &str) -> Result<Signed, String> {
        let name = release::build_name(version, cpu);
        let build = self.fetch(version, &name, true).await?;
        self.forget_builds_but(version);
        Ok(build)
    }

    /// `version`'s file `name`, with its signature: the one kept, or else
    /// downloaded, gzipped when `packed`. Either way it's checked first.
    async fn fetch(&self, version: &str, name: &str, packed: bool) -> Result<Signed, Unfetched> {
        let key = (self.key.as_deref()).ok_or(Unfetched::Refused(NO_KEY.into()))?;
        let folder = self.cache.join(version);
        let path = folder.join(name);
        let sig_path = signature_of(&path);
        if let (Ok(bytes), Ok(signature)) =
            (std::fs::read(&path), std::fs::read_to_string(&sig_path))
        {
            if release::verify(key, &bytes, &signature).is_ok() {
                return Ok(Signed {
                    path,
                    bytes,
                    signature,
                });
            }
        }
        let url = format!("{}/v{version}/{name}", self.base);
        let signature = download(&format!("{url}{}", release::SIGNATURE), LIST_WAIT).await?;
        let signature = String::from_utf8_lossy(&signature).into_owned();
        let bytes = match packed {
            true => {
                let packed = format!("{url}{}", release::PACKED);
                let bytes = download(&packed, BUILD_WAIT).await?;
                unpacked(&bytes).map_err(|why| Unfetched::Refused(format!("{packed}: {why}")))?
            }
            false => download(&url, LIST_WAIT).await?,
        };
        if let Err(why) = release::verify(key, &bytes, &signature) {
            return Err(Unfetched::Refused(format!(
                "{url} isn't what Rovik's release signed. {why}"
            )));
        }
        std::fs::create_dir_all(&folder).map_err(|e| Unfetched::Failed(e.to_string()))?;
        keep(&path, &bytes).map_err(Unfetched::Failed)?;
        keep(&sig_path, signature.as_bytes()).map_err(Unfetched::Failed)?;
        Ok(Signed {
            path,
            bytes,
            signature,
        })
    }

    /// The builds kept for versions other than `version`.
    fn forget_builds_but(&self, version: &str) {
        let Ok(folders) = std::fs::read_dir(&self.cache) else {
            return;
        };
        for folder in folders.flatten().filter(|f| f.file_name() != version) {
            let files = std::fs::read_dir(folder.path()).into_iter().flatten();
            for file in files.flatten() {
                if file
                    .file_name()
                    .to_string_lossy()
                    .starts_with("rovik-agentd-")
                {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
    }
}

/// A gzipped build, unpacked, if it's no bigger than any build is.
fn unpacked(packed: &[u8]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut reader = flate2::read::GzDecoder::new(packed).take(MAX_BUILD + 1);
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| format!("it isn't gzip: {e}"))?;
    if bytes.len() as u64 > MAX_BUILD {
        return Err("it unpacks to more than any build.".into());
    }
    Ok(bytes)
}

/// Why nothing is downloaded.
pub const NO_KEY: &str = "This build of Rovik has no release key to check a downloaded daemon \
                          with. Set ROVIK_AGENTD_BINARY to a rovik-agentd built for the server \
                          (scripts/build-agentd.sh makes one).";

/// `path` with `.sig` after it.
pub fn signature_of(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(release::SIGNATURE);
    PathBuf::from(name)
}

/// Writes `bytes` beside `path`, then moves them there: a file half
/// written is never one kept. Under a name of this process's own: two of
/// Rovik's windows may fetch the same file at once.
fn keep(path: &Path, bytes: &[u8]) -> Result<(), String> {
    rovik_core::atomic_file::write_atomic(path, bytes)
        .map_err(|e| format!("{}: {e}", path.display()))
}

async fn download(url: &str, wait: Duration) -> Result<Vec<u8>, Unfetched> {
    // Settings → Network's timeout and proxy.
    let client = rovik_core::net::builder()
        .await
        .timeout(wait)
        .user_agent(concat!("Rovik/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Unfetched::Failed(e.to_string()))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| Unfetched::Failed(format!("{url} couldn't be reached: {e}")))?;
    match response.status() {
        status if status.is_success() => {}
        reqwest::StatusCode::NOT_FOUND => {
            return Err(Unfetched::Refused(format!(
                "{url} isn't there: the release doesn't have it."
            )))
        }
        status => return Err(Unfetched::Failed(format!("{url} answered {status}."))),
    }
    let bytes = response.bytes().await;
    Ok(bytes
        .map_err(|e| Unfetched::Failed(format!("{url}: {e}")))?
        .to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovik_protocol::release::build_name;
    use rovik_protocol::release::fixture::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A release server: each path's body, the rest 404. Each request's
    /// path is counted.
    async fn serving(
        files: Vec<(String, Vec<u8>)>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let heard = asked.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (files, heard) = (files.clone(), heard.clone());
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buffer[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request);
                    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                    heard.lock().unwrap().push(path.clone());
                    let found = files.iter().find(|(p, _)| *p == path);
                    let (status, body) = match found {
                        Some((_, body)) => ("200 OK", body.clone()),
                        None => ("404 Not Found", Vec::new()),
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                });
            }
        });
        (format!("http://127.0.0.1:{port}"), asked)
    }

    fn gzipped(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(bytes).unwrap();
        gz.finish().unwrap()
    }

    /// Release 9.9.8 as the workflow publishes it: the x86_64 build
    /// gzipped, its signature, and the signed list.
    fn release(build: &str) -> Vec<(String, Vec<u8>)> {
        let name = build_name("9.9.8", "x86_64");
        vec![
            (format!("/v9.9.8/{name}.gz"), gzipped(build.as_bytes())),
            (format!("/v9.9.8/{name}.sig"), BUILD_SIG.as_bytes().to_vec()),
            ("/v9.9.8/SHA256SUMS".into(), SHA256SUMS.as_bytes().to_vec()),
            (
                "/v9.9.8/SHA256SUMS.sig".into(),
                SHA256SUMS_SIG.as_bytes().to_vec(),
            ),
        ]
    }

    #[tokio::test]
    async fn a_release_build_is_downloaded_once_and_checked_each_time() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("releases");
        // An older version's build, and its list.
        std::fs::create_dir_all(cache.join("9.9.7")).unwrap();
        std::fs::write(cache.join("9.9.7/rovik-agentd-9.9.7-x86_64"), "old").unwrap();
        std::fs::write(cache.join("9.9.7/SHA256SUMS"), "old").unwrap();
        let (base, asked) = serving(release(BUILD)).await;
        let releases = Releases::at(cache.clone(), &base, Some(KEY.into()));
        let build = releases.build("9.9.8", "x86_64").await.unwrap();
        assert_eq!(build.bytes, BUILD.as_bytes(), "unpacked");
        assert_eq!(build.signature, BUILD_SIG);
        let name = build_name("9.9.8", "x86_64");
        assert_eq!(build.path, cache.join("9.9.8").join(&name));
        assert_eq!(std::fs::read(&build.path).unwrap(), BUILD.as_bytes());
        assert_eq!(
            asked.lock().unwrap().len(),
            2,
            "the build and its signature"
        );
        assert!(!cache.join("9.9.7/rovik-agentd-9.9.7-x86_64").exists());
        assert!(cache.join("9.9.7/SHA256SUMS").exists(), "lists stay");
        // Kept: the server isn't asked again.
        let again = releases.build("9.9.8", "x86_64").await.unwrap();
        assert_eq!(again.bytes, build.bytes);
        assert_eq!(asked.lock().unwrap().len(), 2);
        let sums = releases.sums("9.9.8").await.unwrap();
        assert_eq!(sums, [BUILD_SHA256, BUILD_SHA256]);

        // A kept build that changed isn't taken: it's downloaded again.
        std::fs::write(&build.path, "changed").unwrap();
        let fixed = releases.build("9.9.8", "x86_64").await.unwrap();
        assert_eq!(fixed.bytes, BUILD.as_bytes());
        assert_eq!(std::fs::read(&build.path).unwrap(), BUILD.as_bytes());
    }

    #[tokio::test]
    async fn what_the_key_didnt_sign_or_the_release_lacks_isnt_taken() {
        let dir = tempfile::tempdir().unwrap();
        let (base, _) = serving(release("#!/bin/sh\necho 'something else'\n")).await;
        let releases = Releases::at(dir.path().to_path_buf(), &base, Some(KEY.into()));
        let refused = releases.build("9.9.8", "x86_64").await.unwrap_err();
        assert!(
            refused.contains("isn't what Rovik's release signed"),
            "{refused}"
        );
        let name = build_name("9.9.8", "x86_64");
        assert!(
            !dir.path().join("9.9.8").join(&name).exists(),
            "nothing kept"
        );
        let missing = releases.build("9.9.8", "aarch64").await;
        assert!(missing.unwrap_err().contains("doesn't have it"));
        let keyless = Releases::at(dir.path().to_path_buf(), &base, None);
        assert_eq!(keyless.build("9.9.8", "x86_64").await.unwrap_err(), NO_KEY);
        assert!(!keyless.has_key());
        let nowhere = Releases::at(dir.path().join("x"), "http://127.0.0.1:1", Some(KEY.into()));
        // The network's failure isn't the release's answer: asking later
        // may bring the list.
        let unreachable = nowhere.sums("9.9.8").await.unwrap_err();
        assert!(
            matches!(&unreachable, Unfetched::Failed(why) if why.contains("couldn't be reached")),
            "{unreachable:?}"
        );
        let unlisted = releases.sums("9.9.7").await.unwrap_err();
        assert!(matches!(unlisted, Unfetched::Refused(_)), "{unlisted:?}");
        assert!(matches!(
            keyless.sums("9.9.8").await,
            Err(Unfetched::Refused(_))
        ));
        assert!(unpacked(b"not gzip").unwrap_err().contains("isn't gzip"));
    }
}
