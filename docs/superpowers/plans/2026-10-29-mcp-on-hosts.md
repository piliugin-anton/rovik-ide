# MCP servers on cloud hosts and in WSL (phase 1a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A chat on a cloud host or in WSL gets the user's enabled MCP servers, run by its daemon, with their secrets kept there, so it works while this computer is away.

**Architecture:** The app stops stripping `mcp_servers` from what it sends a host, and sends the servers' saved secret values with each start, as it sends providers' keys (D11). The daemon keeps them in `keys.json`, gives its core's `McpState` a secret source that reads them, stops idle servers, and answers `McpTest`. The core's catalog stops refusing runs on a server. Loopback URL servers (Q11's relay through this computer) are phase 1b, a plan of its own; here they behave like any server the host can't reach.

**Tech Stack:** Rust (rovik-protocol, rovik-core, rovik-agentd, rovik-client, the Tauri app), SolidJS + vitest.

**Spec:** `docs/design/mcp-plan-2.md` (Q11), on top of `docs/design/mcp-plan.md` (Q01-Q04).

## Global Constraints

- New protocol fields get serde defaults; stored settings and older daemons/apps keep working (AGENTS.md).
- A daemon that doesn't announce the `mcp` feature in its `Welcome` is sent no MCP secrets and is not offered for Test.
- Secrets: never in a log, argv, `Debug` output or a settings file; on the wire as `rovik_protocol::Secret`; on the daemon only in `keys.json` (written with `write_atomic_private`).
- Idle stop on a daemon: a server unused for **30 minutes**; checked every **5 minutes**. On this computer servers never stop for idleness.
- Test values: `#[cfg(any(test, feature = "testing"))]` for the idle durations, as `chat_request::BACKOFF_MS` does; no test waits them out.
- Rust formatting: `rustfmt --edition 2021 <changed files>`; `cargo clippy -p <crate> --tests` adds no warnings; `npx prettier --write` for TS/MD.
- Copy (exact): the warning on Cloud hosts and in Move chat becomes "Your API keys and MCP servers' secrets are stored in your account on this server, so chats there run while this computer is away. The agent's commands there and anyone with root on it can read them."

## Review Focus

- A secret that can't be read from the keychain at start (locked keychain): the start still goes, without that value; the host reports the server unavailable with "isn't saved" — the start must not fail. (Task 4 test.)
- A server's secret changed on this computer after the host already runs it: the next start replaces it and the host's running server restarts with the new value. (Task 3 test.)
- "Remove keys from this server" with no provider: MCP secrets go too; with a provider named, they stay. (Task 3 test.)
- A call in flight when the idle sweep runs: a server in use is never stopped (its use refreshes `last_used` before and after the call). (Task 2 test.)
- An old daemon (no `mcp` feature): nothing secret is sent to it, and the Test place list leaves it out. (Task 4 and Task 5 tests.)

---

### Task 1: Protocol: the `mcp` feature, start secrets, `McpTest`

**Files:**

- Modify: `src-tauri/crates/rovik-protocol/src/lib.rs` (features), `src/agent.rs` (`StartRequest`, remove `AgentConfig::for_host`), `src/mcp.rs` (move `Tested`, `TestedTool` here), `src/request.rs` (`Request::McpTest`, `Response::McpTested`)
- Modify: `src-tauri/crates/rovik-core/src/mcp/mod.rs` (re-export the moved types: `pub use rovik_protocol::mcp::{Tested, TestedTool};`)
- Test: `src-tauri/crates/rovik-protocol/src/tests.rs`

**Interfaces:**

- Produces:
  - `features::MCP: &str = "mcp"` — "MCP servers run on the daemon (mcp-plan-2.md Q11): `McpTest`, and starts' `mcp_secrets`."
  - `StartRequest.mcp_secrets: BTreeMap<String, BTreeMap<String, Secret>>` — by server id, then by key (`env/NAME`, `header/Name`); `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`.
  - `Request::McpTest { server: McpServer, values: BTreeMap<String, Secret> }` — in the Host group (`features::MCP` gates it), not a chat request.
  - `Response::McpTested(Tested)`.
  - `Tested`, `TestedTool` now `Serialize + Deserialize` in `rovik_protocol::mcp`.
  - `AgentConfig::for_host` is gone (it only cleared `mcp_servers`).

- [ ] **Step 1: Write the failing tests** in `rovik-protocol/src/tests.rs`; delete the `for_host` assertions at lines ~368-376.

```rust
#[test]
fn a_start_from_an_older_app_has_no_mcp_secrets() {
    let json = serde_json::to_value(start_request_fixture()).unwrap(); // the existing StartRequest fixture
    assert!(json.get("mcpSecrets").is_none(), "empty: left out");
    let back: StartRequest = serde_json::from_value(json).unwrap();
    assert!(back.mcp_secrets.is_empty());
}

#[test]
fn a_start_s_mcp_secrets_go_and_come_back_but_never_show_in_debug() {
    let mut start = start_request_fixture();
    start.mcp_secrets.insert("s1".into(), [("env/TOKEN".into(), Secret("t0p".into()))].into());
    let back: StartRequest = serde_json::from_str(&serde_json::to_string(&start).unwrap()).unwrap();
    assert_eq!(back.mcp_secrets["s1"]["env/TOKEN"].0, "t0p");
    assert!(!format!("{start:?}").contains("t0p"));
}

#[test]
fn mcp_test_and_its_answer_round_trip() { /* Request::McpTest and Response::McpTested(Tested{..}) through serde_json, equal after */ }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd src-tauri && cargo nextest run -p rovik-protocol`
Expected: compile errors: no field `mcp_secrets`, no variant `McpTest`.

- [ ] **Step 3: Implement** the Interfaces above. `Request::McpTest` joins the Host group's `target()`/feature mapping in `request.rs` the way `KeysList` does (lines ~424, ~474). Fix every compile error from removing `for_host` by deleting the call (rovik-client `remote.rs:582` and `:786`); the client's behaviour change is tested in Task 4.

- [ ] **Step 4: Run tests**

Run: `cd src-tauri && cargo nextest run -p rovik-protocol -p rovik-core`
Expected: PASS (rovik-client's `a_host_is_sent_no_mcp_servers` now fails; Task 4 replaces it).

- [ ] **Step 5: Commit** `git commit -m "Protocol: MCP secrets with a start, McpTest, the mcp feature"`

---

### Task 2: Core: MCP on a daemon, its secrets out, idle stop

**Files:**

- Modify: `src-tauri/crates/rovik-core/src/mcp/mod.rs`, `src-tauri/crates/rovik-core/src/agent_run/mcp_tool.rs:61-80`
- Test: `src-tauri/crates/rovik-core/src/mcp/tests.rs`, `src-tauri/crates/rovik-core/src/agent_run/mcp_e2e_tests.rs`

**Interfaces:**

- Consumes: Task 1's types.
- Produces (on `McpState`):
  - `pub async fn saved_secrets(&self, servers: &[McpServer]) -> BTreeMap<String, BTreeMap<String, Secret>>` — the enabled servers' secret values the source has, read on a blocking thread; a missing or unreadable one is left out (logged by key name only, never value).
  - `pub fn stop_idle(&self, unused_for: Duration)` — stops instances whose `last_used` is older; none in use (a list or call running) is stopped.
  - `pub fn stop_in(&self, folder: &Path)` — stops the instances started in `folder` (a chat's worktree that went).
  - `Instance` gains `last_used: Mutex<Instant>` and an in-use count, set by `list_one` and `call` before and after.
- `Catalog::list` no longer returns `None` on a server (`core.agents.on_server()`); its doc comment drops "none on a cloud host's or WSL's daemon".

- [ ] **Step 1: Write the failing tests**

In `mcp/tests.rs`:

```rust
#[tokio::test]
async fn saved_secrets_are_the_enabled_servers_values_the_keychain_has() {
    // two servers: s1 enabled with secret header Authorization (saved "Bearer x") and secret header X-Missing (not saved);
    // s2 disabled with a saved secret.
    let secrets = state.saved_secrets(&[s1, s2]).await;
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets["s1"]["header/Authorization"].0, "Bearer x");
    assert!(!secrets["s1"].contains_key("header/X-Missing"));
}

#[tokio::test]
async fn an_unreadable_secret_is_left_out_not_an_error() { /* source returns Err("locked") for one key: the other key is still there */ }

#[cfg(unix)]
#[tokio::test]
async fn an_idle_server_is_stopped_and_one_in_use_is_not() {
    // `fake(dir, "slow")` command server: list it, then start a call that waits;
    // state.stop_idle(Duration::ZERO) while the call runs → the call still completes;
    // after it, stop_idle(Duration::ZERO) → the fake's log shows it exited, and the next list starts it again.
}

#[cfg(unix)]
#[tokio::test]
async fn a_worktree_s_servers_stop_when_it_goes() {
    // list the same server for folders a/ and b/; stop_in(a) → a's process exited, b's still answers.
}
```

In `agent_run/mcp_e2e_tests.rs`:

```rust
/// Q11 (mcp-plan-2.md): a daemon's runs get the `mcp` tool too.
#[tokio::test]
async fn a_run_on_a_daemon_calls_mcp_tools() {
    // as an_mcp_tool_is_called_once_the_user_approves_it, after h.core.agents.set_on_server():
    // the model's first request's tools include "mcp", and the approved call reaches docs_server.
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd src-tauri && cargo nextest run -p rovik-core mcp`
Expected: FAIL: no method `saved_secrets`/`stop_idle`/`stop_in`; the e2e test finds no `mcp` tool.

- [ ] **Step 3: Implement** the Interfaces. `stop_idle` and `stop_in` remove the instances from `servers` under the lock and hand them to the existing `stop_all`. The in-use guard is a counter incremented around `session().await` + the call in `list_one`/`call` (a drop guard, so an error path still decrements).

- [ ] **Step 4: Run tests**

Run: `cd src-tauri && cargo nextest run -p rovik-core && cargo test -p rovik-core mcp`
Expected: PASS under both.

- [ ] **Step 5: Commit** `git commit -m "Core: MCP tools on a daemon's runs; saved secrets out; idle and worktree stops"`

---

### Task 3: Daemon: keep the secrets, run the servers, Test

**Files:**

- Modify: `src-tauri/crates/rovik-agentd/src/keys.rs` (an `mcp` map), `store.rs` (`started`, `remove_keys`, a lookup), `serve.rs` (secret source, sweep), `daemon.rs` (`McpTest`, forget changed servers, stop a gone worktree's), `moves.rs:114` and `work.rs:489` (stop on `worktree_gone`), `session.rs:31-36` (announce `features::MCP`)
- Test: `src-tauri/crates/rovik-agentd/src/store/tests.rs`, the daemon tests beside `daemon.rs` (or `store/tests.rs` where the daemon tests live)

**Interfaces:**

- Consumes: Task 1's `StartRequest.mcp_secrets`, `Request::McpTest`; Task 2's `McpState::{stop_idle, stop_in, forget, test}`.
- Produces:
  - `Keys.mcp: BTreeMap<String, BTreeMap<String, String>>`, `#[serde(default)]`.
  - `Keys::take_mcp(&mut self, secrets: &BTreeMap<String, BTreeMap<String, Secret>>) -> Vec<String>` — replaces each given server's values (a server given replaces all its keys); returns the ids whose values changed.
  - `Keys::mcp_value(&self, server: &str, key: &str) -> Option<String>`.
  - `Keys::remove(None)` also clears `mcp`; `remove(Some(provider))` leaves it. Its count counts each MCP server as one.
  - `Store::mcp_value(&self, server: &str, key: &str) -> Option<String>` (reads under the store lock).
  - `Store::started(&self, computer, start) -> Result<Vec<String>, Fail>`: the ids whose MCP secrets changed; `Daemon::start` calls `core.mcp.forget(id)` for each.
  - `const IDLE_AFTER: Duration` (30 min; tests: 50 ms) and `const SWEEP_EVERY: Duration` (5 min; tests: 10 ms) in `serve.rs`.

- [ ] **Step 1: Write the failing tests** in `store/tests.rs`:

```rust
#[test]
fn a_start_s_mcp_secrets_go_to_keys_json_alone() {
    // start with mcp_secrets {"s1": {"env/TOKEN": "t0p"}}:
    // keys.json has it; state.json and every other file (`others(dir)`) don't contain "t0p";
    // store.mcp_value("s1", "env/TOKEN") == Some("t0p").
}

#[test]
fn a_changed_mcp_secret_is_reported_and_an_unchanged_one_is_not() {
    // first start: ["s1"]; same again: []; new value: ["s1"].
}

#[test]
fn removing_every_key_removes_mcp_secrets_and_removing_a_provider_s_does_not() {}

#[test]
fn a_store_from_before_mcp_reads_with_no_mcp_secrets() {
    // keys.json written as `{"providers":{…}}` reads, mcp empty.
}
```

And a daemon test (serve-level, with a fake MCP server as in `mcp/tests.rs`' `rovik_tools` / `RovikServer`):

```rust
#[tokio::test]
async fn mcp_test_on_the_daemon_lists_the_server_s_tools() {
    // daemon.answer(computer, Request::McpTest{server, values}) → Response::McpTested with the tools.
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd src-tauri && cargo nextest run -p rovik-agentd`
Expected: FAIL: no field `mcp_secrets` handling, no `mcp_value`, `McpTest` unanswered.

- [ ] **Step 3: Implement.**
  - `serve.rs`, after `Core::new`: `core.mcp.set_secrets(Arc::new(move |server, key| Ok(store.mcp_value(server, key))))`; spawn the sweep: every `SWEEP_EVERY`, `core.mcp.stop_idle(IDLE_AFTER)`.
  - `moves.rs` and `work.rs`, where `worktree_gone` is called: take the worktree path from `store.place(&chat)` first, then `core.mcp.stop_in(&path)`.
  - `daemon.rs` `serve`: `Request::McpTest { server, values }` → `core.mcp.test(&server, &given, None)` (home folder) → `Response::McpTested`.
  - `session.rs`: add `features::MCP`.

- [ ] **Step 4: Run tests**

Run: `cd src-tauri && cargo nextest run -p rovik-agentd`
Expected: PASS.

- [ ] **Step 5: Commit** `git commit -m "Daemon: keep MCP secrets in keys.json, run and test the servers, stop idle ones"`

---

### Task 4: Client: send the servers and their secrets to a host

**Files:**

- Modify: `src-tauri/crates/rovik-client/src/remote.rs` (`config_for` ~572-584, `start` ~770-800, a test entry point)
- Test: `src-tauri/crates/rovik-client/src/remote/tests.rs:775-820` (replace `a_host_is_sent_no_mcp_servers`)

**Interfaces:**

- Consumes: Task 1 (`features::MCP`, `mcp_secrets`, `McpTest`), Task 2 (`McpState::saved_secrets`).
- Produces: `Remote::mcp_test(&self, host: &str, server: McpServer, given: BTreeMap<String, Secret>) -> Result<Tested, Fail>` — the values typed over the saved ones (`saved_secrets(&[server])`), sent as `McpTest` via `host_call`; a host without `features::MCP` fails with "<host name> runs an older Rovik: update it to test MCP servers there."

- [ ] **Step 1: Write the failing tests** (replacing the old one):

```rust
/// Q11 (mcp-plan-2.md): a host gets the MCP servers in its configuration
/// and its starts, and a start carries their saved secrets.
#[tokio::test]
async fn a_host_is_sent_the_mcp_servers_and_a_start_their_secrets() {
    // fake host announcing features::MCP; core.mcp source with s1's "header/Authorization" = "Bearer x".
    // everywhere(AgentConfigure(config)) → the host's configured mcp_servers == config.mcp_servers.
    // start → starts(&fake)[0].config.mcp_servers == config.mcp_servers
    //         and .mcp_secrets["s1"]["header/Authorization"].0 == "Bearer x".
}

#[tokio::test]
async fn an_older_host_is_sent_no_mcp_secrets() {
    // fake host without features::MCP: the start's mcp_secrets is empty.
}

#[tokio::test]
async fn a_start_goes_when_a_secret_can_t_be_read() {
    // source returns Err("locked") for s1's key: start Ok, mcp_secrets has no s1 entry.
}

#[tokio::test]
async fn mcp_test_on_a_host_sends_typed_values_over_saved_ones() {
    // saved "header/Authorization"="old", given "new": the fake's McpTest request has "new".
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd src-tauri && cargo nextest run -p rovik-client remote`
Expected: FAIL: no secrets sent, no `mcp_test`.

- [ ] **Step 3: Implement.** In `start`, after the config is set for the host: when `self.serves(&host, features::MCP)` and the config has servers, `request.mcp_secrets = self.core.mcp.saved_secrets(&config.mcp_servers).await`. `config_for` loses `.for_host()` (done in Task 1); update its doc comment ("its MCP servers, which run here only" goes).

- [ ] **Step 4: Run tests**

Run: `cd src-tauri && cargo nextest run -p rovik-client`
Expected: PASS.

- [ ] **Step 5: Commit** `git commit -m "Send hosts the MCP servers, and each start their saved secrets"`

---

### Task 5: App and page: Test on a host, the warning

**Files:**

- Modify: `src-tauri/src/commands/mcp.rs` (`mcp_test` gains `host: Option<String>`), `src/lib/mcpServers.ts:395-401` (`testServer` gains `host?: string`), `src/components/McpServersSettings.tsx` (a "Test on" select beside Test), `src/components/CloudHostsSettings.tsx:981`, `src/components/MoveChatDialog.tsx:29`
- Test: `src/components/McpServersSettings.test.tsx`, `src/components/CloudHostsSettings.test.tsx` / `MoveChatDialog.test.tsx` (whichever assert the warning), `src/lib/mcpServers.test.ts`

**Interfaces:**

- Consumes: Task 4's `Remote::mcp_test`.
- Produces: `mcp_test(core, remote, server, values, root, host: Option<String>)`: `None` → as today; `Some(id)` → `remote.mcp_test(&id, server, values)`. Frontend: `testServer(server, values, root, host?)`.
- The select lists "This computer" and each host whose state is `ready` and whose daemon serves `mcp` (the hosts list the page already loads for Cloud hosts; add `serves: string[]` from the daemon's `Welcome` features to `HostRow` if it isn't there). It shows only when there is such a host. Choosing a place clears the last result, as an edit does (`forgetTest`).

- [ ] **Step 1: Write the failing tests**

```tsx
it('tests a server on the host chosen in Test on', async () => {
  // two hosts: "build" ready serving mcp, "old" ready without it.
  // options are ["This computer", "build"]; pick "build", click Test →
  // invoke('mcp_test', expect.objectContaining({ host: 'build' })).
});

it('has no Test on choice without a host that runs MCP servers', () => {});

it("warns that MCP servers' secrets are kept on the server too", () => {
  // the exact copy from Global Constraints, in Cloud hosts and in Move chat.
});
```

- [ ] **Step 2: Run them to see them fail**

Run: `npx vitest run src/components/McpServersSettings.test.tsx src/components/CloudHostsSettings.test.tsx src/components/MoveChatDialog.test.tsx`
Expected: FAIL.

- [ ] **Step 3: Implement** the Interfaces and the copy.

- [ ] **Step 4: Run tests**

Run: `npm test && npm run typecheck && npm run format:check`
Expected: PASS.

- [ ] **Step 5: Commit** `git commit -m "MCP servers page: Test on a host; the hosts' warning names MCP secrets"`

---

### Task 6: Docs

**Files:**

- Modify: `docs/cloud-hosts.md` ("What can read what": MCP servers run as the user outside the sandbox and the private network, can read what the user can there; their secrets are in `~/.rovik/keys.json`), `docs/design/mcp-plan.md` (Q01's "This computer only" and Q10's first item point to mcp-plan-2.md Q11), `README.md` (the MCP section: servers run where the chat runs; their commands must be installed there)

- [ ] **Step 1: Edit the three files** as listed; `npx prettier --write` them.
- [ ] **Step 2: Verify** `npm run format:check` passes.
- [ ] **Step 3: Commit** `git commit -m "Docs: MCP servers on cloud hosts and in WSL"`

---

## After this plan

Phase 1b (its own plan): Q11's loopback URL servers reached through this computer, and WSL's mirrored networking. Until then, a `localhost` URL server on a host fails to connect and is listed unavailable with the connection error.
