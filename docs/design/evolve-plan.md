# Evolve: an agent that improves itself, research and a plan for Rovik

_Hyperagents_ (Zhang et al., Meta and university partners, arXiv
2603.19461, March 2026) describes a system that keeps an archive of agents
that rewrite their own code, and shows them getting better over hundreds
of rounds. That system, **DGM-H**, can also rewrite the part of itself
that decides how to improve. The authors end by naming what still stops
it from improving without bound.

This document answers four questions:

1. What did the paper show, and what holds DGM-H back?
2. Which parts of such a loop does Rovik already have, and which of them
   can't be trusted yet?
3. How could Rovik get past each limit, and what does each way cost?
4. What should we build, in which order?

**Short answer:** the authors name two limits: the tasks never change, and
the loop that picks and scores agents is fixed. A close reading adds more
that bind a coding agent sooner: the model is frozen, the score can be
gamed, scores are noisy, every round is expensive, and what one branch
learns stays in that branch. Rovik already has the hard half of such a
loop: a headless runner, 30 graded tasks with hidden tests, isolated
working copies, a sandbox and budgets. It lacks the other half: an archive
of variants, an improver, something the improver may change, and a supply
of new tasks. We propose **Evolve**, a fourth mode beside Agent, Plan and
Autopilot. One message starts a **campaign** that mines graded tasks from
the project's own history, evolves the agent's **profile** (its
instructions, additions to its prompts and a short list of settings) with
an archive and an **improver** that may rewrite its own procedure, keeps
the grader and a held-out set of tasks out of the agent's reach, and
leaves adoption to the user.

**Fix this first:** in today's eval harness, a run being evaluated can
read the answers. The sandbox leaves the whole disk readable. Workspaces
sit inside the repository, so `.git`, `scripts/eval/expected/` and the
upstream clones are all visible. Web search is always on. The grader also
ignores the gate's flags for weakened tests, so adding a `conftest.py`
that skips tests could count as a pass. A search for a higher score would
find all of these. The first milestone (P, section 7) fixes them, then
runs the baseline, which has never run with a real model. The details are
in 3.2.

Contents:

1. [What the paper did](#1-what-the-paper-did)
2. [What holds DGM-H back](#2-what-holds-dgm-h-back)
3. [What Rovik has today](#3-what-rovik-has-today)
4. [Evolve, in short](#4-evolve-in-short)
5. [Each limit: options, pros and cons](#5-each-limit-options-pros-and-cons)
6. [Proposed decisions](#6-proposed-decisions)
7. [Milestones](#7-milestones)
8. [Risks and known limits](#8-risks-and-known-limits)
9. [Questions to decide](#9-questions-to-decide)
10. [Sources](#10-sources)

---

## 1. What the paper did

References such as §7 or app. D.4 point into the paper; bare numbers such
as 3.2 or 5.4 A point into this document.

### The loop

- An **archive** starts with one agent, and every agent in it has a score.
- Each round picks **parents** from the archive: more often the further a
  parent scores above the archive's best three, less often the more
  children it already has (app. A.2).
- Each parent **changes its own code**. The child is **evaluated**, first
  on a few tasks and on the rest if it does well enough, and joins the
  archive if it works, even when it scores lower than its parent. Low
  scorers stay as **stepping stones**: the lineages of the best agents pass
  through weaker ones (app. E.2).
- In the original Darwin Gödel Machine (DGM; Zhang et al., 2025) a fixed
  prompt, written for coding agents, decides what to change. That works
  because improving a coding agent is itself a coding task: getting better
  at the tasks makes it better at improving itself. Outside coding this
  **alignment** breaks, and the prompt has to be rewritten by hand for each
  domain.
- A **hyperagent** is one program holding the task agent and the **meta
  agent** that writes the next version, and the meta agent may change all
  of it, itself included. It starts with one instruction, "Modify any part
  of the codebase at `{repo_path}`", the location of past evaluation
  results, and the number of rounds left. The authors call this
  **metacognitive self-modification**.
- What stays fixed: the tasks, the parent selection, the evaluation, and
  the model (frozen, mostly at temperature 0).

### Numbers

| Experiment                                       | Finding                                                                                                                                       | What it means for Rovik                                                                                                                                                                       |
| ------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Coding (Polyglot), 80 rounds                     | DGM-H 0.084 → 0.267 on the full benchmark; the original DGM 0.142 → 0.307                                                                     | When an agent improves its own code, an editable improver matches the handcrafted one; it doesn't beat it.                                                                                    |
| Paper review and robot reward design, 100 rounds | DGM-H 0.0 → 0.710 and 0.060 → 0.372 on test tasks. The original DGM: 0.0 and 0.0. The DGM with its prompt customised by hand: 0.590 and 0.348 | When improving the agent is a different skill from the task, a fixed improver fails and an editable one wins. Editing a profile isn't writing code, so Evolve's product version is this case. |
| Ablations                                        | Without an improving meta agent: 0.0 and 0.213. Without the archive (each child replaces its parent): 0.0 and 0.116                           | Both parts are needed: stepping stones, and an improver that improves.                                                                                                                        |
| Transfer to grading maths proofs                 | Meta agents from DGM-H runs: imp@50 = 0.630. From the customised DGM's runs: 0.0                                                              | What carries to a new domain is the improver (its tracking, memory and analysis), not the task agent.                                                                                         |
| Compounding                                      | 200 rounds of maths grading started from transferred agents: 0.640; started from the initial agent: 0.610 (not significant)                   | Gains carry over, modestly. Near the top, each point costs more.                                                                                                                              |
| Editable parent selection                        | Random selection 0.428; selection the agents could edit 0.491; the handcrafted rule 0.532 (not significant)                                   | The agents rebuilt UCB, softmax with a temperature and stagnation detection on their own, and still didn't beat a good fixed rule.                                                            |
| Cost                                             | About 33 million tokens for 100 rounds of self-modification, plus about 0.5 million a round for evaluation: about 88.6 million a run          | Polyglot's whole 60-task evaluation used 0.89 million tokens. One of our tasks may use 1.5 million (the runner's cap). Cost is the first constraint.                                          |

Results are medians of 5 runs with 95% bootstrap intervals. imp@50 is the
best improvement a fixed meta agent reaches within 50 children.

### What the agents built for themselves

Nobody asked for these (app. E.3):

- **performance tracking**: a class that records each generation's score
  and its trend;
- **persistent memory**: a JSON file of findings with causes and plans
  ("Gen 66 … over-corrected", "Need to combine gen55's critical reasoning
  with gen64's balance");
- **analysis of past evaluations** before any change;
- **planning by the budget left**: large changes early, cautious ones at
  the end;
- **bias detection**: noticing a reviewer that accepted 99% of papers
  (accuracy 49%), and fixing it (63%);
- reusable prompt templates.

One finding is worth keeping as it is: "for LLMs, use structured
processes, not attitude instructions" (paper review, generation 54). A
"rigorous and critical" persona gave way to a checklist of weaknesses,
then a decision rule.

---

## 2. What holds DGM-H back

The authors name the first two limits (§7). The rest come from the
results and the appendices.

1. **The tasks never change.** Each run optimises one fixed set. Once the
   archive has taken what it can from it, the score stops pointing anywhere
   new and starts rewarding the set's quirks. The authors suggest
   generating tasks and curricula that follow the agent's abilities.
2. **The outer loop is fixed.** Parent selection, the staged evaluation
   and what counts as a valid child are handcrafted, and no agent can
   change them. Making selection editable helped a little and didn't beat
   the handcrafted rule (app. E.5).
3. **The model is frozen.** Everything that improves is scaffolding around
   one model (§4, app. D.1). Scaffolding can only draw out what the model
   can already do.
4. **The score can be gamed.** An optimiser of a measured signal finds the
   signal's blind spots (app. F). The best maths grader's prompt states the
   share of each grade in its training data ("~35% score 7 … ~6% score 6",
   app. E.1.3), and the best reviewer is told that "about 50% of papers
   should be rejected". These are facts about the training set, written
   into the agent. The more of the loop an agent may change (limit 2), the
   worse this gets.
5. **Scores are noisy, evaluations small.** A child is scored once, mostly
   without repetitions, and a first stage of about 10 tasks decides
   whether it gets the rest; failing it scores zero on all of them (§5.1,
   app. C.1 to C.4). The best of many noisy scores is partly luck, and a
   slow starter that would have been a stepping stone is dropped.
6. **One number, one parent, nothing pruned.** Selection uses one score,
   averaged across domains (app. A.4). A child has one parent, so two lines
   can't combine their finds: the paper's own memory asks for exactly that
   ("combine gen55's critical reasoning with gen64's balance"), and the
   loop has no way to make it. Prompts only grow: the best grader got there
   rule by rule.
7. **What a branch learns stays in it.** Tracking and memory appeared as
   code inside one lineage (app. E.3). A sibling branch doesn't get them
   unless it descends from that lineage, and every run invents them again.
8. **No credit for being a good improver.** Parents are picked by their own
   task score. How much an agent's descendants improved on it (the **growth
   score**, app. D.4) is used only afterwards, to choose agents for
   transfer.
9. **Cost bounds the number of rounds.** About 88.6 million tokens a run
   (app. D.2). For a coding agent, every evaluation is many full agent
   runs.
10. **Oversight can't keep up.** The experiments' safety rests on
    sandboxes, limits and people watching (§6). A system that changes
    faster than people can read its changes has to be slowed down or
    trusted.
11. **A run ends.** After a fixed number of rounds, the next run starts
    from an agent chosen afterwards (app. D.4). Nothing improves between
    runs.

Open-ended progress needs three things that improve themselves (Clune,
_AI-GAs_, 2019): the agent, the way it learns, and the problems it learns
from. DGM-H has the first and half of the second: its improver can
change, its outer loop can't. It lacks the third, and it sits on a frozen
model and a fixed score. A product should keep two of these limits on
purpose: a person decides what's adopted, and nothing graded may change
its grader. The aim here is progress that doesn't stall at the first
plateau, not progress without any bound.

---

## 3. What Rovik has today

### 3.1 The parts

| Part of the loop                | Rovik today                                                                                                                                                                                                                                                                                                   | Missing                                                 |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------- |
| Task agent                      | Agent, Plan and Autopilot runs in `rovik-core` (`agent_run/`); `rovik-headless` runs one without a window, with the same tools and checks                                                                                                                                                                     | —                                                       |
| Tasks with a grader             | `scripts/eval`: 30 real changes taken back to the commit before them (20 from this repository, 5 Python, 5 Go); hidden tests from the reference; required tests are those that pass at the reference twice, flaky ones left out                                                                               | New tasks; a held-out set; a campaign with a real model |
| Evaluation                      | Caps per run (1.5 million tokens, 30 minutes) and per campaign; pass@k and pass^k; `compareCampaigns` and `compareVariant` with the spread between repetitions as the noise; the subagents plan's G03 rule, which turns a measurement into a default                                                          | Paired comparisons; early stopping; runs side by side   |
| A variant                       | `eval.mjs --runner` runs any build of the runner; `variants` covers six fixed changes of settings                                                                                                                                                                                                             | Variants made by an agent, and their identity recorded  |
| Isolation                       | An overlay per chat; every command sandboxed (bubblewrap, Landlock or Seatbelt) with the credential stores hidden; deny rules; write scopes for subagents                                                                                                                                                     | The answers hidden from evaluated runs (3.2)            |
| Defences against weakened tests | Hidden tests restored over the agent's copies; a deleted or skipped test counts as failed; workspaces without history; the gate's integrity flags (`repair::integrity_flags`, `is_check_setup`: removed tests, skips, fewer assertions, changed test setup); a test writer that can't read the implementation | A grader that reads the integrity flags (3.2)           |
| Candidates and a pick           | Autopilot Thorough: three planners and a judge, two implementers in two working copies, scored mechanically (autopilot-plan D13, D14)                                                                                                                                                                         | An archive that outlives one task                       |
| Signals                         | The agent log's `tool` lines with error classes (`tools::error_class`), `verify` lines with progress verdicts, stage records, usage and cost; each evaluated run's chat database and log in its `data/` folder                                                                                                | A summary an improver can read                          |
| Compute                         | Cloud hosts run several chats at once, each in its own worktree                                                                                                                                                                                                                                               | Evaluation runs on hosts                                |
| An improver                     | —                                                                                                                                                                                                                                                                                                             | All of it                                               |
| What an improver could change   | The source, for us. For users, one instruction file (`AGENTS.md`, `CLAUDE.md` or two others, the first found, up to 6,000 characters) added to the system prompt. Every prompt is a Rust constant (`CODING_AGENT_SYSTEM_PROMPT`, each subagent kind's), and settings only switch behaviour on and off         | A profile the app loads and an improver may write       |
| Memory across chats             | None, beyond the instruction file and saved plans                                                                                                                                                                                                                                                             | A notebook                                              |

### 3.2 What would mislead a search today

A search finds what a person measuring a hand-made change wouldn't try.
These gaps hardly matter for the comparisons the evaluation was built for
(subagents-plan G01 to G03); they matter as soon as something optimises
the score.

- **The answers can be read.** The sandbox makes the whole file system
  readable and hides only the credential stores (`overlay/confine.rs`:
  `--ro-bind / /`, then `secret_dirs`). Evaluation workspaces are inside
  the repository (`.eval-cache/work/`), so a command can read this
  repository's `.git` (the self tasks' reference commits),
  `scripts/eval/expected/`, and the full upstream clones in
  `.eval-cache/repos/`. The web tools are always offered (`tools_for` in
  `agent_run/tools.rs`), and the Python and Go answers are public. One
  prompt line stands in the way ("All file paths are relative to the
  project root and confined to it").
- **The grader trusts the test setup.** It puts back the hidden test files,
  not what decides how they run: a new `conftest.py`, a pytest setting, a
  Go `TestMain`, a macro shadowing `assert_eq`, a changed manifest or test
  script. The gate flags these during the run (`verify-flagged`), but
  `grade` in `lib.mjs` reads only the test results, so a flagged run that
  passes the hidden tests counts as a success.
- **What was evaluated isn't recorded.** No commit, binary hash or settings
  hash goes into a result, and resuming a campaign reuses the results that
  are there, so two builds can mix without a word.
- **Comparisons aren't paired.** The noise is the spread of at most three
  repetition scores, and two averages are compared: no pairing per task,
  no interval.
- **One run at a time.** Runs are awaited one by one, headless runs take a
  process-wide lock because the agent log's folder is global, and the
  shared cargo target folder makes parallel Rust tasks fragile.
- **Never run for real.** Only the scripted dry run has run. The
  evaluation sets `CARGO_TARGET_DIR` inside `.eval-cache`, which isn't
  among the folders the sandbox lets a command write (`writable_dirs`), so
  the agent's own `cargo test` will probably fail on the Rust tasks. The
  dry run can't show it: its scripted model applies the patch and runs no
  checks.

Milestone P (section 7) fixes these, and no loop starts before it.

---

## 4. Evolve, in short

### 4.1 What the user sees

A fourth choice on the mode switch, **Evolve**: "improves how the agent
works on this project, measured on tasks from its history; you decide
what's kept".

The message is a goal in the user's words: "get better at our Rust
changes", "the same success for less money", or just "improve". Before it
spends anything, the chat shows:

- the tasks it found: how many commits it looked at, how many became
  tasks, in which languages, and how they're split (5.4 A);
- the current agent's cost per task, measured on a few of them (as the
  evaluation's pilot does), and the campaign's projected cost and time;
- the budget it will keep to (Settings → Evolve, changeable here), with
  **Start** and **Cancel**.

While it runs, the chat shows the archive as a tree (each variant a node,
coloured by its score, as in the paper's figure 7), the best score over
the rounds, the money and time spent, and the notebook's latest lessons.
It runs in the background like any chat, on this computer or on a host.

At the end (budget spent, no progress for five rounds, or Stop) comes the
**adoption card**: the best variants on the front of success and cost,
each with its held-out results per task against the current agent, its
cost, the diff of its profile and the lessons behind it. **Adopt** writes
that profile to the project; **Keep current** changes nothing. The profile
page lists the versions, with **Undo**.

**Why a mode,** and not a settings page or a command:

- **Pros:** the shape users know from Autopilot (one message, a pipeline the
  core drives, records in the chat); it runs anywhere a chat runs, hosts
  included; the adoption card arrives where the campaign started.
- **Cons:** a campaign isn't a coding request, and its results belong to the
  project, not the chat; a mode is easy to pick by accident, so Evolve
  asks before it spends (D14).
- **Verdict:** a mode, with its results kept on the project's profile page
  once the campaign ends.

### 4.2 A campaign's stages

Like Autopilot's (autopilot-plan D2, D3), a pipeline driven by the core
(`agent_run/evolve/`), each stage leaving a record in the chat:

1. **Tasks.** Mine the history (5.1 A), validate each candidate as
   `scripts/eval` does, split the usable ones.
2. **Baseline.** Evaluate the current agent on the selection tasks, with
   repetitions, for its score and the noise.
3. **Rounds.** Pick parents (5.2); for each, an improver run writes a child
   profile; schema and tamper checks; the child is evaluated against its
   parent, task by task, until a sequential test decides (5.2 E); the
   results go into the archive and the notebook.
4. **Confirm.** The best few on the front are evaluated on the held-out
   tasks, with repetitions, against the current agent.
5. **Adopt.** The card (4.1). Nothing changes until the user picks.

### 4.3 What may change: three options

This choice sets how far Evolve can go.

**Option 1: a profile.** Text and a few settings, no code. It holds:

- `instructions.md`, added to the system prompt after the project's
  instruction file;
- additions to the named sections of the built-in prompt
  (`CODING_AGENT_SYSTEM_PROMPT` has "Core behavior", "Tool use rules",
  "Communication style" and "Boundaries") and to each subagent kind's
  prompt: additions only, never replacements, and never to "Boundaries";
- settings from an allowed list: which subagent kinds are on, their
  models, reasoning effort and budgets within today's limits
  (`SubagentConfig`); the reasoning effort per role; the context mode
  (trimming or CLM); the checklist mode; Autopilot's effort.

- **Pros:** a round costs only its evaluation, with no build; it works for
  every project and language; its diffs are short and readable; it can't
  run code, so its worst case is a bad prompt; undoing it is deleting a
  file.
- **Cons:** bounded: it can't add a tool, a stage or a rule the harness
  enforces, and the paper's biggest gains were structural (a two-stage
  decision, retries, parsing of outputs); additions can fight the built-in
  text, and a Rovik update changes what they sit next to; it is tied to the
  models it was measured with.

**Option 2: scripts at fixed hooks.** Small programs the harness calls at
set points (before a turn, after a failed tool call, before the
end-of-run checks, when choosing a subagent), run in a script engine with
no file or network access, a time limit and an output the harness checks.

- **Pros:** structural changes without a rebuild (a check before finishing, a
  note after the same failure twice); still per project.
- **Cons:** an embedded engine (JavaScript or WebAssembly) and a hook API to
  design, document and keep stable; a new attack surface; scripts are
  harder to review than text; the API still bounds what can change.

**Option 3: Rovik's own source.** The improver edits the Rust and the
prompts in a worktree; each child is built and evaluated with `--runner`.

- **Pros:** nothing is out of reach but the rules of 4.5; what's found reaches
  every user in a release; it's reviewed as a pull request, with CI; it's
  the paper's coding setting, where improving the agent and doing the tasks
  are the same skill.
- **Cons:** only for us; each child needs a build; changes to safety code have
  to be kept out by write scope and review; 20 of the 30 tasks come from
  this repository, so it would learn Rovik's own habits.

**Proposal:** option 3 first, for us (milestone E): the harness exists,
it's the setting where the paper's loop works without help, and it tells
us which kinds of change pay. Then option 1 for users (milestone V), with
the allowed settings chosen by what E found. Option 2 only if V's
measurements show the profile has stopped improving.

### 4.4 The improver, and how it changes itself

A variant is a profile with two halves. The **agent half** (4.3) is what
every chat of the project uses once it's adopted. The **improver half** is
used only by Evolve: the improver's brief (`improver/brief.md`, its
instructions) and its analysis scripts, run in the sandbox over the
archive's records. That is the paper's hyperagent: one editable thing
holding the task agent and the agent that writes the next version.

The improver is an Agent-mode run. It reads the parent's profile, the
archive's summary, the notebook and the budget and rounds left; it may
read the project's code; it writes the child's profile and nothing else (a
write scope, as step workers have in `worker.rs`). It may change either
half. A child whose improver half changed carries that improver: when the
child is picked as a parent, its own improver writes its children (the
paper's `a.Modify(a, A)`). Better improvers spread the way better agents
do.

The first brief hands the improver what the paper's agents spent dozens of
rounds inventing: the per-task results of parent and child, the failure
classes, the regressions, the notebook and the budget. It may replace any
of that.

### 4.5 What never changes

These are outside the profile's schema, so no profile can name them, and
outside the improver's reach:

- the grader: the hidden tests, the required tests, the integrity check,
  how success is computed;
- the task sets, and which task is in which;
- budgets and caps, per run and per campaign;
- permissions, approvals, the AI approver, denied commands;
- the sandbox and the network rules;
- the adoption step;
- the profile's schema.

Changing any of them is a change to Rovik, made by a person. And the
adopted profile changes only by adoption: no chat's agent writes
`.rovik/profile/` (the write tool refuses it, and commands see it
read-only where the sandbox can do that).

---

## 5. Each limit: options, pros and cons

Numbered as the limits in section 2. Each verdict says where an option
goes in the plan (section 7).

### 5.1 The tasks never change (limit 1)

**A. Mine the project's history.** A commit that changes code and tests is
a candidate task: the base is its parent, the reference is the commit, the
hidden tests are the test files it changed. A model writes the request
from the commit message and the diff, naming the interfaces the tests
call. `validate`'s rule keeps it only if some required test fails at the
base and passes at the reference twice.

- **Pros:** real work of this project, graded by its own tests, not by a
  model; the supply grows with every merge; the same format and grader as
  `scripts/eval`; commits made after a campaign began are a held-out set no
  variant has seen (5.4 A).
- **Cons:** many commits have no tests or mix several changes, so few are
  usable; the request writer sees the answer and can leak it (the check:
  no line of the reference's non-test code in the request); each task
  needs its dependencies at that commit, the slowest part; public projects
  are in the models' training data.
- **Verdict:** first (milestone M): the cheapest endless supply of tasks
  graded by something other than a model.

**B. Learn from the user's chats.** A chat that landed, passed its checks
and was kept becomes a candidate: its request, the project before it, the
landed diff as the reference, and the tests it added as the hidden tests.

- **Pros:** exactly what this user asks for; the request is real; a steady
  supply.
- **Cons:** the reference was written by the agent, so it may be wrong, and its
  tests were written to pass its own change, which favours variants that
  code like the old agent; it keeps the user's code and requests in the
  archive; few chats add tests.
- **Verdict:** later, opt-in, and training tasks only.

**C. Generated tasks.** A task maker writes new tasks from the code: break
a function its tests cover and ask for the fix; remove a function and ask
for it again from its doc comment and tests; join two tasks into one
request. A task is kept only if the reference passes and the base fails.

- **Pros:** unlimited, and aimed: the maker can target what the notebook says
  is weak (edit misses in long files, say); nothing to leak on private
  code; harder tasks as the agent improves, which is the co-evolution the
  authors suggest.
- **Cons:** made-up bugs are different from real ones; a maker that can itself
  evolve drifts towards the tasks the current agent happens to pass or
  fail; it needs a filter that keeps learnable tasks (some variants pass
  them, not all).
- **Verdict:** milestone L: at most half the training tasks, never in the
  held-out set, and the maker's rules fixed.

**D. Keep the tasks at the frontier.** Track each task's pass rate across
the archive. A task every recent variant passes retires to a regression
set that runs less often; one that none passes is parked; the rest are
drawn more often.

- **Pros:** more information per run; the score keeps a slope as the agent
  gets better (the saturation the paper notes in app. D.3); cheaper.
- **Cons:** the task mix moves, so scores from different times can't be
  compared unless old champions run again; a parked task may be the hard
  one that matters.
- **Verdict:** milestone M, with a fixed anchor set that never moves.

### 5.2 The outer loop is fixed (limits 2 and 8)

**A. The paper's rule, fixed.** Parents that score above the best three
are favoured, and so are those with fewer children (app. A.2).

- **Pros:** it beat the editable version (app. E.5); simple and stable.
- **Cons:** the ceiling the authors name.
- **Verdict:** the starting rule.

**B. Add the growth score.** Give some weight to how much a variant's
descendants improved on it (app. D.4), so that a variant whose improver writes
good children is picked more, even when its own score is middling.

- **Pros:** it rewards the improver directly, which the paper did only
  afterwards; it's computed from the archive, so it's free.
- **Cons:** it lags (it needs children first), it's noisy with few
  descendants, and it can favour a lucky line.
- **Verdict:** milestone E, as a small weight that grows with the number
  of descendants.

**C. A portfolio of strategies, with a bandit.** A few known strategies (A's
rule, UCB, softmax with a temperature, the niches of 5.6 A); each round
goes to one of them, more often to those whose children improved lately.
The improver may add a strategy, never remove the default.

- **Pros:** never much worse than the best strategy it holds; the paper's
  agents reinvented UCB and softmax anyway, so we start where they ended;
  small and testable.
- **Cons:** not fully self-referential; a bandit needs many rounds, and its
  rewards shift as the archive grows.
- **Verdict:** milestone L, first.

**D. Strategies the agent writes.** The strategy (which parents, how many
children, which tasks to run, when to stop evaluating a child) becomes a
short script in the improver half. It runs in the sandbox with a time
limit; its choice is checked (valid ids, within the budget); the default
is used when it fails. As in the paper's algorithm 5, the newest variant's
strategy picks the next parents. The grader and the rules of 4.5 stay
outside it.

- **Pros:** the self-modification the authors ask for, without touching the
  score; the scripts are short enough for a person to read.
- **Cons:** a strategy is judged by how fast the archive improves, which takes
  many rounds to see and is very noisy; app. E.5 found no gain over the
  fixed rule yet; a bad strategy can starve exploration in ways that show only
  much later.
- **Verdict:** milestone L, after C, and compared with A on the same
  budget.

**E. Stop evaluating early, by a test rather than a threshold.** Evaluate a
child against its parent task by task, and stop as soon as a sequential
test (Wald's) says it is clearly worse, or clearly better, which is then
confirmed. This replaces the paper's guessed stage thresholds.

- **Pros:** most children are worse, so this saves the most money; the error
  rate is chosen, not guessed; it uses the noise we measure.
- **Cons:** tasks differ in difficulty, so their order matters (draw it at
  random, a language at a time); it stops a child that is weak now but
  would have been a stepping stone (keep a small share evaluated in full
  whatever happens, 5.5 C).
- **Verdict:** milestone E.

### 5.3 The model is frozen (limit 3)

**A. Models and effort as settings the improver may change.** Which model
each role uses (lead, scout, reviewer, triage, test writer, judges), its
reasoning effort, the context mode.

- **Pros:** real gains in cost and success with no new code, since the
  settings exist; cost-aware from the start.
- **Cons:** only among the models the user has keys for; a profile is tied to
  the models it was measured with, so a new model needs a new check
  (5.10 A); what helps one model can hurt another.
- **Verdict:** milestone V. A profile records its models, and Evolve
  doesn't offer it for others without a new check.

**B. Examples from the archive.** Short excerpts of runs that went well
(how the tests are run here, a repair that worked), kept as lessons the
agent loads when they're relevant.

- **Pros:** the model learns in context from its own successes on this
  project; no training.
- **Cons:** tokens on every request (prompt caching helps); they go stale as
  the code changes; they can teach habits that fit only the evaluation
  tasks.
- **Verdict:** milestone L, inside the notebook's size limit (5.7 A).

**C. Fine-tune a local model on the archive's runs.**

- **Pros:** the only option that changes the model.
- **Cons:** far outside an editor's job (GPUs, training, evaluating the
  model); training on its own output risks collapse; every provider
  differs.
- **Verdict:** no. The runs are kept in a form someone could train on
  later.

### 5.4 The score can be gamed (limit 4)

**A. Three task sets.** Training tasks: the improver sees everything
(diffs, logs, results). Selection tasks: it sees only scores. Held-out
tasks: never seen, never used to pick parents, used only at adoption, and
topped up from commits made after the campaign began.

- **Pros:** the standard defence against fitting the set; a held-out set made
  of later commits can't have leaked into the archive.
- **Cons:** small projects have few tasks; each adoption decision spends the
  held-out set a little, so it has to keep growing.
- **Verdict:** milestone E.

**B. The grader out of reach.** Milestone P's fixes (answers hidden, test
setup graded, web tools off during evaluation), plus: the improver writes
only the profile, and a child whose profile names a task's id, its hidden
test files or a line of its reference is rejected before it's evaluated.

- **Pros:** closes the cheap ways to cheat; most of the machinery exists (the
  sandbox, the integrity flags, write scopes).
- **Cons:** text checks stop accidents, not a determined optimiser (the repo
  already says this of deny rules); a profile can still learn the tasks'
  quirks without naming them, as the paper's grader learned the share of
  each grade.
- **Verdict:** milestones P and E, with A as the defence that counts.

**C. More than one number.** Success decides; cost, time and
interventions are limits (the G03 rule already caps a subagent kind's cost
at twice the baseline's); the archive keeps the front of success and cost.

- **Pros:** no "win by spending ten times the tokens"; it finds the cheaper
  variants users want; it matches the G03 rule.
- **Cons:** a front needs more evaluations to fill, and the user has to pick a
  point on it.
- **Verdict:** milestone E: success first, cost as a limit and a second
  objective.

**D. Quality beyond the tests.** Hidden tests say a change works, not that
it's good. Add the change reviewer's high and medium findings and the
diff's size (autopilot-plan D14's order) as a tie-break.

- **Pros:** it penalises hacks that pass the tests; it reuses an existing
  subagent.
- **Cons:** a model's opinion enters the score, and a variant can learn to
  please the reviewer; a review per run costs.
- **Verdict:** milestone L, as a tie-break only.

**E. A person adopts, with the evidence.** Evolve never changes the
project's profile on its own. The card shows the diff, the held-out
results per task, the cost and the lessons; every adoption can be undone.

- **Pros:** oversight where the paper puts it; diffs of text are short.
- **Cons:** people approve what looks reasonable; a slow step between
  campaigns.
- **Verdict:** always.

### 5.5 Noisy scores, small evaluations (limit 5)

**A. Paired comparisons.** Compare a child with its parent on the same
tasks, counting wins and losses per task, and report a bootstrap interval
of the difference, rather than two averages.

- **Pros:** much less noise for the same runs; the interval says how sure we
  are.
- **Cons:** both need the same tasks, which the sequential test (5.2 E) gives
  anyway.
- **Verdict:** milestone P (`compareCampaigns` gains it).

**B. Confirm before believing.** The best of many noisy children is lucky
on average (the winner's curse). Run the best few on the front again, with
repetitions and on held-out tasks, before calling them better, and rank by
a shrunken estimate (the parent's score plus part of the difference).

- **Pros:** the archive stops chasing luck; cheap next to a wrong adoption.
- **Cons:** the extra runs; a real but small gain can look like noise.
- **Verdict:** milestone E.

**C. Keep stepping stones.** A small share of children are evaluated in
full however they start, and kept if they differ from what's there
(5.6 A).

- **Pros:** it keeps the slow starters that the paper's lineages show matter.
- **Cons:** the runs it costs.
- **Verdict:** milestone E, about one child in ten.

### 5.6 One number, one parent, nothing pruned (limit 6)

**A. Niches.** Keep the best variant per niche (the task's language, the
kind of change, a cost tier) rather than one ranking: quality-diversity, as
in MAP-Elites.

- **Pros:** it keeps stepping stones; it gives users a choice (a cheap variant,
  a careful one); it can grow a specialist per language.
- **Cons:** the niches are a handcrafted choice; more evaluations to fill them.
- **Verdict:** milestone L.

**B. Two parents.** The improver may merge two parents' profiles, section
by section, or by an instruction such as "take B's notes on running the
tests into A".

- **Pros:** it joins finds made on separate lines, which the paper's memory
  asked for; profiles are text and settings, so a merge is cheap.
- **Cons:** merged instructions can conflict, and it's harder to tell which
  half helped.
- **Verdict:** milestone E, as one of the improver's options.

**C. Pressure to stay small.** The profile's tokens count in its cost; the
improver may propose removals; a smaller child that isn't worse takes its
parent's place in the front.

- **Pros:** cheaper requests, less brittleness, less fitting of the set (the
  paper's grader grew rule by rule).
- **Cons:** more evaluations; a removal can hurt rare tasks the sample missed.
- **Verdict:** milestone E.

### 5.7 What a branch learns stays in it (limit 7)

**A. A notebook in the harness.** Every evaluated run adds records (task,
variant, outcome, failure classes, verify rounds, cost, and the change from
the parent on that task). The improver gets a summary and can query the
rest; it writes lessons as claims with evidence (variant ids and results),
shared by every branch.

- **Pros:** the paper's most portable inventions (tracking, memory, analysis)
  exist from the first round, not the fiftieth; sibling branches share
  them; most of the data is logged already.
- **Cons:** a built-in notebook shapes what the improver notices, which is the
  paper's point about handcrafting; lessons can be wrong and spread, so each
  needs its evidence and can be retired; context cost.
- **Verdict:** milestone E, with the improver free to add its own analysis
  scripts.

**B. Lessons into everyday chats.** Lessons about the project ("the tests
here need `--features x`") go into the adopted profile, so Agent, Plan and
Autopilot chats use them.

- **Pros:** value outside Evolve; text a person can review.
- **Cons:** lessons go stale as the project changes; they can clash with the
  instruction file (the profile comes after it).
- **Verdict:** milestone V, with adoption.

### 5.8 Cost (limit 9)

**A. Runs side by side,** on this computer (milestone P removes the lock)
and on cloud hosts.

- **Pros:** hours become minutes; hosts exist.
- **Cons:** it saves time, not money; providers' rate limits; the heavy-command
  slots bound a laptop.
- **Verdict:** milestone P on this computer, hosts in V.

**B. Spend where it informs:** early stopping (5.2 E) and frontier tasks
(5.1 D).

- **Pros:** the largest savings.
- **Cons:** those options' cons.
- **Verdict:** milestones E and M.

**C. Reject broken children before evaluating them:** schema validation, the
tamper check, a size limit and one smoke task.

- **Pros:** nearly free; the paper keeps only children that compile, and a
  typed profile breaks less often than free code.
- **Cons:** a smoke task can reject an odd but good child.
- **Verdict:** milestone E.

**D. An improver that knows its budget.** The rounds and money left are in
its brief from the start; the paper's agents learned to plan by them.

- **Pros:** large changes early and careful ones late, without a handcrafted
  schedule.
- **Cons:** a little context.
- **Verdict:** milestone E.

Roughly, one campaign of 30 children on our tasks, if a run costs what the
baseline's cap allows ($75 for 90 runs, under $0.85 a run): the baseline's
60 runs, 6 to 10 runs per child, the improver's runs and the
confirmation's 120 come to $300 to $500. The baseline campaign (P06) gives
the real figure before anything else is planned.

### 5.9 Oversight (limit 10)

**A. Power in steps.** The options of 4.3 are steps of risk: text and
settings; scripts at hooks; source changes behind review.

- **Pros:** risk grows with power, and each step is useful on its own.
- **Cons:** the first step bounds what can be learned.
- **Verdict:** the order of the plan.

**B. The rules of 4.5, enforced by the harness, not the prompt.**

- **Pros:** no text in a profile can lift them; the harness already fixes tools
  and budgets this way (subagents-plan D2).
- **Cons:** some useful changes are blocked (allowing a command the tasks
  need); those become suggestions on the adoption card.
- **Verdict:** milestone V.

Always, whatever the option: every evaluated run is sandboxed, with the
deny rules and without the web tools; every campaign has a budget, stops
after five rounds without progress, and can be stopped at any time.

### 5.10 A run ends (limit 11)

**A. Standing campaigns.** A campaign stays open: new commits bring new
tasks; a new model or a Rovik update re-checks the adopted profile; rounds
run when the computer is idle or on a host, within a monthly budget.

- **Pros:** progress that doesn't wait for a person to start it again; it
  follows model changes.
- **Cons:** it spends money in the background; it needs stop rules and a
  visible bill.
- **Verdict:** milestone O, opt-in.

**B. Carry the improver to the next project.** A new project's campaign
starts with the best improver half found so far, not its project lessons:
the part the paper found to transfer.

- **Pros:** the transfer result (imp@50 of 0.630 against 0.0); faster starts.
- **Cons:** an improver shaped on Rust projects may suit Python ones less; it
  stays on this computer, never shared between people.
- **Verdict:** milestone O.

---

## 6. Proposed decisions

| #   | Decision                                                                                                                                                                                                                                                                      | Why                                                                                                                 |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| D1  | **Evolve is a chat mode**, the fourth beside Agent, Plan and Autopilot (`ChatMode` in `src/lib/chatMode.ts` gains `'evolve'`; `StartRequest.mode = "evolve"`). One message is a **campaign**: a goal and a budget. Its archive and the adopted profile belong to the project. | Users know the shape from Autopilot: one message, a pipeline the core drives, records in the chat.                  |
| D2  | **A campaign is a pipeline driven by the core** (`agent_run/evolve/`, beside `autopilot/`): tasks, baseline, rounds, confirm, adopt (4.2), each stage leaving a record.                                                                                                       | Autopilot's driver, stage records and budget (autopilot-plan D2, D3, D16) are reused.                               |
| D3  | **Users evolve a profile** (4.3 option 1); **we evolve the source** (option 3) in our own campaign (milestone E), through a pull request. Scripts at hooks (option 2) wait for a measured plateau.                                                                            | Power in steps (5.9 A): text and settings can't run code, and source changes get a person's review and CI.          |
| D4  | **A profile has two halves**: the agent half, used by every chat of the project once adopted, and the improver half (its brief and analysis scripts), used only by Evolve. A child may change either; a parent's own improver writes its children.                            | That is the hyperagent: the improver improves the way the agent does, and it's the part that transfers (section 1). |
| D5  | **The rules of 4.5 never evolve**: grader, task sets, budgets, permissions, approvals, denied commands, sandbox, network rules, adoption, the schema. They're absent from the profile's schema and enforced by the harness.                                                   | Nothing graded may change its grader; the harness, not the model, fixes the limits (subagents-plan D2).             |
| D6  | **Measurement before search** (milestone P): the answers hidden from evaluated runs, the integrity flags graded, identities recorded, comparisons paired, the baseline run with a real model.                                                                                 | Today an optimiser would find the answers (3.2).                                                                    |
| D7  | **Tasks from the project's history first** (5.1 A); generated tasks later, never held out (5.1 C); chats only when the user opts in (5.1 B).                                                                                                                                  | Graded by the project's own tests, and they keep coming.                                                            |
| D8  | **Three task sets** (5.4 A). Adoption is decided on the held-out tasks only, topped up from commits made after the campaign began.                                                                                                                                            | The defence against fitting the set that no text check gives.                                                       |
| D9  | **Selection: the paper's rule with a small growth-score weight** (5.2 A, B); a portfolio with a bandit next; strategies the agent writes last (5.2 C, D).                                                                                                                     | App. E.5: the fixed rule wasn't beaten. Start there and measure each step.                                          |
| D10 | **A child is evaluated against its parent, task by task, until a sequential test decides** (5.2 E), and one child in ten in full (5.5 C).                                                                                                                                     | The largest saving, without losing every slow starter.                                                              |
| D11 | **Success decides; cost is a limit and a second objective** (5.4 C): at most the cost ratio the G03 rule allows (twice the baseline's by default), with the archive keeping the front.                                                                                        | No win by spending more, and users get a cheaper choice.                                                            |
| D12 | **A notebook from the first round** (5.7 A): records per run, a matrix of results per task, failure classes, regressions; lessons with evidence, shared by every branch.                                                                                                      | What the paper's agents built for themselves, there from the start.                                                 |
| D13 | **A person adopts** (5.4 E): the card shows the diff, the held-out results, the cost and the lessons; versions are kept in `.rovik/profile/`, with undo; a profile records the models it was measured with.                                                                   | Oversight where the paper puts it.                                                                                  |
| D14 | **Evolve asks before it spends**: it projects the campaign's cost from a pilot and starts on the user's go, then keeps to its budget and stops after five rounds without progress.                                                                                            | Unlike an Autopilot task, a campaign's cost is set by its size, which the user should see first.                    |

---

## 7. Milestones

Rules for every task are autopilot-plan section 6's: one task per pull
request, the exact check commands, tests in the same task, and a note under
"Notes from implementation" when a task doesn't fit reality.

### Milestone P: a measurement a search can't fool

| ID  | Task                                                                                                                                                                                                                                        | Needs   | Done when                                                                                                                    |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------- | ---------------------------------------------------------------------------------------------------------------------------- |
| P01 | Evaluation workspaces outside the repository and the cache (a temporary root); the runner's sandbox hides the repository, `.eval-cache` and `scripts/eval` (`Confine.hide`); the web tools off in evaluation runs unless a task allows them | —       | A scripted cheater in `dry-run` that runs `git log --all`, reads `expected/` and calls `browse_url` finds nothing, and fails |
| P02 | The grader reads the run's integrity flags (`repair::integrity_flags`, `is_check_setup`): a flag on a file the reference didn't change grades the run `tampered`                                                                            | —       | A cheater that adds a `conftest.py` skipping the hidden tests fails, with the reason                                         |
| P03 | `result.json` records the runner's commit and binary hash and the hashes of the settings and the profile; resuming with another identity refuses                                                                                            | —       | Resuming a campaign with a rebuilt runner stops with a message                                                               |
| P04 | `compareCampaigns` adds paired differences per task and a bootstrap interval; the G03 rule stays as it is                                                                                                                                   | —       | Tests on made-up results with a known difference                                                                             |
| P05 | A run id and a log folder per headless run (no process-wide lock); `--jobs N`; a cargo target folder per job inside the folders the sandbox lets a command write                                                                            | —       | Two runs at once grade as they do one after the other                                                                        |
| P06 | The subagents plan's G01 for real: the pilot, then the baseline, with a real model; fix what it finds first                                                                                                                                 | P01–P05 | The baseline's `summary.md`, and the cost per run known                                                                      |

### Milestone E: the loop on Rovik's own source

| ID  | Task                                                                                                                                                                                                                                                                                                                         | Needs | Done when                                                                                              |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ------------------------------------------------------------------------------------------------------ |
| E01 | `eval.mjs evolve`: the archive (`archive.json`: id, parent, commit, results per task, children, status), the paper's selection with the growth score's weight, children as worktrees built for `--runner`                                                                                                                    | P06   | A dry run with the scripted model grows an archive of 5 children                                       |
| E02 | The improver: the headless runner in Agent mode on the parent's worktree, its brief in `scripts/evolve/improver.md` (in the tree, so a child can rewrite it), the notebook's summary and the budget in its request; write scope `agent.rs` and `agent_run/` without their permission and sandbox code, and `scripts/evolve/` | E01   | A child that changes `scripts/eval/` or `overlay/` is rejected before its build                        |
| E03 | Evaluation by sequential test against the parent; one child in ten in full; the smoke task; the tamper check                                                                                                                                                                                                                 | E01   | Scripted children that are clearly worse stop within 8 runs; the error rate holds on simulated results |
| E04 | The notebook: records per run from `report.json` and the run's log, the matrix per task, regressions, `lessons.md` with evidence                                                                                                                                                                                             | E01   | The improver's request carries the summary; a lesson without evidence is refused                       |
| E05 | Held-out tasks and the result: the 30 tasks split into 20 for training and selection and 10 held out, until M adds fresh ones; the best on the front confirmed with 3 repetitions; a branch and a pull request with the evidence                                                                                             | E03   | One campaign's pull request, reviewed                                                                  |

### Milestone M: tasks from history

| ID  | Task                                                                                                        | Needs    | Done when                                                                                                            |
| --- | ----------------------------------------------------------------------------------------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------- |
| M01 | `eval.mjs mine`: candidate commits (code and tests changed), the request writer, the leak check, `validate` | P06      | Mining this repository's last 300 commits gives validated tasks, and no request holds a line of its reference's code |
| M02 | Setup from the detected stack (`project_profile.rs`) and Autopilot's installs (`prepare.rs`)                | M01      | Tasks mined from one outside Python project and one TypeScript project validate                                      |
| M03 | The frontier: each task's pass rate across the archive; retired, parked and anchor tasks                    | E01, M01 | A retired task's share of the runs falls; the anchors never change                                                   |

### Milestone V: the Evolve mode

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                  | Needs    | Done when                                                                                                                                                |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| V01 | The profile: `.rovik/profile/` (the agent half: `instructions.md`, `sections/*.md`, `roles/*.md`, `settings.json`; the improver half: `improver/`); the core adds the agent half after the instruction file in the "## Project" section (`agent_run/project.rs`) and to the named sections in `lead_system_prompt`; a schema of the allowed settings; no chat's agent writes it (4.5) | —        | A profile with a setting outside the schema is refused with the reason; the project's chats get its text; a chat's write to `.rovik/profile/` is refused |
| V02 | The mode: `'evolve'` in `ChatMode` and the mode switch; the driver `agent_run/evolve/` with stage records                                                                                                                                                                                                                                                                             | V01      | A campaign's stages show as records                                                                                                                      |
| V03 | Tasks and grading in the core: E's archive, selection, sequential test and notebook in Rust; mining, and the parsers of the four check kinds from `lib.mjs`                                                                                                                                                                                                                           | E05, M02 | The core grades the 30 tasks as `lib.mjs` does                                                                                                           |
| V04 | The campaign's view: the tree, progress, the front, the notebook; the adoption card; versions and Undo on the profile page                                                                                                                                                                                                                                                            | V02      | Adopting and undoing pass in the end-to-end tests                                                                                                        |
| V05 | The budget, the confirmation before spending and the stop rules (D14); campaigns on hosts                                                                                                                                                                                                                                                                                             | V02      | A campaign stops at its budget, and after five rounds without progress                                                                                   |

### Milestone L: a loop that changes itself

| ID  | Task                                                                          | Needs | Done when                                                               |
| --- | ----------------------------------------------------------------------------- | ----- | ----------------------------------------------------------------------- |
| L01 | A portfolio of strategies with a bandit (5.2 C)                               | E05   | Compared with the fixed rule on the same budget (W03)                   |
| L02 | Strategies written by the improver (5.2 D)                                    | L01   | A strategy that times out or returns a bad id falls back to the default |
| L03 | Niches (5.6 A)                                                                | E05   | The archive keeps the best per niche                                    |
| L04 | Generated tasks (5.1 C), at most half the training tasks                      | M03   | Every generated task validated; none in the held-out set                |
| L05 | Examples from the archive (5.3 B); the change reviewer as a tie-break (5.4 D) | E04   | Measured against E's campaign on the same tasks                         |

### Milestone O: open-ended operation

| ID  | Task                                                                                       | Needs | Done when                                                    |
| --- | ------------------------------------------------------------------------------------------ | ----- | ------------------------------------------------------------ |
| O01 | Standing campaigns (5.10 A)                                                                | V05   | A new commit with tests becomes a task without a person      |
| O02 | The improver carried to the next project (5.10 B)                                          | V05   | A second project's campaign starts from the first's improver |
| O03 | A design note on scripts at hooks (4.3 option 2), if W02 shows the profile stops improving | W02   | The note, with what a hook could reach that a profile can't  |

### Milestone W: was it worth it

| ID  | Task                                                                                                                                 | Needs | Done when                       |
| --- | ------------------------------------------------------------------------------------------------------------------------------------ | ----- | ------------------------------- |
| W01 | E's campaign: does its best child beat the current agent on the held-out tasks by more than the noise, within the cost limit of D11? | E05   | `docs/design/evolve-results.md` |
| W02 | V on two outside projects (one Python, one TypeScript): the same question for a profile                                              | V05   | The same file                   |
| W03 | L01 and L02 against the fixed rule: the paper's app. E.5, on our tasks                                                               | L02   | The same file                   |

Order at a glance:

```
P01..P05 ─▶ P06 ─▶ E01 ─▶ E02..E04 ─▶ E05 ─▶ W01
                    └──▶ M01 ─▶ M02 ─▶ M03
E05 + M02 ─▶ V01..V05 ─▶ W02 ─▶ L01..L05 ─▶ W03 ─▶ O01..O03
```

Each W decides whether the next milestone is worth its cost. If W01 finds
no child that beats the current agent beyond the noise, V waits.

---

## 8. Risks and known limits

- **Cost.** A campaign on our own tasks costs hundreds of dollars (5.8), and
  a user's campaign tens. The budget and the confirmation before spending
  are the bound; W01 says whether it pays at all.
- **Few tasks.** A project with little test history gives few tasks. Below
  a minimum (15 usable tasks, say), Evolve says so and doesn't start: its
  numbers would be noise.
- **Fitting the set.** The held-out set is the defence, and each adoption
  spends some of it.
- **Profiles across updates.** A Rovik update changes the built-in prompts a
  profile adds to. The adopted profile stays in use, the profile page says
  which version it was measured on, and the next campaign checks it again.
- **Models.** A profile measured with one model may not suit another (5.3 A).
- **A profile is in every system prompt.** Like an instruction file, it can
  say anything. It's written only on adoption, shown as a diff first, and
  no text in it can lift the harness's rules. A profile a teammate commits
  changes everyone's agent, as their `AGENTS.md` does.
- **Text checks can be beaten.** The tamper check is for accidents; the
  held-out set and the sandbox are the defence.
- **Review fatigue.** After many adoptions people approve without reading.
  The card leads with the held-out numbers and the size of the diff.
- **Known limits of v1:** no generated tasks, no strategies written by the
  agent, no hooks, no sharing between people, no fine-tuning.

---

## 9. Questions to decide

| Question                                                        | Proposal                                                                       |
| --------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| The name: Evolve, Improve or Tune?                              | **Evolve**                                                                     |
| Our source loop (E) before the product mode (V)?                | **Yes**: it's cheaper to build and tells us which changes pay                  |
| Is an adopted profile committed, for the team, or kept locally? | **Committed** in `.rovik/profile/`, as plans are, and the profile page says so |
| Chats as tasks (5.1 B), at all?                                 | **Later, opt-in**                                                              |
| Where does the product's grader live?                           | **In the core** (V03), so the app, the runner and a host grade alike           |
| A user campaign's default budget?                               | **Projected from a pilot**, with $30 and 6 hours as the default cap            |
| Generated tasks before there are many held-out ones?            | **No** (L04)                                                                   |

---

## 10. Sources

| Source                                                                                                                                                                                       | Used for                                    |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------- |
| Zhang, Zhao, Yang, Foerster, Clune, Jiang, Devlin, Shavrina, _Hyperagents_, arXiv 2603.19461 (2026); code at [facebookresearch/Hyperagents](https://github.com/facebookresearch/Hyperagents) | Everything in sections 1 and 2              |
| Zhang, Hu, Lu, Lange, Clune, _Darwin Gödel Machine: open-ended evolution of self-improving agents_, arXiv 2505.22954 (2025)                                                                  | The original loop                           |
| Clune, _AI-GAs: AI-generating algorithms_, arXiv 1905.10985 (2019)                                                                                                                           | The three things that improve themselves    |
| Mouret and Clune, _Illuminating search spaces by mapping elites_, arXiv 1504.04909 (2015)                                                                                                    | Niches (5.6 A)                              |
| Faldor, Zhang, Cully and Clune, _OMNI-EPIC_, ICLR 2025                                                                                                                                       | Generated tasks that stay learnable (5.1 C) |
| Auer, Cesa-Bianchi and Fischer, _Finite-time analysis of the multiarmed bandit problem_, Machine Learning 47 (2002)                                                                          | UCB and the portfolio (5.2 C)               |
| Wald, _Sequential tests of statistical hypotheses_, Annals of Mathematical Statistics 16 (1945)                                                                                              | Early stopping (5.2 E)                      |
| Strathern, _'Improving ratings': audit in the British university system_, European Review 5 (1997)                                                                                           | Goodhart's law, as the paper cites it       |
