//! What an Autopilot task spends, and its budget (D16, U04): the time its
//! runs ran and the cost their providers reported, against the limits in
//! the Autopilot settings ("Stop a task after [hours] or [cost]").
//!
//! - A task's lead run adds up what its replies, its subagents' included,
//!   cost as it goes (`AgentState::note_reply`), and the time since it
//!   started; each stage record keeps what its run spent (`spent`), so the
//!   task's total survives a restart (a run a crash cut loses its part).
//! - The run checks the total after every reply, and the driver before
//!   each stage; past a limit, the task ends with the outcome `budget`, and
//!   delivers what it has.
//! - The cost is what the providers say a request cost (OpenRouter does).
//!   A reply whose provider says nothing isn't in it, and is counted
//!   (`uncosted`): with such a provider only the time limit is kept.
//!
//! The headless runner's accounting of a run's log (`Totals`) is here too,
//! with its prices for providers that report no cost.

use super::log::usage_counts;
use rovik_protocol::autopilot::Budget;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a task's runs spent: the time they ran, and the cost their
/// providers reported.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Spent {
    /// Milliseconds its runs ran.
    pub ms: u64,
    /// In the provider's currency, from the replies whose provider said.
    pub cost: f64,
    /// Replies whose provider said no cost: `cost` leaves them out.
    pub uncosted: u32,
}

impl Spent {
    pub fn plus(self, other: Spent) -> Spent {
        Spent {
            ms: self.ms + other.ms,
            cost: self.cost + other.cost,
            uncosted: self.uncosted + other.uncosted,
        }
    }

    /// One reply's usage, as its provider reported it (none: nothing said).
    pub fn note_reply(&mut self, usage: Option<&Value>) {
        match usage.and_then(|u| usage_counts(u).2) {
            Some(cost) if cost.is_finite() && cost >= 0.0 => self.cost += cost,
            _ => self.uncosted += 1,
        }
    }
}

/// A limit that limits: a positive, finite number.
fn limit(v: Option<f64>) -> Option<f64> {
    v.filter(|v| v.is_finite() && *v > 0.0)
}

/// Why a task that spent `spent` is past its budget, if it is: the first
/// limit it reached, in words ("it ran for 2 h, past its limit of 2 h").
pub fn over(budget: &Budget, spent: &Spent) -> Option<String> {
    if let Some(hours) = limit(budget.max_hours) {
        if spent.ms as f64 >= hours * 3_600_000.0 {
            let ran = std::time::Duration::from_millis(spent.ms);
            return Some(format!(
                "it ran for {}, past its limit of {}",
                crate::chat_request::human_duration(ran),
                hours_text(hours)
            ));
        }
    }
    if let Some(cost) = limit(budget.max_cost) {
        if spent.cost >= cost {
            return Some(format!(
                "it cost {:.2}, past its limit of {cost:.2}",
                spent.cost
            ));
        }
    }
    None
}

/// "2 h", "1.5 h", "30 min".
fn hours_text(hours: f64) -> String {
    if hours < 1.0 {
        format!("{} min", (hours * 60.0).round() as u64)
    } else if hours.fract() == 0.0 {
        format!("{hours} h")
    } else {
        format!("{hours:.1} h")
    }
}

/*
 * ---------------------------------------------------------------------------
 * A run's log, as the headless runner reports it
 * ---------------------------------------------------------------------------
 */

/// US dollars per million tokens.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

/// Tokens in and out, and the cost when the provider says, of one reply's
/// usage (OpenAI's names, or the Responses API's).
pub fn usage_of(u: &Value) -> (u64, u64, Option<f64>) {
    let (i, o, cost) = usage_counts(u);
    (i.unwrap_or(0), o.unwrap_or(0), cost)
}

/// What a run's requests used, from its run log's reply lines.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Totals {
    pub lead_in: u64,
    pub lead_out: u64,
    pub sub_in: u64,
    pub sub_out: u64,
    /// The largest request the lead sent (its context at its fullest).
    pub peak_lead_context: u64,
    /// Of the prompt tokens, those the provider served from its cache
    /// (prompt_cache.rs); 0 when it doesn't say.
    pub lead_cached: u64,
    pub sub_cached: u64,
    /// Reported cost, when every reply with usage reported one.
    pub reported_cost: Option<f64>,
    /// Replies with usage.
    pub replies: usize,
    /// Replies that came back whole without usage: the provider doesn't
    /// report it, so neither tokens nor cost are known.
    pub missing_usage: usize,
}

impl Totals {
    pub fn tokens(&self) -> u64 {
        self.lead_in + self.lead_out + self.sub_in + self.sub_out
    }

    /// One reply line of the run log: the lead's (`dialog_id`) or a
    /// subagent's.
    pub fn add(&mut self, line: &Value, dialog_id: &str) {
        if line["event"] != "reply" {
            return;
        }
        // P6.2: an object without token counts reports nothing either.
        let counted = |u: &&Value| {
            let (i, o, _) = usage_counts(u);
            i.is_some() || o.is_some()
        };
        let Some(u) = line.get("usage").filter(|u| u.is_object()).filter(counted) else {
            // A request that failed or was cut short may have none.
            if line.get("error").is_none() {
                self.missing_usage += 1;
            }
            return;
        };
        let (i, o, cost) = usage_of(u);
        let cached = crate::prompt_cache::cached_tokens(u).unwrap_or(0);
        if line["dialogId"] == dialog_id {
            self.lead_in += i;
            self.lead_out += o;
            self.lead_cached += cached;
            self.peak_lead_context = self.peak_lead_context.max(i);
        } else {
            self.sub_in += i;
            self.sub_out += o;
            self.sub_cached += cached;
        }
        self.reported_cost = match (self.replies, self.reported_cost, cost) {
            (0, _, c) => c,
            (_, Some(a), Some(c)) => Some(a + c),
            _ => None,
        };
        self.replies += 1;
    }

    /// The share of the lead's prompt tokens served from the provider's
    /// cache, in percent; None before any prompt tokens were counted.
    pub fn cache_hit_percent(&self) -> Option<u64> {
        (self.lead_in > 0).then(|| self.lead_cached * 100 / self.lead_in)
    }

    /// Its cost: as reported, else by the settings' price; None: unknown.
    pub fn cost(&self, price: Option<Price>) -> (Option<f64>, &'static str) {
        // Some replies' tokens aren't known: nor is the cost.
        if self.missing_usage > 0 {
            return (None, "unknown");
        }
        if let Some(c) = self.reported_cost {
            return (Some(c), "provider");
        }
        match price {
            Some(p) => (
                Some(
                    (self.lead_in + self.sub_in) as f64 * p.input_per_million / 1e6
                        + (self.lead_out + self.sub_out) as f64 * p.output_per_million / 1e6,
                ),
                "price",
            ),
            None if self.replies == 0 => (Some(0.0), "none"),
            None => (None, "unknown"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_reply_adds_its_cost_or_counts_as_uncosted() {
        let mut spent = Spent::default();
        spent.note_reply(Some(
            &json!({ "prompt_tokens": 10, "completion_tokens": 2, "cost": 0.25 }),
        ));
        spent.note_reply(Some(
            &json!({ "prompt_tokens": 10, "completion_tokens": 2 }),
        ));
        spent.note_reply(None);
        spent.note_reply(Some(&json!({ "cost": -1.0 })));
        assert_eq!(spent.cost, 0.25);
        assert_eq!(spent.uncosted, 3);
        let more = Spent {
            ms: 1_000,
            cost: 1.0,
            uncosted: 1,
        };
        assert_eq!(
            spent.plus(more),
            Spent {
                ms: 1_000,
                cost: 1.25,
                uncosted: 4
            }
        );
    }

    #[test]
    fn a_task_is_over_its_budget_at_the_first_limit_it_reaches() {
        let budget = Budget {
            max_hours: Some(2.0),
            max_cost: Some(5.0),
        };
        let hour = 3_600_000;
        assert_eq!(
            over(
                &budget,
                &Spent {
                    ms: hour,
                    cost: 4.99,
                    uncosted: 0
                }
            ),
            None
        );
        assert_eq!(
            over(
                &budget,
                &Spent {
                    ms: 2 * hour,
                    cost: 0.0,
                    uncosted: 0
                }
            )
            .as_deref(),
            Some("it ran for 2 h, past its limit of 2 h")
        );
        assert_eq!(
            over(
                &budget,
                &Spent {
                    ms: 0,
                    cost: 5.0,
                    uncosted: 0
                }
            )
            .as_deref(),
            Some("it cost 5.00, past its limit of 5.00")
        );
        // No limit, or a limit that isn't one.
        let none = Budget::default();
        assert_eq!(
            over(
                &none,
                &Spent {
                    ms: 100 * hour,
                    cost: 1e9,
                    uncosted: 0
                }
            ),
            None
        );
        let odd = Budget {
            max_hours: Some(0.0),
            max_cost: Some(f64::NAN),
        };
        assert_eq!(
            over(
                &odd,
                &Spent {
                    ms: hour,
                    cost: 1.0,
                    uncosted: 0
                }
            ),
            None
        );
        let half = Budget {
            max_hours: Some(0.5),
            max_cost: None,
        };
        assert_eq!(
            over(
                &half,
                &Spent {
                    ms: hour,
                    ..Spent::default()
                }
            )
            .as_deref(),
            Some("it ran for 1 h, past its limit of 30 min")
        );
    }
}
