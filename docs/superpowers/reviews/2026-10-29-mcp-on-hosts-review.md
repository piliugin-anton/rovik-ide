# Review: MCP servers on cloud hosts and in WSL (phase 1a)

Commits `8d5f5417..0c3f4480`, against `docs/design/mcp-plan-2.md` (Q11) and
`docs/superpowers/plans/2026-10-29-mcp-on-hosts.md`. A code review at high
effort; no tests were run, and the findings are not yet verified by tests.

## Secrets and servers that outlive the user's change

1. **Deleted or disabled servers keep their secrets on the host.**
   `src-tauri/crates/rovik-agentd/src/keys.rs:76`. `take_mcp` only sees the
   servers a start lists, so when the user deletes server s1 (say its token
   leaked), `keys.mcp["s1"]` stays in `~/.rovik/keys.json`, where the agent's
   commands and root can read it, until the user removes the host's keys.
   This contradicts the docs, which say a value cleared on the computer is
   cleared on the host.

2. **Turning a server off doesn't stop it on a host.**
   `src-tauri/crates/rovik-agentd/src/daemon.rs:172`. The daemon's
   `AgentConfigure` never calls `core.mcp.configure`. When the user turns
   off a command server, the app stops it locally, but each host only stores
   the new config. The process there keeps running, with its token in its
   environment, for up to 30 minutes plus the 5-minute sweep interval.

3. **A failed `keys.json` write leaves a stale token running.**
   `src-tauri/crates/rovik-agentd/src/store.rs:293`. `take_mcp` changes the
   keys in memory before `keys.write`. If the write fails (disk full,
   EACCES), `started` returns an error before the changed ids reach
   `mcp.forget`. The memory copy already holds the new token, so once the
   disk recovers, the next start sees no change and the server keeps its
   old, possibly revoked token until the idle sweep. `remove_keys` (line 387) has the same gap: a failed write means `mcp.shutdown()` never runs.

4. **Test on a host skips a disabled server's saved secrets.**
   `src-tauri/crates/rovik-client/src/remote.rs:834`. Test on a host reads
   the saved secrets through `saved_secrets`, which leaves out disabled
   servers. Testing a server that is off (or not yet on) without retyping
   its token sends no values, so the daemon fails with "Authorization's
   value isn't saved" or falls back to a stale value in `keys.json`. The
   same Test on this computer passes, because `values()` reads the keychain
   whether or not the server is enabled.

5. **An empty typed value wipes the saved secret on a host Test.**
   `src-tauri/src/commands/mcp.rs:81`. The host branch of `mcp_test` passes
   typed values through without dropping empty ones; the local branch drops
   them (`filter(|(_, value)| !value.0.is_empty())`). With
   `{"header/Authorization": ""}`, `values.extend(given)` in
   `Remote::mcp_test` replaces the saved token with "", the server starts
   with an empty header and gets 401. Today only the frontend's filter
   prevents this.

## Tests that break AGENTS.md rules

6. **A credential-leak test was weakened.**
   `src-tauri/crates/rovik-client/src/remote/tests.rs:535`. The check was
   narrowed from `"secret"` to `"me:secret"` so it passes now that
   `StartRequest`'s Debug output prints an `mcp_secrets` field. A leak of
   the password alone (or as `secret@github.com`) no longer fails it. AGENTS.md:
   "Never skip, disable or weaken a test to get green." Check for the
   credential in a form that can't match the field name instead.

7. **Two daemon MCP tests pass without testing anything when `python3` is
   missing.** `src-tauri/crates/rovik-agentd/tests/mcp.rs:61`. Both return
   early instead of being `#[ignore]` and env-gated, so on a machine without
   python3 the worktree-stop and secret-restart behaviour goes untested
   while reported green.

## Smaller issues

8. **Host Test errors are mislabelled.**
   `src-tauri/crates/rovik-agentd/src/daemon.rs:124`. On a host, `McpTest`
   reports every failure (a missing command, a 401, a missing secret) as
   `FailCode::Internal`, so a missing `npx` looks like a daemon bug. The
   shared error text also says the value couldn't be read from the
   "keychain" or should be entered in "Settings → MCP servers", which don't
   exist on a host; the fix there is to start a chat from the computer or
   check its keychain.

9. **Each host message copies the whole config.**
   `src-tauri/crates/rovik-client/src/remote.rs:805`. Every start to an MCP
   host clones the whole `AgentConfig` (or rebuilds it with
   `agents.config()`) only to read `mcp_servers`, then makes one blocking
   keychain read per secret field of every enabled server. Where the
   keychain is slow or prompts, each send waits on it. Borrowing
   `request.config.as_ref().map(|c| &c.mcp_servers)` avoids the clone.

10. **Duplicated lookup and a slower details view.**
    `src-tauri/crates/rovik-client/src/remote.rs:570`. `Remote::features`
    repeats the client and daemon lookup in `serves` right above it.
    `HostsPage::details` adds a fourth request in a row (`McpSecretsKept`,
    after HostInfo, KeysList and Storage) only to fetch one count; it could
    come with KeysList or HostInfo (with a serde default), or the requests
    could run together with `join`.

## Suggested order

Fix 1-7 before phase 1b, each with a failing test first; 8-10 can follow.
