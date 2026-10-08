use eoka::{Element, Page, Result as EokaResult, TextMatch};
use log::info;
use rand::RngExt;
use serde_json::Value as Json;
use std::collections::HashMap;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::time::{sleep, Duration, Instant};

/// A condition to check before each step, and what to do if it's true.
#[derive(Clone)]
pub struct Watcher {
    pub name: String,
    pub check: Arc<dyn Fn(StepContext) -> BoxFuture<'static, bool> + Send + Sync>,
    pub handler: Vec<Step>,
    /// Cap on how many times this watcher may fire in one run. `None` = unlimited.
    pub max_triggers: Option<usize>,
    /// How long to keep polling `check` after the handler runs, waiting for
    /// it to clear, before deciding whether to fire the handler again.
    pub settle_timeout: Duration,
    /// How often to poll `check` while waiting for it to clear.
    pub poll_interval: Duration,
    /// If `max_triggers` is reached and the condition is *still* true (the
    /// last attempt didn't resolve it), abort the whole `run_steps` call
    /// instead of letting the main step sequence proceed against a page
    /// that's still blocked. If the condition happens to have cleared by
    /// the time the cap is hit, this has no effect — the run continues
    /// normally either way.
    pub abort_on_exhaustion: bool,
}

impl Watcher {
    /// Convenience constructor for the common case: "if this text is on the
    /// page, run these steps." Uses a cheap `text_exists` check, not a full
    /// `find_by_text_match` (no element needed just to detect presence).
    pub fn on_text(name: impl Into<String>, text: impl Into<String>, handler: Vec<Step>) -> Self {
        let text = text.into();
        Self {
            name: name.into(),
            check: Arc::new(move |ctx| {
                let text = text.clone();
                Box::pin(async move { ctx.text_exists(&text).await })
            }),
            handler,
            max_triggers: None,
            settle_timeout: Duration::from_secs(10),
            poll_interval: Duration::from_millis(500),
            abort_on_exhaustion: false,
        }
    }
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Break,
}

/// One piece of data a step recorded, tagged with where it came from.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Capture {
    pub label: String,
    pub value: Json,
}

#[derive(Clone)]
pub struct StepContext {
    page: Page,
    captures: Arc<Mutex<Vec<Capture>>>,
    watchers: Arc<Vec<Watcher>>,
    trigger_counts: Arc<Mutex<HashMap<String, usize>>>,
}

impl StepContext {
    fn new(page: Page, watchers: Vec<Watcher>) -> Self {
        Self {
            page,
            captures: Arc::new(Mutex::new(Vec::new())),
            watchers: Arc::new(watchers),
            trigger_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record a value to be returned at the end of the run. Accepts anything
    /// `Into<serde_json::Value>` — `String`, `&str`, `f64`, `bool`,
    /// `serde_json::json!({...})`, etc.
    pub fn capture(&self, label: impl Into<String>, value: impl Into<Json>) {
        self.captures.lock().unwrap().push(Capture {
            label: label.into(),
            value: value.into(),
        });
    }

    fn take_captures(&self) -> Vec<Capture> {
        std::mem::take(&mut *self.captures.lock().unwrap())
    }

    /// Snapshot of everything captured so far (does not clear it).
    pub fn captures(&self) -> Vec<Capture> {
        self.captures.lock().unwrap().clone()
    }

    /// Most recent capture with this label, if any.
    pub fn get_capture(&self, label: &str) -> Option<Json> {
        self.captures
            .lock()
            .unwrap()
            .iter()
            .rev() // most recent first
            .find(|c| c.label == label)
            .map(|c| c.value.clone())
    }

    /// All captures with this label, oldest to newest — useful after a
    /// Repeat loop where the same label was captured multiple times.
    pub fn get_captures(&self, label: &str) -> Vec<Json> {
        self.captures
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.label == label)
            .map(|c| c.value.clone())
            .collect()
    }

    /// Most recent capture with this label, deserialized into `T`.
    pub fn get_capture_as<T: serde::de::DeserializeOwned>(&self, label: &str) -> Option<T> {
        self.get_capture(label)
            .and_then(|v| serde_json::from_value(v).ok())
    }
}

// Keeps every existing `page.foo(...)` call site working unchanged as
// `ctx.foo(...)`.
impl Deref for StepContext {
    type Target = Page;
    fn deref(&self) -> &Page {
        &self.page
    }
}

pub type PageAction =
    Arc<dyn Fn(StepContext) -> BoxFuture<'static, EokaResult<Flow>> + Send + Sync>;

#[derive(Default)]
pub struct FindOutcome {
    pub element: Option<Element>,
}

impl FindOutcome {
    fn found(element: Element) -> Self {
        Self {
            element: Some(element),
        }
    }
    fn not_found() -> Self {
        Self { element: None }
    }
}

pub type FoundCallback =
    Arc<dyn Fn(StepContext, FindOutcome) -> BoxFuture<'static, EokaResult<Flow>> + Send + Sync>;

#[derive(Clone)]
pub enum Step {
    Setup(PageAction),
    Goto {
        url: String,
    },
    WaitForNetworkIdle {
        quiet_ms: u64,
        timeout_ms: u64,
    },
    WaitForAny {
        selectors: Vec<String>,
        timeout_ms: u64,
    },
    FindByTextMatch {
        text: String,
        match_type: TextMatch,
        on_result: FoundCallback,
    },
    /// Human-like click on an element. Agent-serializable (no callbacks),
    /// so the agent's `browser_automate` tool can build these from JSON.
    Click {
        selector: String,
    },
    ClickText {
        text: String,
    },
    /// Human-like typing into an element.
    Type {
        selector: String,
        text: String,
    },
    /// Human-like form fill: click, clear, type with natural delays.
    Fill {
        selector: String,
        value: String,
    },
    /// Press a key ("Enter", "Escape", "Tab", ...).
    Press {
        key: String,
    },
    /// Fixed pause, in milliseconds.
    Wait {
        ms: u64,
    },
    WaitForText {
        text: String,
        timeout_ms: u64,
    },
    /// Read an element's (or the body's) text into the captures under `label`.
    CaptureText {
        label: String,
        selector: Option<String>,
    },
    /// Instant scroll of a container by `y` CSS pixels.
    Scroll {
        selector: String,
        y: f64,
    },
    Repeat {
        steps: Vec<Step>,
        max_times: usize,
    },
    Custom(PageAction),
    ScreenShot {
        path: String,
    },
}

/// Runs the steps; returns the terminal `Flow` plus everything captured
/// along the way (via `StepContext::capture`), in the order it happened.
pub async fn run_steps(
    page: &Page,
    steps: &[Step],
    watchers: Vec<Watcher>,
) -> EokaResult<(Flow, Vec<Capture>)> {
    let ctx = StepContext::new(page.clone(), watchers);
    // watch = true: this is the top-level call, so it should check watchers
    // at every step boundary.
    let flow = run_steps_inner(&ctx, steps, true).await?;
    Ok((flow, ctx.take_captures()))
}

/// `watch` controls whether `check_watchers` runs at each step boundary in
/// this call. It's `false` when running a watcher's own handler steps (and
/// anything nested inside them, e.g. a `Repeat`) — a handler is the
/// *reaction* to a condition firing, not a step sequence that itself needs
/// to watch for that same condition. Without this, firing a handler would
/// recursively re-check (and potentially re-fire) watchers before the
/// handler's own steps had a chance to actually change anything on the
/// page — which is exactly the bug that caused the same handler to run
/// multiple times per single real detection.
fn run_steps_inner<'a>(
    ctx: &'a StepContext,
    steps: &'a [Step],
    watch: bool,
) -> BoxFuture<'a, EokaResult<Flow>> {
    Box::pin(async move {
        for step in steps {
            if watch {
                if check_watchers(ctx).await? == Flow::Break {
                    return Ok(Flow::Break);
                }
            }
            if run_step(ctx, step, watch).await? == Flow::Break {
                return Ok(Flow::Break);
            }
        }
        Ok(Flow::Continue)
    })
}

fn check_watchers<'a>(ctx: &'a StepContext) -> BoxFuture<'a, EokaResult<Flow>> {
    Box::pin(async move {
        for watcher in ctx.watchers.iter() {
            loop {
                let already_maxed = {
                    let counts = ctx.trigger_counts.lock().unwrap();
                    let count = counts.get(&watcher.name).copied().unwrap_or(0);
                    watcher.max_triggers.is_some_and(|max| count >= max)
                };
                if already_maxed {
                    info!(
                        "watcher '{}': giving up, max_triggers reached",
                        watcher.name
                    );
                    if watcher.abort_on_exhaustion && (watcher.check)(ctx.clone()).await {
                        info!(
                            "watcher '{}': condition still active after max_triggers — aborting run",
                            watcher.name
                        );
                        return Ok(Flow::Break);
                    }
                    break;
                }

                if !(watcher.check)(ctx.clone()).await {
                    break; // condition already clear — nothing to do
                }

                *ctx.trigger_counts
                    .lock()
                    .unwrap()
                    .entry(watcher.name.clone())
                    .or_insert(0) += 1;
                let attempt = *ctx
                    .trigger_counts
                    .lock()
                    .unwrap()
                    .get(&watcher.name)
                    .unwrap();
                info!(
                    "watcher '{}': firing handler (attempt {}/{:?})",
                    watcher.name, attempt, watcher.max_triggers
                );
                ctx.capture(format!("watcher_triggered:{}", watcher.name), true);

                // watch = false — see the doc comment on run_steps_inner.
                if run_steps_inner(ctx, &watcher.handler, false).await? == Flow::Break {
                    return Ok(Flow::Break);
                }

                // Debounced re-check: require several consecutive clear
                // reads before trusting that the handler actually resolved
                // things, rather than acting on a single lucky read.
                if wait_for_condition_clear(ctx, watcher).await {
                    break;
                }
                // else: still there — loop again, subject to max_triggers
            }
        }
        Ok(Flow::Continue)
    })
}

async fn wait_for_condition_clear(ctx: &StepContext, watcher: &Watcher) -> bool {
    const REQUIRED_CONSECUTIVE_CLEAR: u32 = 3;
    let deadline = Instant::now() + watcher.settle_timeout;
    let mut consecutive_clear = 0u32;
    let mut poll_count = 0u32;

    info!(
        "wait_for_condition_clear: '{}' starting, settle_timeout={:?}",
        watcher.name, watcher.settle_timeout
    );

    loop {
        sleep(watcher.poll_interval).await;
        poll_count += 1;
        let still_present = (watcher.check)(ctx.clone()).await;

        if poll_count == 1 || Instant::now() >= deadline {
            // Cheap extra visibility on the boundary polls without spamming every 500ms.
            if let Ok(title) = ctx.title().await {
                info!(
                    "wait_for_condition_clear: '{}' poll {} title={title}",
                    watcher.name, poll_count
                );
            }
        }

        if !still_present {
            consecutive_clear += 1;
            if consecutive_clear >= REQUIRED_CONSECUTIVE_CLEAR {
                info!(
                    "wait_for_condition_clear: '{}' confirmed cleared",
                    watcher.name
                );
                return true;
            }
        } else {
            consecutive_clear = 0;
        }

        if Instant::now() >= deadline {
            info!(
                "wait_for_condition_clear: '{}' gave up after {:?}, still not clear",
                watcher.name, watcher.settle_timeout
            );
            return false;
        }
    }
}

fn run_step<'a>(
    ctx: &'a StepContext,
    step: &'a Step,
    watch: bool,
) -> BoxFuture<'a, EokaResult<Flow>> {
    Box::pin(async move {
        let flow = match step {
            Step::Setup(action) | Step::Custom(action) => action(ctx.clone()).await?,

            Step::Goto { url } => {
                ctx.goto(url).await?;
                Flow::Continue
            }

            Step::WaitForNetworkIdle {
                quiet_ms,
                timeout_ms,
            } => {
                ctx.wait_for_network_idle(*quiet_ms, *timeout_ms).await?;
                Flow::Continue
            }

            Step::WaitForAny {
                selectors,
                timeout_ms,
            } => {
                wait_for_any(&ctx.page, selectors, *timeout_ms).await?;
                Flow::Continue
            }

            Step::FindByTextMatch {
                text,
                match_type,
                on_result,
            } => {
                let outcome = match ctx.find_by_text_match(text, *match_type).await {
                    Ok(el) => FindOutcome::found(el),
                    Err(_) => FindOutcome::not_found(),
                };
                on_result(ctx.clone(), outcome).await?
            }

            Step::Repeat { steps, max_times } => {
                for _ in 0..*max_times {
                    if run_steps_inner(ctx, steps, watch).await? == Flow::Break {
                        break;
                    }
                }
                Flow::Continue
            }

            Step::Click { selector } => {
                ctx.human_click(selector).await?;
                Flow::Continue
            }

            Step::ClickText { text } => {
                ctx.human_click_by_text(text).await?;
                Flow::Continue
            }

            Step::Type { selector, text } => {
                ctx.human_type(selector, text).await?;
                Flow::Continue
            }

            Step::Fill { selector, value } => {
                ctx.human_fill(selector, value).await?;
                Flow::Continue
            }

            Step::Press { key } => {
                ctx.press_key(key).await?;
                Flow::Continue
            }

            Step::Wait { ms } => {
                ctx.wait(*ms).await;
                Flow::Continue
            }

            Step::WaitForText { text, timeout_ms } => {
                let _ = ctx.wait_for_text(text, *timeout_ms).await?;
                Flow::Continue
            }

            Step::CaptureText { label, selector } => {
                let expr = match selector {
                    Some(sel) => {
                        let quoted = serde_json::to_string(sel).expect("string serializes");
                        format!("((document.querySelector({quoted})?.innerText) || '')")
                    }
                    None => "(document.body?.innerText || '')".to_string(),
                };
                let text: String = ctx.evaluate_sync(&expr).await.unwrap_or_default();
                ctx.capture(label.clone(), text);
                Flow::Continue
            }

            Step::Scroll { selector, y } => {
                let _ = scroll_by(&ctx.page, selector, *y).await?;
                Flow::Continue
            }

            Step::ScreenShot { path } => {
                if let Ok(png) = ctx.screenshot().await {
                    let _ = std::fs::write(&path, png);
                }
                Flow::Continue
            }
        };
        Ok(flow)
    })
}

async fn wait_for_any(page: &Page, selectors: &[String], timeout_ms: u64) -> EokaResult<()> {
    let refs: Vec<&str> = selectors.iter().map(String::as_str).collect();
    let deadline = Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if page.find_any(&refs).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eoka::Error::Timeout(format!(
                "none of {:?} appeared within {}ms",
                selectors, timeout_ms
            )));
        }
        sleep(std::time::Duration::from_millis(150)).await;
    }
}

/// Wait for a frame matching `frame_url_part` to finish loading *and*
/// settle (no further DOM mutations), returning its frame id.
pub async fn wait_for_frame_load(
    page: &eoka::Page,
    frame_url_part: &str,
    timeout: Duration,
) -> Result<String, eoka::Error> {
    let deadline = Instant::now() + timeout;

    // 1. Find the frame and wait for readyState === "complete".
    let frame_id = loop {
        let mut found = None;
        for frame in page.frames().await? {
            if frame.url.contains(frame_url_part) {
                found = Some(frame.id.clone());
                break;
            }
        }

        if let Some(id) = found {
            match page
                .evaluate_in_frame_id::<String>(&id, "document.readyState")
                .await
            {
                Ok(ready) if ready == "complete" => break id,
                Ok(_) | Err(_) => {}
            }
        }

        if Instant::now() >= deadline {
            return Err(eoka::Error::Timeout(format!(
                "frame matching '{frame_url_part}' did not reach readyState=complete in time"
            )));
        }
        sleep(Duration::from_millis(50)).await;
    };

    // 2. Wait for the frame's DOM to stop mutating — two consecutive
    // snapshots of outerHTML that match, some quiet interval apart.
    wait_for_frame_stable(page, &frame_id, Duration::from_millis(300), deadline).await?;

    Ok(frame_id)
}

/// Poll a frame's outerHTML until it's unchanged across `quiet` interval,
/// or bail out at `deadline`.
async fn wait_for_frame_stable(
    page: &eoka::Page,
    frame_id: &str,
    quiet: Duration,
    deadline: Instant,
) -> Result<(), eoka::Error> {
    let mut last: Option<String> = None;

    loop {
        let snapshot: Result<String, _> = page
            .evaluate_in_frame_id(frame_id, "document.documentElement.outerHTML.length + ':' + document.documentElement.outerHTML")
            .await;

        if let Ok(snapshot) = snapshot {
            if last.as_deref() == Some(snapshot.as_str()) {
                return Ok(()); // unchanged since last check -> stable
            }
            last = Some(snapshot);
        }

        if Instant::now() >= deadline {
            return Err(eoka::Error::Timeout(format!(
                "frame {frame_id} did not stabilize in time"
            )));
        }
        sleep(quiet).await;
    }
}

fn random_f64(min: f64, max: f64) -> f64 {
    let mut rng = rand::rng();
    let value = rng.random_range(min..max);
    let sign = if rng.random_bool(0.5) { 1.0 } else { -1.0 };

    value * sign
}

pub(crate) fn random_u64(min: u64, max: u64) -> u64 {
    rand::rng().random_range(min..=max)
}

pub fn get_cloudflare_captcha_watcher() -> Watcher {
    Watcher {
        name: "cloudflare_challenge".into(),
        check: Arc::new(|ctx| Box::pin(async move { is_cloudflare_challenge_active(&ctx).await })),
        max_triggers: Some(3),
        settle_timeout: Duration::from_secs(10),
        poll_interval: Duration::from_millis(500),
        abort_on_exhaustion: true,
        handler: vec![Step::Custom(Arc::new(|ctx| {
            Box::pin(async move { solve_cloudflare_challenge(&ctx).await })
        }))],
    }
}

/// If a Cloudflare challenge is showing on `page`, run the solver once.
/// Best-effort for the read-only tools (`browse_url` etc.): Ok(true) when
/// no challenge was present or it cleared, Ok(false) when it's still
/// there. Err only on a transport-level failure.
pub async fn settle_cloudflare(page: &Page) -> EokaResult<bool> {
    let ctx = StepContext::new(page.clone(), Vec::new());
    if !is_cloudflare_challenge_active(&ctx).await {
        return Ok(true);
    }
    solve_cloudflare_challenge(&ctx).await?;
    Ok(!is_cloudflare_challenge_active(&ctx).await)
}

/// True if a Cloudflare challenge appears to be active, using only signals
/// Cloudflare itself controls — never anything scraped from the host site.
pub(crate) async fn is_cloudflare_challenge_active(ctx: &StepContext) -> bool {
    // Cloudflare's own hardcoded interstitial title, same across every
    // site that uses their challenge product.
    if matches!(ctx.title().await, Ok(t) if t.trim() == "Just a moment...") {
        return true;
    }
    // Fallback: a live challenge iframe served from Cloudflare's own domain.
    if let Ok(frames) = ctx.frames().await {
        if frames
            .iter()
            .any(|f| f.url.contains("challenges.cloudflare.com"))
        {
            return true;
        }
    }
    false
}

async fn solve_cloudflare_challenge(ctx: &StepContext) -> EokaResult<Flow> {
    info!("CAPTCHA_DETECTED");

    let _ = match wait_for_frame_load(
        ctx,
        "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/",
        Duration::from_secs(15),
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            info!("CAPTCHA_FRAME_NOT_FOUND: {e}");
            return Ok(Flow::Continue);
        }
    };

    info!("CAPTCHA_FRAME_LOADED");

    let header_el = match ctx
        .find("div.main-wrapper > div.main-content > h2[id]")
        .await
    {
        Ok(el) => el,
        Err(e) => {
            info!("CAPTCHA_HEADER_ELEMENT_NOT_FOUND: {e}");
            return Ok(Flow::Continue);
        }
    };

    let rect = match header_el.bounding_box().await {
        Some(r) => r,
        None => {
            info!("CAPTCHA_HEADER_HAS_NO_BOUNDING_BOX");
            return Ok(Flow::Continue);
        }
    };
    info!("GOT_CAPTCHA_HEADER_BOUNDING_BOX");

    // Turnstile's checkbox sits near the widget's left edge, vertically
    // centered — this is Cloudflare's own widget chrome, so the offset is
    // consistent regardless of which site embeds it.
    let x = 20.0 + random_f64(0.0, 2.0) + rect.x;
    let y = 111.0 + random_f64(0.0, 2.0) + rect.y + rect.height;
    info!("COMPUTED_CLICK_COORDS x={x} y={y}");

    ctx.wait(random_u64(2345, 4567)).await;
    ctx.human().move_and_click(x, y).await?;
    info!("CLICKED_CAPTCHA_CHECKBOX");

    ctx.wait_for_text(
        "Verifying you are human. This may take a few seconds.",
        3_000,
    )
    .await?;
    info!("CAPTCHA_IS_VERIFYING...");

    /* let _ = ctx.wait_for_network_idle(6000, 15_000).await;
    info!("NETWORK_SETTLED_AFTER_CLICK");

    // Confirm resolution with the same Cloudflare-native signals used for
    // detection, debounced against a transient blank/flickering read.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut consecutive_ok = 0u32;
    const REQUIRED_CONSECUTIVE: u32 = 3;
    let mut solved = false;

    while Instant::now() < deadline {
        let still_active = is_cloudflare_challenge_active(ctx).await;
        if !still_active {
            consecutive_ok += 1;
            if consecutive_ok >= REQUIRED_CONSECUTIVE {
                solved = true;
                break;
            }
        } else {
            consecutive_ok = 0;
        }
        ctx.wait(150).await;
    }

    if solved {
        info!("CAPTCHA_SOLVED!");
    } else {
        info!("CAPTCHA_NOT_SOLVED!!!");
    } */

    Ok(Flow::Continue)
}

/* pub fn get_cloudflare_captcha_watcher() -> Watcher {
    Watcher {
        name: "cloudflare_challenge".into(),
        check: Arc::new(|ctx| {
            Box::pin(async move {
                // Same signal the handler itself uses to confirm resolution —
                // avoids the two checks disagreeing about page state.
                matches!(ctx.title().await, Ok(title) if title == "Just a moment...")
            })
        }),
        max_triggers: Some(3), // don't loop forever if it can't be solved
        settle_timeout: Duration::from_secs(10),
        poll_interval: Duration::from_millis(500),
        handler: vec![
            Step::Custom(Arc::new(|ctx| {
                Box::pin(async move {
                    info!("CAPTCHA_DETECTED");
                    let _ = wait_for_frame_load(
                        &ctx,
                        "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/",
                        Duration::from_secs(15),
                    )
                    .await;
                    info!("CAPTCHA_FRAME_LOADED");

                    match ctx
                        .wait_for("div.main-wrapper > div.main-content > h2[id]", 10_000)
                        .await
                    {
                        Ok(el) => {
                            info!("HAS_CAPTCHA_TEXT_ELEMENT");
                            match el.bounding_box().await {
                                Some(rect) => {
                                    info!("GOT_CAPTCHA_TEXT_BOUNDING_BOX");
                                    let x = 20.0 + random_f64(0.0, 2.0) + rect.x;
                                    let y = 111.0 + random_f64(0.0, 2.0) + rect.y + rect.height;
                                    info!("COMPUTED_CLICK_COORDS x={x} y={y}");

                                    ctx.wait(random_u64(2345, 4567)).await;

                                    ctx.human().move_and_click(x, y).await?;
                                    if let Ok(png) = ctx.screenshot().await {
                                        let _ = std::fs::write("/tmp/after_captcha_click.png", png);
                                    }
                                    info!("CLICKED_CAPTCHA_CHECKBOX");

                                    ctx.wait_for_text(
                                        "Verifying you are human. This may take a few seconds.",
                                        3_000,
                                    )
                                    .await?;
                                    info!("CAPTCHA_IS_VERIFYING...");
                                    ctx.wait_for_network_idle(6000, 15_000).await?;
                                    info!("NETWORK_IS_IDLE");

                                    let deadline = Instant::now() + Duration::from_secs(15);
                                    let mut title_changed = false;

                                    while Instant::now() < deadline {
                                        let current_title = ctx.title().await?;

                                        if current_title != "Just a moment..." {
                                            title_changed = true;
                                            break;
                                        }
                                        ctx.wait(50).await;
                                    }

                                    if title_changed {
                                        info!("CAPTCHA_SOLVED!");
                                    } else {
                                        info!("CAPTCHA_NOT_SOLVED!!!");
                                    }
                                }
                                None => info!("CAPTCHA_TEXT_ELEMENT_HAS_NO_BOUNDING_BOX"),
                            }
                        }
                        Err(e) => info!("CAPTCHA_TEXT_ELEMENT_NOT_FOUND: {e}"),
                    }

                    Ok(Flow::Continue)
                })
            })),
            Step::ScreenShot {
                path: "/tmp/after_captcha.png".into(),
            },
        ],
    }
} */

pub async fn is_visible_in_list(target: &Element, list: &Element) -> eoka::Result<bool> {
    let Some(target_box) = target.bounding_box().await else {
        return Ok(false);
    };

    let Some(list_box) = list.bounding_box().await else {
        return Ok(false);
    };

    let target_left = target_box.x;
    let target_right = target_box.x + target_box.width;
    let target_top = target_box.y;
    let target_bottom = target_box.y + target_box.height;

    let list_left = list_box.x;
    let list_right = list_box.x + list_box.width;
    let list_top = list_box.y;
    let list_bottom = list_box.y + list_box.height;

    Ok(target_right > list_left
        && target_left < list_right
        && target_bottom > list_top
        && target_top < list_bottom)
}

pub async fn scroll_by(ctx: &eoka::Page, selector: &str, y: f64) -> eoka::Result<f64> {
    let sel = serde_json::to_string(selector).expect("string serializes");
    ctx.evaluate::<f64>(&format!(
        r#"(() => {{
            const list = document.querySelector({sel});
            if (!list) throw new Error("scroll container not found");
            list.scrollBy({{ top: {y}, left: 0, behavior: "instant" }});
            return list.scrollTop;
        }})()"#
    ))
    .await
}

pub async fn scroll_list_to_item(
    ctx: &eoka::Page,
    target_selector: &str,
    list_selector: &str,
) -> eoka::Result<Option<Element>> {
    const MAX_SCROLL_ATTEMPTS: usize = 40;
    const SETTLE_DELAY: Duration = Duration::from_millis(200);

    let list = ctx.wait_for_visible(list_selector, 3_000).await?;
    let target = ctx.find(target_selector).await?;
    let mut previous_scroll_top: Option<f64> = None;

    for _ in 0..=MAX_SCROLL_ATTEMPTS {
        let (Some(t), Some(l)) = (target.bounding_box().await, list.bounding_box().await) else {
            return Ok(None);
        };
        let center = t.y + t.height / 2.0;
        if center >= l.y && center <= l.y + l.height {
            return Ok(Some(target));
        }
        // scroll toward the target, not always down
        let dir = if center < l.y { -1.0 } else { 1.0 };
        let scroll_top = scroll_by(ctx, list_selector, dir * (l.height * 0.5).max(1.0)).await?;
        if previous_scroll_top.is_some_and(|old| (old - scroll_top).abs() < f64::EPSILON) {
            return Ok(None); // hit the top/bottom without finding it
        }
        previous_scroll_top = Some(scroll_top);
        tokio::time::sleep(SETTLE_DELAY).await;
    }
    Ok(None)
}
