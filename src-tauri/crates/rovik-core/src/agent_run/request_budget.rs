//! Admission for requests sharing a run's token limits. Estimates reserve
//! capacity before dispatch; reported usage replaces them when available.
//! This bounds admitted work, not a provider's billing: tokenization is an
//! estimate and an opaque harness may run additional model turns itself.

use std::sync::{Arc, Mutex, MutexGuard};

/// The reply allowance when a request does not specify its own limit.
pub const DEFAULT_OUTPUT_TOKENS: u64 = 4_096;
const MIN_OUTPUT_TOKENS: u64 = 256;

/// Estimates the actual adapter body after history, schemas and route
/// translation. Image and document contents use an allowance rather than
/// counting their encoded bytes as text; the provider's usage settles it.
pub fn prompt_tokens(body: &serde_json::Value, chars_per_token: f64) -> u64 {
    fn size(value: &serde_json::Value) -> (u64, u64) {
        use serde_json::Value;
        match value {
            Value::String(text) => (text.chars().count() as u64, 0),
            Value::Array(values) => values.iter().map(size).fold((0u64, 0u64), |a, b| {
                (a.0.saturating_add(b.0), a.1.saturating_add(b.1))
            }),
            Value::Object(fields) => {
                if fields
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| {
                        matches!(
                            kind,
                            "image"
                                | "image_url"
                                | "input_image"
                                | "document"
                                | "file"
                                | "input_file"
                        )
                    })
                {
                    return (0, 1_000);
                }
                fields.iter().fold((0u64, 4u64), |a, (key, value)| {
                    let b = size(value);
                    (
                        a.0.saturating_add(key.len() as u64).saturating_add(b.0),
                        a.1.saturating_add(b.1),
                    )
                })
            }
            _ => (4, 0),
        }
    }
    let (chars, fixed) = ["messages", "input", "instructions", "system", "tools"]
        .iter()
        .filter_map(|key| body.get(*key))
        .map(size)
        .fold((0u64, 0u64), |a, b| {
            (a.0.saturating_add(b.0), a.1.saturating_add(b.1))
        });
    let ratio = if chars_per_token.is_finite() && chars_per_token > 0.0 {
        chars_per_token
    } else {
        3.0
    };
    ((chars as f64 / ratio).ceil() as u64)
        .saturating_add(fixed)
        .max(1)
}

#[derive(Debug, Default)]
struct Counter {
    consumed: u64,
    reserved: u64,
}

#[derive(Debug, Default)]
struct State {
    task: Counter,
    children: Counter,
    before: u64,
    task_limit: Option<u64>,
    child_limit: Option<u64>,
    task_denied: Option<String>,
}

/// One run's counters, shared by its lead and all descendant requests.
#[derive(Debug, Default)]
pub struct Ledger(Mutex<State>);

impl Ledger {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sets the applicable limits without resetting usage or reservations.
    pub fn configure(&self, task: Option<u64>, children: Option<u64>, before: u64) {
        let mut state = self.lock();
        state.task_limit = task.filter(|n| *n > 0);
        state.child_limit = children.filter(|n| *n > 0);
        state.before = before;
    }

    /// Settled tokens in this run, including conservative unknown usage.
    /// The second count contains just its descendants' requests.
    pub fn spending(&self) -> (u64, u64) {
        let state = self.lock();
        (state.task.consumed, state.children.consumed)
    }

    /// Adds reported usage from a turn that had no admission handle. Such
    /// turns may overlap requests limited only by the child budget, so their
    /// usage must be added rather than merged with settled estimates by max.
    pub fn record_unadmitted(&self, tokens: u64, child: bool) {
        let mut state = self.lock();
        state.task.consumed = state.task.consumed.saturating_add(tokens);
        if child {
            state.children.consumed = state.children.consumed.saturating_add(tokens);
        }
    }

    /// Restores settled counts at a verification boundary. Reservations
    /// are process-local and are never restored.
    pub fn restore(&self, task: u64, children: u64) {
        let mut state = self.lock();
        state.task.consumed = state.task.consumed.max(task);
        state.children.consumed = state.children.consumed.max(children);
    }

    /// A task limit that refused a request, for the run's budget outcome.
    pub fn task_denied(&self) -> Option<String> {
        self.lock().task_denied.clone()
    }

    /// A handle for requests belonging to the lead or one of its children.
    /// No handle means the current configuration places no token limit on it.
    pub fn admission(self: &Arc<Self>, child: bool) -> Option<Admission> {
        let state = self.lock();
        (state.task_limit.is_some() || (child && state.child_limit.is_some())).then(|| Admission {
            ledger: self.clone(),
            child,
            chars_per_token: 3.0,
        })
    }
}

/// Request admission independent of the transport or model provider.
#[derive(Clone, Debug)]
pub struct Admission {
    ledger: Arc<Ledger>,
    child: bool,
    chars_per_token: f64,
}

impl Admission {
    /// Uses the prompt calibration learned for this model, while retaining
    /// the same shared counters across retries and provider route changes.
    pub fn with_chars_per_token(mut self, ratio: f64) -> Self {
        self.chars_per_token = ratio;
        self
    }

    /// Estimates the prepared adapter body using this request's calibration.
    pub fn estimate(&self, body: &serde_json::Value) -> u64 {
        prompt_tokens(body, self.chars_per_token)
    }

    /// Atomically reserves input plus a reply allowance against every
    /// applicable scope. A declined request does not wait behind another
    /// request while holding a worker slot.
    pub fn reserve(&self, input: u64, wanted_output: u64) -> Result<Reservation, String> {
        let mut state = self.ledger.lock();
        let wanted_output = wanted_output.max(1).min(u32::MAX as u64);
        let minimum = wanted_output.min(MIN_OUTPUT_TOKENS);
        let mut output = wanted_output;
        for (name, limit, used) in [
            (
                "task",
                state.task_limit,
                state
                    .before
                    .saturating_add(state.task.consumed)
                    .saturating_add(state.task.reserved),
            ),
            (
                "subagents",
                state.child_limit.filter(|_| self.child),
                state
                    .children
                    .consumed
                    .saturating_add(state.children.reserved),
            ),
        ] {
            let Some(limit) = limit else { continue };
            let remaining = limit.saturating_sub(used);
            if remaining < input.saturating_add(minimum) {
                let message = format!(
                    "Token budget for {name} cannot admit this request: {used} of {limit} tokens are spent or reserved; it needs an estimated {input} input tokens and at least {minimum} output tokens."
                );
                if name == "task" {
                    state.task_denied = Some(message.clone());
                }
                return Err(message);
            }
            output = output.min(remaining - input);
        }
        let reserved = input.saturating_add(output);
        state.task.reserved = state.task.reserved.saturating_add(reserved);
        if self.child {
            state.children.reserved = state.children.reserved.saturating_add(reserved);
        }
        Ok(Reservation {
            admission: self.clone(),
            input,
            output,
            reported_input: None,
            reported_output: None,
            reported_total: None,
            dispatched: false,
            refunded: false,
        })
    }
}

/// An attempt's reservation. Dropping it always releases capacity; after
/// dispatch it also charges usage, or its estimate when usage is unknown.
pub struct Reservation {
    admission: Admission,
    input: u64,
    output: u64,
    reported_input: Option<u64>,
    reported_output: Option<u64>,
    reported_total: Option<u64>,
    dispatched: bool,
    refunded: bool,
}

impl Reservation {
    /// The maximum output to request from an adapter that supports it.
    pub fn output_tokens(&self) -> u64 {
        self.output
    }

    /// From this point, cancellation or a dropped future may have cost
    /// tokens. Mark immediately before the adapter starts its request.
    pub fn dispatched(&mut self) {
        self.dispatched = true;
    }

    /// Cumulative usage may arrive more than once. Count it once and keep
    /// the largest reported value for each component.
    pub fn observe(&mut self, usage: &serde_json::Value) {
        let (input, output, _) = super::log::usage_counts(usage);
        self.reported_input = self.reported_input.max(input);
        self.reported_output = self.reported_output.max(output);
        self.reported_total = self
            .reported_total
            .max(usage.get("total_tokens").and_then(|v| v.as_u64()));
    }

    /// A definitive HTTP refusal before any generation used no tokens.
    /// Network failures and cancellations are not such refusals.
    pub fn rejected(&mut self) {
        self.refunded = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let reserved = self.input.saturating_add(self.output);
        let consumed = if !self.dispatched || self.refunded {
            0
        } else if let Some(total) = self.reported_total {
            total.max(
                self.reported_input
                    .unwrap_or(0)
                    .saturating_add(self.reported_output.unwrap_or(0)),
            )
        } else {
            self.reported_input
                .unwrap_or(self.input)
                .saturating_add(self.reported_output.unwrap_or(self.output))
        };
        let mut state = self.admission.ledger.lock();
        state.task.reserved = state.task.reserved.saturating_sub(reserved);
        state.task.consumed = state.task.consumed.saturating_add(consumed);
        if self.admission.child {
            state.children.reserved = state.children.reserved.saturating_sub(reserved);
            state.children.consumed = state.children.consumed.saturating_add(consumed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Barrier;

    fn ledger(task: Option<u64>, children: Option<u64>) -> Arc<Ledger> {
        let ledger = Arc::new(Ledger::default());
        ledger.configure(task, children, 0);
        ledger
    }

    #[test]
    fn concurrent_children_cannot_reserve_the_same_remaining_tokens() {
        let ledger = ledger(Some(1_000), Some(1_000));
        let start = Arc::new(Barrier::new(9));
        let reserved = Arc::new(Barrier::new(9));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let admission = ledger.admission(true).unwrap();
                let (start, reserved) = (start.clone(), reserved.clone());
                std::thread::spawn(move || {
                    start.wait();
                    let permit = admission.reserve(300, 300).ok();
                    reserved.wait();
                    permit.is_some()
                })
            })
            .collect();
        start.wait();
        reserved.wait();
        assert_eq!(
            workers
                .into_iter()
                .map(|w| w.join().unwrap())
                .filter(|admitted| *admitted)
                .count(),
            1
        );
        assert_eq!(ledger.spending(), (0, 0));
        assert_eq!(ledger.lock().task.reserved, 0);
    }

    #[test]
    fn roots_are_independent_and_child_limits_do_not_limit_the_lead() {
        let first = ledger(None, Some(600));
        let second = ledger(None, Some(600));
        assert!(first.admission(false).is_none());
        let _a = first.admission(true).unwrap().reserve(300, 300).unwrap();
        assert!(first.admission(true).unwrap().reserve(1, 300).is_err());
        assert!(second.admission(true).unwrap().reserve(300, 300).is_ok());
    }

    #[test]
    fn reported_usage_reconciles_once_and_unknown_components_stay_reserved_as_spend() {
        let ledger = ledger(Some(5_000), None);
        let admission = ledger.admission(false).unwrap();
        {
            let mut r = admission.reserve(100, 300).unwrap();
            r.dispatched();
            r.observe(&json!({"input_tokens": 40}));
            r.observe(&json!({"input_tokens": 40}));
        }
        assert_eq!(ledger.spending().0, 340);
        {
            let mut r = admission.reserve(100, 300).unwrap();
            r.dispatched();
            r.observe(&json!({"prompt_tokens": 120, "completion_tokens": 500}));
        }
        assert_eq!(
            ledger.spending().0,
            960,
            "provider overspend is never clamped to the estimate"
        );
    }

    #[test]
    fn cancellation_before_dispatch_refunds_but_after_dispatch_charges_unknown_usage() {
        let ledger = ledger(Some(1_000), None);
        let admission = ledger.admission(false).unwrap();
        drop(admission.reserve(200, 300).unwrap());
        assert_eq!(ledger.spending().0, 0);
        {
            let mut r = admission.reserve(200, 300).unwrap();
            r.dispatched();
        }
        assert_eq!(ledger.spending().0, 500);
        assert_eq!(ledger.lock().task.reserved, 0);
    }

    #[test]
    fn refusal_refunds_and_retries_reserve_again_against_spent_tokens() {
        let ledger = ledger(Some(1_000), None);
        let admission = ledger.admission(false).unwrap();
        {
            let mut r = admission.reserve(400, 400).unwrap();
            r.dispatched();
            r.rejected();
        }
        assert_eq!(ledger.spending().0, 0);
        {
            let mut r = admission.reserve(400, 400).unwrap();
            r.dispatched();
        }
        assert!(admission.reserve(400, 400).is_err());
        assert_eq!(ledger.spending().0, 800);
    }

    #[test]
    fn previous_task_stages_and_restored_children_reduce_available_capacity() {
        let ledger = ledger(Some(2_000), Some(1_000));
        ledger.configure(Some(2_000), Some(1_000), 1_000);
        ledger.restore(300, 300);
        ledger.restore(100, 100);
        let reservation = ledger.admission(true).unwrap().reserve(200, 1_000).unwrap();
        assert_eq!(reservation.output_tokens(), 500);
        assert!(ledger.admission(false).unwrap().reserve(1, 300).is_err());
    }
}
