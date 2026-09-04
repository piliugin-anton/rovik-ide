use serde::Serialize;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use tauri::Manager;
use futures_util::StreamExt;
use tauri::ipc::Channel;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum ChatStreamEvent {
    Delta(String),
    Done,
    Error(String),
}

static CALL_COUNT: AtomicU32 = AtomicU32::new(0);
const BINARY_SNIFF_BYTES: usize = 8000;

#[derive(Serialize)]
pub struct DirEntryInfo {
    name: String,
    path: String,
    is_dir: bool,
}

/// Lists the immediate children of `path` — non-recursive by design. The
/// frontend calls this again for each folder the user expands, so cost
/// scales with what's actually opened, not with total project size.
#[tauri::command]
pub fn read_dir_entries(path: String) -> Result<Vec<DirEntryInfo>, String> {
    let n = CALL_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    eprintln!("[debug] read_dir_entries call #{n} for {path}");

    let dir = Path::new(&path);
    let read = fs::read_dir(dir).map_err(|e| format!("{path}: {e}"))?;

    let mut entries: Vec<DirEntryInfo> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip dotfiles and the usual heavy/noisy directories by default.
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }

        entries.push(DirEntryInfo {
            name,
            path: entry.path().to_string_lossy().to_string(),
            is_dir: file_type.is_dir(),
        });
    }

    // Directories first, then alphabetical (case-insensitive) within each group.
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));

    Ok(entries)
}

fn looks_binary(path: &Path) -> Result<bool, String> {
    use std::io::Read;
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; BINARY_SNIFF_BYTES];
    let n = file.read(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf[..n].contains(&0))
}

#[tauri::command]
pub fn read_file_content(path: String) -> Result<String, String> {
    let p = Path::new(&path);
    if looks_binary(p)? {
        return Err(format!("{path}: binary file, cannot open in editor"));
    }
    fs::read_to_string(p).map_err(|e| format!("{path}: {e}"))
}

#[tauri::command]
pub fn write_file_content(path: String, content: String) -> Result<(), String> {
    fs::write(&path, content).map_err(|e| format!("{path}: {e}"))
}

// --- AI chat: settings persistence + the actual API call ------------------

fn ai_settings_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("ai-settings.json"))
}

#[tauri::command]
pub fn load_ai_settings(app: tauri::AppHandle) -> Result<String, String> {
    let path = ai_settings_path(&app)?;
    if !path.exists() {
        return Ok(r#"{"profiles":[],"activeProfileId":null}"#.to_string());
    }
    fs::read_to_string(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_ai_settings(app: tauri::AppHandle, json: String) -> Result<(), String> {
    let path = ai_settings_path(&app)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(&path, json).map_err(|e| e.to_string())
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct WireChatMessage {
    role: String,
    content: String,
}


#[tauri::command]
pub async fn send_chat_message(
    base_url: String,
    api_key: String,
    model: String,
    messages: Vec<WireChatMessage>,
    stream: bool,
    on_event: Channel<ChatStreamEvent>,
) -> Result<(), String> {
    let client = reqwest::Client::new();

    let url = format!(
        "{}/chat/completions",
        base_url.trim_end_matches('/')
    );

    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": stream,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();

    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();

        let error = format!("{status}: {text}");

        let _ = on_event.send(ChatStreamEvent::Error(error.clone()));

        return Err(error);
    }

    // Non-streaming request can still use the same command.
    if !stream {
        let text = resp.text().await.map_err(|e| e.to_string())?;

        on_event
            .send(ChatStreamEvent::Delta(text))
            .map_err(|e| e.to_string())?;

        on_event
            .send(ChatStreamEvent::Done)
            .map_err(|e| e.to_string())?;

        return Ok(());
    }

    let mut stream = resp.bytes_stream();

    // SSE data can be split across HTTP chunks, so don't parse
    // each reqwest chunk independently.
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;

        let text = String::from_utf8_lossy(&chunk);
        buffer.push_str(&text);

        // SSE events are separated by a blank line.
        while let Some(pos) = buffer.find("\n\n") {
            let event = buffer[..pos].to_string();
            buffer.drain(..pos + 2);

            for line in event.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };

                let data = data.trim();

                if data == "[DONE]" {
                    on_event
                        .send(ChatStreamEvent::Done)
                        .map_err(|e| e.to_string())?;

                    return Ok(());
                }

                let value: serde_json::Value =
                    match serde_json::from_str(data) {
                        Ok(value) => value,
                        Err(_) => continue,
                    };

                if let Some(delta) = value
                    .pointer("/choices/0/delta/content")
                    .and_then(|v| v.as_str())
                {
                    on_event
                        .send(ChatStreamEvent::Delta(delta.to_string()))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }

    // Some servers may terminate without an explicit [DONE].
    if !buffer.trim().is_empty() {
        // Optional: parse the final buffered SSE event here.
    }

    on_event
        .send(ChatStreamEvent::Done)
        .map_err(|e| e.to_string())?;

    Ok(())
}