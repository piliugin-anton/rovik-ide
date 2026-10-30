//! The agent's questions to the user (`ask_user`, Agent and Plan mode):
//! questions only the user can answer. The run waits for the answer in the
//! chat; it's the tool's result.

use super::tools::ToolResult;
use serde_json::{Map, Value};

/// Questions one run may ask; past that, the agent decides.
pub const MAX_QUESTIONS_PER_RUN: u32 = 3;

/// What an `ask_user` call past `MAX_QUESTIONS_PER_RUN` gets: the agent
/// decides, and says what it assumed (in Plan mode, in the plan).
pub fn past_the_question_limit(asked: u32, plan_mode: bool) -> String {
    let say = match plan_mode {
        true => "list your assumptions under \"Decisions\" in the plan",
        false => "say what you assumed",
    };
    format!("You've already asked {asked} times in this task. Decide the rest yourself and {say}.")
}

/*
 * ---------------------------------------------------------------------------
 * Questions (ask_user)
 * ---------------------------------------------------------------------------
 */

pub use rovik_protocol::plan::{Question, QuestionAnswer, QuestionOption, QuestionsOutcome};

const MAX_QUESTIONS: usize = 3;
const MAX_OPTIONS: usize = 6;

fn trimmed(v: Option<&Value>) -> Option<String> {
    super::permissions::trimmed_str(v).map(str::to_string)
}

/// The questions from the tool's arguments, tolerating the shapes models
/// produce: { questions: [...] }, or one question at the top level; options
/// as objects or plain strings.
pub fn parse_questions(args: &Map<String, Value>) -> Vec<Question> {
    let top = Value::Object(args.clone());
    let raw: Vec<&Value> = match args.get("questions").and_then(|q| q.as_array()) {
        Some(list) => list.iter().collect(),
        None => vec![&top],
    };
    let mut out = Vec::new();
    for q in raw.into_iter().take(MAX_QUESTIONS) {
        let Some(r) = q.as_object() else { continue };
        let Some(question) = trimmed(r.get("question")) else {
            continue;
        };
        let options: Vec<QuestionOption> = r
            .get("options")
            .and_then(|o| o.as_array())
            .into_iter()
            .flatten()
            .filter_map(|o| match o {
                Value::String(s) => {
                    let label = s.trim();
                    (!label.is_empty()).then(|| QuestionOption {
                        label: label.into(),
                        description: None,
                        recommended: false,
                    })
                }
                Value::Object(opt) => trimmed(opt.get("label")).map(|label| QuestionOption {
                    label,
                    description: opt
                        .get("description")
                        .and_then(|d| d.as_str())
                        .map(str::to_string),
                    recommended: opt.get("recommended") == Some(&Value::Bool(true)),
                }),
                _ => None,
            })
            .take(MAX_OPTIONS)
            .collect();
        // Without options the only way to answer is in words.
        let allow_custom = r.get("allow_custom") != Some(&Value::Bool(false)) || options.is_empty();
        out.push(Question {
            question,
            reason: trimmed(r.get("reason")),
            options,
            allow_custom,
        });
    }
    out
}

/// What the model reads as the tool's result. What the user leaves open,
/// the agent decides and says so: in Plan mode, in the plan.
pub fn describe_outcome(
    questions: &[Question],
    outcome: &QuestionsOutcome,
    plan_mode: bool,
) -> ToolResult {
    let (say_so, in_the_plan, next) = match plan_mode {
        true => ("say so in the plan", " in the plan", "Continue planning."),
        false => ("say what you assumed", "", "Continue with the task."),
    };
    let text = match outcome {
        QuestionsOutcome::Stopped { .. } => {
            return ToolResult::error("Error: the user stopped the task instead of answering.")
        }
        QuestionsOutcome::Reply { reply } => format!(
            "The user replied in their own words instead of picking options:\n\n{reply}\n\n\
             Treat this as their answer to your questions. For anything it leaves open, decide yourself and {say_so}."
        ),
        QuestionsOutcome::Answers { answers } => {
            let mut lines = vec!["The user answered:".to_string()];
            for (i, q) in questions.iter().enumerate() {
                let answer = match answers.get(i).unwrap_or(&QuestionAnswer::Agent) {
                    QuestionAnswer::Option { label } => label.clone(),
                    QuestionAnswer::Custom { text } => format!("\"{text}\" (in their own words)"),
                    QuestionAnswer::Agent => {
                        format!("left it to you: decide, and say what you chose and why{in_the_plan}")
                    }
                };
                lines.push(format!("{}. {}\n   → {answer}", i + 1, q.question));
            }
            lines.push(String::new());
            lines.push(format!("Treat these answers as requirements. {next}"));
            lines.join("\n")
        }
    };
    ToolResult::ok(text)
}

/// How each was answered, for the agent log.
pub fn outcome_kinds(outcome: &QuestionsOutcome) -> Vec<String> {
    match outcome {
        QuestionsOutcome::Answers { answers } => answers
            .iter()
            .map(|a| match a {
                QuestionAnswer::Option { .. } => "option",
                QuestionAnswer::Custom { .. } => "custom",
                QuestionAnswer::Agent => "agent",
            })
            .map(str::to_string)
            .collect(),
        QuestionsOutcome::Reply { .. } => vec!["reply".into()],
        QuestionsOutcome::Stopped { .. } => vec!["stopped".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn questions_in_the_shapes_models_send() {
        let args = json!({ "questions": [
            { "question": " Which DB? ", "options": ["Postgres", { "label": "SQLite", "recommended": true }], "allow_custom": false },
            { "question": "" },
            { "question": "Name?" }
        ]});
        let qs = parse_questions(args.as_object().unwrap());
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].question, "Which DB?");
        assert!(!qs[0].allow_custom && qs[0].options[1].recommended);
        assert!(qs[1].allow_custom, "no options: answered in words");
        let single = json!({ "question": "Top level?", "options": [] });
        assert_eq!(parse_questions(single.as_object().unwrap()).len(), 1);

        let outcome: QuestionsOutcome =
            serde_json::from_value(json!({ "answers": [{ "kind": "option", "label": "SQLite" }] }))
                .unwrap();
        let text = describe_outcome(&qs, &outcome, true).content;
        assert!(
            text.contains("1. Which DB?\n   → SQLite")
                && text.contains("2. Name?\n   → left it to you")
        );
        let stopped: QuestionsOutcome = serde_json::from_value(json!({ "stopped": true })).unwrap();
        let stopped_result = describe_outcome(&qs, &stopped, true);
        assert!(stopped_result.content.starts_with("Error:") && stopped_result.outcome.failed());
        assert_eq!(
            serde_json::to_value(&stopped).unwrap(),
            json!({ "stopped": true })
        );
    }

    #[test]
    fn question_edge_cases() {
        // At most 3 questions, 6 options; junk skipped.
        let many = json!({ "questions": [
            { "question": "1", "options": ["a","b","c","d","e","f","g"] },
            { "question": "2", "options": [1, null, { "label": " " }, { "label": "ok" }] },
            "not an object",
            { "question": "3" },
            { "question": "4" }
        ]});
        let qs = parse_questions(many.as_object().unwrap());
        assert_eq!(qs.len(), 2, "the string and the 4th are out");
        assert_eq!(qs[0].options.len(), 6);
        assert_eq!(
            qs[1]
                .options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            ["ok"]
        );
        assert!(parse_questions(json!({}).as_object().unwrap()).is_empty());
        // Fewer answers than questions: the rest are left to the agent.
        let qs = parse_questions(
            json!({ "questions": [{ "question": "A?" }, { "question": "B?" }] })
                .as_object()
                .unwrap(),
        );
        let outcome = QuestionsOutcome::Answers {
            answers: vec![QuestionAnswer::Custom {
                text: "mine".into(),
            }],
        };
        let text = describe_outcome(&qs, &outcome, true).content;
        assert!(
            text.contains("\"mine\" (in their own words)")
                && text.contains("B?\n   → left it to you")
        );
        assert_eq!(outcome_kinds(&outcome), ["custom"]);
    }

    /// In Agent mode there's no plan: what the user leaves open, the agent
    /// decides and says in its reply, and it goes on with the task.
    #[test]
    fn answers_in_agent_mode_point_at_the_reply_not_a_plan() {
        let qs = parse_questions(
            json!({ "questions": [{ "question": "A?" }, { "question": "B?" }] })
                .as_object()
                .unwrap(),
        );
        let answers = QuestionsOutcome::Answers {
            answers: vec![QuestionAnswer::Option {
                label: "Yes".into(),
            }],
        };
        let reply = QuestionsOutcome::Reply {
            reply: "Up to you.".into(),
        };
        for outcome in [&answers, &reply] {
            let plan = describe_outcome(&qs, outcome, true).content;
            assert!(plan.contains("in the plan"), "{plan}");
            let agent = describe_outcome(&qs, outcome, false).content;
            assert!(!agent.contains("plan"), "{agent}");
        }
        let agent = describe_outcome(&qs, &answers, false).content;
        assert!(
            agent.contains("B?\n   → left it to you: decide, and say what you chose and why\n")
                && agent.ends_with("Treat these answers as requirements. Continue with the task."),
            "{agent}"
        );
        assert!(describe_outcome(&qs, &reply, false)
            .content
            .ends_with("For anything it leaves open, decide yourself and say what you assumed."));
        assert_eq!(
            past_the_question_limit(3, false),
            "You've already asked 3 times in this task. Decide the rest yourself and say what you assumed."
        );
        assert!(past_the_question_limit(3, true).ends_with("under \"Decisions\" in the plan."));
    }
}
