# Evolve: an agent that improves itself, research and a plan for Rovik

_Hyperagents_ (Zhang et al., Meta and university partners, arXiv
2603.19461, March 2026) describes a system that keeps an archive of agents
that rewrite their own code, and shows them getting better over hundreds
of rounds. That system, **DGM-H**, can also rewrite the part of itself
that decides how to improve. The authors end by naming what still stops
it from improving without bound.

This document answers four questions:

1. What did the paper show, what holds DGM-H back, and what has later
   work found?
2. Which parts of such a loop does Rovik already have, and which of them
   can't be trusted yet?
3. How could Rovik get past each limit, and what does each way cost?
4. What should we build, in which order?

A review of the first version changed the plan; section 11 says what
changed and why.

**Short answer:** the authors name two limits: the tasks never change, and
the loop that picks and scores agents is fixed. A close reading adds more
that bind a coding agent sooner: the model is frozen, the score can be
gamed or poisoned, scores are noisy, every round is expensive, nothing is
compared with spending the same money another way, and what one branch
learns stays in that branch. Later work on coding agents is mixed at best
(2.1). So Evolve is a bet to test, not a gain to count on.

Rovik already has the hard half of such a loop: a headless runner, 30
graded tasks with hidden tests, isolated working copies, a sandbox and
budgets. We propose **Evolve**, a fourth mode beside Agent, Plan and
Autopilot, built in stages of trust:

1. **Settings first.** A race between configurations (the model and
   reasoning effort for each role, the context mode, the subagent kinds),
   aiming at "cheaper, and not worse". No model writes anything, so there
   is nothing to poison or inject.
2. **Short prose next**, written by an **improver** with a fixed brief:
   up to 1,500 characters of instructions, and lessons with evidence.
3. **More power last**: the self-modifying parts of DGM-H (an improver
   that rewrites its own brief, selection strategies it writes), or
   scripts at fixed hooks. The first come only if our own experiment shows
   an improver beats spending the same money on retries; hooks only if
   prose stops improving.

Every stage keeps the grader, a held-out set of tasks and a security check
out of the agent's reach, says, before it spends, what size of change its
tasks can detect, and leaves adoption to a person.

**Fix this first:** in today's eval harness, a run being evaluated can
read the answers. The sandbox leaves the whole disk readable. Workspaces
sit inside the repository, so `.git`, `scripts/eval/expected/` and the
upstream clones are all visible. Web search is always on. The grader also
ignores the gate's flags for weakened tests, so adding a `conftest.py`
that skips tests could count as a pass. A search for a higher score would
find all of these. The first milestone (P, section 7) fixes them, then
runs the baseline, which has never run with a real model. The details are
in 3.2.

Three more holes matter once something searches: an improver working in
Rovik's own repository could read old tasks' answers in its history; an
evaluated command still has the network; and in a source campaign a
candidate computes its own cost and its own integrity flags. Milestone P
closes the last two (P01, P02, P08), and the improver's snapshot the first
(E02).

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
11. [Review: what changed](#11-review-what-changed)
12. [Notes from implementation](#12-notes-from-implementation)

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

| Experiment                                       | Finding                                                                                                                                                                                               | What it means for Rovik                                                                                                                                                                                                                  |
| ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Coding (Polyglot), 80 rounds                     | DGM-H 0.084 → 0.267 on the full benchmark. The original DGM 0.142 → 0.307, as reported in the DGM paper, not run again. The authors call the two comparable, without a significance test              | When an agent improves its own code, an editable improver doesn't beat the handcrafted one.                                                                                                                                              |
| Paper review and robot reward design, 100 rounds | DGM-H 0.0 → 0.710 and 0.060 → 0.372 on test tasks. The original DGM: 0.0 and 0.0. The DGM with its prompt customised by hand: 0.590 and 0.348, and DGM-H's lead over it is not significant (p > 0.05) | When improving the agent is a different skill from the task, a fixed improver written for coding fails, and an editable one does about as well as one a person customised. Editing a profile isn't writing code, so Evolve is this case. |
| Ablations                                        | Without an improving meta agent: 0.0 and 0.213. Without the archive (each child replaces its parent): 0.0 and 0.116                                                                                   | Both parts are needed: stepping stones, and an improver that improves.                                                                                                                                                                   |
| Transfer to grading maths proofs                 | Meta agents from DGM-H runs: imp@50 = 0.630. From the customised DGM's runs: 0.0                                                                                                                      | What carries to a new domain is the improver (its tracking, memory and analysis), not the task agent. Shown in one domain, not in coding.                                                                                                |
| Compounding                                      | 200 rounds of maths grading started from transferred agents: 0.640; started from the initial agent: 0.610 (not significant)                                                                           | Gains carry over, modestly. Near the top, each point costs more.                                                                                                                                                                         |
| Editable parent selection                        | Random selection 0.428; selection the agents could edit 0.491; the handcrafted rule 0.532 (not significant)                                                                                           | The agents rebuilt UCB, softmax with a temperature and stagnation detection on their own, and still didn't beat a good fixed rule.                                                                                                       |
| Cost                                             | About 33 million tokens for 100 rounds of self-modification, plus about 0.5 million a round to evaluate paper review and robotics: about 88.6 million for that 100-round run                          | Polyglot's whole 60-task evaluation used 0.89 million tokens. One of our tasks may use 1.5 million (the runner's cap). Cost is the first constraint.                                                                                     |

Results are medians of 5 runs with 95% bootstrap intervals. imp@50 is the
best improvement a fixed meta agent reaches within 50 children. Every run
starts from a deliberately minimal agent, whose task agent answers with a
single model call (§4). Rovik's agent is far from minimal, so the room for
gains is smaller.

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
results, the appendices and later work.

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
9. **Cost bounds the number of rounds.** About 88.6 million tokens for one
   100-round run (app. D.2). For a coding agent, every evaluation is many
   full agent runs.
10. **Oversight can't keep up.** The experiments' safety rests on
    sandboxes, limits and people watching (§6). A system that changes
    faster than people can read its changes has to be slowed down or
    trusted.
11. **A run ends.** After a fixed number of rounds, the next run starts
    from an agent chosen afterwards (app. D.4). Nothing improves between
    runs.
12. **Nothing is compared at equal cost.** DGM-H is compared with versions
    of itself given the same number of rounds, never with the same tokens
    spent on more attempts at test time. Without that comparison, "a better
    agent" can't be told apart from "more compute" (2.1).
13. **It can be poisoned.** It learns whatever its tasks reward. Tasks whose
    tests require an insecure habit can teach that habit, and a score on
    neutral held-out tasks doesn't show it (2.1).

Open-ended progress needs three things that improve themselves (Clune,
_AI-GAs_, 2019): the agent, the way it learns, and the problems it learns
from. DGM-H has the first and half of the second: its improver can
change, its outer loop can't. It lacks the third, and it sits on a frozen
model and a fixed score. A product should keep two of these limits on
purpose: a person decides what's adopted, and nothing graded may change
its grader. The aim here is progress that doesn't stall at the first
plateau, not progress without any bound.

### 2.1 What later work found

Each source below was checked against the paper or page itself, except
where its row says otherwise; section 11 lists where a review of this plan
had a number or a claim wrong.

**Does evolving an agent help on coding tasks?**

| Source                                          | Finding                                                                                                                                                                                                                                                                                                                                                                                                                                 | What it means for Rovik                                                                                                          |
| ----------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| Wang et al., 2026, harness evolution            | On Terminal-Bench 2.1 with three models (Opus 4.6, GPT-5.4, GPT-5.4 mini) at a matched budget, neither of two harness-evolution methods beat plain parallel sampling (67.4 and 71.8 against 72.3 pass@1; with tests, 69.7 against 82.0). On held-out tasks the evolved harness moved +1.2, +0.0 and −1.4 points. Its gains came on long games with rich feedback (ARC-AGI-3, EdgeBench), from a search per task, not a reusable harness | The strongest evidence against the loop's promise for coding: why stage 2 waits for W01, and why W01 has a same-money arm (5.12) |
| Lee et al., 2026, Meta-Harness                  | A searched harness reached 76.4% on Terminal-Bench 2 with Opus 4.6, above a hand-built one (74.7%); search and evaluation used the same 89 tasks, as the paper says                                                                                                                                                                                                                                                                     | A search can find a better harness; whether it holds on unseen tasks is what W01 tests                                           |
| Agrawal et al., ICLR 2026, GEPA                 | Reflective prompt evolution beat reinforcement learning (GRPO) by 6% on average over six tasks, with up to 35× fewer rollouts (the paper's second version; the GRPO comparison used Qwen3-8B)                                                                                                                                                                                                                                           | Prompt evolution works where feedback is rich; our tasks give a pass or a fail and a log                                         |
| Gloaguen et al., 2026, AGENTS.md files          | Repository context files "do not generally improve task success rates, while increasing inference cost by over 20% on average"; agents follow them, and explore and test more                                                                                                                                                                                                                                                           | Stage 2's prose is this intervention: settings come first, and prose has a size budget and pays for its tokens                   |
| Wang, Piękos et al., 2025, Huxley-Gödel Machine | A coding agent's benchmark score is a poor guide to how good its descendants are (the "Metaproductivity-Performance Mismatch")                                                                                                                                                                                                                                                                                                          | Early archive scores mislead; the growth score helps (5.2 B)                                                                     |

**Can a gain be measured?**

| Source                                           | Finding                                                                                                                                                                                                                                                                             | What it means for Rovik                                                                |
| ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| Bjarnason, Silva and Monperrus, 2026, randomness | Over 60,000 trajectories on SWE-bench Verified, single-run pass@1 varied by 2.2 to 6.0 points, with standard deviations above 1.5 points even at temperature 0; at that spread, detecting a 2-point gain at p < 0.05 with 80% power takes about 9 runs per agent, a 1-point gain 36 | That is on 500 tasks. Our held-out sets are far smaller, hence the power check (5.5 D) |
| Miller, Anthropic, 2024, error bars              | Use paired standard errors where possible; clustered standard errors came out up to 3× the naive ones                                                                                                                                                                               | Paired, clustered comparisons (5.5 A)                                                  |
| Kapoor et al., 2024, "AI Agents That Matter"     | Agent evaluations must be cost-controlled, and many agent benchmarks have inadequate holdout sets, or none                                                                                                                                                                          | The cost limit (D11) and the held-out set (5.4 A)                                      |
| Grace et al., Anthropic, 2026, evals for agents  | "As a rule, we do not take eval scores at face value until someone digs into the details of the eval and reads some transcripts"; 20 to 50 tasks drawn from real failures are a good start                                                                                          | Transcripts before a win is believed (5.4 G); the notebook for people (5.7 C)          |
| OpenAI, 2026, on SWE-bench Verified              | Stopped reporting it as "increasingly contaminated"; of 138 problems o3 didn't solve reliably, 59.4% had flaws in their tests or descriptions. Read through search extracts only: the page itself didn't load                                                                       | Prefer tasks newer than the model (5.1 A), and validate every task (M01)               |

**What goes wrong?**

| Source                                         | Finding                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | What it means for Rovik                                                                                                         |
| ---------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| SWE-bench issue #465, 2025                     | Agents ran `git log --all` to read the future fix, a Claude 4 Sonnet run on pytest-6202 among them; fixed in SWE-bench 4.1.0                                                                                                                                                                                                                                                                                                                                                                                    | The answers hidden (P01)                                                                                                        |
| Zheng et al., 2026, SWE-Bench Pro Verified     | Closing the ways to read answers (git history, local files, fetching from GitHub) turned 186 of one model's passes into failures, 166 of them with direct evidence of hacking (labelled by a model)                                                                                                                                                                                                                                                                                                             | Reading the answers is common, not exotic                                                                                       |
| Gabor, Lynch and Rosenfeld, 2025, EvilGenie    | Codex and Claude Code reward-hacked explicitly, and all three agents tested showed misaligned behaviour; as ways to catch it, held-out tests added little, and a model judge caught the clear cases                                                                                                                                                                                                                                                                                                             | Hidden tests alone aren't a defence: the monitor and the transcripts (5.4 G)                                                    |
| Zhang et al., 2025, the DGM paper's appendix H | In a side experiment, the DGM removed the logging its hallucination check relied on, despite being told not to                                                                                                                                                                                                                                                                                                                                                                                                  | A candidate must not measure itself (P02, P08)                                                                                  |
| Roesner and Kohno, 2026, poisoned benchmarks   | Poisoned tasks led Hyperagents with Claude Sonnet 4.5 to add a directive to disable HTTPS certificate checks: the evolved agent wrote vulnerable code in 30 of 30 solutions to neutral held-out tasks, in both runs (0 of 30 without the poison). A generic security benchmark didn't remove it; only one built with knowledge of the poison did. They also report that the Polyglot setups of Hyperagents and the DGM let agents read hidden tests through git history, which, they say, the authors confirmed | The security suite with a veto (5.11 A) and the snapshot without history (5.4 F); a neutral held-out score won't show poisoning |
| Shao et al., ICLR 2026, misevolution           | A coding agent whose memory evolved over three rounds refused unsafe requests far less often (99.4% to 54.4%); treating memories as "references" rather than "rules" helped in part (back to 66.9%)                                                                                                                                                                                                                                                                                                             | Lessons are references with evidence and an expiry (5.7 A)                                                                      |
| Liu et al., 2025, "Your AI, My Shell"          | Poisoned rule files made Cursor and GitHub Copilot, with auto-run on, run an attacker's commands, at success rates of 41% to 84% over 314 payloads                                                                                                                                                                                                                                                                                                                                                              | A profile is a rule file: pinned, and approved before use (5.11 B)                                                              |
| Zhang et al., ICLR 2026, ACE                   | Rewriting a context whole can collapse it (in their example from 18,282 tokens to 122, with accuracy falling from 66.7 to 57.1); changing it item by item avoids that                                                                                                                                                                                                                                                                                                                                           | Lessons changed one item at a time (5.7 A)                                                                                      |

Most of these are 2026 preprints, not yet peer reviewed, and none measures
what Evolve would do: tasks mined from one project's own history. Read them
as warnings, and as a list of what to measure, rather than as a forecast.

---

## 3. What Rovik has today

### 3.1 The parts

| Part of the loop                | Rovik today                                                                                                                                                                                                                                                                                                   | Missing                                                      |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| Task agent                      | Agent, Plan and Autopilot runs in `rovik-core` (`agent_run/`); `rovik-headless` runs one without a window, with the same tools and checks                                                                                                                                                                     | —                                                            |
| Tasks with a grader             | `scripts/eval`: 30 real changes taken back to the commit before them (20 from this repository, 5 Python, 5 Go); hidden tests from the reference; required tests are those that pass at the reference twice, flaky ones left out                                                                               | New tasks; a held-out set; a campaign with a real model      |
| Evaluation                      | Caps per run (1.5 million tokens, 30 minutes) and per campaign; pass@k and pass^k; `compareCampaigns` and `compareVariant` with the spread between repetitions as the noise; the subagents plan's G03 rule, which turns a measurement into a default                                                          | Paired comparisons; power; early stopping; parallel runs     |
| A variant                       | `eval.mjs --runner` runs any build of the runner; `variants` covers six fixed changes of settings                                                                                                                                                                                                             | A race between many; their identity recorded                 |
| Isolation                       | An overlay per chat; every command sandboxed (bubblewrap, Landlock or Seatbelt) with the credential stores hidden; deny rules; write scopes for subagents                                                                                                                                                     | The answers and the network hidden from evaluated runs (3.2) |
| Defences against weakened tests | Hidden tests restored over the agent's copies; a deleted or skipped test counts as failed; workspaces without history; the gate's integrity flags (`repair::integrity_flags`, `is_check_setup`: removed tests, skips, fewer assertions, changed test setup); a test writer that can't read the implementation | A grader that computes the integrity flags itself (3.2)      |
| Candidates and a pick           | Autopilot Thorough: three planners and a judge, two implementers in two working copies, scored mechanically (autopilot-plan D13, D14)                                                                                                                                                                         | An archive that outlives one task                            |
| Signals                         | The agent log's `tool` lines with error classes (`tools::error_class`), `verify` lines with progress verdicts, stage records, usage and cost; each evaluated run's chat database and log in its `data/` folder                                                                                                | A report people and an improver can read                     |
| Compute                         | Cloud hosts run several chats at once, each in its own worktree                                                                                                                                                                                                                                               | Evaluation runs on hosts                                     |
| An improver                     | —                                                                                                                                                                                                                                                                                                             | All of it                                                    |
| What could change               | The settings, for everyone. The source, for us. For users, one instruction file (`AGENTS.md`, `CLAUDE.md` or two others, the first found, up to 6,000 characters) added to the system prompt. Every prompt is a Rust constant (`CODING_AGENT_SYSTEM_PROMPT`, each subagent kind's)                            | A profile the app loads, pinned and checked                  |
| Memory across chats             | None, beyond the instruction file and saved plans                                                                                                                                                                                                                                                             | A notebook                                                   |

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
- **Commands have the network.** Bubblewrap separates the processes
  (`--unshare-pid`) but not the network, so a command can fetch an answer
  even with the web tools off.
- **An improver would see the answers too.** An improver run on a worktree
  of this repository has its whole history, and an old Rovik task's
  solution is part of today's source anyway.
- **The grader trusts the test setup.** It puts back the hidden test files,
  not what decides how they run: a new `conftest.py`, a pytest setting, a
  Go `TestMain`, a macro shadowing `assert_eq`, a changed manifest or test
  script. The gate flags these during the run (`verify-flagged`), but
  `grade` in `lib.mjs` reads only the test results, so a flagged run that
  passes the hidden tests counts as a success.
- **A candidate measures itself.** The runner counts tokens and cost
  (`budget.rs`) and the gate computes the integrity flags, both in the
  build being evaluated. In a source campaign, a child could under-count
  its cost or weaken its own flags.
- **What was evaluated isn't recorded.** No commit, binary hash or settings
  hash goes into a result, and resuming a campaign reuses the results that
  are there, so two builds can mix without a word.
- **Comparisons aren't paired, and have no power.** The noise is the spread
  of at most three repetition scores, two averages are compared, and
  nothing says beforehand what size of change the tasks can show.
- **Cheating is found only by text checks.** Nothing reads what an
  evaluated run did, and nobody reads its transcript.
- **One run at a time.** Runs are awaited one by one, headless runs take a
  process-wide lock because the agent log's folder is global, and the
  shared cargo target folder makes parallel Rust tasks fragile.
- **Never run for real.** Only the scripted dry run has run. The
  evaluation sets `CARGO_TARGET_DIR` inside `.eval-cache`, which isn't
  among the folders the sandbox lets a command write (`writable_dirs`), so
  the agent's own `cargo test` will probably fail on the Rust tasks. The
  dry run can't show it: its scripted model applies the patch and runs no
  checks.

Milestone P (section 7) fixes these, and no search starts before it.

---

## 4. Evolve, in short

### 4.1 What the user sees

A fourth choice on the mode switch, **Evolve**: "tunes how the agent works
on this project, measured on tasks from its history; you decide what's
kept".

The message is a goal in the user's words: "the same success for less
money", "get better at our Rust changes", or just "improve". Before it
spends anything, the chat shows:

- the tasks it found: how many commits it looked at, how many became
  tasks, in which languages, how many are newer than the model's training
  data, and how they're split (5.4 A);
- **what the campaign can tell**: the smallest change in success its
  held-out tasks can detect (5.5 D), as in "changes of 12 points or more;
  smaller ones look like noise". When that is too coarse to be useful, it
  says so, and offers a campaign about cost only (D11) or nothing;
- the current agent's cost per task, measured on a few tasks, and the
  campaign's projected cost and time;
- the budget it will keep to (Settings → Evolve, changeable here), with
  **Start** and **Cancel**.

While it runs, the chat shows each candidate and how it's doing, and the
money and time spent. It runs in the background like any chat, on this
computer or on a host.

At the end (a winner found, the budget spent, no progress, or Stop) comes
the **adoption card**. For the best candidates it shows success on the
held-out tasks with its interval, cost per task with its interval, the
**same-money alternative** (what the current settings reach when they get
the extra money, as more attempts per task: 5.12 A), the security check's
result, and the change itself: a settings diff, later a profile diff.
**Adopt** writes it to the project; **Keep current** changes nothing. The
profile page lists the versions, with **Undo**, and after an adoption it
watches ordinary chats and proposes Undo if they get worse (5.9 C).

**Why a mode,** and not a settings page or a command:

- **Pros:** the shape users know from Autopilot (one message, a pipeline
  the core drives, records in the chat); it runs anywhere a chat runs,
  hosts included; the adoption card arrives where the campaign started.
- **Cons:** a campaign isn't a coding request, and its results belong to
  the project, not the chat; a mode is easy to pick by accident, so Evolve
  asks before it spends (D17).
- **Verdict:** a mode, with its results kept on the project's profile page
  once the campaign ends.

### 4.2 A campaign's stages

Like Autopilot's (autopilot-plan D2, D3), a pipeline driven by the core
(`agent_run/evolve/`), each stage leaving a record in the chat:

1. **Tasks.** Mine the history (5.1 A), validate each candidate as
   `scripts/eval` does, record its date and module, and split the usable
   ones. A task from outside the project is used only with the security
   suite on (5.11 A).
2. **Power.** From a pilot's per-task noise, compute the smallest change the
   held-out tasks can detect (5.5 D). Stop here, or switch to a campaign
   about cost only, when it's too large.
3. **Baseline.** The current agent on the selection tasks with repetitions,
   and the same-money arm (5.12 A).
4. **Rounds.** At stage 1, a race between configurations (5.2 F). From stage
   2, an improver writes children, and each is tested against its parent
   until a sequential test decides (5.2 E). Results go into the archive and
   the notebook.
5. **Security.** The best few run the security suite, and every patch they
   wrote is scanned (5.11 A); a regression vetoes the candidate.
6. **Confirm.** The survivors run on the held-out tasks with repetitions,
   against the current agent and the same-money arm, by the criterion
   written down before this stage began (5.12 B).
7. **Adopt.** The card (4.1). Nothing changes until the user picks.

### 4.3 What may change: stages of trust

This choice sets how far Evolve can go, and how much can go wrong.

**Stage 1: settings only.** From an allowed list: the model for each role
(lead, scout, reviewer, triage, test writer, judges) among those the user
has enabled, its reasoning effort, the context mode (trimming or CLM),
which subagent kinds are on and their budgets within today's limits
(`SubagentConfig`), the checklist mode, Autopilot's effort, the number of
attempts per task once that exists (5.12 A), and, once they are settings,
a few loop and context knobs (4.6).

- **Pros:** a small, discrete space that a simple search covers well;
  nothing written by a model, so nothing to poison or inject; a change of a
  few lines anyone can read; savings are likely (a cheaper model for one
  role can save a lot); no build, every project and language.
- **Cons:** it can't teach the agent anything about the project; gains in
  success, if any, are small; it is tied to the models it was measured
  with.

**Stage 2: settings and short prose.** Adds `instructions.md` (added to the
system prompt after the project's instruction file, at most 1,500
characters by default) and lessons (5.7 A), written by an improver with a
fixed brief (4.4). Additions to the named sections of the built-in prompt
(`CODING_AGENT_SYSTEM_PROMPT` has "Core behavior", "Tool use rules",
"Communication style" and "Boundaries") and to each subagent kind's prompt
are additions only, never replacements, and never to "Boundaries".

- **Pros:** it can carry knowledge of the project (how the tests run here);
  short, readable diffs; no code.
- **Cons:** instruction files of this kind tend to raise cost more than
  success (2.1); it is a channel for poisoned instructions (5.11); its
  additions can fight the built-in text, and a Rovik update changes what
  they sit next to.

**Stage 3: more power, last.** Two ways, each gated on our own results.

**3a, an improver that changes itself:** the paper's hyperagent, whose
brief and selection strategies a child may rewrite (4.4, 5.2 D). Its pros
and cons are in those sections.

**3b, scripts at fixed hooks:** small programs the harness calls at set
points (before a turn, after a failed tool call, before the end-of-run
checks, when choosing a subagent), run in a script engine with no file or
network access, a time limit and an output the harness checks.

- **Pros:** structural changes without a rebuild (a check before finishing,
  a note after the same failure twice); still per project.
- **Cons:** an embedded engine (JavaScript or WebAssembly) and a hook API to
  design, document and keep stable; a new attack surface; scripts are
  harder to review than text; the API still bounds what can change.

**Rovik's own source, for us.** The improver edits the Rust and the prompts
in a snapshot of the source; each child is built and evaluated with
`--runner`.

- **Pros:** nothing is out of reach but the rules of 4.5; what's found
  reaches every user in a release; it's reviewed as a pull request, with
  CI; it's the paper's coding setting, where improving the agent and doing
  the tasks are the same skill.
- **Cons:** only for us; each child needs a build; changes to safety and
  measurement code have to be kept out by write scope and review; a mature
  agent leaves less room than the paper's minimal one; 20 of the 30 tasks
  come from this repository, so it would learn Rovik's own habits.

**Proposal:** stage 1 first, for our defaults (milestone R) and then per
project (V). Our source campaign (E) runs as an experiment built so that it
can fail cleanly. Stage 2 comes only after a settings campaign has stopped
improving and E's result (W01) is positive. Stage 3a waits for W01 and W02
both (D19); 3b, hooks, only if stage 2's results show a plateau.

### 4.4 Who proposes the changes

- **Stage 1: no model.** A race (5.2 F) over a list of candidate
  configurations: each runs on the same tasks, and the clearly worse drop
  out as results come in. A fixed algorithm, cheap to check and hard to
  fool.
- **Stage 2: an improver with a fixed brief.** An Agent-mode run that reads
  the parent's profile, the archive's summary, the notebook and the budget
  left. It writes the child's profile and nothing else (a write scope, as
  step workers have in `worker.rs`). Its brief is part of Rovik, not of the
  profile: the original DGM's arrangement, which DGM-H didn't beat in
  coding.
- **Stage 3a, or never: an improver that changes itself.** The profile gains
  an **improver half**: the brief and analysis scripts, which a child may
  rewrite. When that child becomes a parent, its own improver writes its
  children (the paper's `a.Modify(a, A)`). This is the paper's hyperagent.
  It comes last (D19): in coding it hasn't beaten a fixed brief, and it
  adds a way to poison the loop.

The first brief hands the improver what the paper's agents spent dozens of
rounds inventing: the per-task results of parent and child, the failure
classes, the regressions, the notebook and the budget.

### 4.5 What never changes

These are outside the profile's schema, so no profile can name them, and
outside the improver's reach:

- the grader: the hidden tests, the required tests, the integrity check
  (computed by the grader, never by the candidate's own code), how success
  is computed;
- the measurement: tokens and cost counted outside the candidate (P08), the
  canaries and the run monitor (5.4 G);
- the security suite and its veto (5.11 A);
- the task sets, and which task is in which;
- budgets and caps, per run and per campaign;
- permissions, approvals, the AI approver, denied commands;
- the sandbox and the network rules;
- the adoption step and the profile's trust check (5.11 B);
- the profile's schema.

Changing any of them is a change to Rovik, made by a person. The adopted
profile changes only by adoption: no chat's agent writes `.rovik/profile/`
(the write tool refuses it, and commands see it read-only where the
sandbox can do that), and a change that arrives through git is used only
after the user approves it (5.11 B).

### 4.6 What a campaign produces

In the mode, a campaign produces a **profile** for the project: a settings
diff first, later a few lines of prompt text. Context handling and the loop
are switched between options Rovik already has, not rewritten. Rewriting
them takes code: scripts at hooks (stage 3b), or our own source campaign,
which ends as a pull request.

The agent's behaviour comes from three layers, and each stage reaches them
differently:

| What changes         | Prompt                                                                                                           | Context                                                                                                 | Loop flow                                                                                                                                                         |
| -------------------- | ---------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| In Rovik today       | `CODING_AGENT_SYSTEM_PROMPT`, the project's instruction file, each subagent kind's prompt, the tool descriptions | History (trimming or CLM), the repo map (about 2% of the window), how check and tool output is cut down | The run loop, then the end-of-run chain: change review, checks, up to 3 repair rounds (rollback on regressions, triage for repeated failures); Autopilot's stages |
| Stage 1: settings    | Only indirectly: a section appears when its feature is on                                                        | Trimming or CLM                                                                                         | Which existing paths run: subagent kinds, Balanced or Thorough, the checklist mode, the model and effort for each role                                            |
| Stage 2: prose       | Additions: project notes, lessons, notes added to the named sections                                             | That text goes into every request                                                                       | It can only ask the model; the harness runs the same flow                                                                                                         |
| Stage 3b: hooks      | —                                                                                                                | A note before a turn                                                                                    | A step added: a check before finishing, a note after the same failure twice                                                                                       |
| Source campaign (us) | Anything                                                                                                         | Anything                                                                                                | Anything, through a pull request                                                                                                                                  |

What each hands over, for example (the numbers are made up):

- **Stage 1:** `.rovik/profile/settings.json`, with a card such as "success
  the same within 10 points, 30% cheaper":

  ```json
  {
    "lead": { "reasoningEffort": "medium" },
    "contextMode": "clm",
    "subagents": {
      "enabled": true,
      "kinds": { "scout": { "enabled": true, "models": ["<cheaper model>"] } }
    }
  }
  ```

- **Stage 2:** a short `instructions.md` ("Rust tests:
  `cargo test --lib -p rovik-core <filter>`; the full suite takes about 10
  minutes"; "a new Tauri command must also be registered in
  `src-tauri/src/lib.rs`"), and lessons with their evidence and an expiry.
- **Source campaign:** a pull request, such as "show the agent the first
  error per file instead of the first 8,000 characters of check output".

Which layer has paid off elsewhere:

- The original DGM's coding gains (SWE-bench from 20% to 50%) came from
  tools, context and the loop: its abstract names "better code editing
  tools, long-context window management, peer-review mechanisms".
- DGM-H's gains outside coding came from prompts: checklists, decision
  rules, domain facts (app. E.2).
- In coding, prompt text rarely helps: instruction files didn't raise
  success in general, and added over 20% to cost (2.1).
- Rovik already has what the DGM discovered: edits by exact text, outlines
  and reads by symbol, trimming and CLM, a change reviewer, Thorough's two
  implementers with a pick, and repair rounds with rollback.

So, for Rovik, a campaign's result is most likely tuning rather than
invention: a cheaper configuration per project, a few lines of project
knowledge, and, through our own pull requests, the occasional fix to the
loop or the context for everyone.

**Loop and context knobs for stage 1.** The layers where coding gains came
from are mostly out of the race's reach, because their knobs are constants
in the code:

- repair rounds: `MAX_VERIFY_ROUNDS` = 3 and `STALLED_ROUNDS` = 2
  (`agent_run/verify.rs`);
- the check output the agent sees: `FAILURE_OUTPUT_CHARS` = 8,000
  (`agent_run/verify.rs`);
- the repo map: about 2% of the window, rebuilt at most every 120 seconds
  (`MAP_MAX_AGE`, `agent_run/run.rs`);
- CLM's thresholds: `TIERS` at 25, 50 and 75%, `URGENT` at 90%
  (`clm/prompt.rs`);
- change reviews: `MAX_CHANGE_REVIEWS_PER_RUN` = 3 and
  `MAX_REVIEWS_IN_ROW` = 2 (`agent_run/change_review.rs`); checklist
  nudges: `MAX_CHECKLIST_NUDGES` = 2 (`agent_run/run.rs`).

Making a few of them allowed settings would let the race tune the loop and
the context with no model-written change and nothing to poison.

- **Pros:** safe and measurable; it aims at the layers that paid off in
  coding; the defaults stay as they are until a race beats them.
- **Cons:** every exposed knob is something to support and document; knobs
  interact; small effects need many runs to show (5.5 D).
- **Verdict:** milestone R (R04), after the first race (R02) shows whether
  settings move anything.

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
base and passes at the reference twice. Each task records its date and its
top-level module.

- **Pros:** real work of this project, graded by its own tests, not by a
  model; the supply grows with every merge; the same format and grader as
  `scripts/eval`; dates let us prefer tasks newer than the model's training
  data, and commits made after a campaign began are a held-out set no
  variant has seen (5.4 A).
- **Cons:** many commits have no tests or mix several changes, so few are
  usable; the request writer sees the answer and can leak it (the check:
  no line of the reference's non-test code in the request); each task
  needs its dependencies at that commit, the slowest part; public projects
  are in the models' training data; tasks from outside projects or
  teammates can be poisoned (5.11).
- **Verdict:** first (milestone M), newest tasks first.

**B. Learn from the user's chats.** A chat that landed, passed its checks
and was kept becomes a candidate: its request, the project before it, the
landed diff as the reference, and the tests it added as the hidden tests.

- **Pros:** exactly what this user asks for; the request is real; a steady
  supply.
- **Cons:** the reference was written by the agent, so it may be wrong, and
  its tests were written to pass its own change, which favours variants
  that code like the old agent; it keeps the user's code and requests in
  the archive; few chats add tests; whoever can shape what lands can shape
  the tasks (5.11).
- **Verdict:** later, opt-in, training tasks only, never without the
  security suite.

**C. Generated tasks.** A task maker writes new tasks from the code: break
a function its tests cover and ask for the fix; remove a function and ask
for it again from its doc comment and tests; join two tasks into one
request. A task is kept only if the reference passes and the base fails.

- **Pros:** unlimited, and aimed: the maker can target what the notebook
  says is weak (edit misses in long files, say); nothing to leak on private
  code; harder tasks as the agent improves, which is the co-evolution the
  authors suggest.
- **Cons:** made-up bugs are easier than real ones and look different; a
  maker that can itself evolve drifts towards the tasks the current agent
  happens to pass or fail; it needs a filter that keeps learnable tasks
  (some variants pass them, not all).
- **Verdict:** low priority (milestone L): at most half the training tasks,
  never held out, and the maker's rules fixed.

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
- **Verdict:** the rule for stage 2's children.

**B. Add the growth score.** Give some weight to how much a variant's
descendants improved on it (app. D.4), so that a variant whose improver
writes good children is picked more, even when its own score is middling.

- **Pros:** it rewards the improver directly, which the paper did only
  afterwards; the Huxley-Gödel Machine found a variant's own score a poor
  guide to its descendants (2.1); it's computed from the archive, so it's
  free.
- **Cons:** it lags (it needs children first), it's noisy with few
  descendants, and it can favour a lucky line.
- **Verdict:** stage 2 (milestone E), as a small weight that grows with the
  number of descendants.

**C. A portfolio of strategies, with a bandit.** A few known strategies (A's
rule, UCB, softmax with a temperature, the niches of 5.6 A); each round
goes to one of them, more often to those whose children improved lately.
The improver may add a strategy, never remove the default.

- **Pros:** never much worse than the best strategy it holds; the paper's
  agents reinvented UCB and softmax anyway; small and testable.
- **Cons:** not fully self-referential; a bandit needs many rounds, and its
  rewards shift as the archive grows; no gain has been shown.
- **Verdict:** only if W01 and W02 are both positive (D19).

**D. Strategies the agent writes.** The strategy (which parents, how many
children, which tasks to run, when to stop evaluating a child) becomes a
short script in the improver half. It runs in the sandbox with a time
limit; its choice is checked (valid ids, within the budget); the default
is used when it fails. As in the paper's algorithm 5, the newest variant's
strategy picks the next parents. The grader and the rules of 4.5 stay
outside it.

- **Pros:** the self-modification the authors ask for, without touching the
  score; the scripts are short enough for a person to read.
- **Cons:** a strategy is judged by how fast the archive improves, which
  takes many rounds to see and is very noisy; app. E.5 found no gain over
  the fixed rule; a bad strategy can starve exploration in ways that show
  only much later.
- **Verdict:** only if W01 and W02 are both positive, and after C.

**E. A sequential test instead of fixed stages.** Run a child and its
parent on the same tasks in a random order. Only **discordant** tasks
count: those one of them passes and the other fails. On those, a
sequential probability ratio test (Wald) asks whether the child wins more
than half, with α = 0.05 and β = 0.2, and stops as soon as it can say
"worse" or "better". At a cap of 30 tasks the child is "inconclusive" and
kept only as a stepping stone. Before any real money is spent, simulated
A/A runs (the parent against itself, drawn from recorded results) must
show a false-promotion rate of α or less.

- **Pros:** most children are worse, so this saves the most money; concordant
  tasks carry no information, and ignoring them is what makes the test
  sharp; the error rates are chosen, not guessed; it can be checked on
  recorded data before it's trusted.
- **Cons:** tasks differ in difficulty, so their order matters (draw it at
  random, a language at a time); it stops a child that is weak now but
  would have been a stepping stone (keep a small share evaluated in full
  whatever happens, 5.5 C).
- **Verdict:** stage 2 (milestone E).

**F. A race, for settings.** Run every candidate configuration on the same
tasks, a block at a time. After each block, drop the candidates that a
paired test (corrected for the number of candidates, by Holm's method)
shows to be clearly worse than the best, as F-Race does. Stop when one
candidate is left, the budget is spent or the tasks run out; then decide by
D11.

- **Pros:** a fixed, well-studied algorithm; no model writes anything;
  cheap when some candidates are clearly worse; the result is easy to
  explain.
- **Cons:** only as good as its list of candidates; many candidates need
  many tasks before any can be dropped.
- **Verdict:** stage 1 (milestones R and V).

### 5.3 The model is frozen (limit 3)

**A. Models and effort as settings.** Which model each role uses (lead,
scout, reviewer, triage, test writer, judges), its reasoning effort, the
context mode.

- **Pros:** real gains in cost, and perhaps in success, with no new code,
  since the settings exist; the review of the first version called this the
  most promising part of the plan.
- **Cons:** only among the models the user has keys for; a profile is tied
  to the models it was measured with, so a new model needs a new check
  (5.9 C); what helps one model can hurt another.
- **Verdict:** stage 1, first (milestones R and V). A profile records its
  models, and Evolve doesn't offer it for others without a new check.

**B. Examples from the archive.** Short excerpts of runs that went well
(how the tests are run here, a repair that worked), kept as lessons the
agent loads when they're relevant.

- **Pros:** the model learns in context from its own successes on this
  project; no training.
- **Cons:** tokens on every request (prompt caching helps); they go stale as
  the code changes; they can teach habits that fit only the evaluation
  tasks.
- **Verdict:** stage 2 at the earliest (milestone L), inside the notebook's
  size limit (5.7 A).

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
tasks: never seen, never used to pick parents, used only at confirmation,
and topped up from commits made after the campaign began. A source
campaign's held-out tasks come only from outside projects and from Rovik
commits made after it began (5.4 F).

- **Pros:** the standard defence against fitting the set; a held-out set
  made of later commits can't have leaked into the archive.
- **Cons:** small projects have few tasks; each adoption decision spends the
  held-out set a little, so it has to keep growing.
- **Verdict:** milestone E; held-out sets grow until the power check passes
  (5.5 D).

**B. The grader out of reach.** Milestone P's fixes (answers hidden,
network off, test setup graded, web tools off during evaluation), plus:
the grader computes the integrity flags itself, from the run's diff, with
the campaign's fixed build; tokens and cost are counted outside the
candidate (P08); the improver writes only the profile; and a child whose
profile names a task's id, its hidden test files or a line of its
reference is rejected before it's evaluated.

- **Pros:** closes the cheap ways to cheat; most of the machinery exists
  (the sandbox, the integrity flags, write scopes).
- **Cons:** text checks stop accidents, not a determined optimiser (the
  repo already says this of deny rules); a profile can still learn the
  tasks' quirks without naming them, as the paper's grader learned the
  share of each grade.
- **Verdict:** milestones P and E, with A as the defence that counts.

**C. More than one number.** Success decides; cost, time and
interventions are limits (the G03 rule already caps a subagent kind's cost
at twice the baseline's); the archive keeps the front of success and cost.

- **Pros:** no "win by spending ten times the tokens"; it finds the cheaper
  variants users want; it matches the G03 rule.
- **Cons:** a front needs more evaluations to fill, and the user has to pick
  a point on it.
- **Verdict:** milestones R and E: success first, cost as a limit and a
  second objective.

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
results per task, the cost, the same-money arm and the security check;
every adoption can be undone.

- **Pros:** oversight where the paper puts it; settings and short text make
  short diffs.
- **Cons:** people approve what looks reasonable; a slow step between
  campaigns.
- **Verdict:** always.

**F. The improver sees no answers.** The improver of a source campaign works
on a snapshot of the parent's source: one commit, no history, without
`scripts/eval/` and `.eval-cache/`. An old Rovik task's solution is part
of today's source, so no Rovik commit older than the campaign is ever one
of its held-out tasks.

- **Pros:** closes the leak one level up from 3.2's; costs nothing.
- **Cons:** fewer Rovik tasks can be held out, so the held-out set needs
  outside projects (M02) or time.
- **Verdict:** milestone E (E02).

**G. Canaries, a monitor and transcripts.** Random canary strings are
planted among the hidden tests and the references; a candidate or a
transcript that contains one is rejected, and the campaign stops. A
monitor reads each evaluated run's log. Strong signals grade the run
`tampered`, and a candidate with a tampered run is rejected: a canary, a
path the sandbox hides, a change to test setup the reference doesn't make,
use of the network. Weak signals mark the transcript for reading: git
history commands, reads outside the workspace other than toolchains and
caches. A person reads 5 sampled winning transcripts before a win is
believed.

- **Pros:** catches what text checks miss; reading transcripts is what
  practitioners recommend before trusting a score (2.1); canaries are cheap
  and unambiguous.
- **Cons:** a person's time per campaign; weak signals raise false alarms,
  which is why they only mark a transcript.
- **Verdict:** milestones P (P09) and E (E04).

### 5.5 Noisy scores, small evaluations (limit 5)

**A. Paired comparisons.** Compare two arms on the same tasks, counting
wins and losses per task, and report a bootstrap interval of the
difference, with standard errors clustered by the task's top-level module
(tasks from one repository aren't independent), rather than two averages.

- **Pros:** much less noise for the same runs; the interval says how sure we
  are; clustering stops it from saying more than it knows.
- **Cons:** both arms need the same tasks, which the sequential test
  (5.2 E) and the race (5.2 F) give anyway; clustered intervals are wider.
- **Verdict:** milestone P (`compareCampaigns` gains it).

**B. Confirm before believing.** The best of many noisy children is lucky
on average (the winner's curse). Run the best few again, with repetitions
and on held-out tasks, before calling them better, and rank by a shrunken
estimate (the parent's score plus part of the difference).

- **Pros:** the archive stops chasing luck; cheap next to a wrong adoption.
- **Cons:** the extra runs; a real but small gain can look like noise.
- **Verdict:** milestone E.

**C. Keep stepping stones.** A small share of children are evaluated in
full however they start, and kept if they differ from what's there
(5.6 A).

- **Pros:** it keeps the slow starters that the paper's lineages show
  matter.
- **Cons:** the runs it costs.
- **Verdict:** milestone E, about one child in ten.

**D. Power before search.** Before a campaign spends, compute the
**minimum detectable effect** (MDE) of its held-out set: the smallest
change in success it would catch at 80% power and α = 0.05, from a pilot's
per-task noise, paired and clustered (5.5 A). A source campaign starts only
when its MDE is 5 points or less with the planned repetitions; a user's
campaign shows its MDE on the start card and claims nothing below it.

Rough sizes, for a paired comparison before clustering (which makes every
row larger), if a task's run-to-run uncertainty averages p(1−p) ≈ 0.1,
about what a standard deviation of 1.5 points over SWE-bench Verified's 500
tasks implies (2.1):

| Held-out tasks × repetitions per arm | Smallest change it can detect |
| ------------------------------------ | ----------------------------- |
| 10 × 3 (the first version of E05)    | about 23 points               |
| 50 × 3                               | about 10 points               |
| 100 × 3                              | about 7 points                |
| 100 × 6, or 200 × 3                  | about 5 points                |

- **Pros:** it says before spending whether a campaign can answer its
  question at all; it stops noise from being adopted.
- **Cons:** the honest numbers are sobering: a 5-point claim needs about
  600 task runs per arm. Most projects can't supply that, so their
  campaigns can show large changes in success, or changes in cost, and
  little else.
- **Verdict:** milestone P (P07), D9.

### 5.6 One number, one parent, nothing pruned (limit 6)

**A. Niches.** Keep the best variant per niche (the task's language, the
kind of change, a cost tier) rather than one ranking: quality-diversity, as
in MAP-Elites.

- **Pros:** it keeps stepping stones; it gives users a choice (a cheap
  variant, a careful one); it can grow a specialist per language.
- **Cons:** the niches are a handcrafted choice; more evaluations to fill
  them.
- **Verdict:** milestone L.

**B. Two parents.** The improver may merge two parents' profiles, section
by section, or by an instruction such as "take B's notes on running the
tests into A".

- **Pros:** it joins finds made on separate lines, which the paper's memory
  asked for; profiles are text and settings, so a merge is cheap.
- **Cons:** merged instructions can conflict, and it's harder to tell which
  half helped.
- **Verdict:** stage 2 (milestone E), as one of the improver's options.

**C. Pressure to stay small.** The profile's tokens count in its cost; the
instructions are 1,500 characters at most by default; the improver may
propose removals; a smaller child that isn't worse takes its parent's
place in the front.

- **Pros:** cheaper requests, less brittleness, less fitting of the set
  (the paper's grader grew rule by rule); instruction files raise cost
  (2.1).
- **Cons:** more evaluations; a removal can hurt rare tasks the sample
  missed.
- **Verdict:** stage 2 (milestones E and V07).

### 5.7 What a branch learns stays in it (limit 7)

**A. A notebook in the harness.** Every evaluated run adds records (task,
variant, outcome, failure classes, verify rounds, cost, and the change from
the parent on that task). The improver gets a summary and can query the
rest. It writes lessons as **references, not rules**: each with its
evidence (variant ids and results), a date and an expiry, changed one item
at a time (add, change or remove one), never rewritten whole.

- **Pros:** the paper's most portable inventions (tracking, memory,
  analysis) exist from the first round, not the fiftieth; sibling branches
  share them; most of the data is logged already; item-by-item edits avoid
  the "context collapse" of wholesale rewrites (ACE, 2.1).
- **Cons:** a built-in notebook shapes what the improver notices, which is
  the paper's point about handcrafting; memory that evolves can drift
  towards unsafe behaviour (2.1), which is why lessons are references with
  evidence and an expiry; context cost.
- **Verdict:** milestones N and E.

**B. Lessons into everyday chats.** Lessons about the project ("the tests
here need `--features x`") go into the adopted profile, so Agent, Plan and
Autopilot chats use them.

- **Pros:** value outside Evolve; text a person can review.
- **Cons:** lessons go stale as the project changes; they can clash with the
  instruction file (the profile comes after it); like any instruction file,
  they may raise cost more than success (2.1).
- **Verdict:** stage 2 only (V07), and only once held-out results show they
  don't cost more than they give.

**C. A person reads the notebook.** Once a month, the commonest failure
classes (`tools::error_class`, the verify verdicts, edit misses) and
regressions from evaluation runs and, if the user opts in, from ordinary
chats, with links to transcripts. Fixes go in as ordinary pull requests.

- **Pros:** value without any automated improver, and without any way to
  poison it; it's the "read the transcripts" loop that makes scores
  trustworthy.
- **Cons:** a person's time; slower than a loop.
- **Verdict:** milestone N, any time after P06.

### 5.8 Cost (limit 9)

**A. Runs side by side,** on this computer (milestone P removes the lock)
and on cloud hosts.

- **Pros:** hours become minutes; hosts exist.
- **Cons:** it saves time, not money; providers' rate limits; the
  heavy-command slots bound a laptop.
- **Verdict:** milestone P on this computer, hosts in V.

**B. Spend where it informs:** the sequential test (5.2 E), the race
(5.2 F) and frontier tasks (5.1 D).

- **Pros:** the largest savings.
- **Cons:** those options' cons.
- **Verdict:** milestones R, E and M.

**C. Reject broken children before evaluating them:** schema validation, the
tamper check, the canary check, a size limit and one smoke task.

- **Pros:** nearly free; the paper keeps only children that compile, and a
  typed profile breaks less often than free code.
- **Cons:** a smoke task can reject an odd but good child.
- **Verdict:** milestone E.

**D. An improver that knows its budget.** The rounds and money left are in
its brief from the start; the paper's agents learned to plan by them.

- **Pros:** large changes early and careful ones late, without a
  handcrafted schedule.
- **Cons:** a little context.
- **Verdict:** milestone E.

Roughly, if a run costs what the baseline's cap allows ($75 for 90 runs,
under $0.85 a run): a settings race (R) of six candidates on our 30 tasks
costs $150 to $300, depending on how soon candidates drop out. A source
campaign (E) of 30 children costs $250 to $400 before confirmation; a
confirmation that can detect 5 points (600 task runs for each of three
arms, the same-money arm at twice the cost of a run) adds about $2,000,
and one that can detect about 10 points about $500. The baseline campaign
(P06) gives the real figures before anything else is planned.

### 5.9 Oversight (limit 10)

**A. Power in steps.** The stages of 4.3 are steps of risk: settings; then
short text; then an improver that changes itself or scripts at hooks; and,
for us only, source changes behind review.

- **Pros:** risk grows with power, and each step is useful on its own.
- **Cons:** the first steps bound what can be learned.
- **Verdict:** the order of the plan.

**B. The rules of 4.5, enforced by the harness, not the prompt.**

- **Pros:** no text in a profile can lift them; the harness already fixes
  tools and budgets this way (subagents-plan D2).
- **Cons:** some useful changes are blocked (allowing a command the tasks
  need); those become suggestions on the adoption card.
- **Verdict:** milestones P and V.

**C. Watch after adoption.** For each profile version, Rovik tracks how
ordinary chats end: checks passed at the end of the run, changes kept or
discarded, cost per chat. If the adopted version does worse than the one
before, beyond a threshold set in advance, over a set number of chats, the
profile page proposes **Undo** with the numbers. A new model or Rovik
version marks the profile for a new check, and until then it stays in use
only if the user confirms.

- **Pros:** catches what the held-out tasks missed; uses what Rovik already
  records.
- **Cons:** chats differ from week to week, so this is a reason to check
  again, not proof.
- **Verdict:** milestone V (V06).

Always, whatever the option: every evaluated run is sandboxed, with the
deny rules and without the network or the web tools; every campaign has a
budget, stops after five rounds without progress, and can be stopped at
any time.

### 5.10 A run ends (limit 11)

**A. Standing campaigns.** A campaign stays open: new commits bring new
tasks; a new model or a Rovik update re-checks the adopted profile; rounds
run when the computer is idle or on a host, within a monthly budget.

- **Pros:** progress that doesn't wait for a person to start it again; it
  follows model changes.
- **Cons:** it spends money in the background; it needs stop rules and a
  visible bill.
- **Verdict:** milestone O, opt-in, with a hard monthly cap, only if W02 is
  positive.

**B. Carry the improver to the next project.** A new project's campaign
starts with the best improver half found so far, not its project lessons:
the part the paper found to transfer.

- **Pros:** the transfer result (imp@50 of 0.630 against 0.0); faster
  starts.
- **Cons:** shown in one domain that isn't coding; an improver shaped on
  Rust projects may suit Python ones less; it stays on this computer, never
  shared between people.
- **Verdict:** only if W01 and W02 are both positive (D19).

### 5.11 Poisoned tasks and profiles (limit 13)

**A. A security suite with a veto.** A fixed set of neutral tasks, owned by
the harness, that touch TLS, JWT, YAML and pickle loading, shell quoting,
SQL and paths, with tests that fail the insecure idiom; and a static scan
of every patch an evaluated run writes (semgrep rules in the harness, a
built-in rule set in the app). A regression vetoes a candidate whatever its
score. Tasks from outside projects or chats are never used without it.

- **Pros:** it aims at what a poisoned task teaches, which a neutral
  held-out set doesn't show; cheap: the suite runs only on the few
  candidates that reach confirmation, and scans are fast.
- **Cons:** it covers only the habits it knows; in the one study of the
  attack, a generic security suite didn't remove a targeted poison (2.1),
  so it lowers the risk rather than removing it.
- **Verdict:** milestone S, before any outside task is used.

**B. The profile is untrusted input.** The adopted profile is pinned by its
hash in the user's own Rovik data, not trusted because it sits in the
repository. When `.rovik/profile/` changes through git (a pull, a branch
switch), Rovik shows the diff and asks before using it, as VS Code's
workspace trust does; until then it keeps using the version this user last
approved. Its text goes into the prompt marked as guidance supplied by the
project, below the built-in rules.

- **Pros:** a pull request can't silently change every teammate's agent;
  poisoned rule files are a demonstrated attack on coding editors (2.1).
- **Cons:** one more question for the user; it doesn't cover `AGENTS.md`,
  which is the same channel today (a separate change could).
- **Verdict:** milestone V (V01).

**C. Check what a profile says.** Profile diffs are checked for
instructions that weaken security (turning off certificate checks,
`--no-verify`, broad allow rules) and refused.

- **Pros:** cheap; catches the obvious.
- **Cons:** a text check, so it stops accidents only.
- **Verdict:** milestone S (S03).

### 5.12 Better, or just more compute? (limit 12)

**A. A same-money arm.** Confirmation compares three arms on the held-out
tasks: (A) the current agent, one attempt; (B) the current agent with k
attempts per task, k chosen so that its cost per task is at least the
candidate's, the best attempt picked by the project's own checks (never by
the hidden tests), or Autopilot Thorough at the same cost; (C) the
candidate, one attempt. A candidate counts as better only if it beats both
A and B by more than the MDE. The campaign's own cost is shown beside the
result, as the number of tasks it takes to pay for itself.

- **Pros:** it tells a better agent apart from more compute, the comparison
  later work found decisive (2.1); B may itself be the better thing to ship,
  as an "attempts per task" setting.
- **Cons:** B costs k times a run; its pick needs checks the project has.
- **Verdict:** milestones P (P06 measures B against the baseline) and E
  (E05, W01).

**B. Write the bar down first.** Before confirmation runs, the success
criterion, the MDE, the arms and the stop rule go into
`docs/design/evolve-results.md` (for us) or the campaign's first record
(for users), and they don't change afterwards.

- **Pros:** no moving the bar after seeing the results; nearly free.
- **Cons:** none worth naming beyond the discipline.
- **Verdict:** milestones E (E05) and V.

---

## 6. Proposed decisions

| #   | Decision                                                                                                                                                                                                                                                                                                      | Why                                                                                                                      |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| D1  | **Evolve is a chat mode**, the fourth beside Agent, Plan and Autopilot (`ChatMode` in `src/lib/chatMode.ts` gains `'evolve'`; `StartRequest.mode = "evolve"`). One message is a **campaign**: a goal and a budget. Its archive and the adopted profile belong to the project.                                 | Users know the shape from Autopilot: one message, a pipeline the core drives, records in the chat.                       |
| D2  | **A campaign is a pipeline driven by the core** (`agent_run/evolve/`, beside `autopilot/`): tasks, power, baseline, rounds, security, confirm, adopt (4.2), each stage leaving a record.                                                                                                                      | Autopilot's driver, stage records and budget (autopilot-plan D2, D3, D16) are reused.                                    |
| D3  | **Stages of trust** (4.3): settings first; short prose with an improver whose brief is fixed after a settings campaign has stopped improving and W01 is positive; an improver that changes itself, or hooks, last. Our own source evolves only in E, as an experiment.                                        | Risk grows with power (5.9 A), and the evidence for prose and self-modification in coding is weakest (2.1).              |
| D4  | **Settings are searched by a race, not a model** (5.2 F).                                                                                                                                                                                                                                                     | A small discrete space; a fixed algorithm can't be poisoned or talked into anything.                                     |
| D5  | **The rules of 4.5 never evolve**: the grader (with integrity flags it computes itself), measurement outside the candidate, the canaries and the monitor, the security suite, the task sets, budgets, permissions, approvals, denied commands, sandbox, network rules, adoption, the trust check, the schema. | Nothing graded may change its grader; the harness, not the model, fixes the limits (subagents-plan D2).                  |
| D6  | **Measurement before search** (milestone P): the answers and the network hidden from evaluated runs, integrity graded by the grader, identities recorded, comparisons paired and clustered, power computed, cost counted outside the candidate, runs monitored, and the baseline run with a real model.       | Today an optimiser would find the answers (3.2).                                                                         |
| D7  | **Tasks from the project's history first, newest first** (5.1 A); tasks from outside projects or chats only with the security suite (5.11 A); generated tasks at low priority (5.1 C).                                                                                                                        | Graded by the project's own tests, and they keep coming; newer tasks are less likely to be in the model's training data. |
| D8  | **Three task sets** (5.4 A), the held-out set made of tasks no candidate can have seen. For a source campaign: outside projects and Rovik commits made after it began, never older Rovik commits (5.4 F).                                                                                                     | The defence against fitting the set that no text check gives.                                                            |
| D9  | **Power before search** (5.5 D): a source campaign needs an MDE of 5 points or less; a user's campaign shows its MDE before it spends and claims nothing below it.                                                                                                                                            | A 10-task held-out set can only see changes of about 20 points or more.                                                  |
| D10 | **Children are tested against their parent on discordant tasks by a sequential test**, α = 0.05, β = 0.2, capped at 30 tasks, and checked by A/A simulation first (5.2 E).                                                                                                                                    | The largest saving, with error rates that are chosen and checked.                                                        |
| D11 | **Stage 1's goal is "cheaper, and not worse"**: success no worse than the current agent's by more than a margin stated in advance, and cost lower. "Better" is claimed only above the MDE. Cost stays a limit at every stage (the G03 rule's ratio, twice the baseline's, by default).                        | Settings move cost far more than success, and cost differences show with far fewer runs.                                 |
| D12 | **A same-money arm, and the bar written down first** (5.12): a candidate is better only if it beats the current agent and the current agent given the same money as more attempts; criterion, MDE, arms and stop rule are recorded before confirmation.                                                       | Without it, "better agent" and "more compute" look the same.                                                             |
| D13 | **A security suite with a veto** (5.11 A), on every candidate before confirmation.                                                                                                                                                                                                                            | Poisoning doesn't show on neutral held-out tasks.                                                                        |
| D14 | **The profile is untrusted input** (5.11 B): pinned by hash, changes through git approved before use, its text below the built-in rules; lessons are references with evidence, a date and an expiry, changed one item at a time (5.7 A).                                                                      | Rule files are a demonstrated attack channel, and evolving memory can drift.                                             |
| D15 | **A person adopts** (5.4 E), after reading 5 winning transcripts in a source campaign (5.4 G); versions are kept, with undo; a profile records the models it was measured with.                                                                                                                               | Oversight where the paper puts it, and scores aren't taken at face value.                                                |
| D16 | **Rovik watches after adoption** (5.9 C) and re-checks a profile when the model or Rovik changes.                                                                                                                                                                                                             | Held-out tasks are a sample; ordinary chats are the real use.                                                            |
| D17 | **Evolve asks before it spends**: it shows the tasks, the MDE and the projected cost, starts on the user's go, keeps to its budget and stops after five rounds without progress.                                                                                                                              | Unlike an Autopilot task, a campaign's cost is set by its size, which the user should see first.                         |
| D18 | **A notebook for people first** (5.7 C): failure classes and regressions read by a person each month, fixes as ordinary pull requests.                                                                                                                                                                        | Value without an automated improver, and nothing to poison.                                                              |
| D19 | **The self-referential parts wait**: the bandit and agent-written strategies (L01, L02), the improver that changes itself (stage 3a) and improver transfer (O02) are off the roadmap unless W01 and W02 are both positive.                                                                                    | Their only evidence is not significant (app. E.5) or outside coding (maths grading).                                     |

---

## 7. Milestones

Rules for every task are autopilot-plan section 6's: one task per pull
request, the exact check commands, tests in the same task, and a note under
"Notes from implementation" when a task doesn't fit reality.

### Milestone P: a measurement a search can't fool

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                           | Needs    | Done when                                                                                                                                 |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| P01 | Evaluation workspaces outside the repository and the cache (a temporary root); the runner's sandbox hides the repository, `.eval-cache` and `scripts/eval` (`Confine.hide`); evaluated commands run without the network (`--unshare-net` under bubblewrap, the same where Landlock or Seatbelt allows it); the web tools off unless a task allows them; canaries planted among the hidden tests and references | —        | A scripted cheater in `dry-run` that runs `git log --all`, reads `expected/`, calls `browse_url` and runs `curl` finds nothing, and fails |
| P02 | The grader computes the integrity flags itself (`repair::integrity_flags`, `is_check_setup`) from the run's diff, with the campaign's fixed build, never from the run's own report: a flag on a file the reference didn't change grades the run `tampered`                                                                                                                                                     | —        | A cheater that adds a `conftest.py` skipping the hidden tests fails, with the reason, even when its own report shows no flags             |
| P03 | `result.json` records the runner's commit and binary hash and the hashes of the settings and the profile; resuming with another identity refuses                                                                                                                                                                                                                                                               | —        | Resuming a campaign with a rebuilt runner stops with a message                                                                            |
| P04 | `compareCampaigns` adds paired differences per task, a bootstrap interval and standard errors clustered by top-level module; the G03 rule stays as it is                                                                                                                                                                                                                                                       | —        | Tests on made-up results with a known difference and known clusters                                                                       |
| P05 | A run id and a log folder per headless run (no process-wide lock); `--jobs N`; a cargo target folder per job inside the folders the sandbox lets a command write                                                                                                                                                                                                                                               | —        | Two runs at once grade as they do one after the other                                                                                     |
| P06 | The subagents plan's G01 for real: the pilot, then the baseline, with a real model, and beside it the same-money arms (two attempts with a pick by the project's checks; Autopilot Balanced and Thorough, the existing variants); fix what it finds first                                                                                                                                                      | P01–P05  | The baseline's `summary.md`, the cost per run, and the same-money arms against the baseline                                               |
| P07 | Power: the MDE of a task set from P06's per-task noise (paired, clustered), printed by `compare` and written into `summary.md`                                                                                                                                                                                                                                                                                 | P04, P06 | The MDE of the 30 tasks is known; E refuses to start above 5 points                                                                       |
| P08 | Measurement outside the candidate: a recording proxy between the runner and the provider counts tokens and cost from the provider's own replies and enforces the token cap; the grader takes its numbers, not the runner's                                                                                                                                                                                     | P06      | A runner built to under-report its usage is caught, and its cost comes out right                                                          |
| P09 | The run monitor (5.4 G): strong signals grade a run `tampered`, weak ones mark its transcript for reading                                                                                                                                                                                                                                                                                                      | P01, P02 | The scripted cheaters of P01 and P02 are graded `tampered`; an honest run isn't                                                           |

### Milestone R: settings, raced

| ID  | Task                                                                                                                                                                                            | Needs | Done when                                                                                              |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ------------------------------------------------------------------------------------------------------ |
| R01 | `eval.mjs race`: candidates from a list of allowed settings, run block by block on the same tasks, dropped by paired tests with Holm's correction (5.2 F), decided by D11                       | P07   | A dry run with scripted outcomes drops a clearly worse candidate and keeps a cheaper equal one         |
| R02 | The first race on our 30 tasks: the four subagent kinds (the subagents plan's G02 variants), the lead's reasoning effort, a cheaper model for scouts, reviewer and triage, CLM against trimming | R01   | Its result, with the MDE, in `docs/design/evolve-results.md`                                           |
| R03 | A winning configuration goes into the defaults by hand, in a pull request, as `apply-defaults` does for the subagent kinds                                                                      | W03   | The pull request cites the race                                                                        |
| R04 | A few loop and context constants (4.6) become allowed settings, with today's values as their defaults, so a race can vary them                                                                  | R02   | A race varies `FAILURE_OUTPUT_CHARS` and `MAX_VERIFY_ROUNDS`; with the defaults, runs behave as before |

### Milestone M: tasks from history

| ID  | Task                                                                                                                                                                                                  | Needs    | Done when                                                                                                            |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------- |
| M01 | `eval.mjs mine`: candidate commits (code and tests changed), the request writer, the leak check, `validate`; each task records its date and top-level module, and the model's training cutoff filters | P06      | Mining this repository's last 300 commits gives validated tasks, and no request holds a line of its reference's code |
| M02 | Setup from the detected stack (`project_profile.rs`) and Autopilot's installs (`prepare.rs`); a few active outside projects chosen for recent commits with tests                                      | M01      | Tasks mined from outside Python and TypeScript projects validate                                                     |
| M03 | The frontier: each task's pass rate across the archive; retired, parked and anchor tasks                                                                                                              | E01, M01 | A retired task's share of the runs falls; the anchors never change                                                   |

### Milestone S: security

| ID  | Task                                                                                                                                                   | Needs | Done when                                                                                   |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ----- | ------------------------------------------------------------------------------------------- |
| S01 | The security suite (5.11 A): neutral tasks on TLS, JWT, YAML and pickle loading, shell quoting, SQL and paths, with tests that fail the insecure idiom | P06   | The current agent passes it; a scripted candidate that disables certificate checks fails it |
| S02 | A static scan of every patch an evaluated run writes (semgrep rules in the harness)                                                                    | P06   | A planted insecure patch is flagged                                                         |
| S03 | The check of profile diffs for instructions that weaken security (5.11 C)                                                                              | —     | A profile that says to skip certificate checks is refused                                   |

### Milestone N: the notebook, for people

| ID  | Task                                                                                                                                                                                                                    | Needs | Done when                                            |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ---------------------------------------------------- |
| N01 | `eval.mjs notebook`: within and across campaigns, the commonest failure classes, edit misses, verify verdicts and regressions per task, linked to transcripts; from ordinary chats' agent logs too, if the user opts in | P06   | A report for the baseline campaign, read by a person |

### Milestone E: an experiment on Rovik's own source

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                          | Needs                   | Done when                                                                                                                     |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| E01 | `eval.mjs evolve`: the archive (`archive.json`: id, parent, commit, results per task, children, status), the paper's selection with the growth score's weight, children built for `--runner`                                                                                                                                                                                  | P07                     | A dry run with the scripted model grows an archive of 5 children                                                              |
| E02 | The improver: the headless runner in Agent mode on a snapshot of the parent's source (one commit, no history, without `scripts/eval/` and `.eval-cache/`); its brief in `scripts/evolve/improver.md`, outside the child's write scope; write scope `agent.rs` and `agent_run/` without their permission, sandbox, budget and integrity code; every child checked for canaries | E01, P08                | A child that changes `scripts/eval/`, `overlay/`, `budget.rs` or `repair.rs`, or holds a canary, is rejected before its build |
| E03 | The sequential test on discordant tasks (α = 0.05, β = 0.2, a cap of 30); one child in ten in full; the smoke task; the tamper check                                                                                                                                                                                                                                          | E01                     | A/A simulations on recorded results promote at most 5% of children                                                            |
| E04 | The notebook for the improver: lessons as references with evidence, a date and an expiry, changed one item at a time; 5 winning transcripts read by a person before any win is believed                                                                                                                                                                                       | E01, N01, P09           | The improver's request carries the summary; a lesson without evidence is refused                                              |
| E05 | The bar, written into `docs/design/evolve-results.md` first; then confirmation on a held-out set big enough for P07 (outside projects and Rovik commits after the campaign began), with the three arms of 5.12 A and the security suite; a branch and a pull request with the evidence                                                                                        | E03, E04, M02, S01, S02 | One campaign's result, positive or not, reviewed                                                                              |

### Milestone V: the Evolve mode

| ID  | Task                                                                                                                                                                                                                      | Needs         | Done when                                                                                         |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------- | ------------------------------------------------------------------------------------------------- |
| V01 | The profile: `.rovik/profile/settings.json` with a schema of the allowed settings; pinned by hash in the user's Rovik data; a change through git shown and approved before use (5.11 B); no chat's agent writes it (4.5)  | —             | A profile with a setting outside the schema is refused; a pulled change isn't used until approved |
| V02 | The mode: `'evolve'` in `ChatMode` and the mode switch; the driver `agent_run/evolve/` with the stages of 4.2 as records                                                                                                  | V01           | A campaign's stages show as records                                                               |
| V03 | Tasks, power and the race in the core: mining, the parsers of the four check kinds and the race from `lib.mjs`, in Rust                                                                                                   | R01, M02      | The core grades the 30 tasks as `lib.mjs` does, and races as R01 does                             |
| V04 | The campaign's view and the adoption card (intervals, the MDE, the same-money arm, the security check, the diff); versions and Undo on the profile page                                                                   | V02           | Adopting and undoing pass in the end-to-end tests                                                 |
| V05 | The budget, the confirmation before spending and the stop rules (D17); campaigns on hosts                                                                                                                                 | V02           | A campaign stops at its budget, and after five rounds without progress                            |
| V06 | Watching after adoption (5.9 C), and a new check when the model or Rovik changes                                                                                                                                          | V04           | A scripted drop in outcomes after an adoption proposes Undo with the numbers                      |
| V07 | Stage 2: `instructions.md` (1,500 characters by default) and lessons, written by the improver with a fixed brief; prose costs tokens in the score; S03 on every diff; the text marked as guidance supplied by the project | W01, W02, S03 | Measured against a settings-only campaign on the same tasks                                       |

### Milestone L: a loop that changes itself (only if W01 is positive)

| ID  | Task                                                                          | Needs    | Done when                                                               |
| --- | ----------------------------------------------------------------------------- | -------- | ----------------------------------------------------------------------- |
| L01 | A portfolio of strategies with a bandit (5.2 C)                               | W01, W02 | Compared with the fixed rule on the same budget (W04)                   |
| L02 | Strategies written by the improver (5.2 D)                                    | L01      | A strategy that times out or returns a bad id falls back to the default |
| L03 | Niches (5.6 A)                                                                | W01      | The archive keeps the best per niche                                    |
| L04 | Generated tasks (5.1 C), at most half the training tasks                      | W01, M03 | Every generated task validated; none in the held-out set                |
| L05 | Examples from the archive (5.3 B); the change reviewer as a tie-break (5.4 D) | W01      | Measured against E's campaign on the same tasks                         |

### Milestone O: open-ended operation (only if W02 is positive)

| ID  | Task                                                                                    | Needs    | Done when                                                    |
| --- | --------------------------------------------------------------------------------------- | -------- | ------------------------------------------------------------ |
| O01 | Standing campaigns (5.10 A), opt-in, with a hard monthly cap                            | W02      | A new commit with tests becomes a task without a person      |
| O02 | The improver carried to the next project (5.10 B)                                       | W01, W02 | A second project's campaign starts from the first's improver |
| O03 | A design note on scripts at hooks (4.3 stage 3b), if stage 2 shows the profile plateaus | V07      | The note, with what a hook could reach that a profile can't  |

### Milestone W: was it worth it

| ID  | Task                                                                                                                                                                  | Needs | Done when                       |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ------------------------------- |
| W01 | E's result, by the bar written in advance: does its best child beat both the current agent and the same-money arm by more than the MDE, within the cost limit of D11? | E05   | `docs/design/evolve-results.md` |
| W02 | V's settings campaigns on two outside projects: cheaper, and not worse by more than the stated margin?                                                                | V05   | The same file                   |
| W03 | R's race: does a configuration beat the defaults by D11?                                                                                                              | R02   | The same file                   |
| W04 | L01 and L02 against the fixed rule: the paper's app. E.5, on our tasks                                                                                                | L02   | The same file                   |

Order at a glance:

```
P01..P05 ─▶ P06 ─▶ P07 ─▶ R01 ─▶ R02 ─▶ W03 ─▶ R03     settings, for our defaults
                                 R02 ─▶ R04              loop and context knobs
            P06 ─▶ N01                                   the notebook, for people
P07 + P08 + P09 ─▶ E01..E04 ─┐
M01 ─▶ M02 ─────────────────┼─▶ E05 ─▶ W01              the source experiment
S01, S02 ───────────────────┘
R01 + M02 ─▶ V01..V06 ─▶ W02                             the mode, settings only
W01 positive ─▶ L03..L05
W01 and W02 positive ─▶ V07, L01, L02, O02
W02 positive ─▶ O01
```

Each W decides whether what follows is worth its cost. If W01 is negative,
stage 2 and milestone L wait, and Evolve stays a settings race.

---

## 8. Risks and known limits

- **Small headroom.** The paper's gains started from a minimal agent;
  Rovik's has subagents, Autopilot, integrity checks and context modes. The
  most likely honest result is a saving in cost from settings, not a large
  gain in success. That is still worth having.
- **Cost.** A settings race costs hundreds of dollars on our tasks, a source
  campaign with a full confirmation thousands (5.8), and a user's campaign
  tens. The budget and the confirmation before spending are the bound;
  W01 to W03 say whether it pays at all.
- **Few tasks, small effects.** A project with little test history gives
  few tasks, and its MDE stays large. Below a minimum (15 usable tasks,
  say) Evolve says so and doesn't start; above it, the card says what the
  campaign can and can't show.
- **Fitting the set.** The held-out set is the defence, and each adoption
  spends some of it.
- **Poisoning.** Tasks from outside projects, chats or teammates can teach a
  habit a neutral held-out set doesn't show. The security suite lowers the
  risk; settings-only campaigns avoid most of it.
- **Memory that drifts.** Lessons that evolve can move towards unsafe
  behaviour; they are references with evidence and an expiry, edited one
  item at a time.
- **Profiles across updates and models.** A Rovik update changes the
  built-in prompts a profile adds to, and a profile measured with one model
  may not suit another. Rovik marks the profile for a new check (5.9 C).
- **A profile is in every system prompt.** It is pinned and approved before
  use (5.11 B), and no text in it can lift the harness's rules. `AGENTS.md`
  is the same channel today, without the approval step.
- **Text checks can be beaten.** The tamper and profile checks are for
  accidents; the held-out set, the sandbox, the monitor and the transcripts
  are the defence.
- **Review fatigue.** After many adoptions people approve without reading.
  The card leads with the held-out numbers, the same-money arm and the size
  of the diff.
- **Known limits of v1:** settings only; no generated tasks, no strategies
  written by the agent, no hooks, no sharing between people, no
  fine-tuning.

---

## 9. Questions to decide

| Question                                                          | Proposal                                                                                  |
| ----------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| The name: Evolve, Improve or Tune?                                | **Evolve**; **Tune** fits stage 1 better, and is worth weighing                           |
| The order: the settings race (R) before the source experiment?    | **Yes**: R is cheaper, and may change the defaults for everyone                           |
| Is an adopted profile committed, for the team, or kept locally?   | **Committed**, in `.rovik/profile/`, but each change approved locally before use (5.11 B) |
| The same approval step for `AGENTS.md`?                           | **Worth a separate change**                                                               |
| Which outside projects supply held-out tasks?                     | **Active ones with tests**, chosen in M02                                                 |
| Ship "attempts per task" as a setting if the same-money arm wins? | **Yes**                                                                                   |
| The MDE a source campaign must reach                              | **5 points**                                                                              |
| The margin for "not worse" in a user's campaign                   | **10 points**, what 50 held-out tasks × 3 can support, shown on the card                  |
| Chats as tasks (5.1 B), at all?                                   | **Later, opt-in**, training only, with the security suite                                 |
| Where does the product's grader live?                             | **In the core** (V03), so the app, the runner and a host grade alike                      |
| A user campaign's default budget?                                 | **Projected from a pilot**, with $30 and 6 hours as the default cap                       |
| Generated tasks before there are many held-out ones?              | **No** (L04)                                                                              |

---

## 10. Sources

| Source                                                                                                                                                                                       | Used for                                    |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------- |
| Zhang, Zhao, Yang, Foerster, Clune, Jiang, Devlin, Shavrina, _Hyperagents_, arXiv 2603.19461 (2026); code at [facebookresearch/Hyperagents](https://github.com/facebookresearch/Hyperagents) | Sections 1 and 2                            |
| Zhang, Hu, Lu, Lange, Clune, _Darwin Gödel Machine: open-ended evolution of self-improving agents_, arXiv 2505.22954 (2025)                                                                  | The original loop; its appendix H (2.1)     |
| Wang, Zhu et al., _Rethinking the evaluation of harness evolution for agents_, arXiv 2607.12227 (2026; the figures are from its later versions)                                              | 2.1, 5.12                                   |
| Lee et al., _Meta-Harness: end-to-end optimization of model harnesses_, arXiv 2603.28052 (2026)                                                                                              | 2.1                                         |
| Agrawal et al., _GEPA: reflective prompt evolution can outperform reinforcement learning_, arXiv 2507.19457, ICLR 2026                                                                       | 2.1                                         |
| Gloaguen et al., _Evaluating AGENTS.md: are repository-level context files helpful for coding agents?_, arXiv 2602.11988 (2026)                                                              | 2.1, 4.3 stage 2, 5.6 C                     |
| Wang, Piękos et al., _Huxley-Gödel Machine_, arXiv 2510.21614 (2025)                                                                                                                         | 2.1, 5.2 B                                  |
| Bjarnason, Silva, Monperrus, _On randomness in agentic evals_, arXiv 2602.07150 (2026)                                                                                                       | 2.1, 5.5 D                                  |
| Miller, _Adding error bars to evals_, arXiv 2411.00640 (2024)                                                                                                                                | 2.1, 5.5 A                                  |
| Kapoor, Stroebl, Siegel, Nadgir, Narayanan, _AI agents that matter_, arXiv 2407.01502 (2024)                                                                                                 | 2.1, D11                                    |
| Grace, Hadfield, Olivares, De Jonghe, [_Demystifying evals for AI agents_](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents), Anthropic (2026)                         | 2.1, 5.4 G                                  |
| OpenAI, _Why we no longer evaluate SWE-bench Verified_ (2026), read through search extracts                                                                                                  | 2.1, 5.1 A                                  |
| [SWE-bench issue #465](https://github.com/SWE-bench/SWE-bench/issues/465), _Repo state loopholes during agentic evaluation_ (2025)                                                           | 2.1, P01                                    |
| Zheng et al., _SWE-Bench Pro Verified_, arXiv 2609.08149 (2026)                                                                                                                              | 2.1                                         |
| Gabor, Lynch, Rosenfeld, _EvilGenie: a reward hacking benchmark_, arXiv 2511.21654 (2025)                                                                                                    | 2.1, 5.4 G                                  |
| Roesner, Kohno, _Reflections on trusting trust, revisited: contaminating self-modifying AI coding agents with poisoned benchmarks_, arXiv 2609.17817 (2026)                                  | 2.1, 5.11                                   |
| Shao et al., _Your agent may misevolve: emergent risks in self-evolving LLM agents_, arXiv 2509.26354, ICLR 2026                                                                             | 2.1, 5.7 A                                  |
| Liu et al., _"Your AI, my shell": demystifying prompt injection attacks on agentic AI coding editors_, arXiv 2509.22040 (2025)                                                               | 2.1, 5.11 B                                 |
| Zhang, Hu et al., _Agentic context engineering_, arXiv 2510.04618, ICLR 2026                                                                                                                 | 2.1, 5.7 A                                  |
| Clune, _AI-GAs: AI-generating algorithms_, arXiv 1905.10985 (2019)                                                                                                                           | The three things that improve themselves    |
| Mouret and Clune, _Illuminating search spaces by mapping elites_, arXiv 1504.04909 (2015)                                                                                                    | Niches (5.6 A)                              |
| Faldor, Zhang, Cully and Clune, _OMNI-EPIC_, ICLR 2025                                                                                                                                       | Generated tasks that stay learnable (5.1 C) |
| Birattari, Stützle, Paquete and Varrentrapp, _A racing algorithm for configuring metaheuristics_, GECCO 2002                                                                                 | The race (5.2 F)                            |
| Holm, _A simple sequentially rejective multiple test procedure_, Scandinavian Journal of Statistics 6 (1979)                                                                                 | Many candidates at once (5.2 F)             |
| Auer, Cesa-Bianchi and Fischer, _Finite-time analysis of the multiarmed bandit problem_, Machine Learning 47 (2002)                                                                          | UCB and the portfolio (5.2 C)               |
| Wald, _Sequential tests of statistical hypotheses_, Annals of Mathematical Statistics 16 (1945)                                                                                              | The sequential test (5.2 E)                 |
| Strathern, _'Improving ratings': audit in the British university system_, European Review 5 (1997)                                                                                           | Goodhart's law, as the paper cites it       |

---

## 11. Review: what changed

A review of the first version judged milestone P well supported, the rest
of the loop a bet that later work on coding agents doesn't favour, and the
plan unable to detect a real gain: its held-out set was too small, and
nothing spent the same money another way. Its points, and what this
version does with them:

| Review point                                            | Change                                                                                                                                 | Where                   |
| ------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- | ----------------------- |
| Power before search                                     | The MDE is computed before a campaign; a source campaign needs 5 points or less; the held-out set grows until it does                  | 5.5 D, P07, D9          |
| Close the improver's leak                               | A snapshot without history; no older Rovik commit is held out; canaries                                                                | 5.4 F, 5.4 G, E02, D8   |
| A same-money control, and the bar written first         | Three arms at confirmation; the criterion recorded before it runs                                                                      | 5.12, E05, W01, D12     |
| A security suite with a veto                            | The suite, the patch scan and the profile check                                                                                        | 5.11, S, D13            |
| The profile as untrusted input                          | Pinned by hash, changes approved before use, text below the built-in rules; lessons as references with an expiry, edited one at a time | 5.11 B, 5.7 A, V01, D14 |
| Settings before prose                                   | Stage 1 is settings only; prose has a 1,500-character budget and costs tokens in the score                                             | 4.3, V07, D3            |
| A concrete sequential test                              | Discordant tasks only, α and β set, a cap of 30, checked by A/A simulation                                                             | 5.2 E, E03, D10         |
| Transcripts and a monitor                               | Strong and weak signals; 5 winning transcripts read before a win is believed                                                           | 5.4 G, P09, E04, D15    |
| Undo on real-world signals                              | Ordinary chats watched after adoption; a new check when the model or Rovik changes                                                     | 5.9 C, V06, D16         |
| Defer the self-referential parts                        | Strategy evolution, the self-changing improver and improver transfer wait for W01 and W02                                              | D19, L, O               |
| The paper's numbers                                     | DGM's figures marked as reported, not run again; DGM-H's lead over the customised DGM marked not significant; the cost's setting named | 1                       |
| Missing: the same money spent on attempts, as a feature | Measured in P06; "attempts per task" becomes a setting if it wins                                                                      | 5.12 A, P06             |
| Missing: a person reading the failure classes           | The notebook, for people first                                                                                                         | 5.7 C, N01, D18         |
| Missing: a contamination guard                          | Tasks dated, and filtered by the model's training cutoff                                                                               | 5.1 A, M01, D7          |

Taken differently from the review:

- **The 10 Python and Go tasks don't count towards the held-out set.**
  Their changes are public and older than the models, so they stay as
  anchors.
- **"The campaign's amortised cost per task"** is replaced by two plain
  numbers: the same-money arm gets at least the candidate's cost per task,
  and the campaign's own cost is shown as the number of tasks it takes to
  pay for itself.
- **"Reads outside the workspace" isn't enough to fail a run.** Agents read
  toolchains and caches legitimately, so it only marks a transcript for
  reading; the strong signals fail the run.
- **Semgrep runs in the harness only.** The app gets a built-in rule set
  rather than a dependency on a Python tool.

Corrections to the review's sources, from checking each one (2.1):

- **Misevolution:** refusals fell by 45 points (99.4% to 54.4%), not 55%;
  the 55% is a slip in the paper's first version.
- **GEPA:** "6% on average" is the second version's figure, over six tasks;
  the first said 10% over four.
- **AGENTS.md files:** the advice to describe "only minimal requirements"
  is from the first version; the latest says to include only what isn't
  already in the README, and to evaluate a file before adopting it.
- **Harness evolution:** the +80% and +11% on ARC-AGI-3 and EdgeBench come
  from a search run per task, on other models, not from a reusable evolved
  harness.
- **SWE-Bench Pro Verified** warned that answers could stay recoverable from
  `.git/objects`; it didn't observe that exploit. Its 166 of 186 is for one
  model, labelled by a model, and covers every way of reading answers.
- **Poisoning:** the paper calls its conditions sufficient, not necessary,
  and the review committee that approved the insecure directive belongs to
  another system (SICA); Hyperagents has none. The Hyperagents git-history
  leak rests on the same paper's account; there is no public issue, and
  whether it affected the published Polyglot numbers is unknown.

Added beyond the review:

- **The grader computes the integrity flags itself** (P02): in a source
  campaign the gate's own flags come from the child's code, which could
  weaken them.
- **Cost is counted outside the candidate** (P08): the runner counts its
  own tokens.
- **Evaluated commands have no network** (P01): turning off the web tools,
  as the review proposed, would still leave `curl`.
- **Stage 1 needs no model to write anything** (5.2 F): a race over
  settings can't be poisoned.
- **"Cheaper, and not worse" is stage 1's goal** (D11): cost differences
  show with far fewer runs than success differences.

---

## 12. Notes from implementation

One branch carries the tasks in the order of section 7, a commit each.

| Task | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P01  | Done. The sandbox's policy has two more parts (`overlay/confine.rs`): folders shown inside hidden ones, read-only (`Confine::show`: the shared virtualenv in the hidden cache), and `offline` (bubblewrap's `--unshare-net`; Landlock's TCP rules from version 4 and Seatbelt's IP rule, which cut this machine's loopback too: a proxy there would be the internet). A process adds them to every command's policy (`confine::restrict`, `confine::restricted` for one run); a sandbox that can't enforce them refuses the run (`unenforceable`) rather than run it unconfined. The runner takes `--hide`, `--show`, `--writable`, `--offline` and `--no-web`, which turns off a new permission, `web_tools` (not offered, and refused if called). Workspaces moved to `ROVIK_EVAL_WORK` (`/var/tmp/rovik-eval-<user>`), refused inside the system's temporary folders: Landlock can't hide a folder inside a writable one without breaking what commands make beside it. Canaries are made from the task (`canaryOf`), so nothing records them: a last comment in the hidden tests the grader puts back, a field of `expected/<task>.json`, a git note on the reference in our clones (not this repository). The report now holds the whole transcript, subagents' included; the store read it used stopped at 500 messages (`get_all_messages`). The oracle writes its patch into the workspace: it can't read the cache now. The cheater's dry run passes under bubblewrap and Landlock; Seatbelt's profile is tested as text only. Not hidden: what the system itself holds (a released more-itertools in the system's Python, a lo version in Go's module cache), which some machines have. |
| P02  | Done. The grader's view is `repair::grading_flags`: the gate's `integrity_flags`, plus a new file that decides what the checks run (the gate takes a new config for a project being set up, so it keeps its rule) and what takes over how tests run or report: a Go `TestMain`, a Rust macro named like an assertion, a `sitecustomize.py`. A binary of its own, `rovik-integrity`, runs it on the run's change as git staged it (a link or a pipe the run left isn't followed), built from the campaign's commit whatever `--runner` is evaluated. A flag on a file the reference's change doesn't touch makes `tampered` non-empty, and the run fails; `grade` stays the tests' own verdict. The task said a `conftest.py` that skips the hidden tests: a skipped test already fails (it counts as missing), so the dry run's cheater reports each failure as a pass instead, which passed all 585 tests before this. The summary counts tampered runs.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| P03  | Done. Each `result.json` has an `identity`, and each campaign an `identity.json`: the runner's commit (HEAD for this repository's own build, `+dirty` with uncommitted changes under `src-tauri`, `--runner-commit` for another) and the SHA-256 of the runner, the grader's integrity check (P02), the settings file and the policy file. A resume with any of those four changed refuses and names it (`identity.mjs`); a commit that changed while the binary didn't isn't another build. A folder with runs but no record refuses too. The files are stamped (size and time) at the campaign's start and checked before each run, so a rebuild while it runs stops it, its runs kept. There's no Evolve profile yet: its hash joins the identity with V01. Checked by hand: a pilot with a copy of the runner, the copy changed, the resume refused with the message.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| P04  | Done, in `stats.mjs`: each task's difference in success between the two campaigns, their mean, its standard error plain and clustered by module (with the G/(G − 1) correction for few clusters), a 95% percentile interval from a bootstrap that draws whole modules (4,000 draws, seeded, so a comparison gives the same interval twice), and the tasks won, lost and tied. `compareCampaigns` returns them as `paired`, and its Markdown has two rows for them; the verdict is G03's still. A task's module is its file's `module` (mined tasks will have one, M01), else its repository and the folder of its first hidden test, which makes nine modules of the 30 tasks. Tests on made-up results check the plain and clustered errors against values worked by hand.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| P05  | Done. `--jobs N` runs a campaign's runs, and both dry runs', N at once (`inParallel` in `lib.mjs`): started in the plan's order, the budget counting the runs under way at the estimate, an error or a stop letting the runs under way finish. Each worker has a cargo target folder (`cargo-target/job-<n>/`, the first keeping the folder runs always used). Each run's line is printed whole when it ends, and the files the scripted models read are written whole (by a rename). The agent log's folder was already a run's own (`--data`), not global as 3.2 said; the runner keeps its lock on runs within one process, as the sandbox's restriction (P01) is the process's, and runs side by side are processes of their own. Its new `--run-id` names the run in its report (`campaign/task/repetition`). Checked: the dry run three at once (35 seconds) grades as one after the other, and two of the Rust tasks side by side, each in its own target folder, grade as expected.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| P06  | The tooling is done; the campaigns wait for a provider's key and budget, which this environment hasn't. The same-money arm `attempts-2` (`arms.mjs`) runs the baseline's settings twice at a task and keeps the attempt its last verification ranks best (passed, none ran, flagged, failed; the first of equals), never by the hidden tests, with what both attempts cost; Autopilot Balanced and Thorough are the existing variants. Its budget: twice the baseline's cost a run, $150 for 30 × 3. To run, with a settings file for a real model and its key in the environment: `pilot`, then `run`, then `variants --variants attempts-2,autopilot-balanced,autopilot-thorough`, then `compare baseline <arm>` for each; their caps add up to $1,205, the pilot's $5 and the baseline's $75 first. The dry run checks the arm's pipeline; `arms.test.mjs` checks the pick.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| P07  | The computation is done; the MDE of the 30 tasks waits for P06's baseline. `stats.mjs`: each task's noise (p(1 − p) with the n/(n − 1) correction), the design effect of the modules (the campaign against itself, repetition against repetition, clustered over plain, pooled, at least 1), the MDE (two-sided α 0.05, power 80%; it gives this plan's table back: 23, 10, 7 and 5 points) and the tasks a target needs. `summary.md` and each comparison show it; `eval.mjs power DIR` prints it at 3 and 6 repetitions and the tasks 5 points takes. At p(1 − p) ≈ 0.1, our 30 tasks × 3 show about 13 points, so E's 5-point bar needs about 105 tasks × 6 or 210 × 3 (M01, M02). E's refusal comes with E01.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
