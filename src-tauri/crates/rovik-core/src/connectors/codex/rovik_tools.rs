//! Rovik's tools mode with Codex (connectors/live.rs): Codex is the model
//! behind Rovik's agent loop. Its app server starts, or continues the
//! chat's thread, with Rovik's tools served over MCP (connectors/mcp.rs) as
//! functions of their own, Rovik's prompt in place of Codex's, and runs a
//! turn with the chat's message. A turn of Rovik's lasts until Codex calls
//! one of Rovik's tools: it ends with that call as the reply's
//! (`Stop::Yielded`) while Codex waits on it. Rovik's next turn finds the
//! run waiting (`LIVE`), gives the call its stored result, and reads on.
//! The end of Codex's turn ends the run, and Rovik's turn with it; one
//! started again continues the thread, told the results it never got.
//!
//! Codex's own tools have nothing to do: those that can be are turned off,
//! Rovik's hook refuses the rest (codex/hooks.rs), and its sandbox is
//! read-only.

use super::events::Parser;
use super::hooks::{self, Event, Hooks};
use super::{
    args_for, conclude, cwd_of, env_for, hook_tool, open, policy, request_for, resolve_program,
    session_of, thread_of, Halt, Served, Settings, HOOKS_INSTRUCTIONS, HOOKS_TOKEN, TOOLS_TOKEN,
};
use crate::agent::ChatStreamEvent;
use crate::connectors::harness::{pump, Process, Stop, MIN_FIRST_LINE};
use crate::connectors::live::{LiveRuns, Matcher};
use crate::connectors::mcp::{loopback_env, McpCall, McpServer, HOOK, SERVER_NAME};
use crate::connectors::Turn;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// What the model is told about Rovik's tools.
const ROVIK_TOOLS_NOTE: &str = "## Your tools\n\nYour tools are Rovik IDE's: the functions of the `mcp__rovik` namespace, which this prompt names by their names alone (`read`). Rovik runs each call in the project: its permissions apply, a call may wait for the user's approval, and one that isn't allowed comes back saying so. Codex's own tools aren't yours here: a call of one is refused.";
/// The MCP server's instructions, for the model.
const MCP_INSTRUCTIONS: &str = "Rovik IDE's tools for the open project: reading, searching and editing its files, running commands, and the agent's checklist. Rovik runs every call under its own permissions.";

/// The runs waiting for Rovik's results, by chat.
pub(super) static LIVE: LiveRuns<Live> = LiveRuns::new();

/// A run of Codex's waiting between Rovik's turns, or being read.
pub(super) struct Live {
    process: Process,
    parser: Parser,
    calls: mpsc::UnboundedReceiver<McpCall>,
    matcher: Matcher,
    hooks: Arc<Hooks>,
    /// Rovik's tools and the hooks, for as long as the run goes on.
    _servers: [McpServer; 2],
}

/// The system prompt the thread gets in place of Codex's: Rovik's, its
/// tools named. The project's part goes apart (`developerInstructions`):
/// it changes between runs, the rest doesn't.
fn system_text(turn: &Turn) -> String {
    let mut text = turn.system_prompt.trim_end().to_string();
    if !turn.tools.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(ROVIK_TOOLS_NOTE);
    }
    text
}

/// How starting a run went.
enum Started {
    Live(Box<Live>),
    /// The thread to continue is gone.
    Gone,
    /// It failed, its error reported.
    Failed,
}

/// Starts a run with Rovik's tools for `prompt`, continuing `thread`.
async fn start(
    turn: &Turn,
    settings: &Settings,
    thread: Option<&str>,
    prompt: &str,
    cancel: &mut watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Result<Started, String> {
    let program = resolve_program(&settings.program)?;
    let cwd = cwd_of(turn);
    // As with Codex's own tools: the agent's network, held to the allowed
    // hosts.
    let chat = turn.dialog_id.as_deref().unwrap_or_default();
    let held = crate::allowed_hosts::hold_program(chat)?;
    let network = held.network.clone();
    let (sender, calls) = mpsc::unbounded_channel();
    let tools = McpServer::start_sending(
        &turn.tools,
        MCP_INSTRUCTIONS,
        network.as_ref(),
        sender.clone(),
    )
    .await
    .map_err(|e| format!("Rovik's tools couldn't be served to Codex: {e}"))?;
    let hooks_server =
        McpServer::start_sending(&[hook_tool()], HOOKS_INSTRUCTIONS, network.as_ref(), sender)
            .await
            .map_err(|e| format!("Couldn't start Rovik's hooks for Codex: {e}"))?;
    let events = [Event::PreToolUse];
    let served = [
        Served {
            name: SERVER_NAME,
            server: &tools,
            token_var: TOOLS_TOKEN,
            hidden: false,
        },
        Served {
            name: hooks::SERVER,
            server: &hooks_server,
            token_var: HOOKS_TOKEN,
            hidden: true,
        },
    ];
    let args = args_for(settings, turn, &served, &events, &turn.api_key);
    let mut env = env_for(settings, &turn.api_key);
    env.push((TOOLS_TOKEN.into(), tools.token().into()));
    env.push((HOOKS_TOKEN.into(), hooks_server.token().into()));
    env.extend(loopback_env());
    let process = Process::spawn_with_input(
        &program,
        &args,
        &env,
        Some(&cwd),
        Some((&held, chat)),
        turn.first_byte_timeout.max(MIN_FIRST_LINE),
        Vec::new(),
    )?;
    let opening = super::Opening {
        policy: policy(settings, turn),
        resume: thread,
        base: Some(system_text(turn)),
        developer: turn
            .system_context
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string),
        hooks: &events,
        ephemeral: false,
    };
    let opened = match open(process, turn, settings, opening, cancel).await {
        Ok(opened) => opened,
        Err(Halt::Gone(mut process)) => {
            process.finish().await;
            return Ok(Started::Gone);
        }
        Err(Halt::Failed(error)) => {
            emit(ChatStreamEvent::Error { error });
            return Ok(Started::Failed);
        }
    };
    let ledger = hooks::Shared::default();
    let session = session_of(&opened.id, true);
    let mut parser = Parser::new(opened.rpc.clone(), &opened.id, &session)
        .with_rovik_tools()
        .in_folder(cwd.clone())
        .running(opened.model.clone(), opened.version.clone())
        .with_ledger(ledger.clone());
    parser.announce(emit);
    // Its commands as Codex's own are (a compaction, a review); a skill's
    // are for Codex's own tools, and go as the message they are.
    let ask = super::ask_of(prompt, &[]);
    let (method, params) = request_for(&ask, &opened.id, turn);
    parser.begin(opened.rpc.request(method, params));
    let hooks = Arc::new(Hooks {
        checks: None,
        asks: None,
        ask_commands: false,
        ask_patches: false,
        rovik_tools: true,
        root: cwd,
        env_file: None,
        ledger,
    });
    Ok(Started::Live(Box::new(Live {
        process: opened.process,
        parser,
        calls,
        matcher: Matcher::default(),
        hooks,
        _servers: [tools, hooks_server],
    })))
}

/// One of Rovik's turns, with Rovik's tools: the chat's waiting run goes on
/// with the results, or a run starts.
pub(super) async fn run(
    turn: Turn,
    settings: Settings,
    cancel: &mut watch::Receiver<bool>,
    emit: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> Result<(), String> {
    let Some(dialog) = turn.dialog_id.clone() else {
        return Err("Rovik's tools are for a chat's run.".into());
    };
    let waiting = LIVE.take(&dialog);
    let mut live = match waiting {
        Some(mut live) if !turn.continue_reply && live.matcher.takes(turn.answers.as_ref()) => {
            if let Some(answers) = &turn.answers {
                live.matcher.deliver(answers);
            }
            live.parser.next_reply();
            live.parser.announce(emit);
            Box::new(live)
        }
        other => {
            drop(other);
            // A thread kept for Codex's own tools isn't continued with
            // Rovik's: a new one, told what the chat was about.
            let thread = thread_of(turn.session.as_deref(), true);
            let fresh = turn.recap.clone().unwrap_or_else(|| turn.prompt.clone());
            let prompt = match (&turn.session, &thread) {
                (Some(_), None) => fresh.clone(),
                _ => turn.prompt.clone(),
            };
            let started =
                match start(&turn, &settings, thread.as_deref(), &prompt, cancel, emit).await? {
                    Started::Gone => start(&turn, &settings, None, &fresh, cancel, emit).await?,
                    started => started,
                };
            match started {
                Started::Live(live) => live,
                Started::Gone | Started::Failed => return Ok(()),
            }
        }
    };
    let Live {
        process,
        parser,
        calls,
        matcher,
        hooks,
        ..
    } = &mut *live;
    let mut on_call = |call: McpCall| match call.name == HOOK {
        true => hooks.answer(call),
        false => matcher.take(call),
    };
    let stop = pump(process, parser, Some(calls), &mut on_call, cancel, emit).await;
    if stop == Stop::Yielded {
        let made = live.parser.yielded.take().unwrap_or_default();
        live.matcher.await_answers(made);
        LIVE.keep(dialog, *live);
        return Ok(());
    }
    conclude(&mut live.process, &live.parser, stop, emit).await;
    Ok(())
}
