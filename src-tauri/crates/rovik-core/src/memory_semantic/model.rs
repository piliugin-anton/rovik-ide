//! One immutable model release, independent of the app's release cadence.
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;

const RELEASE: &str =
    "https://github.com/piliugin-anton/rovik-ide/releases/download/memory-minilm-l6-v2-v1";

struct Asset {
    name: &'static str,
    size: u64,
    hash: &'static str,
}

const ASSETS: [Asset; 2] = [
    Asset {
        name: "model.onnx",
        size: 90_387_606,
        hash: "4f148ba8ae9c2c7fbee4af2b132db8d06c6a6545b47fc83bbb98c3d22b8393e6",
    },
    Asset {
        name: "tokenizer.json",
        size: 711_661,
        hash: "da0e79933b9ed51798a3ae27893d3c5fa4a201126cef75586296df9b4d2c62a0",
    },
];

fn digest(hash: Sha256) -> String {
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn verify(directory: &Path, assets: &[Asset]) -> Result<(), String> {
    for asset in assets {
        let path = directory.join(asset.name);
        let mut file =
            std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if file.metadata().map_err(|e| e.to_string())?.len() != asset.size {
            return Err(format!(
                "{} has the wrong size; download the pinned model again",
                path.display()
            ));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        if digest(hash) != asset.hash {
            return Err(format!(
                "{} failed its SHA-256 check; download the pinned model again",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Checks existing files once before loading them; no network request on a hit.
pub(super) fn check(directory: &Path) -> Result<(), String> {
    verify(directory, &ASSETS)
}

/// Downloads missing assets to a temporary sibling, then installs the complete
/// checked directory. A failed download cannot expose a partial model.
pub(super) async fn install(directory: &Path) -> Result<(), String> {
    install_from(directory, RELEASE, &ASSETS).await
}

async fn install_from(directory: &Path, release: &str, assets: &[Asset]) -> Result<(), String> {
    if verify(directory, assets).is_ok() {
        return Ok(());
    }
    // An existing damaged or custom directory is never overwritten silently.
    if directory.exists() {
        return Err(format!(
            "{} is incomplete or corrupt; remove it to retry the model download",
            directory.display()
        ));
    }
    let parent = directory.parent().ok_or("model directory has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let stage = tempfile::Builder::new()
        .prefix(".minilm-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let client = crate::net::builder()
        .await
        .user_agent(concat!("rovik/", env!("CARGO_PKG_VERSION")))
        .read_timeout(std::time::Duration::from_secs(60))
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    for asset in assets {
        download(
            &client,
            &format!("{release}/{}", asset.name),
            asset,
            stage.path(),
        )
        .await?;
    }
    verify(stage.path(), assets)?;
    match std::fs::rename(stage.path(), directory) {
        Ok(()) => Ok(()),
        // Another process may have installed the same version while we fetched.
        Err(_) if verify(directory, assets).is_ok() => Ok(()),
        Err(e) => Err(format!("install {}: {e}", directory.display())),
    }
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    asset: &Asset,
    stage: &Path,
) -> Result<(), String> {
    use futures_util::StreamExt;
    let response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("download {}: {e}", asset.name))?;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.join(asset.name))
        .map_err(|e| e.to_string())?;
    let mut body = response.bytes_stream();
    let mut size = 0;
    let mut hash = Sha256::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        size += chunk.len() as u64;
        if size > asset.size {
            return Err(format!("{} exceeds its pinned size", asset.name));
        }
        hash.update(&chunk);
        out.write_all(&chunk).map_err(|e| e.to_string())?;
    }
    if size != asset.size || digest(hash) != asset.hash {
        return Err(format!("{} failed its size or SHA-256 check", asset.name));
    }
    out.sync_all().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve(bodies: Vec<&'static str>) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            for body in bodies {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                    .unwrap();
                let mut request = Vec::new();
                loop {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (url, thread)
    }

    #[test]
    fn failed_downloads_leave_no_partial_model_and_a_retry_installs_both_files() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("model");
        let hash = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let assets = [
            Asset {
                name: "model.onnx",
                size: 5,
                hash,
            },
            Asset {
                name: "tokenizer.json",
                size: 5,
                hash,
            },
        ];
        let (url, thread) = serve(vec!["hello", "jello"]);
        assert!(crate::rt::block_on(install_from(&directory, &url, &assets)).is_err());
        thread.join().unwrap();
        assert!(!directory.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        let (url, thread) = serve(vec!["hello", "hello"]);
        crate::rt::block_on(install_from(&directory, &url, &assets)).unwrap();
        thread.join().unwrap();
        verify(&directory, &assets).unwrap();
        // A warm install succeeds with an unreachable URL: no repeat download.
        crate::rt::block_on(install_from(&directory, "http://127.0.0.1:1", &assets)).unwrap();
        std::fs::write(directory.join("model.onnx"), "jello").unwrap();
        assert!(
            crate::rt::block_on(install_from(&directory, "http://127.0.0.1:1", &assets)).is_err()
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("model.onnx")).unwrap(),
            "jello"
        );
    }

    #[test]
    fn model_pins_reject_truncation_and_same_size_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let assets = [Asset {
            name: "model.onnx",
            size: 5,
            hash: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        }];
        assert!(verify(dir.path(), &assets).is_err());
        std::fs::write(dir.path().join("model.onnx"), "hello").unwrap();
        verify(dir.path(), &assets).unwrap();
        for bytes in ["hell", "jello"] {
            std::fs::write(dir.path().join("model.onnx"), bytes).unwrap();
            assert!(verify(dir.path(), &assets).is_err());
        }
    }

    #[test]
    #[ignore = "requires ROVIK_REAL_MEMORY_DOWNLOAD=1 and access to the pinned GitHub release"]
    fn github_model_download_is_verified_and_runs_without_python() {
        assert_eq!(
            std::env::var("ROVIK_REAL_MEMORY_DOWNLOAD").as_deref(),
            Ok("1")
        );
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("model");
        crate::rt::block_on(install(&directory)).unwrap();
        check(&directory).unwrap();
        let backend = verified_code_memory::embedding::OnnxMiniLm::new(&directory).unwrap();
        use verified_code_memory::MemoryEmbedder;
        let vector = backend
            .embed("Preserve source evidence for historical claims")
            .unwrap();
        assert_eq!(vector.len(), 384);
        assert!((vector.iter().map(|value| value * value).sum::<f32>() - 1.0).abs() < 1e-5);
    }
}
