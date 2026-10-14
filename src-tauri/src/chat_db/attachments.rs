//! Binary attachments (browser_screenshot results, images pasted into the
//! chat input) stored as files named by SHA-256. Identical images are
//! stored once; unreferenced ones are removed later.

use super::store::{err, now_ms, DbResult};
use super::DbState;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const DIR: &str = "attachments";
/// An attachment must stay unreferenced this long before cleanup removes
/// it: an image is stored before the message that references it.
const GRACE_MS: i64 = 60 * 60 * 1000;
/// A pasted image at most. The chat input scales big ones down before
/// storing them; this only refuses what no provider would take.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

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
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
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
    let id: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let path = db
        .data_dir()
        .join(DIR)
        .join(format!("{id}.{}", extension(mime)));
    let mime = mime.to_string();
    let size = bytes.len() as i64;

    if !path.exists() {
        let write_path = path.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::atomic_file::write_atomic(&write_path, &bytes)
        })
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

/// The type of an image the providers take (PNG, JPEG, WebP, GIF), from its
/// first bytes; None for anything else.
pub fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Stores an image pasted into the chat input; its type is read from the
/// bytes, never taken on trust.
pub async fn store_image(db: &DbState, bytes: Vec<u8>) -> DbResult<String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "The image is too large ({} MB; {} MB at most).",
            bytes.len().div_ceil(1024 * 1024),
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    let mime = image_mime(&bytes).ok_or("Only PNG, JPEG, WebP and GIF images can be attached.")?;
    store(db, bytes, mime).await
}

/// The ids that have no stored attachment (malformed ones included).
pub fn missing(conn: &Connection, ids: &[String]) -> DbResult<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT 1 FROM attachments WHERE id = ?1")
        .map_err(err)?;
    let mut out = Vec::new();
    for id in ids {
        if !is_valid_id(id) || !stmt.exists(params![id]).map_err(err)? {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// Images that unsent chat input still shows (`composer_drafts`, whose
/// content is the input's segments as JSON: lib/composerDraft.ts). No
/// message references them yet, but the user may still send them.
fn in_drafts(conn: &Connection) -> DbResult<HashSet<String>> {
    let mut stmt = conn
        .prepare("SELECT content FROM composer_drafts WHERE content LIKE '%\"image\"%'")
        .map_err(err)?;
    let drafts = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    let mut ids = HashSet::new();
    for content in drafts {
        // Rows from before drafts were JSON are plain text: no images.
        let Ok(serde_json::Value::Array(segments)) = serde_json::from_str(&content) else {
            continue;
        };
        for segment in &segments {
            if segment.get("type").and_then(|t| t.as_str()) != Some("image") {
                continue;
            }
            if let Some(id) = segment.pointer("/image/id").and_then(|id| id.as_str()) {
                ids.insert(id.to_string());
            }
        }
    }
    Ok(ids)
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

/// Removes attachments no message references (older than the grace period),
/// except images unsent chat input still holds.
pub fn collect_garbage(conn: &Connection, data_dir: &Path) -> DbResult<usize> {
    let drafts = in_drafts(conn)?;
    let ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT a.id FROM attachments a
                 WHERE a.created_at < ?1
                   AND NOT EXISTS (SELECT 1 FROM message_attachments m WHERE m.attachment_id = a.id)
                 LIMIT ?2",
            )
            .map_err(err)?;
        let rows: Vec<String> = stmt
            .query_map(
                params![now_ms() - GRACE_MS, 500 + drafts.len() as i64],
                |r| r.get(0),
            )
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        rows.into_iter().filter(|id| !drafts.contains(id)).collect()
    };
    for id in &ids {
        conn.execute("DELETE FROM attachments WHERE id = ?1", params![id])
            .map_err(err)?;
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
