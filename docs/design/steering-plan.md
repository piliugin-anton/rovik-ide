# Steering: messages to the agent while it works

In Agent mode, while the agent works on a request, the user can send it a
message. The agent reads it at its next step, as part of the same request:
"use the existing HTTP client", "the tests are in `tests/unit`", "don't
touch the config". Today the message box can't do this: Enter does nothing
while a run goes, and Stop is the only control.

This plan says what to build, in which files, in which order, and how each
step is tested. Most decisions in section 1 were made with the user; section 8
lists their answers, and the ones taken while planning for them to check. The
rest follows from the code as it is (section 2).

Why it's built this way, in short (from the research the user brought,
"Mid-Run Steering of Coding Agents", October 2026):

- **Steering helps when it adds information the agent can't get itself:** a
  missing requirement, a hidden constraint, a wrong early assumption. So a
  message reaches the agent as the user's words, framed as part of the
  request it's working on, not as a new task.
- **An action cut short loses work, and timing beats frequency.** So a message
  is read at the agent's next step, never by cancelling a reply or a command.
- **Corrections often don't stick.** Constraint violations are the most common
  misalignment in real sessions (38%). So the end-of-run restatement and the
  reviewers see the messages too.
- **After two corrections that didn't work, a fresh start beats more
  steering.** So a quiet hint suggests one from the third message.
- **People misjudge whether supervising an agent pays off.** So every message
  is logged, and the headless runner can steer scripted tasks.

Contents:

1. [Decisions](#1-decisions)
2. [What the code does today](#2-what-the-code-does-today)
3. [How steering works](#3-how-steering-works)
4. [Milestones](#4-milestones)
5. [Implementation order for an AI agent](#5-implementation-order-for-an-ai-agent)
6. [Testing](#6-testing)
7. [Risks and known limits](#7-risks-and-known-limits)
8. [Decided questions](#8-decided-questions)

---

## 1. Decisions

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | Why                                                                                                                                                                                                                                                                                        |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| D1  | **Agent mode only.** A message goes to the chat's lead run when that run is in Agent mode. Plan mode, an Autopilot task's runs (any stage), and subagents' and candidates' runs take none. There, the message box works as today.                                                                                                                                                                                                                                                    | The user's scope. Plan mode already has `ask_user` replies, and an Autopilot task runs with nobody there (autopilot-plan U01).                                                                                                                                                             |
| D2  | **At the next step.** A message is read once every call of the agent's current reply has its result, before the agent's next request to the model. No reply and no command is cut short to deliver it.                                                                                                                                                                                                                                                                               | Every call keeps its result (providers reject a history without one), and nothing in flight is lost. Claude Code delivers its queued messages the same way, once the running tool calls finish.                                                                                            |
| D3  | **Steering only.** Enter, or a Send arrow beside Stop, sends a message to the run. There's no "interrupt and send", and no follow-up held for after the run, but for a connector's command picked from the `/` menu: the harness can't take one mid-turn, so it's sent as a new message once the run has ended. Stop is unchanged.                                                                                                                                                   | The user's choice: one new action, easy to learn.                                                                                                                                                                                                                                          |
| D4  | **The user's message, part of the request.** A message that was read is stored as a user-role row at the point it was read, marked `meta.steer`. It's the user's words: shown as theirs, and protected in CLM mode. But it doesn't start a new request: every rule that looks for "the request" skips it (3.6).                                                                                                                                                                      | Stored as a plain user message, it would become the request. The run's own results would be trimmed as an old request's, its checklist would read as carried, `use_sent_content` would put the original request's text in its place, and the reviewers would judge the change by it alone. |
| D5  | **Queued in memory, stored when read.** Until it's read, a message waits in the run's `SteerQueue` (in `RunControl`), not in the messages table.                                                                                                                                                                                                                                                                                                                                     | Stored at once, it would sit between a reply's calls and their results, a history providers reject.                                                                                                                                                                                        |
| D6  | **Approval first.** A message sent while an approval waits leaves the card as it is. After Allow, the reply's other calls run, then the message is read. After the user's Deny, the reply's later calls are answered "not run" and the message is read at once. A denial by the AI reviewer, or by the permission settings, keeps the next-step rule.                                                                                                                                | The user's choices. A Deny with a message waiting reads as a redirect. Running the calls after it would ask about work the user has just turned down.                                                                                                                                      |
| D7  | **Subagents finish first.** A reply's `task` calls are calls like any other: a message waits until the subagents they started report back. The subagents aren't told.                                                                                                                                                                                                                                                                                                                | The user's choice. Only the lead talks to the user (subagents-plan D9).                                                                                                                                                                                                                    |
| D8  | **The end waits for the message.** When the model says it's done, a message already waiting is read instead of starting the end-of-run checks. One that arrives while the change review, the checks or the merge of the project's newer changes run waits for the one under way and is read with its results, before the next starts. The run then goes on, and the checks run again at its next end if it changed files since. Nothing lands before the agent has read the message. | The user's choice ("deliver after checks"). Checks that haven't started aren't started for a change that's about to change.                                                                                                                                                                |
| D9  | **Nothing is lost or left unread.** Each point where a run would end takes what's waiting or closes the queue, under one lock, and `Run::go` closes it whatever ended the run. A message is read or handed back to the input. If it was sent after the queue closed, it's sent as a new message once the run has ended. After Stop nothing more is read: what's waiting is handed back.                                                                                              | A message that arrives just as the run ends must not vanish, nor be stored without anyone reading it.                                                                                                                                                                                      |
| D10 | **What the agent reads.** The message as a normal send sends it (the editor context, file references, the text), after a short frame: "[The user sent this message while you were working…]". It's kept in `meta.sentContent`, so every later request sends the same bytes.                                                                                                                                                                                                          | The agent must know it's the same request, not a new task to start over on. The provider's cache needs the start of the history to stay the same.                                                                                                                                          |
| D11 | **The request includes what was added.** Wherever the request is restated or judged, the steering messages come with it: in the focus note; in `latest_user_request` (the AI reviewer, the change reviewer, the test writer, subagent briefs); and in the repository map's focus.                                                                                                                                                                                                    | Instructions given mid-run are the ones most often not followed. Restating them at the end of long runs and giving them to the reviewers makes them stick.                                                                                                                                 |
| D12 | **A message is a go-ahead.** A checkpoint or loop pause that falls due where a message is read doesn't happen. The next checkpoint counts from there, and the loop guard's evidence starts fresh, as when Autopilot goes on after a loop's pause (`resume_after_loop`: patterns already warned about stay so).                                                                                                                                                                       | The user is there and has just said what to do.                                                                                                                                                                                                                                            |
| D13 | **Attachments as in a message.** Images, PDFs and @file chips, within the model's Vision and PDF settings. On a host they upload first.                                                                                                                                                                                                                                                                                                                                              | The user's choice.                                                                                                                                                                                                                                                                         |
| D14 | **Queued in the chat.** The message shows at once at the end of the chat, dimmed and labelled with what it waits for, with ✕ to take it back into the input. Once it's read, it's a plain bubble where the agent read it.                                                                                                                                                                                                                                                            | The user's choice.                                                                                                                                                                                                                                                                         |
| D15 | **Handed back, never resent on its own.** A message the run held but didn't read (Stop, an error, an end that can't go on) goes back into the input, with a notice. A message sent once the run could no longer take one (it was stopping or ending) is sent as a new message once the run has ended, in the mode of the run it was meant for; a second such message, or one the chat can't take then (it's moving), goes back into the input.                                       | Stop after a message was queued may mean "stop everything". Text typed after Stop is a new instruction.                                                                                                                                                                                    |
| D16 | **Two strikes.** From the third steering message in one request, a quiet line under the input suggests a fresh start. Nothing else changes.                                                                                                                                                                                                                                                                                                                                          | The user's choice. After two corrections that didn't work, a clean start with one consolidated message usually does better than more steering.                                                                                                                                             |
| D17 | **On hosts from the start.** `AgentSteer` and `AgentUnsteer` are requests like `AgentStop`, routed by chat. The app and its hosts are updated together (one user), so there's no feature flag and no path for an older host.                                                                                                                                                                                                                                                         | The user's choice. On a host the run is the same code; only the routing, the paths in the editor context and the files' upload are new.                                                                                                                                                    |
| D18 | **Measured.** The run log gets a `steer` line at each step of a message's life (queued, read, taken back, handed back), with how long it waited, what the run was doing and where it was read. The run line counts them. The headless runner can send scripted messages (`--steer`).                                                                                                                                                                                                 | The user's choice. People misjudge whether supervising an agent pays off; the logs and the evaluations can tell.                                                                                                                                                                           |
| D19 | **Safe through a crash.** The frontend keeps a draft of each queued message until it's read or taken back, so a crash puts it back into the input, chips and files included; one stored but not answered yet comes back too (3.7). Nothing is sent on its own after a restart. A move or a drain stores what's waiting before the run pauses, so it's read wherever the chat goes. A queued message's files are kept from the cleanup while it waits.                                | In-memory controls go when a process ends, and a message is the user's work.                                                                                                                                                                                                               |

---

## 2. What the code does today

### The run

- `Run::drive` (`agent_run/run.rs`) loops: a model turn, the reply stored, its
  calls run (`execute.rs`), then the next turn. The top of the loop is where
  every call has its result: Stop, and the move and drain pauses (6.8, R02),
  are checked there.
- When the model replies with no calls, the end-of-run checks decide whether
  it's done: the checklist reminders, conflict markers, the change review,
  the verification gate (`verify.rs`), the project's newer changes
  (`take_project_changes`). `Run::go` then lands an isolated chat's changes
  and writes the run's log line.
- Every call gets a result, even after Stop: providers reject a history with
  a call left unanswered (`execute.rs`).
- The history is read from the database before every request (`agent.rs`,
  then `chat_db/history.rs::build_wire`, or `clm::prepare` in CLM mode). A
  row stored at the top of the loop is in the next request.
- `AgentState` (`agent_run/mod.rs`) keeps one `RunControl` per chat: Stop,
  pause, effort. The effort picked mid-run (`set_effort`) is taken at the
  next request (`take_effort`), and settings changed mid-run
  (`agent_configure`) reach the run where it reads them (`config_for`).
  Nothing the user writes reaches an Agent-mode run today.

### "The request" is the user's latest message

Many rules find the current request by looking back for the latest
user-role message that isn't the IDE's (`meta.harness`):

- `chat_db/history.rs`, `starts_request`: trimming (`fit_keeping`), outdated
  content (`replace_outdated`), the files sent (`attached_files`), the
  reasoning sent back (`attach_reasoning`), the focus note (`focus_note`), the
  message the repository map goes on (`build_wire`, `add_request_context`),
  and `use_sent_content`, which puts the run's `current_user_content` on that
  message.
- `agent_run/plan.rs`, `is_users`: `current_checklist` ("a second user
  message stops the search").
- `agent_run/run.rs`: `run_mode_for`, `plan_check_at_start`,
  `last_user_wire_content`, `previous_user_wire_content`,
  `latest_user_request` and `finish_plan_reply`. `latest_user_request` feeds
  the AI reviewer, the change reviewer, the plan reviewer, the test writer,
  the acceptance check and subagent briefs.
- `agent_run/execute.rs` (`submit_plan`'s request) and
  `agent_run/subagent_run.rs` (`implementing_plan`).
- `clm/mod.rs`: `is_users`, the messages protected from the agent's edits,
  and `request_id`, taken from it.
- `rovik-client`, `leave_out_known_text`, through `last_user_wire_content`.
- The frontend: `src/lib/todo.ts` (`latestChecklist`), `src/lib/autopilot.ts`
  (`currentTask`, `isRequest`), `src/lib/subagents.ts` (`runUsage`),
  `src/components/MessageList.tsx` (`requestUndelivered`), and
  `src/components/ChatPanel.tsx` (`runModeFor`, `runPlanFile`,
  `lastUserWireContent`).

### The message box

- `src/components/ChatPanel.tsx`: `handleSend` returns early while `sending()`
  (`isRunning`) and leaves the text in the input. The Send button becomes
  Stop. Enter without Shift sends; Ctrl+Enter does the same. There's no
  Esc-to-stop.
- The only text that reaches a running agent today is a reply to an
  `ask_user` question (Plan mode): what's typed becomes
  `QuestionsOutcome::Reply`.
- A sent message shows at once (`chatStore.addLocal`), and its stored copy
  replaces it by id. `receiveMessages` doesn't move a message it knows, so a
  bubble added early would stay above rows stored after it.
- Unsent text is a composer draft (`composer_drafts`). A sent message that
  hasn't had its answer yet is a pending draft (`src/lib/pendingDraft.ts`,
  localStorage), one per chat.
- A new event type has to be in `test-fixtures/agent-events.json`, which
  `src/lib/agentRuns.test.ts` and the rovik-protocol tests both check.

### Hosts

- Every control is a `rovik_protocol::Request` (`request.rs`), routed by chat
  (`rovik-client/src/remote.rs`, `route`). A chat here goes to
  `rovik_core::dispatch`. A chat on a host goes over the host's connection to
  `Daemon::answer`, then to the same `dispatch`.
- A new request is found in `Frame::Welcome`'s features (`features` in
  `rovik-protocol/src/lib.rs`, `FEATURES` in `rovik-agentd/src/session.rs`).
  A daemon answers a request it doesn't know with "This daemon doesn't know
  the request."
- A message's files go up before it (`Remote::send_files`), and Stop cancels
  the upload.
- Events come back through the daemon's `Events` and the client's
  `agent_event`; rows are mirrored (`remote/mirror.rs`).

---

## 3. How steering works

### 3.1 A message's way

```
message box                 AgentState (app or daemon)      Run::drive
───────────                 ──────────────────────────      ──────────
Enter ─► agent_steer ──────► SteerQueue: push ─► Queued
bubble "Queued…" ✕                                           …the reply's calls finish…
  ✕ ─► agent_unsteer ──────► taken out ─► true               next step: take()
                                                               rows stored (meta.steer)
bubble where it was read ◄── MessagesAdded (same id) ◄─────── emitted
                                                             model turn: reads them

an end that didn't read it:
text back in the input ◄──── SteerReturned ◄───────────────── close_and_take()
```

### 3.2 The queue

`SteerQueue` (new, `agent_run/steer.rs`) belongs to one run. `RunControl` and
the run's `Controls` hold it.

```rust
/// Messages the user sent the run while it works (steering-plan.md): read
/// at its next step, or handed back when it ends without reading them.
pub struct SteerQueue(Mutex<SteerState>);

struct SteerState {
    /// The run takes messages: an Agent-mode lead's (D1).
    takes: bool,
    /// Taken until the run decides to end (D9).
    open: bool,
    /// What the run is doing, for the log line (D18): a reply, its calls,
    /// the end-of-run checks.
    doing: Doing,
    queued: Vec<Queued>,
}

struct Queued {
    message: UserMessage,
    at: Instant,
    /// The run's tool calls when it was queued.
    calls: u32,
}
```

| Method             | What it does                                                                                 |
| ------------------ | -------------------------------------------------------------------------------------------- |
| `push(message)`    | `Queued`; `Ended` when closed; `Refused` when the run takes none, or 10 are waiting already. |
| `withdraw(id)`     | Takes a waiting message out: true. False when it was read already, or was never there.       |
| `take()`           | What's waiting, in order. The queue stays open.                                              |
| `take_or_close()`  | What's waiting; when nothing is, closes the queue, in the same step.                         |
| `close_and_take()` | Closes the queue and returns what's left.                                                    |
| `waiting()`        | Copies, for `agent_status`.                                                                  |

`start_run` (`agent_run/mod.rs`) sets `takes` once `Run::prepare` is done, for a
lead's run that is in Agent mode: not `plan_mode`, not `autopilot`, not a
candidate's (T05). It says so in `RunStarted { steerable }`. A subagent's run
gets a queue that never takes anything.

A queued message's files are renewed when it's pushed (`attachments::renew`),
and their ids are held in a registry the cleanup reads (`collect_garbage`
keeps them) until the message is stored or handed back: a message can wait
behind an approval for longer than the cleanup's hour.

A queue's lock is taken under `AgentState`'s (`agent_status` lists what
waits), never the other way round: what the run is doing (`doing`) is the
run's own note, and what it waits on (an approval, subagents) is read from
`AgentState` before the queue's lock is taken.

`agent_steer(core, dialog_id, message)`:

1. `check_attachments`, as a send does.
2. The chat's run (a lead's, `parent` none, not a candidate): none, or Stop
   set → `Ended`.
3. `push`. Log a `queued` line (3.9).

`agent_unsteer(core, dialog_id, message_id)` → `withdraw`, and a `withdrawn`
line. `agent_status` lists the waiting messages (`steering`) and each run's
`steerable`.

### 3.3 Where the run reads messages

Reading a message means storing it: `Run::read_steering(messages, at)` appends
one row per message (3.5), in order, and emits `MessagesAdded`. The next model
turn sends them. It calls `Guard::steered()` (D12) and writes a `delivered`
line for each. No point reads once the run is stopped (`stopped()`): what's
waiting stays for `go` to hand back (3.4).

| Point                    | Where                                                                                                                                                                               | What happens                                                                                                                                                                                                                                                                                      |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P1 the next step         | The top of `drive`'s loop: after the Stop check, before the pause check                                                                                                             | `take()`, and read. Not while an interrupted reply is being continued (`continue_reply`): the continuation must follow the reply, so they're read at the next point.                                                                                                                              |
| P2 a guard pause         | After `execute_tool_calls`, where `guard.pause()` would pause (checkpoint, loop)                                                                                                    | `take_or_close()`. Something waiting: read, no pause (D12), the loop goes on.                                                                                                                                                                                                                     |
| P3 done                  | The start of the "no tool calls" branch                                                                                                                                             | `take()`. Something waiting: read, then `continue`. No checklist reminder, review or check starts for a change that's about to change (D8).                                                                                                                                                       |
| P3a before the checks    | After `review_change`, before `verify_at_end`; and before `take_project_changes`                                                                                                    | `take()`. Something waiting: read, then `continue`. A message sent while the review ran is read with its findings; the checks start at the next end.                                                                                                                                              |
| P4 the end               | Each end of the "no tool calls" branch (`unfinished-checklist`, and after `take_project_changes` and the markers' reminder), and the ends of the `Verify` and `CheckMerged` entries | `take_or_close()`. Something waiting: read, then `continue` (from an entry: on into the loop). Otherwise the run ends. This is where messages sent while the checks or the merge ran are read, with their results already in the chat.                                                            |
| P5 between repair rounds | `verify_at_end`, where it looks for a pause between rounds (R04)                                                                                                                    | `take()`. Something waiting: read, then return `"continue"`. The checks run at the next end.                                                                                                                                                                                                      |
| P6 a move or a drain     | `pause_for`                                                                                                                                                                         | `take()`, read, then the pause row. The chat takes them where it goes, and the `ContinueRun` there sends them. `go`'s close then finds the queue empty.                                                                                                                                           |
| Deny                     | `execute_tool_calls`, after a call the user denied (a lead's run, not stopped)                                                                                                      | `take()`. Something waiting: the reply's later calls are answered "Error: not run: the user denied an earlier call in this reply and sent a message. Read it first.", and the messages are read once every call has its result. The loop guard doesn't count those calls (like `stopped_result`). |

At P4 a run goes on with a message after every end but these, which hand
back what's waiting (3.4): `stopped`, `verify-stopped`, `error`,
`repetition`, `verify-needs-approval` (the checks' state is already put aside
for "Run checks"), the Autopilot-only ends (`budget`, `unmet`), and
`review-open`, which every mode can reach now that the change review blocks
in Agent mode too: a finding still open after the last review keeps the
change from being accepted, and a message read there would carry the run
past the block, so what's waiting is handed back. So it goes on after `done`, `verified`, `unverified` (no checks set
up), `verify-unverified`, `verify-unchecked`, `verify-flagged`,
`verify-unresolved`, `verify-gave-up` and `unfinished-checklist`.

The user's Deny is told apart from the others by a new `Authorized::by_user`,
set in `authorize`'s `ApprovalDecision::Deny` arm (not the deadline's), and
read by `execute_tool_calls` only while the run isn't stopped: Stop answers a
waiting approval with the same `Deny` (`stop_in`).

`Guard::steered()` (`agent_run/guard.rs`) does what `resume_after_loop` does
(no stop reason, fresh evidence; patterns already warned about stay so, and
rejected calls are still remembered), and moves the next checkpoint to the
current count plus the setting's interval. `Guard` keeps that interval for it.

### 3.4 Ends, and what's left

`Run::go` closes the queue as soon as `drive` returns, whatever ended it
(`close_and_take`), before the landing. What it held goes back to the user:
`SteerReturned { dialog_id, message_ids, outcome }`, emitted before
`RunEnded`, and a `returned` line. The frontend's notice says why, by the
outcome: "The agent stopped before it read your message." The task wrapper in
`start_run` does the same when a run panics, and the `Err` path of
`read_steering` (a failed write) hands back what it couldn't store.

A message that arrives after the queue closed, after Stop, or when no run is
going gets `Ended`. The message box sends it as a new message once
`RunEnded` has come (3.7).

### 3.5 What the agent reads, and what's stored

The row: role `user`; `content` is what the user typed; the attachments as
sent; the meta:

```json
{
  "steer": { "queuedAt": 1761060000000 },
  "sentContent": "[The user sent this message while you were working…]\n\n<what a normal send sends>",
  "pdfs": []
}
```

`<what a normal send sends>` is `wire_content(message)`: the editor context
and file references the frontend puts before the text, then the text. No
`mode`: a steering message isn't a request. The frame:

> [The user sent this message while you were working. It's part of the same
> request: take it into account from here on. Keep what's done unless it says
> otherwise, change course where it asks, and update your checklist if the
> steps change.]

In CLM mode it adds: "Update your task card (@context/TASK.md) too."

`meta.sentContent` is what every later request sends, as for any user
message, so the start of the history doesn't change between requests and the
provider's cache keeps serving it.

### 3.6 The request, with what the user added

One rule: a request starts at the latest user message that is neither the
IDE's (`meta.harness`) nor a steering message (`meta.steer`).

Backend:

- `history::Msg` gets `steer: bool`, and `starts_request` skips such messages.
  That alone keeps trimming, outdated content, the reasoning sent back, the
  repository map's message and `use_sent_content` as they are for the run.
- `attached_files`: the current request's files are those of its message and
  of its steering messages.
- `focus_note`: after the request's quote, "Added by the user while you
  worked (quoted):" and the steering messages (their typed text, 300
  characters each, the last five, "and N earlier" before them).
- `plan.rs`: `is_request(m)` is `is_users(m)` without steering messages. It's
  used by `current_checklist` and by the `run.rs`, `execute.rs` and
  `subagent_run.rs` sites in section 2. `is_users` stays for "the user's
  words".
- `latest_user_request`: the request, then "Added by the user while the agent
  worked:" and each steering message's typed text, numbered. The AI reviewer,
  the change reviewer, the test writer and subagent briefs judge against the
  request as it is now.
- The repository map: `refresh_repo_map_if_needed` builds its focus from the
  request and its steering messages, from the next rebuild on (at most every
  two minutes, as now).
- CLM: `CtxMsg` (`clm/document.rs`) gets `steer`. `is_users` (protected) keeps
  steering messages, and `request_id` uses the request rule.

Frontend: `isRequestMessage(m)` in a small module (`src/lib/requests.ts`),
used by every helper listed in section 2, and `steer` in the message meta
type (`src/types.ts`).

### 3.7 The message box

**Sending.**

- While a run that takes messages goes (`steerable`), Stop stays where it is.
  A Send arrow appears beside it once there's text, and Enter sends. The
  placeholder: "Add to the task: the agent reads it at its next step".
- Plan mode and Autopilot runs: as today.
- A run that's still starting (before `RunStarted`): as today, the text
  stays until it has started.
- The message is built as `handleSend` builds one (text, chips, attachments,
  editor context, the model's Vision and PDF checks), without `mode`. Its
  draft is saved (`src/lib/steerDrafts.ts`, localStorage, by message id). A
  queued bubble is added, the input is cleared, and `agent_steer` is called.
  - `Queued`: the bubble waits.
  - `Ended`: the bubble reads "Sent as a new message once the agent has
    finished". On `RunEnded` (at once, when it came already) it's sent the
    normal way (`sendMessage`), in the mode of the run it was meant for, its
    draft becoming the pending draft. One message is sent so: a second, or
    one the chat can't take then (it's moving), comes back into the input
    with a notice.
  - `Refused`, or an error: the bubble goes, the text comes back into the
    input, and a notice says why.

**The queued bubble.**

- It lives in a list of its own per chat (`chatStore`, `queued`), shown after
  the chat's rows (`MessageList`). It's dimmed, with ✕, and labelled by what
  the run waits on, from what the chat already shows:
  - an approval card in the chat: "Queued: the agent reads it after you
    answer the approval"
  - a running subagent's block: "Queued: the agent reads it when its
    subagents report back"
  - a running change review or checks record: "Queued: the agent reads it
    when the checks finish"
  - otherwise: "Queued: the agent reads it at its next step".
- ✕ calls `agent_unsteer`. True: the bubble goes and the text goes back into
  the input. False: the message was read already.
- Read: `MessagesAdded` brings a row with the bubble's id. The bubble leaves
  `queued`, and the row shows where it was stored, as a plain user bubble.
- `SteerReturned`: those bubbles go and their texts come back into the input
  (after what's there, as a failed message's text does), with the notice.
- `RunEnded` with a bubble still queued and no word of it (the run's process
  crashed): back into the input. On a host's chat that's judged once the host
  has caught up (K23), as the pending drafts are: the row can come with the
  pull after a reconnect, or after a `Resync`, and a bubble whose row is there
  was read.

**The hint (D16).** With three or more steering messages in the current
request (its rows plus the queued bubbles), a line under the input: "Several
corrections in one run: a fresh start with one message that says it all often
works better."

**After a restart.** Nothing is sent on its own. A message the agent hadn't
read comes back into the input, as an unanswered message does today; one it
had read stays in the chat. Each message is in one of four states when the
app ends:

- **Still queued** (the common case: the agent was running a command or
  waiting on subagents). The run went with the app. `recoverDrafts` also goes
  through the steer drafts: this one's draft has no stored row, so its text
  comes back into the input.
- **Stored, but no reply finished after it** (the moment between P1 and the
  agent's next reply). The steer drafts are judged as the pending drafts are
  (`recoverDrafts`): a stored row with no answer after it is marked failed, so
  it's never sent to the model, and the draft brings it back into the input,
  chips and files included. For a chat another window ran, the restart check
  for unanswered sends (`store::unsettled_sends`) takes the chat's latest send
  and every steering row after it, not only the latest user message, and
  their texts come back, in order. A row judged once isn't judged again, so
  each comes back once.
- **Read** (a reply finished after it). It stays in the chat, and its draft
  goes.
- **On a host, when only the app ended.** The run there still holds the
  message (`agent_status.steering`), so its bubble shows again and the run
  reads it as usual.

A page loaded mid-run gets `steerable` and the queued bubbles from
`agent_status`.

### 3.8 On a cloud host

rovik-protocol:

- `Request::AgentSteer { dialog_id, message: UserMessage }` and
  `Request::AgentUnsteer { dialog_id, message_id }`. Both target
  `(Chat(id), Run)`, as Stop does, so they're allowed while the chat moves
  (P6 takes the messages along).
- `Response::Steer(SteerReply)`, where
  `SteerReply = Queued | Ended | Refused { reason }`. `AgentUnsteer` answers
  `Bool`.
- `AgentEvent::RunStarted { steerable }` and
  `AgentEvent::SteerReturned { dialog_id, message_ids, outcome }`, which
  `dialog_id()` covers.
- `RunningRun { steerable }` and `AgentStatus { steering: Vec<QueuedSteer> }`,
  `#[serde(default)]` as the protocol's new fields are.

There's no feature flag: the app and its hosts are updated together.

The daemon: `dispatch` serves both requests, and its `status` keeps the
computer's own chats' `steering`, as it does their runs (D16).

The client (`rovik-client/src/remote.rs`):

- `Remote::steer`, like `start`. A chat here goes to `dispatch`. For a chat on
  a host, the editor context's local paths are written from the project's
  root (`relative_paths`, as a start's are, K08), and the attachments go up
  first, in an upload of the steer's own (`steer_uploads`, by message id).
  Then the call.
- Stop cancels a steer's upload and still goes to the host: `route` answers
  Stop itself only for a send's upload (6.11), which has no run yet to stop.
  The cancelled steer comes back into the input.
- `Remote::unsteer` goes through `route`.
- `Remote::status` adds each host's `steering`.
- `catch_up` copies `steerable` into the `RunStarted` it makes up.

The app: `agent_steer` and `agent_unsteer` in `src-tauri/src/commands/agent.rs`,
registered in `src-tauri/src/lib.rs`, with wrappers in `src/lib/agentRuns.ts`.

### 3.9 Logs and the headless runner

Each line is `{"event": "steer", "dialogId", "messageId", "phase", …}`
(`agent_run/log.rs`):

| `phase`     | Fields                                                                                                                     |
| ----------- | -------------------------------------------------------------------------------------------------------------------------- |
| `queued`    | `chars`, `attachments`, `doing` (`reply`, `calls`, `approval`, `subagents`, `checks`)                                      |
| `delivered` | `waitedMs`, `callsMeanwhile`, `at` (`step`, `pause`, `done`, `review`, `checks`, `end`, `repair`, `move`, `drain`, `deny`) |
| `withdrawn` | `waitedMs`                                                                                                                 |
| `returned`  | `waitedMs`, `why` (the run's outcome)                                                                                      |

`doing` is the queue's `doing`, which the run sets when a reply starts, when
its calls start and around the review and the checks; an approval or
subagents waiting are read from `AgentState`. The run's `run` line gets
`steers` and `steersReturned`.

The headless runner (`rovik-headless`): `--steer "TEXT@N"`, which can be
repeated, sends TEXT through `agent_steer` once the lead chat has N tool
results. The report gets `steers`: each one's text, N, and its `delivered` or
`returned` line. An evaluation task can name steers (`scripts/eval/tasks`),
so a task can be compared with and without a fact given mid-run.

---

## 4. Milestones

| Milestone              | What it delivers                                                                                                      | Done when                                                    |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| **A. The core**        | The protocol's types, the request rule, the queue, the delivery points, the ends, what the agent reads, the logs.     | The core's tests in section 6 pass with a scripted model.    |
| **B. The message box** | The commands, Send beside Stop, Enter, queued bubbles and ✕, handing back, late sends, drafts and recovery, the hint. | The frontend tests pass, and the app's e2e steers a run.     |
| **C. Hosts**           | The feature, the daemon, the client's routing and upload, the catch-up.                                               | A run on a host is steered end to end in the daemon's tests. |
| **D. Measure**         | `--steer` in the headless runner, the report, steers in evaluation tasks.                                             | An evaluation run with a scripted steer reports it.          |

A and B ship together: A changes nothing visible on its own, since no
frontend calls `agent_steer` yet. C can follow in the same release or the
next; until then, a chat on a host behaves as today.

---

## 5. Implementation order for an AI agent

Rules for every task:

- Read the files a task names before changing them; follow the comments'
  style (what the code does, in plain words).
- Run what CI runs for the part you changed:
  - the backend: `cargo test --workspace` in `src-tauri` (while working,
    `cargo test -p rovik-core --lib`);
  - the frontend and this file: `npm test`, `npm run typecheck` and
    `npm run format:check`.
- A new protocol variant gets a sample in `covers()`; a new event gets a
  sample in `test-fixtures/agent-events.json` and a check in
  `src/lib/agentRuns.test.ts`.
- Write the task's notes under "Notes from implementation" at the end of this
  file, as the other plans do.

| Task | Files                                                                                                                                               | What                                                                                                                                                                                                              | Tests                                                                                                                                                                                                                                                                                                     |
| ---- | --------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A01  | `rovik-protocol/src/{request,agent,events}.rs`                                                                                                      | `AgentSteer`, `AgentUnsteer`, `Response::Steer`, `SteerReply`, `QueuedSteer`, `RunningRun.steerable`, `AgentStatus.steering`, `RunStarted.steerable`, `SteerReturned`, the targets.                               | `covers()` samples; older peers' defaults; the fixture.                                                                                                                                                                                                                                                   |
| A02  | `chat_db/history.rs`, `agent_run/plan.rs`, `agent_run/run.rs`, `agent_run/execute.rs`, `agent_run/subagent_run.rs`, `clm/mod.rs`, `clm/document.rs` | `meta.steer` and the request rule (3.6), without the restatements (A05).                                                                                                                                          | Unit: a steering row doesn't start a request; `use_sent_content` leaves its text; the run's earlier results aren't trimmed as an old request's; `current_checklist` isn't carried; `run_mode_for` and `last_user_wire_content` keep the request's; CLM protects it, and `request_id` stays the request's. |
| A03  | `agent_run/steer.rs` (new), `agent_run/mod.rs`, `dispatch.rs`, `chat_db/attachments.rs`                                                             | `SteerQueue`; `RunControl.steer` and `Controls.steer`; `takes` set in `start_run`; `agent_steer`, `agent_unsteer`; `agent_status`; `dispatch` arms; the files held while queued.                                  | Unit: push, withdraw, take, `take_or_close`, `close_and_take`, the cap, `Ended` after Stop and after the close; `collect_garbage` keeps a queued message's files.                                                                                                                                         |
| A04  | `agent_run/run.rs`, `agent_run/verify.rs`, `agent_run/execute.rs`, `agent_run/guard.rs`                                                             | `read_steering` and the rows (3.5); P1–P6 and P3a, the entries' ends; no read after Stop; Deny (`Authorized::by_user`); `Guard::steered`; `go` closing the queue, `SteerReturned`; the panic path in `start_run`. | E2E 1–12 and 15–18 in section 6.                                                                                                                                                                                                                                                                          |
| A05  | `chat_db/history.rs`, `agent_run/run.rs`, `repo_map` call site                                                                                      | The focus note, `latest_user_request`, `attached_files`, the map's focus, the CLM line of the frame.                                                                                                              | Unit: the focus note quotes them; `latest_user_request` lists them; a steering message's image is sent. E2E 12–13.                                                                                                                                                                                        |
| A06  | `agent_run/steer.rs`, `agent_run/run.rs`                                                                                                            | The log lines (3.9).                                                                                                                                                                                              | The e2e tests read the lines they expect.                                                                                                                                                                                                                                                                 |
| B01  | `src-tauri/src/commands/agent.rs`, `src-tauri/src/lib.rs`, `rovik-client/src/remote.rs`                                                             | `agent_steer`, `agent_unsteer`; `Remote::steer` and `Remote::unsteer` for a chat here (a host's chat follows in C02).                                                                                             | Client unit test: a local chat's steer reaches `dispatch`.                                                                                                                                                                                                                                                |
| B02  | `src/lib/requests.ts` (new), `src/types.ts`, `todo.ts`, `autopilot.ts`, `subagents.ts`, `MessageList.tsx`, `ChatPanel.tsx`                          | `isRequestMessage` in every request helper; `steer` in the meta type.                                                                                                                                             | Unit: each helper skips a steering message.                                                                                                                                                                                                                                                               |
| B03  | `src/lib/agentRuns.ts`, `src/lib/chatStore.ts`, `test-fixtures/agent-events.json`                                                                   | `steer`, `unsteer`; `RunStarted.steerable`, `SteerReturned`, status seeding; the per-chat `queued` list.                                                                                                          | `agentRuns.test.ts`, `chatStore.test.ts`: queued, read (row with the id), handed back, still queued at `RunEnded`.                                                                                                                                                                                        |
| B04  | `src/components/ChatPanel.tsx`                                                                                                                      | Send beside Stop, Enter, the placeholder, the send path, `Ended` (send after `RunEnded`), `Refused`.                                                                                                              | A lib-level test of the send decision (as `remoteGuards.ts` is tested).                                                                                                                                                                                                                                   |
| B05  | `src/components/MessageList.tsx`                                                                                                                    | Queued bubbles after the rows, the labels, ✕.                                                                                                                                                                     | `chatInput.test.tsx`-style render: a bubble per queued message, its label, ✕ calls `agent_unsteer`.                                                                                                                                                                                                       |
| B06  | `src/lib/steerDrafts.ts` (new), `src/lib/remoteGuards.ts`, `ChatPanel.tsx`, `chat_db/store.rs`                                                      | Drafts, and recovery after a restart (3.7): the steer drafts judged as the pending drafts are; `unsettled_sends` returns the chat's latest send and every steering row after it.                                  | Unit: a draft with no row → input; a row with no answer → marked failed, input; answered → dropped; a host's chat waits for its catch-up. Test 14 in section 6.                                                                                                                                           |
| B07  | `ChatPanel.tsx`                                                                                                                                     | The hint.                                                                                                                                                                                                         | Unit: shown from the third message in a request, not across requests.                                                                                                                                                                                                                                     |
| B08  | `scripts/e2e/run.mjs`, `scripts/e2e/fake-model.mjs`                                                                                                 | The app steers a run: type while a slow command runs; the bubble is queued, then read; the fake model's next request has it.                                                                                      | The e2e itself.                                                                                                                                                                                                                                                                                           |
| C01  | `rovik-agentd/src/daemon.rs`, `rovik-agentd/tests/remote.rs`                                                                                        | The daemon serves both requests through `dispatch`; its `status` keeps the computer's own chats' `steering`.                                                                                                      | `rovik-agentd/tests/remote.rs`: steer a run on the daemon; the row is mirrored; `SteerReturned` reaches the app on Stop.                                                                                                                                                                                  |
| C02  | `rovik-client/src/remote.rs`                                                                                                                        | The host branch of `Remote::steer` (the paths, its own upload), Stop with a steer's upload under way, `Remote::status`'s `steering`, `catch_up`'s `steerable`.                                                    | Client tests with the fake daemon: the paths are written from the root; attachments go up first; Stop cancels the upload and stops the run; the status adds a host's `steering`.                                                                                                                          |
| D01  | `rovik-headless/src/lib.rs`, `rovik-headless/src/e2e_tests.rs`                                                                                      | `--steer "TEXT@N"`, the report's `steers`.                                                                                                                                                                        | Headless e2e: a scripted steer after N results is read and reported.                                                                                                                                                                                                                                      |
| D02  | `scripts/eval/eval.mjs`, `scripts/eval/tasks`                                                                                                       | Steers in an evaluation task, passed as `--steer`.                                                                                                                                                                | `npm run test:eval`.                                                                                                                                                                                                                                                                                      |

---

## 6. Testing

**The core** (`agent_run/e2e_tests.rs`, or a new `steer_e2e_tests.rs`, with
`testing::model` and `requests_seen`; the template is
`an_effort_picked_mid_run_applies_to_the_next_request`):

1. A message sent while a reply's calls run is read after the last call's
   result, before the next request: `seen[n]` has the frame after the results;
   the row has `meta.steer`; the original message still carries
   `current_user_content` and the repository map.
2. Sent while an approval waits, then Allow: the reply's other calls run,
   then it's read.
3. Sent while an approval waits, then Deny: the later calls are answered "not
   run…", and it's read at once. The AI reviewer's denial: next-step rule.
4. Sent while a scout runs: read after the `task` result.
5. Waiting when the model says it's done: read; no checks ran (no record).
6. Sent while the checks run (a slow check command): the checks finish, the
   message is read after their record, the run goes on, and the checks run
   again at its end once it has changed files. An isolated chat lands only at
   that end.
7. Sent, then Stop: `SteerReturned` with its id; no row stored.
8. A checkpoint due where a message is read: no pause; the next one counts
   from there.
9. Plan mode, an Autopilot stage: `Refused`, `RunStarted.steerable` false.
10. Taken back before it's read: true and no row; after: false.
11. A message sent just as the run ends, in a loop with short delays: each is
    either read (the run goes on) or answered `Ended`, never lost.
12. A move with a message waiting: stored before the pause row; the
    `ContinueRun` reads it.
13. A long run: the focus note quotes the message; the change reviewer's and
    the AI reviewer's request has it; the checklist stays the request's; a
    steering message's image is sent; CLM mode keeps it in the context file.
14. The app ends with a request and two steering messages stored after the
    last finished reply: `unsettled_sends` returns all three, in order
    (`chat_db/tests.rs`). One with a reply after it isn't returned.
15. No checks set up (`unverified`): a message waiting at the end is read, and
    the run goes on.
16. "Run checks" (`Entry::Verify`), a message sent while they run: read at
    their end, and the run goes on into the loop.
17. Stop while the checks run with a message waiting: `SteerReturned`, no row.
    Stop while an approval waits, with a message queued: the reply's later
    calls are answered as stopped, not "the user denied…".
18. Sent while the change review runs: read before the checks start; the
    checks run at the next end.

**The protocol:** every new variant in `covers()`; a `RunStarted` without
`steerable` reads as false; an `AgentStatus` without `steering` reads as
empty, as the protocol's tests do for every new field.

**Hosts:** `rovik-agentd/tests/remote.rs` steers a run on a real daemon, sees
the row mirrored, and gets `SteerReturned` after Stop. The client's tests
cover the paths, the upload, and Stop during it.

**The frontend:** the tests named in tasks B02–B07, and the app's e2e (B08).

**The headless runner:** D01's test, and a dry run of an evaluation task
with a steer (`node scripts/eval/eval.mjs dry-run`).

---

## 7. Risks and known limits

- **A long step delays the message.** A ten-minute test run, or a step worker,
  holds it until it ends. The bubble says what it waits for, and Stop is the
  way to cut in (no interrupt, D3).
- **The model may ignore the message, or start over because of it.** The frame
  says it's the same request and to keep what's done; the focus note and the
  reviewers restate it. The logs show how runs with steers end.
- **Repeated steering fills the context with failed attempts.** The hint
  (D16) suggests a fresh start; nothing enforces it.
- **A message is read, then the next request fails.** It's in the history,
  and Retry sends it. After a crash instead, it comes back into the input
  (3.7).
- **Fix rounds don't start over.** A run whose fix rounds ran out and then
  gets a message gets no new rounds: at its next end the gate reports as it
  would.
- **The provider's cache.** A message goes at the end of the history, so the
  cached start stays. The repository map stays on the request's message; its
  focus changes with the message's text at the next rebuild, which can cost
  one miss, as any rebuild does.
- **Hosts:** the user's local edits aren't synced to a host mid-run (as
  today: `hold_files` refuses while a run goes), so "I changed X here"
  doesn't bring the change. A daemon that crashes loses what's waiting; the
  frontend hands it back on the `RunEnded` the catch-up makes up. A message
  read on a host just before a crash, whose row the app hasn't pulled yet,
  can come back into the input as well: a rare duplicate.
- **A message's words go to the reviewers.** They're the user's own, so the
  AI reviewer trusts them as it trusts the request (unlike subagent reports,
  subagents-plan D16).

---

## 8. Decided questions

Asked of the user while planning:

| Question                                                                | Answer                                                                          |
| ----------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| When does the agent see a message?                                      | At the next step: once the calls of its current reply have finished (D2).       |
| Other send actions while it works (interrupt, follow-up after the run)? | None: steering only (D3).                                                       |
| A message while an approval waits?                                      | Approval first: the card stays; the message is read after the answer (D6).      |
| A message while the end-of-run checks run?                              | Read after the checks, with their results; the run goes on (D8).                |
| A message while subagents run?                                          | Wait for them; they aren't told (D7).                                           |
| Cloud hosts?                                                            | From the start (D17). Older hosts aren't supported: one user, updated together. |
| How a queued message looks?                                             | In the chat, dimmed and labelled, with ✕ to take it back (D14).                 |
| The "two strikes" rule?                                                 | A quiet hint from the third message (D16).                                      |
| Attachments?                                                            | As in a message: images, PDFs, @file chips (D13).                               |
| The buttons while the agent works?                                      | Send beside Stop; Enter sends (D3, 3.7).                                        |
| Deny with a message waiting: do the reply's other calls still run?      | No: they're answered "not run" and the message is read at once (D6).            |
| Measuring?                                                              | Log lines and headless steers (D18).                                            |

Decided while planning, for the user to check:

- A message waiting when the model says it's done is read before the checks
  start, not after them (D8): checks on a change that's about to change would
  only be run again.
- A checkpoint or loop pause doesn't happen where a message is read (D12).
- A message the run held but didn't read goes back into the input; one sent
  after the run could no longer take it is sent as a new message after the run
  (D15).
- The reviewers get the steering messages' typed text, not the editor context
  sent with them.
- At most ten messages wait at once.
- The frame's wording (3.5).

The plan was checked against the code before it was built. What that changed:
`unverified` goes on at the end; the `Verify` and `CheckMerged` entries end
at P4; P3a reads before the checks start; nothing is read after Stop, and
Stop's denial isn't the user's; P5 and the Deny rule take the messages
themselves, so ✕ can't leave a note with nothing after it; on a host, the
paths, Stop during a steer's upload, the status merge and the owner filter;
a queued message's files are kept from the cleanup; the restart check takes
every unanswered steering row.

---

## Notes from implementation

Built in one go, all milestones (A–D), after the review against the code
(section 8) and the user's word that older hosts needn't be supported (one
user: the app and its hosts are updated together).

- **A01:** `SteerReturned` carries the run's `outcome`, not a `why`: the
  frontend words the notice (`lib/steering.ts`, `returnedNotice`).
- **A03:** the queue is `agent_run/steer.rs`. A queued message's files are
  renewed when it's pushed, and a registry (`steer::is_held`) keeps them
  from `collect_garbage` while it waits. Every run gets a queue; only an
  Agent-mode lead's opens (`Run::steering_refusal` says why the others
  don't).
- **A04:** the read points are `read_waiting` (take), `read_or_close` (take
  or close), `read_before_merge` (P3a, after an end that goes on) and
  `read_at_end` (P4), all `Run`'s; none reads once the run is stopped. The
  ends that go on are those `run::steer_goes_on` doesn't list. P6 closes the
  queue as it reads (`close_and_take`). The Deny rule is in
  `execute_tool_calls`, after `execute_one`, from `Run::denied_by_user`.
- **A05:** the focus note quotes the last five, 300 characters each;
  `latest_user_request` numbers them (`run::with_steering`); the map's
  focus adds their text (`run::steering_text`).
- **A06:** the `at` values are those of 3.9, `review` and `checks` added for
  P3a's two places.
- **B01:** besides `agent_steer` and `agent_unsteer`, the app has
  `chat_messages_stored`, which the frontend's recovery asks whether a
  steering message's row is there.
- **B02–B07:** the frontend's rules are in `lib/requests.ts`,
  `lib/steering.ts` (the send decision, the labels, the hint, the judging of
  drafts) and `lib/steerDrafts.ts`. A message the run could no longer take
  is sent after it in the mode of its request; only the first such one is,
  later ones come back into the input.
- **B06:** `unsettled_sends` marks steering rows (`steer`), so another
  window's are brought back as returned steer drafts (a chat has one
  pending draft), after the send they followed.
- **B08:** not done: the app's e2e needs the built desktop app with
  WebKitWebDriver and tauri-driver, which the sandbox it was built in
  doesn't have. The core's e2e tests (`steer_e2e_tests.rs`) and the
  daemon's cover the same ground below the webview.
- **C:** no feature flag. The daemon serves both requests through
  `dispatch` and keeps its status's `steering` to the computer's chats. The
  client keeps steer uploads apart from sends' (`steer_uploads`): Stop
  cancels them and still reaches the host (`rovik-agentd/tests/attachments.rs`).
- **D:** `--steer TEXT@N` splits at the last `@`; `N` = 0 sends once the run
  has started. Evaluation tasks take `steers`, and `--no-steers` runs a
  campaign without them.
- **Tests:** the core's e2e covers 1–13 and 15–18 (`steer_e2e_tests.rs`;
  15 as a unit test of `steer_goes_on`), test 14 is in `chat_db/tests.rs`.
