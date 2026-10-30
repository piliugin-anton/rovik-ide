//! The tools Rovik serves the model, one row each: what a tool is and may
//! do, whatever the run (docs/design/backend-refactoring-plan.md, phase 2).
//! Dispatch reads it, and so do approval, the restricted files, the test
//! writer's hidden files, the allowed hosts, the modes, the checklist gate,
//! parallel runs, the agent's context folder and the subagent kinds' tools.
//! A new tool is a variant, its row and its schema: a variant without a row
//! doesn't build, and a test checks each has one schema and a dispatcher.
//!
//! What depends on the run stays code: whether a tool is offered (the
//! project, vision, settings, CLM, Autopilot, the lead or a kind), its
//! schema's text, approval decisions from the settings, and refusals that
//! depend on its arguments. Names stay strings on the wire and in the
//! database; `parse` reads them, and a name that isn't here (an MCP
//! server's tool through a harness, a typo) is no built-in tool.

use crate::restricted_files::Op;

/// A tool Rovik serves to the model, by its name on the wire. An MCP
/// server's tools go through `Mcp`; a harness's own tools aren't these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinTool {
    Read,
    Write,
    ListDir,
    SearchCode,
    Neighbours,
    Callers,
    Outline,
    Diagnostics,
    WebSearch,
    BrowseUrl,
    BrowserScreenshot,
    Bash,
    Background,
    AskUser,
    Todo,
    SubmitPlan,
    RestoreContext,
    Task,
    ExtendScope,
    AnswerReview,
    Mcp,
}

/// What a call of the tool does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Reads the project's files: changes nothing.
    Reads,
    /// Changes the project's files.
    Writes,
    /// Runs a command, which may do whatever the user's settings let it.
    Runs,
    /// Reads the web.
    Web,
    /// The run's own business: questions, the checklist, the plan, the
    /// agent's context, subagents and their scope, answers to a review,
    /// the commands it left running.
    Control,
    /// Calls an MCP server's tool, which does whatever that server does.
    Mcp,
}

/// Which of the run's dispatchers takes a call of the tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    /// `execute_tool_calls`: subagents, started together.
    Subagents,
    /// `execute_one`: the run's own tools, answered with the run's state.
    Run,
    /// `tools::run`: the project's, the web's and the commands' tools.
    Tools,
}

/// The modes a tool works in. A call in a mode that doesn't offer it is
/// refused: a model may call it from an earlier turn's history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modes {
    Any,
    PlanOnly,
    AgentOnly,
}

impl Modes {
    /// Whether a run in Plan mode (`plan_mode`), or in Agent mode, has it.
    pub fn offered_in(self, plan_mode: bool) -> bool {
        match self {
            Modes::Any => true,
            Modes::PlanOnly => plan_mode,
            Modes::AgentOnly => !plan_mode,
        }
    }
}

/// How a call keeps to the restricted files (restricted_files.rs): each
/// check that applies to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Restricted {
    /// Its `path` is refused when the agent may not do this with the file.
    pub path: Option<Op>,
    /// Its `command` is read for the files it would read or change.
    pub command: bool,
    /// What it shows of other files leaves the restricted ones out.
    pub results: bool,
}

impl Restricted {
    /// No check: the tool doesn't touch the project's files' contents.
    pub const NONE: Restricted = Restricted {
        path: None,
        command: false,
        results: false,
    };
}

/// How a call keeps the test writer from the change's code (K11, P2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hidden {
    /// Its `path` is refused when it's one of the hidden files.
    pub path: bool,
    /// What it shows of other files leaves the hidden ones out.
    pub results: bool,
}

impl Hidden {
    /// Nothing hidden: the tool shows no file's code (an outline is the
    /// interface the test writer works from).
    pub const NONE: Hidden = Hidden {
        path: false,
        results: false,
    };
}

/// What the user's allowed hosts (allowed-hosts-plan.md, J02) check of a
/// call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCheck {
    None,
    /// The search engine it asks.
    SearchEngine,
    /// The host of the page its `url` names.
    UrlArgument,
}

/// What a tool is and may do, whatever the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolSpec {
    /// Its name on the wire and in the database.
    pub name: &'static str,
    pub effect: Effect,
    pub dispatch: Dispatch,
    pub modes: Modes,
    /// Refused without an open project folder.
    pub needs_project: bool,
    /// A local read: it may run beside other such calls (and an MCP client
    /// is told it changes nothing).
    pub parallel: bool,
    /// It works on the agent's context folder too (`@context/`, CLM mode).
    pub context_folder: bool,
    pub restricted: Restricted,
    pub hidden: Hidden,
    pub host: HostCheck,
}

/// What a row doesn't say: one of the run's own tools, in any mode, with
/// nothing to check.
const BASE: ToolSpec = ToolSpec {
    name: "",
    effect: Effect::Control,
    dispatch: Dispatch::Run,
    modes: Modes::Any,
    needs_project: false,
    parallel: false,
    context_folder: false,
    restricted: Restricted::NONE,
    hidden: Hidden::NONE,
    host: HostCheck::None,
};

/// A read of the project's files.
const LOCAL_READ: ToolSpec = ToolSpec {
    effect: Effect::Reads,
    dispatch: Dispatch::Tools,
    needs_project: true,
    parallel: true,
    ..BASE
};

/// A tool that reads the web.
const WEB: ToolSpec = ToolSpec {
    effect: Effect::Web,
    dispatch: Dispatch::Tools,
    ..BASE
};

/// A `path` the agent may not read.
const READ_PATH: Restricted = Restricted {
    path: Some(Op::Read),
    ..Restricted::NONE
};

impl BuiltinTool {
    pub const ALL: [BuiltinTool; 21] = [
        BuiltinTool::Read,
        BuiltinTool::Write,
        BuiltinTool::ListDir,
        BuiltinTool::SearchCode,
        BuiltinTool::Neighbours,
        BuiltinTool::Callers,
        BuiltinTool::Outline,
        BuiltinTool::Diagnostics,
        BuiltinTool::WebSearch,
        BuiltinTool::BrowseUrl,
        BuiltinTool::BrowserScreenshot,
        BuiltinTool::Bash,
        BuiltinTool::Background,
        BuiltinTool::AskUser,
        BuiltinTool::Todo,
        BuiltinTool::SubmitPlan,
        BuiltinTool::RestoreContext,
        BuiltinTool::Task,
        BuiltinTool::ExtendScope,
        BuiltinTool::AnswerReview,
        BuiltinTool::Mcp,
    ];

    /// The seven tools that read the project, which most subagent kinds
    /// work with.
    pub const READS: [BuiltinTool; 7] = [
        BuiltinTool::Read,
        BuiltinTool::ListDir,
        BuiltinTool::SearchCode,
        BuiltinTool::Neighbours,
        BuiltinTool::Callers,
        BuiltinTool::Outline,
        BuiltinTool::Diagnostics,
    ];

    /// Its row: one exhaustive match, so a new tool doesn't build without.
    pub fn spec(self) -> ToolSpec {
        use BuiltinTool::*;
        match self {
            Read => ToolSpec {
                name: "read",
                context_folder: true,
                restricted: READ_PATH,
                hidden: Hidden {
                    path: true,
                    results: false,
                },
                ..LOCAL_READ
            },
            // Names only, no contents: nothing restricted or hidden shows.
            ListDir => ToolSpec {
                name: "list_dir",
                context_folder: true,
                ..LOCAL_READ
            },
            SearchCode => ToolSpec {
                name: "search_code",
                restricted: Restricted {
                    results: true,
                    ..Restricted::NONE
                },
                hidden: Hidden {
                    path: false,
                    results: true,
                },
                ..LOCAL_READ
            },
            Neighbours => ToolSpec {
                name: "neighbours",
                restricted: Restricted {
                    results: true,
                    ..READ_PATH
                },
                hidden: Hidden {
                    path: true,
                    results: true,
                },
                ..LOCAL_READ
            },
            Callers => ToolSpec {
                name: "callers",
                restricted: Restricted {
                    results: true,
                    ..Restricted::NONE
                },
                hidden: Hidden {
                    path: false,
                    results: true,
                },
                ..LOCAL_READ
            },
            Outline => ToolSpec {
                name: "outline",
                restricted: READ_PATH,
                ..LOCAL_READ
            },
            Diagnostics => ToolSpec {
                name: "diagnostics",
                restricted: READ_PATH,
                hidden: Hidden {
                    path: true,
                    results: true,
                },
                ..LOCAL_READ
            },
            Write => ToolSpec {
                name: "write",
                effect: Effect::Writes,
                dispatch: Dispatch::Tools,
                modes: Modes::AgentOnly,
                needs_project: true,
                context_folder: true,
                restricted: Restricted {
                    path: Some(Op::Write),
                    ..Restricted::NONE
                },
                ..BASE
            },
            WebSearch => ToolSpec {
                name: "web_search",
                host: HostCheck::SearchEngine,
                ..WEB
            },
            BrowseUrl => ToolSpec {
                name: "browse_url",
                host: HostCheck::UrlArgument,
                ..WEB
            },
            BrowserScreenshot => ToolSpec {
                name: "browser_screenshot",
                host: HostCheck::UrlArgument,
                ..WEB
            },
            Bash => ToolSpec {
                name: "bash",
                effect: Effect::Runs,
                dispatch: Dispatch::Tools,
                modes: Modes::AgentOnly,
                needs_project: true,
                restricted: Restricted {
                    command: true,
                    ..Restricted::NONE
                },
                ..BASE
            },
            // The commands bash left running: their output, or a stop.
            Background => ToolSpec {
                name: "background",
                dispatch: Dispatch::Tools,
                modes: Modes::AgentOnly,
                needs_project: true,
                ..BASE
            },
            AskUser => ToolSpec {
                name: "ask_user",
                ..BASE
            },
            Todo => ToolSpec {
                name: "todo",
                modes: Modes::AgentOnly,
                ..BASE
            },
            SubmitPlan => ToolSpec {
                name: "submit_plan",
                modes: Modes::PlanOnly,
                ..BASE
            },
            RestoreContext => ToolSpec {
                name: "restore_context",
                ..BASE
            },
            Task => ToolSpec {
                name: "task",
                dispatch: Dispatch::Subagents,
                ..BASE
            },
            ExtendScope => ToolSpec {
                name: "extend_scope",
                modes: Modes::AgentOnly,
                ..BASE
            },
            AnswerReview => ToolSpec {
                name: "answer_review",
                modes: Modes::AgentOnly,
                ..BASE
            },
            Mcp => ToolSpec {
                name: super::mcp_tool::NAME,
                effect: Effect::Mcp,
                ..BASE
            },
        }
    }

    /// Its name on the wire.
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    /// The tool a call names, if it's one of Rovik's.
    pub fn parse(name: &str) -> Option<BuiltinTool> {
        BuiltinTool::ALL.into_iter().find(|t| t.name() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_parses_back_to_its_tool_once() {
        for tool in BuiltinTool::ALL {
            assert_eq!(BuiltinTool::parse(tool.name()), Some(tool));
            let same = BuiltinTool::ALL.iter().filter(|t| t.name() == tool.name());
            assert_eq!(same.count(), 1, "{tool:?}");
        }
        assert_eq!(BuiltinTool::parse("Bash"), None, "a harness's name");
        assert_eq!(BuiltinTool::parse("mcp__github__create_issue"), None);
    }

    /// Only the tools that read the project run beside each other: the
    /// browser's share one tab, and everything else changes something or
    /// asks someone.
    #[test]
    fn only_local_reads_run_in_parallel() {
        for tool in BuiltinTool::ALL {
            let spec = tool.spec();
            assert_eq!(spec.parallel, spec.effect == Effect::Reads, "{tool:?}");
            assert_eq!(
                spec.parallel,
                BuiltinTool::READS.contains(&tool),
                "{tool:?}"
            );
        }
    }

    #[test]
    fn modes_offer_what_they_say() {
        assert!(Modes::Any.offered_in(true) && Modes::Any.offered_in(false));
        assert!(Modes::PlanOnly.offered_in(true) && !Modes::PlanOnly.offered_in(false));
        assert!(!Modes::AgentOnly.offered_in(true) && Modes::AgentOnly.offered_in(false));
    }
}
