//! Binary attachments (browser_screenshot results, images and PDFs
//! attached in the chat input) stored as files named by SHA-256. Identical
//! files are stored once; unreferenced ones are removed later.

use super::images;
use super::store::{err, now_ms, DbResult};
use super::DbState;
use regex::bytes::Regex;
use rusqlite::{params, Connection};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const DIR: &str = "attachments";
/// An attachment must stay unreferenced this long before cleanup removes
/// it: a file is stored before the message that references it.
const GRACE_MS: i64 = 60 * 60 * 1000;
/// An attached PDF at most: it's sent as it is, under the 32 MB a request
/// OpenAI and Anthropic take.
pub const MAX_PDF_BYTES: usize = 20 * 1024 * 1024;
/// An attached image file at most, before it's scaled down (images.rs).
pub const MAX_IMAGE_BYTES: usize = 50 * 1024 * 1024;
pub const PDF: &str = "application/pdf";
/// Images read at once: a big photo takes hundreds of MB while it's read,
/// and the chat input attaches up to ten files together.
static PREPARING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// Stored types and their files' extensions.
const TYPES: [(&str, &str); 5] = [
    ("image/jpeg", "jpg"),
    ("image/png", "png"),
    ("image/webp", "webp"),
    ("image/gif", "gif"),
    (PDF, "pdf"),
];

fn extension(mime: &str) -> &'static str {
    TYPES
        .iter()
        .find(|(m, _)| *m == mime)
        .map_or("bin", |(_, ext)| ext)
}

fn mime_for(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str());
    TYPES
        .iter()
        .find(|(_, e)| Some(*e) == ext)
        .map_or("application/octet-stream", |(mime, _)| mime)
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
    TYPES
        .iter()
        .map(|(_, ext)| *ext)
        .chain(["bin"])
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
        crate::rt::spawn_blocking(move || crate::atomic_file::write_atomic(&write_path, &bytes))
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

/// The type of a file the providers take (PNG, JPEG, WebP and GIF images,
/// PDFs), from its first bytes; None for anything else.
pub fn attachment_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if is_pdf(bytes) {
        Some(PDF)
    } else {
        None
    }
}

/// A PDF starts with `%PDF-`, which readers look for in its first kilobyte
/// (some files have a few bytes of junk before it).
fn is_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
}

/// Pages in a PDF, as far as they can be counted without a PDF reader: its
/// page objects (`/Type /Page`), else its page tree's `/Count`. Files that
/// compress those (object streams) are counted by size, at a typical 50 KB
/// a page. It's for estimating what the PDF costs a request.
pub fn pdf_pages(bytes: &[u8]) -> usize {
    // ASCII only: a PDF's bytes aren't UTF-8.
    static PAGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)/Type\s*/Page\b").unwrap());
    static COUNT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)/Count\s+(\d+)").unwrap());
    let pages = PAGE.find_iter(bytes).count();
    if pages > 0 {
        return pages;
    }
    let count = COUNT
        .captures_iter(bytes)
        .filter_map(|c| std::str::from_utf8(&c[1]).ok()?.parse::<usize>().ok())
        .max()
        .unwrap_or(0);
    if count > 0 {
        count
    } else {
        bytes.len().div_ceil(50 * 1024).max(1)
    }
}

/// An attachment as stored, for the chat input.
#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Stored {
    pub id: String,
    /// Its type: a PDF, or an image made a PNG.
    pub mime: &'static str,
    /// A PDF's pages (`pdf_pages`).
    pub pages: Option<usize>,
    /// An image's size as stored.
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Stores an image or a PDF attached in the chat input; its type is read
/// from the bytes, never taken on trust. A PDF is stored as it is, an image
/// as a PNG at most `images::MAX_SIDE` pixels a side.
pub async fn store_attached(db: &DbState, bytes: Vec<u8>) -> DbResult<Stored> {
    let mime = attachment_mime(&bytes)
        .ok_or("Only PNG, JPEG, WebP and GIF images and PDFs can be attached.")?;
    let max = if mime == PDF {
        MAX_PDF_BYTES
    } else {
        MAX_IMAGE_BYTES
    };
    if bytes.len() > max {
        return Err(format!(
            "The file is too large ({} MB; {} MB at most).",
            bytes.len().div_ceil(1024 * 1024),
            max / (1024 * 1024)
        ));
    }
    if mime == PDF {
        let pages = pdf_pages(&bytes);
        let id = store(db, bytes, PDF).await?;
        return Ok(Stored {
            id,
            mime: PDF,
            pages: Some(pages),
            width: None,
            height: None,
        });
    }
    let image = {
        let _turn = PREPARING.acquire().await.map_err(err)?;
        crate::rt::spawn_blocking(move || images::prepare(&bytes))
            .await
            .map_err(err)??
    };
    let id = store(db, image.png, images::MIME).await?;
    Ok(Stored {
        id,
        mime: images::MIME,
        pages: None,
        width: Some(image.width),
        height: Some(image.height),
    })
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

/// Images and PDFs that unsent chat input still shows (`composer_drafts`,
/// whose content is the input's segments as JSON: lib/composerDraft.ts). No
/// message references them yet, but the user may still send them.
fn in_drafts(conn: &Connection) -> DbResult<HashSet<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT content FROM composer_drafts
             WHERE content LIKE '%\"image\"%' OR content LIKE '%\"pdf\"%'",
        )
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
            let id = match segment.get("type").and_then(|t| t.as_str()) {
                Some("image") => segment.pointer("/image/id"),
                Some("pdf") => segment.pointer("/pdf/id"),
                _ => None,
            };
            if let Some(id) = id.and_then(|id| id.as_str()) {
                ids.insert(id.to_string());
            }
        }
    }
    Ok(ids)
}

/// Reads an attachment as a `data:` URL, for sending to a model.
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

/// The file behind `chat-attachment://localhost/<id>` (and the Windows
/// form, `http://chat-attachment.localhost/<id>`): its type and bytes, read
/// from disk. None when there is no such attachment.
pub fn read(data_dir: &Path, uri_path: &str) -> Option<(&'static str, Vec<u8>)> {
    let path = file_for(data_dir, uri_path.trim_start_matches('/'))?;
    let bytes = std::fs::read(&path).ok()?;
    Some((mime_for(&path), bytes))
}
