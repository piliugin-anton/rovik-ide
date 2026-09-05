use reqwest::Client;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use tauri::Manager;
use futures_util::StreamExt;
use tauri::ipc::Channel;
use bytes::{BytesMut, Buf};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ChatStreamEvent {
    Delta { content: String },
    Done,
    Error { message: String },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArgumentsDelta {
        id: String,
        arguments_delta: String,
    },
    ToolCallComplete {
        id: String,
        name: String,
        arguments: String, // fully accumulated JSON string
    },
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

struct ToolCallState {
    name: String,
    arguments: String,
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let crlf = buffer.windows(4).position(|w| w == b"\r\n\r\n");
    let lf = buffer.windows(2).position(|w| w == b"\n\n");

    match (crlf, lf) {
        (Some(a), Some(b)) if a <= b => Some((a, 4)),
        (Some(a), Some(_)) => Some((a, 4)),
        (Some(a), None) => Some((a, 4)),
        (None, Some(b)) => Some((b, 2)),
        (None, None) => None,
    }
}

#[tauri::command]
pub async fn send_chat_message(
    base_url: String,
    api_key: String,
    model: String,
    messages: Vec<WireChatMessage>,
    streaming: bool,
    on_event: Channel<ChatStreamEvent>,
) -> Result<(), String> {
    let client = Client::new();

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
            "stream": streaming,
            // You likely also pass "tools" here from the frontend or config.
            // "tools": tools,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();

    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let error = format!("{status}: {text}");

        let _ = on_event.send(ChatStreamEvent::Error{ message: error.clone() });
        return Err(error);
    }

    // Non-streaming request
    if !streaming {
        let text = resp.text().await.map_err(|e| e.to_string())?;

        // For non-streaming, we can just send the whole body as a delta
        // or parse it more finely; here we keep your original behavior.
        let _ = on_event.send(ChatStreamEvent::Delta { content: text.clone() });

        // Also parse tool calls if present
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(tool_calls) = value
                .pointer("/choices/0/message/tool_calls")
                .and_then(|v| v.as_array())
            {
                for tc in tool_calls {
                    let id = tc
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let name = tc
                        .pointer("/function/name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let arguments = tc
                        .pointer("/function/arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    if !id.is_empty() {
                        let _ = on_event.send(ChatStreamEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                        });

                        if !arguments.is_empty() {
                            let _ = on_event.send(ChatStreamEvent::ToolCallArgumentsDelta {
                                id: id.clone(),
                                arguments_delta: arguments.clone(),
                            });
                        }

                        let _ = on_event.send(ChatStreamEvent::ToolCallComplete {
                            id,
                            name,
                            arguments,
                        });
                    }
                }
            }
        }

        let _ = on_event.send(ChatStreamEvent::Done);
        return Ok(());
    }

    // Streaming request
    let mut stream = resp.bytes_stream();

    let mut buffer = BytesMut::new();

    // Track in-progress tool calls for this completion
    let mut tool_calls: HashMap<String, ToolCallState> = HashMap::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        // Keep raw bytes. Do NOT decode each network chunk independently.
    buffer.extend_from_slice(&chunk);

       while let Some((pos, separator_len)) = find_sse_boundary(&buffer) {
        let event_bytes = buffer.split_to(pos);

        // Consume \n\n or \r\n\r\n
        buffer.advance(separator_len);

            let event = std::str::from_utf8(&event_bytes)
            .map_err(|e| format!("Invalid UTF-8 in SSE event: {e}"))?;

            for line in event.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };

                let data = data.trim();

                if data == "[DONE]" {
                    // Finalize any in-progress tool calls
                    for (id, state) in tool_calls.drain() {
                        let _ = on_event.send(ChatStreamEvent::ToolCallComplete {
                            id,
                            name: state.name,
                            arguments: state.arguments,
                        });
                    }

                    let _ = on_event.send(ChatStreamEvent::Done);
                    return Ok(());
                }

                let value: serde_json::Value = match serde_json::from_str(data) {
                    Ok(value) => value,
                    Err(err) => {
        eprintln!("Failed to parse SSE data: {err}; data={data:?}");
        continue;
    }
                };

                // Extract delta from choices[0].delta
                let Some(delta) = value.pointer("/choices/0/delta") else {
                    continue;
                };

                // Content delta
                if let Some(content) = delta
                    .pointer("/content")
                    .and_then(|v| v.as_str())
                {
                    let _ = on_event.send(ChatStreamEvent::Delta { content: content.to_string() });
                }

                // Tool calls delta
                if let Some(tool_calls_arr) = delta
                    .pointer("/tool_calls")
                    .and_then(|v| v.as_array())
                {
                    for tc in tool_calls_arr {
                        let id = tc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        let name = tc
                            .pointer("/function/name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        let args_delta = tc
                            .pointer("/function/arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        if id.is_empty() {
                            continue;
                        }

                        let entry = tool_calls.entry(id.clone()).or_insert_with(|| ToolCallState {
                            name: String::new(),
                            arguments: String::new(),
                        });

                        // If this is the first time we see this call id + name, emit start
                        if entry.name.is_empty() && !name.is_empty() {
                            entry.name = name.clone();
                            let _ = on_event.send(ChatStreamEvent::ToolCallStart {
                                id: id.clone(),
                                name: name.clone(),
                            });
                        }

                        if !args_delta.is_empty() {
                            entry.arguments.push_str(args_delta);
                            let _ = on_event.send(ChatStreamEvent::ToolCallArgumentsDelta {
                                id: id.clone(),
                                arguments_delta: args_delta.to_string(),
                            });
                        }
                    }
                }

                // Check finish_reason to know when tool calls are complete
                if let Some(finish_reason) = value
                    .pointer("/choices/0/finish_reason")
                    .and_then(|v| v.as_str())
                {
                    if finish_reason == "tool_calls" {
                        // Finalize all in-progress tool calls
                        for (id, state) in tool_calls.drain() {
                            let _ = on_event.send(ChatStreamEvent::ToolCallComplete {
                                id,
                                name: state.name,
                                arguments: state.arguments,
                            });
                        }
                    }
                }
            }
        }
    }

    // Stream ended without explicit [DONE]; finalize any pending tool calls
    for (id, state) in tool_calls.drain() {
        let _ = on_event.send(ChatStreamEvent::ToolCallComplete {
            id,
            name: state.name,
            arguments: state.arguments,
        });
    }

    // Optionally parse any final buffered SSE event here if needed.

    let _ = on_event.send(ChatStreamEvent::Done);
    Ok(())
}