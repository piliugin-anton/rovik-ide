use reqwest::Client;
use serde::{Deserialize, Serialize};
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
pub struct WireToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: WireToolCallFunction,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct WireChatMessage {
    role: String,
    #[serde(default)]
    content: String,
    // Present on an assistant message that requested tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall>>,
    // Present on a role:"tool" message replying to one of the above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OpenAIChatOptions {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stop: Option<Vec<String>>,
    // add more as needed: frequency_penalty, presence_penalty, etc.
}

const CODING_AGENT_SYSTEM_PROMPT: &str = r#"You are a coding agent operating inside a developer's IDE. You have access to tools that let you read, write, and search the user's actual project. Your job is to complete coding tasks by using these tools — not by describing what should be done.

## Core behavior

- You are an agent, not a chatbot. When a task requires inspecting files or searching the codebase, use the appropriate tool immediately. Do not ask the user to paste file contents you can read yourself, and do not guess at file contents, APIs, or project structure — verify with a tool call.
- Never claim you made a change unless you actually called write_file. Never describe code you "would" write instead of writing it, unless the user explicitly asked for a plan or explanation only.
- If a tool call fails or returns an error string, read it, adjust, and retry with a corrected call rather than giving up or fabricating a result.
- If you're missing information a tool could get you, get it — don't ask the user for something you can retrieve yourself.
- Only ask the user a clarifying question when the task is genuinely ambiguous in a way that changes the approach. Otherwise, make a reasonable assumption, state it briefly, and proceed.

## Tool use rules

- Available tools: read_file (read a file, optionally a line range), write_file (create or fully overwrite a file), list_dir (recursively list files/directories, gitignore-aware), search_code (case-insensitive text search across the project).
- Always use `search_code` / `list_dir` before assuming a file doesn't exist or a symbol isn't defined somewhere.
- There is no patch/diff tool — write_file always replaces the entire file. For edits to an existing file, read_file first, then write_file back the complete updated content, not just the changed lines.
- Never invent tool output. If a tool call hasn't been made, you don't have the result.
- Chain tool calls as needed within a turn to fully resolve the task before responding — don't stop halfway and ask "should I continue?" unless you hit a real decision point.

## Communication style

- Keep prose minimal. Lead with the change/result, not a restatement of the request.
- When you finish, summarize concretely: what files changed and any remaining risk or TODO.
- If you're uncertain about correctness, say so explicitly rather than presenting a guess as fact.

## Boundaries

- All file paths are relative to the project root and confined to it — you cannot and should not attempt to access anything outside the opened project folder.
- Don't create or modify config/dependency files unless the task calls for it."#;

#[tauri::command]
pub async fn send_chat_message(
    base_url: String,
    api_key: String,
    model: String,
    mut messages: Vec<WireChatMessage>,
    streaming: bool,
    tools: Option<Vec<serde_json::Value>>,
    system_prompt: Option<String>,
    options: Option<OpenAIChatOptions>,
    on_event: Channel<ChatStreamEvent>,
) -> Result<(), String> {
    let client = Client::new();

    let url = format!(
        "{}/chat/completions",
        base_url.trim_end_matches('/')
    );

    // Only frame this as an agentic coding session when there's actually a
    // project open for the tools to operate on — otherwise this is just chat.
    if tools.as_ref().is_some_and(|t| !t.is_empty()) {
        let content = system_prompt.unwrap_or(CODING_AGENT_SYSTEM_PROMPT.to_string());
        messages.insert(
            0,
            WireChatMessage {
                role: "system".to_string(),
                content,
                tool_calls: None,
                tool_call_id: None,
            },
        );
    }

    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": streaming,
    });

    // Merge optional OpenAI parameters from `options`
    if let Some(opts) = options {
        if let Some(max_tokens) = opts.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temperature) = opts.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }
        if let Some(top_p) = opts.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(stop) = opts.stop.filter(|s| !s.is_empty()) {
            body["stop"] = serde_json::json!(stop);
        }
    }

    // Only attach "tools"/"tool_choice" when the frontend actually gave us
    // schemas (e.g. no project folder is open yet, or the profile disabled
    // tool use) — some OpenAI-compatible backends reject an empty tools array.
    if let Some(tools) = tools.filter(|t| !t.is_empty()) {
        body["tools"] = serde_json::json!(tools);
        body["tool_choice"] = serde_json::json!("auto");
    }

    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
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