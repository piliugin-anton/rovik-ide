//! Binary attachments (screenshots) stored as files named by SHA-256.
//! Identical images are stored once; unreferenced ones are removed later.

use super::store::{err, now_ms, DbResult};
use super::DbState;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const DIR: &str = "attachments";
/// An attachment must stay unreferenced this long before cleanup removes
/// it: a screenshot is stored before the message that references it.
const GRACE_MS: i64 = 60 * 60 * 1000;

fn extension(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("jpg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "application/octet-stream",
    }
}

pub fn is_valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The file for an attachment id, whatever its extension.
pub fn file_for(data_dir: &Path, id: &str) -> Option<PathBuf> {
    if !is_valid_id(id) {
        return None;
    }
    ["jpg", "png", "webp", "gif", "bin"]
        .iter()
        .map(|ext| data_dir.join(DIR).join(format!("{id}.{ext}")))
        .find(|p| p.exists())
}

/// Stores bytes (deduplicated by hash) and returns the attachment id.
pub async fn store(db: &DbState, bytes: Vec<u8>, mime: &str) -> DbResult<String> {
    let id: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    let path = db.data_dir().join(DIR).join(format!("{id}.{}", extension(mime)));
    let mime = mime.to_string();
    let size = bytes.len() as i64;

    if !path.exists() {
        let write_path = path.clone();
        tauri::async_runtime::spawn_blocking(move || crate::atomic_file::write_atomic(&write_path, &bytes))
            .await
            .map_err(err)?
            .map_err(err)?;
    }

    let row_id = id.clone();
    db.write(move |conn| {
        conn.execute(
            "INSERT INTO attachments (id, mime, bytes, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET created_at = excluded.created_at",
            params![row_id, mime, size, now_ms()],
        )
        .map_err(err)?;
        Ok(())
    })
    .await?;
    Ok(id)
}

/// Reads an attachment as a `data:` URL, for sending to vision models.
pub fn data_url(data_dir: &Path, id: &str) -> Option<String> {
    use base64::Engine;
    let path = file_for(data_dir, id)?;
    let bytes = std::fs::read(&path).ok()?;
    Some(format!(
        "data:{};base64,{}",
        mime_for(&path),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// Removes attachments no message references (older than the grace period).
pub fn collect_garbage(conn: &Connection, data_dir: &Path) -> DbResult<usize> {
    let ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT a.id FROM attachments a
                 WHERE a.created_at < ?1
                   AND NOT EXISTS (SELECT 1 FROM message_attachments m WHERE m.attachment_id = a.id)
                 LIMIT 500",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map(params![now_ms() - GRACE_MS], |r| r.get(0))
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        rows
    };
    for id in &ids {
        conn.execute("DELETE FROM attachments WHERE id = ?1", params![id]).map_err(err)?;
        if let Some(path) = file_for(data_dir, id) {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(ids.len())
}

/// Serves `chat-attachment://localhost/<id>` (and the Windows form,
/// `http://chat-attachment.localhost/<id>`) from disk.
pub fn serve(data_dir: &Path, uri_path: &str) -> tauri::http::Response<Vec<u8>> {
    let id = uri_path.trim_start_matches('/');
    let found = file_for(data_dir, id).and_then(|p| std::fs::read(&p).ok().map(|b| (p, b)));
    match found {
        Some((path, bytes)) => tauri::http::Response::builder()
            .status(200)
            .header("Content-Type", mime_for(&path))
            // Content-addressed: the bytes for an id never change.
            .header("Cache-Control", "public, max-age=31536000, immutable")
            .body(bytes)
            .unwrap_or_else(|_| not_found()),
        None => not_found(),
    }
}

fn not_found() -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(404)
        .body(Vec::new())
        .expect("static response")
}
