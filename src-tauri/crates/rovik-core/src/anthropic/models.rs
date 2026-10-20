//! What a model takes, from the Models API (`GET /v1/models/{id}`): its
//! thinking modes, effort levels, output limit, context window, images and
//! PDFs. Asked once per model while the app runs (the model list in
//! Settings → Model providers tells it for every model at once), and kept
//! with what requests found since (`super::adjust`). When the API can't
//! say, it's guessed from the model's name.

use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Effort levels, lowest first.
pub const LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// How long to wait for the Models API before guessing.
const TIMEOUT: Duration = Duration::from_secs(10);
/// A model the API couldn't tell about is asked about again after this.
const ASK_AGAIN_AFTER: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    /// The Models API said what it takes (else it's guessed from the name).
    pub reported: bool,
    /// The most `max_tokens` it takes.
    pub max_output: Option<u32>,
    /// Its context window in tokens.
    pub context: Option<u64>,
    pub vision: Option<bool>,
    pub pdf: Option<bool>,
    /// It thinks at all.
    pub thinking: bool,
    /// `thinking: {type: "adaptive"}`: it decides when and how much.
    pub adaptive: bool,
    /// `thinking: {type: "enabled", budget_tokens}`: a budget of its own.
    pub budget: bool,
    /// `thinking: {type: "disabled"}`.
    pub disabled: bool,
    /// `thinking: {type: "between_tools"}`: off, with short progress notes
    /// between tool calls (Claude Sonnet 5.5's way of turning it off).
    pub between_tools: bool,
    /// The `output_config.effort` levels it takes; none: no effort at all.
    pub efforts: Vec<&'static str>,
    /// It takes `thinking.display` (to show its reasoning, summarized).
    pub display: bool,
    /// It thinks when the request says nothing about thinking (Claude 5).
    pub thinks_by_default: bool,
    /// No longer served.
    pub retired: bool,
}

/// The Claude version in a model id: "claude-opus-4-1-20250805" (4.1),
/// "claude-sonnet-4-5" (4.5), "claude-opus-5" (5.0), and the older order,
/// "claude-3-7-sonnet-20250219" (3.7). A date isn't a version.
pub fn version(model: &str) -> Option<(u32, u32)> {
    let model = model.trim().to_ascii_lowercase();
    let name = model.rsplit('/').next().unwrap_or("");
    let mut parts = name.strip_prefix("claude-")?.split('-');
    let number = |part: &str| {
        (!part.is_empty() && part.len() <= 2 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<u32>().ok())
            .flatten()
    };
    let major = parts.by_ref().find_map(number)?;
    let minor = parts.next().and_then(number).unwrap_or(0);
    Some((major, minor))
}

impl ModelInfo {
    /// What a model takes, judged by its name: adaptive thinking and the
    /// effort from Claude 4.6, a thinking budget from Claude 3.7, no
    /// thinking before; on by default from Claude 5. A name that isn't a
    /// Claude version's gets the newest forms (a refusal moves it on).
    pub fn guess(model: &str) -> ModelInfo {
        let version = version(model);
        let (thinking, adaptive, budget) = match version {
            Some(v) if v >= (4, 6) => (true, true, v < (4, 7)),
            Some(v) if v >= (3, 7) => (true, false, true),
            Some(_) => (false, false, false),
            None => (true, true, true),
        };
        let efforts = if adaptive {
            vec!["low", "medium", "high"]
        } else {
            Vec::new()
        };
        ModelInfo {
            reported: false,
            max_output: None,
            context: None,
            vision: None,
            pdf: None,
            thinking,
            adaptive,
            budget,
            // Tried, where it thinks: a model that can't turn thinking off
            // says so (`super::adjust`).
            disabled: thinking,
            between_tools: false,
            efforts,
            display: true,
            thinks_by_default: version.is_some_and(|v| v.0 >= 5),
            retired: false,
        }
    }

    /// A model as the Models API describes it (`GET /v1/models/{id}`, or an
    /// entry of `GET /v1/models`). Where it says nothing (no
    /// `capabilities`), the guess from its name stands.
    pub fn from_api(model: &Value) -> ModelInfo {
        let id = model.get("id").and_then(|i| i.as_str()).unwrap_or("");
        let mut info = ModelInfo::guess(id);
        info.max_output = model
            .get("max_tokens")
            .and_then(|n| n.as_u64())
            .filter(|n| *n > 0)
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
        info.context = model
            .get("max_input_tokens")
            .and_then(|n| n.as_u64())
            .filter(|n| *n > 0);
        info.retired = model.get("lifecycle").and_then(|l| l.as_str()) == Some("retired");
        let Some(caps) = model.get("capabilities").filter(|c| c.is_object()) else {
            return info;
        };
        let supported = |path: &str| {
            caps.pointer(&format!("{path}/supported"))
                .and_then(|s| s.as_bool())
        };
        info.reported = true;
        info.vision = supported("/image_input");
        info.pdf = supported("/pdf_input");
        if let Some(thinking) = supported("/thinking") {
            info.thinking = thinking;
            info.adaptive = thinking && supported("/thinking/types/adaptive").unwrap_or(false);
            info.budget = thinking && supported("/thinking/types/enabled").unwrap_or(false);
            info.disabled = supported("/thinking/types/disabled").unwrap_or(true);
            info.between_tools = supported("/thinking/types/between_tools").unwrap_or(false);
        }
        if let Some(effort) = supported("/effort") {
            info.efforts = LEVELS
                .iter()
                .copied()
                .filter(|l| effort && supported(&format!("/effort/{l}")).unwrap_or(false))
                .collect();
        }
        info.thinks_by_default &= info.thinking;
        info
    }

    /// The effort level to send for `level`: itself when the model takes
    /// it, else the nearest it does take (on a tie, the higher). None for a
    /// model without the effort.
    pub fn effort_for(&self, level: &str) -> Option<&'static str> {
        let at = |l: &str| LEVELS.iter().position(|x| *x == l);
        let wanted = at(level)?;
        self.efforts
            .iter()
            .copied()
            .filter_map(|l| at(l).map(|i| (l, i)))
            .min_by_key(|(_, i)| (i.abs_diff(wanted), usize::MAX - i))
            .map(|(l, _)| l)
    }

    /// The lowest effort the model takes, if it takes one.
    pub fn lowest_effort(&self) -> Option<&'static str> {
        LEVELS.iter().copied().find(|l| self.efforts.contains(l))
    }
}

/*
 * ---------------------------------------------------------------------------
 * What's known, by API base and model
 * ---------------------------------------------------------------------------
 */

#[derive(Clone)]
enum Known {
    Info(ModelInfo),
    /// The API couldn't say, at this time.
    Unknown(Instant),
}

static KNOWN: LazyLock<Mutex<HashMap<String, Known>>> = LazyLock::new(Default::default);

fn key(base: &str, model: &str) -> String {
    format!("{base}|{model}")
}

/// Keeps what is known of `model`: from the model list, or what requests
/// found it doesn't take.
pub fn remember(base: &str, model: &str, info: ModelInfo) {
    if let Ok(mut known) = KNOWN.lock() {
        known.insert(key(base, model), Known::Info(info));
    }
}

/// What `model` takes: asked of the Models API the first time, then
/// remembered while the app runs. None when the API can't say (it fails, or
/// doesn't know the id), and isn't asked again for a while.
pub async fn info(client: &Client, base: &str, api_key: &str, model: &str) -> Option<ModelInfo> {
    let key = key(base, model);
    match KNOWN.lock().ok().and_then(|k| k.get(&key).cloned()) {
        Some(Known::Info(info)) => return Some(info),
        Some(Known::Unknown(at)) if at.elapsed() < ASK_AGAIN_AFTER => return None,
        _ => {}
    }
    let fetched = fetch(client, base, api_key, model).await;
    if let Ok(mut known) = KNOWN.lock() {
        let entry = match &fetched {
            Some(info) => Known::Info(info.clone()),
            None => Known::Unknown(Instant::now()),
        };
        known.insert(key, entry);
    }
    fetched
}

async fn fetch(client: &Client, base: &str, api_key: &str, model: &str) -> Option<ModelInfo> {
    if model.trim().is_empty() {
        return None;
    }
    let mut url = reqwest::Url::parse(&format!("{base}/models/")).ok()?;
    url.path_segments_mut().ok()?.pop_if_empty().push(model);
    let response = client
        .get(url)
        .timeout(TIMEOUT)
        .headers(super::headers(api_key, None))
        .send()
        .await;
    let response = match response {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            eprintln!(
                "anthropic: no model info for {model} ({}); judging it by its name",
                r.status()
            );
            return None;
        }
        Err(e) => {
            eprintln!(
                "anthropic: no model info for {model} ({}); judging it by its name",
                crate::net::error_text(&e)
            );
            return None;
        }
    };
    let body: Value = response.json().await.ok()?;
    Some(ModelInfo::from_api(&body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_are_read_from_model_ids() {
        for (id, version) in [
            ("claude-opus-4-1-20250805", Some((4, 1))),
            ("claude-opus-4-20250514", Some((4, 0))),
            ("claude-sonnet-4-5", Some((4, 5))),
            ("claude-haiku-4-5-20251001", Some((4, 5))),
            ("claude-opus-5", Some((5, 0))),
            ("claude-opus-5-5", Some((5, 5))),
            ("claude-fable-5-1", Some((5, 1))),
            ("claude-3-7-sonnet-20250219", Some((3, 7))),
            ("claude-3-haiku-20240307", Some((3, 0))),
            ("Claude-Sonnet-4-6", Some((4, 6))),
            ("anthropic/claude-sonnet-4-5", Some((4, 5))),
            ("gpt-5", None),
            ("claude-instant-1.2", None),
            ("claude", None),
        ] {
            assert_eq!(super::version(id), version, "{id}");
        }
    }

    #[test]
    fn guesses_follow_the_claude_generations() {
        let opus55 = ModelInfo::guess("claude-opus-5-5");
        assert!(opus55.adaptive && !opus55.budget && opus55.thinks_by_default);
        assert_eq!(opus55.efforts, ["low", "medium", "high"]);
        let opus46 = ModelInfo::guess("claude-opus-4-6");
        assert!(opus46.adaptive && opus46.budget && !opus46.thinks_by_default);
        let opus48 = ModelInfo::guess("claude-opus-4-8");
        assert!(opus48.adaptive && !opus48.budget);
        let sonnet45 = ModelInfo::guess("claude-sonnet-4-5-20250929");
        assert!(sonnet45.thinking && sonnet45.budget && !sonnet45.adaptive);
        assert!(sonnet45.efforts.is_empty());
        let haiku3 = ModelInfo::guess("claude-3-haiku-20240307");
        assert!(!haiku3.thinking && !haiku3.adaptive && !haiku3.budget);
        // Not a Claude version: the newest forms first.
        let unknown = ModelInfo::guess("my-model");
        assert!(unknown.adaptive && unknown.budget && !unknown.thinks_by_default);
    }

    #[test]
    fn the_models_api_says_what_a_model_takes() {
        let model = json!({
            "type": "model", "id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5",
            "max_input_tokens": 1000000, "max_tokens": 128000, "lifecycle": "active",
            "capabilities": {
                "image_input": { "supported": true },
                "pdf_input": { "supported": true },
                "thinking": { "supported": true, "types": {
                    "adaptive": { "supported": true }, "enabled": { "supported": false },
                    "disabled": { "supported": false }, "between_tools": { "supported": true } } },
                "effort": { "supported": true, "low": { "supported": true }, "medium": { "supported": true },
                    "high": { "supported": true }, "xhigh": null, "max": { "supported": true } },
            },
        });
        let info = ModelInfo::from_api(&model);
        assert!(info.reported);
        assert_eq!(info.max_output, Some(128_000));
        assert_eq!(info.context, Some(1_000_000));
        assert_eq!((info.vision, info.pdf), (Some(true), Some(true)));
        assert!(info.adaptive && !info.budget && !info.disabled && info.between_tools);
        assert_eq!(info.efforts, ["low", "medium", "high", "max"]);
        assert!(info.thinks_by_default && !info.retired);

        // No capabilities: limits from the API, the rest guessed.
        let bare =
            json!({ "id": "claude-sonnet-4-5", "max_tokens": 64000, "lifecycle": "retired" });
        let info = ModelInfo::from_api(&bare);
        assert!(!info.reported && info.retired && info.budget);
        assert_eq!(info.max_output, Some(64_000));
        assert_eq!(info.vision, None);

        // No thinking at all.
        let plain = json!({ "id": "claude-3-5-haiku", "capabilities": {
            "thinking": { "supported": false, "types": { "disabled": { "supported": true } } },
            "effort": { "supported": false } } });
        let info = ModelInfo::from_api(&plain);
        assert!(!info.thinking && !info.adaptive && info.efforts.is_empty());
    }

    #[test]
    fn efforts_go_to_the_nearest_level_taken() {
        let mut info = ModelInfo::guess("claude-opus-5-5");
        info.efforts = vec!["low", "high", "max"];
        assert_eq!(info.effort_for("high"), Some("high"));
        // Between low and high: the higher.
        assert_eq!(info.effort_for("medium"), Some("high"));
        assert_eq!(info.lowest_effort(), Some("low"));
        info.efforts.clear();
        assert_eq!(info.effort_for("high"), None);
        assert_eq!(info.lowest_effort(), None);
    }
}
