# MCP servers, part two: full support

The first plan (mcp-plan.md) gave the agent the tools of the user's MCP
servers on this computer. This one makes Rovik a full MCP client: any
server that works in another client works here, old servers included,
wherever the chat runs. It takes up what mcp-plan.md's Q10 left out.

Choices made with the user:

- **Old servers work too.** Features the newest spec (2026-07-28)
  deprecates (roots, sampling, logging, the HTTP+SSE transport, dynamic
  client registration) are still supported, beside their replacements.
- **On the host, autonomously.** A chat on a cloud host or in WSL runs its
  MCP servers there, so it keeps working while the user's computer is off.
  Every host gets all the enabled servers as they're set up; there is no
  per-host choice.
- **Project servers, approved.** `.mcp.json` (Claude Code),
  `.cursor/mcp.json` (Cursor) and `.vscode/mcp.json` (VS Code) are read,
  duplicates merged by URL and API key, and the user approves them once
  per project, again when they change.
- **OAuth in Rovik, one sign-in for each place.** Rovik signs in itself
  (no bridge needed), separately for this computer and for each host, so
  each refreshes its own tokens.
- **The agent uses tools only.** Resources and prompts are the user's:
  attached and picked in the composer.
- **Parallel MCP calls are not in this plan.** They still run one at a
  time (mcp-plan.md Q05).

Decision ids continue the first plan's: `Q11`…; comments in the code cite
them with this file's name. Where this plan changes a decision of the
first, it says so, and mcp-plan.md is updated to point here.

## Phases at a glance

| Phase | What                                               | Proof                                                                  |
| ----- | -------------------------------------------------- | ---------------------------------------------------------------------- |
| 1     | Servers on cloud hosts and in WSL (Q11)            | Client tests: a host gets the servers; daemon tests with a fake server |
| 2     | Project servers (Q12)                              | Parsing and dedup tests; approval component tests; run e2e tests       |
| 3     | The HTTP+SSE transport (Q13)                       | Tests against a local SSE server                                       |
| 4     | OAuth sign-in (Q14)                                | Tests against a local authorization server                             |
| 5     | The server's requests, both eras; roots (Q15, Q16) | Fake legacy and modern servers asking for roots                        |
| 6     | Elicitation (Q17)                                  | Run e2e tests; the chat's form component tests                         |
| 7     | Sampling (Q18)                                     | Run e2e tests with a fake model                                        |
| 8     | Tasks, progress and logs (Q19, Q20)                | Fake servers of both task designs                                      |
| 9     | Resources and prompts in the composer (Q21, Q22)   | Component and lib tests                                                |
| 10    | README, cloud-hosts.md, mcp-plan.md                |                                                                        |

Each phase is a commit of its own (or a few, one topic each), its tests
passing. Phases 3-9 work the same on a host as here once phase 1 is in.

---

## Q11. Cloud hosts and WSL

Replaces mcp-plan.md Q01's "This computer only".

- **Every host gets the servers.** `AgentConfig::for_host` stops clearing
  `mcp_servers` (rovik-client `remote.rs`); a host's runs take the same
  enabled servers, with the host's approval policy over the MCP rule as
  over the others (`apply_policy`).
- **The daemon's core offers `mcp`** as this computer's does: `McpState`,
  the catalog, the prompt's list, approvals.
- **Secrets go to the host**, as the providers' keys do (D11 of the hosts'
  design: keys travel with a start and the daemon keeps them in
  `~/.rovik`, in its key store). The values a start's servers need go with
  the start; the daemon's `McpState` reads them from its store
  (`set_secrets`). Settings → Cloud hosts' "remove the keys" removes them
  too, and its warning names MCP servers' secrets beside the API keys.
- **A server per chat on a host.** On a host each chat works in its own
  worktree, which is the run's project root: a command's server starts
  there (`folder_for`), so a filesystem or git server sees that chat's
  copy. On this computer nothing changes: one per project folder.
- **Idle servers stop on a host**: one unused for 30 minutes, and one whose
  chat's worktree is deleted. A host has many chats and memory limits; on
  this computer servers keep running while the app is open, as now.
- **What can't run is unavailable, with why.** A command missing on the
  host (`npx`, `uvx`, a Windows-only `npx.cmd` in WSL) fails to start, and
  the run's list says so, as for any server that can't start (Q04). Rovik
  installs and translates nothing.
- **A URL on this computer is reached through this computer.** A URL
  server at `localhost`, `127.0.0.1` or `[::1]` is something running on the
  user's computer (a desktop app's endpoint, a server started on Windows),
  which a host can't reach: on a cloud host `localhost` is the server, and
  in WSL2's default NAT networking Windows' loopback isn't WSL's. So a
  host's runs send its messages to the app, which posts them here, over
  the connection the app already has to the host (the reverse of the
  pages' port forwarding). What it serves lives on this computer, so it's
  only there while the computer and the app are: when they aren't, the
  server is unavailable on the host, saying "runs on <computer>, which is
  away". In WSL with mirrored networking (`networkingMode=mirrored`),
  loopback is shared and the daemon posts to it directly. Command servers
  are unaffected: they run on the host.
- **Outside the sandbox there too.** A server runs as the user on the host,
  unsandboxed, as here: it can read what the user can read on that server
  (their `~/.rovik` included), nothing of other users'. cloud-hosts.md's
  "What can read what" says so.
- **In the daemon's network** (our call): a command's server is started
  by the daemon, and a URL server posted to by it, outside the user's
  private network for commands, so the allowed hosts don't hold it,
  as mcp-plan.md Q03 says. It can't reach a chat's dev servers on that
  private network's `localhost`.
- **Test where it runs.** **Test** on Settings → MCP servers takes a place:
  this computer, WSL, or a cloud host. On a host, `mcp_test` runs on its
  daemon with the secrets as typed or saved.

## Q12. Project servers

**Where they come from.** At a run's start, the chat's project root (on a
host, its worktree) is read for `.mcp.json` (`mcpServers`),
`.cursor/mcp.json` (`mcpServers`) and `.vscode/mcp.json` (`servers`, with
its `inputs`), with the import code Settings already has for pasted JSON.
An entry turned off in its file (`disabled`) is left out.

**Variables.** The files rarely hold secrets; they name them:
`${VAR}` and `${VAR:-default}` (Claude Code), `${env:VAR}` (Cursor, VS
Code), `${input:id}` (VS Code's `inputs`), `${workspaceFolder}` (the
project root where the server runs) and `${userHome}`. Every variable but
the folder ones is given a value by the user in the approval, prefilled
from this computer's environment when it's set there. The values are
secrets: kept in the keychain (`SecretName::McpValue`, account
`mcp/project/<project id>/<variable>`) and sent to hosts as Q11's are.

**Duplicates.** Before asking, entries are merged:

- two URL servers with the same URL and the same resolved auth values (the
  headers that hold a key or token, Q01's rule for what looks secret) are
  one;
- two command servers with the same command, arguments and resolved env
  are one;
- an entry that matches one of the user's own servers the same way is
  dropped: it already runs.

Of duplicates, the first in the order `.mcp.json`, `.cursor`, `.vscode`
keeps its name. A project server whose name is one of the user's servers'
is left out, the approval saying why: the user's own wins.

**The approval.** A run whose project has servers not yet approved starts
without them; the chat shows a card listing each, what it runs or which
URL it posts to, its file, and a field for each variable, with **Allow**
and **Not now**. The run's list tells the agent that project servers await
the user's approval, so it can say so. Allowed servers join the next run's
list beside the user's.

- An approval is kept per project in the app's settings (not in the
  repository), as each server's definition before its variables are
  filled, hashed. A server whose definition changes (an edit, a pull, the
  agent editing the file) is new: it waits for approval again, while the
  unchanged ones keep running.
- It travels to hosts with the start, as the project's settings do
  (`request.project_settings`): a host's runs use the approved servers,
  with no one to ask.
- Settings → MCP servers lists each project's approved servers, with
  **Revoke**, which also forgets their variables' values.
- Project servers take the MCP approval rule (Q07) as the user's do, are
  marked as the project's in the list and the chat, and have the same
  lifecycle (Q04, Q11).
- Claude Code in Agent mode would start the `.mcp.json` servers itself: a
  `-p` run loads project-scoped servers without asking
  (code.claude.com/docs/en/mcp). Each of its runs gets their names in
  `disabledMcpjsonServers` in its `--settings` (a rejection wins over any
  approval Claude Code has), not `--strict-mcp-config`, which would drop
  the user's own Claude Code servers too; those allowed reach it through
  Rovik's `mcp`.

## Q13. The HTTP+SSE transport

The 2024-11-05 transport is a transport, beside stdio and Streamable HTTP:
a GET opens an event stream, its `endpoint` event names the URL messages
are POSTed to, and answers come on the stream.

- A pasted or imported entry with `"type": "sse"` becomes a URL server with
  the transport `sse` (Q01's import stops refusing it).
- A URL server set to Streamable HTTP whose first POST is answered 400, 404
  or 405 is tried as SSE, as the spec's backwards compatibility says, and
  remembered so for the session. A 400 whose body is a JSON-RPC error is
  the server's answer, not a sign of SSE: it goes to mcp-plan.md Q03's
  `server/discover` fallback.
- Proxies (`net::client()`), headers, secrets, OAuth (Q14) apply as to
  Streamable HTTP.

## Q14. OAuth sign-in

A URL server that answers 401 needs a sign-in. Rovik does it itself, as the
spec's Authorization describes; `npx mcp-remote` still works for anyone who
set it up.

**Discovery.** The protected resource metadata (RFC 9728: the
`WWW-Authenticate` header's `resource_metadata`, else the well-known
URLs), then the authorization server's (RFC 8414, or OpenID Connect
discovery).

**Registration**, in the spec's order:

1. a client ID the user entered on the server's form (with a secret, kept
   as one);
2. Client ID Metadata Documents: Rovik's client ID is
   `https://rovik-ide.dev/oauth/client.json`, published with the site
   (`website/public/`, GitHub Pages), with its name, logo and loopback
   redirect URIs. It must never move: servers cache it;
3. Dynamic Client Registration (deprecated, kept for older servers), with
   `application_type: "native"`.

A registration is kept by issuer and never reused with another
authorization server.

**Sign-in.** Authorization code with PKCE (S256) and the `resource`
parameter (RFC 8707), in the user's browser, back to a loopback redirect
(`http://127.0.0.1:<port>/callback`, a free port). A present `iss` in the
answer must match the issuer (RFC 9207). Scopes are those the 401 or the
metadata name; a 403 `insufficient_scope` later asks for a sign-in with the
wider scopes.

**Tokens.** Kept in the keychain by server and issuer, never in a log or a
command line. The access token goes on each request as `Authorization:
Bearer`; it's refreshed before it expires, and on a 401.

**One sign-in for each place.** Refresh tokens rotate: two places sharing
one would sign each other out. So each place has its own grant:

- **Sign in** on the server's row signs in for this computer, then for
  each cloud host and WSL, one browser tab after another, each flow run
  from this computer with its redirect here; the host's tokens are sent to
  its daemon, kept in its key store, and refreshed there from then on.
- The row says where it's signed in ("This computer, WSL, build-server")
  and offers **Sign in** for a place that isn't, and **Sign out**, which
  forgets the tokens of every place.
- A place without tokens, or whose refresh is refused, has the server
  unavailable there, saying "sign in again from Settings → MCP servers".
- A project server (Q12) signs in the same way, from its approval card or
  the page.

## Q15. The server's requests, both eras

Replaces mcp-plan.md Q03's "method not found" and its refusal of
`input_required`. Rovik now declares what it supports:

- legacy, in `initialize`: `roots` (`listChanged`), `sampling` (with
  `tools`), `elicitation` (`form` and `url`), `tasks`;
- modern, in each request's `_meta` capabilities: the same, and the tasks
  extension (`io.modelcontextprotocol/tasks`).

What a server asks for is answered by one handler per kind (roots, Q16;
elicitation, Q17; sampling, Q18), whichever era it came in:

- **legacy**: a request from the server (`roots/list`,
  `elicitation/create`, `sampling/createMessage`, `ping`), on stdio's
  stdout or a Streamable HTTP response stream, answered with a message
  back (a POST on HTTP);
- **modern**: a result with `resultType: "input_required"`, whose
  `inputRequests` are answered by the same handlers, then the request sent
  again with `inputResponses` and the server's `requestState`, until a
  result is `complete`. A result without `resultType` is complete.

A request that comes outside a call (a legacy server asking in between)
is answered as if for the server's last call, or refused when there's
none to ask.

## Q16. Roots

The roots are the folder the server runs for: the project's on this
computer, the chat's worktree on a host, the user's home without a
project (`folder_for`). A legacy server is told when they change
(`notifications/roots/list_changed`), which happens only when a server is
started again for another folder.

## Q17. Elicitation

A server asks the user something during a call.

- **Form**: the requested schema (flat, of strings, numbers, booleans and
  enums, as the spec allows) is shown in the chat as a card on the call's
  line, like an approval, with the server's message and **Submit**,
  **Decline** and **Cancel**, answered as `accept`, `decline`, `cancel`. A
  field the schema marks as a password is kept out of the chat's record.
- **URL**: the card shows the server's message and the URL's domain, and
  **Open** opens it in the user's browser; the answer is `accept` once
  opened, and the call goes on when the server is done (legacy: its
  completion notification; modern: the retry, Q15).
- **Waiting.** The call waits for the answer, as an approval does,
  whatever the place: a chat on a host waits until the user answers from
  any computer. Stop cancels it.

## Q18. Sampling

A server asks Rovik's model for a message.

- It's answered by the **sampling model**, a setting of Settings → MCP
  servers (a model profile, stored in `AgentConfig` with a serde default),
  the same for every chat: a chat's model may be a connector (Claude Code,
  Codex) that Rovik can't send a sampling request to. Unset, it's the
  chat's model when that's an API model (our call); otherwise the request
  is refused, saying to choose a sampling model. A host gets the setting
  and its key as it gets the AI reviewer's. The request's model
  preferences are ignored. Its messages, system prompt, `maxTokens` and stop sequences are
  sent as asked; images too for models that take them.
- `includeContext` other than `none` (deprecated) is treated as `none`.
- Tools in sampling (2025-11-25): the request's tools are given to the
  model, and a reply that calls one is returned as such, for the server to
  run and ask again.
- It goes under the MCP approval rule (Q07), with one approval before the
  model is called (our call): it shows which server asks and the request,
  so a declined request costs nothing. The reply is shown on the call's
  line as it's sent back. The AI reviewer, where it answers, judges the
  request as it would a tool call.
- Its tokens count in the chat's usage, shown on the call's line.

## Q19. Tasks

A long call a server runs as a task.

- **Legacy (2025-11-25)**: a tool that requires a task
  (`execution.taskSupport: "required"`) is called with a `task`; one that
  only allows it is called as usual.
- **Modern (the tasks extension)**: a server may answer any call with a
  task handle.
- Rovik polls it (`tasks/get`, at the server's `pollInterval`) until it
  ends, and takes its result (`tasks/result` for legacy). Input a task
  asks for goes through Q15's handlers (`tasks/update` for modern). Stop
  cancels it (`tasks/cancel`).
- The agent sees one call; the chat's line shows the task's status
  message. A task isn't held to Q03's 10 minutes; it ends with its TTL,
  Stop, or its failure.

## Q20. Progress, logs and changes

- Each call carries a `progressToken`; `notifications/progress` show on the
  call's line in the chat (its message, and the share done when the server
  gives a total).
- Logs: the level is `info` (legacy `logging/setLevel`; modern `_meta`
  `io.modelcontextprotocol/logLevel`), and `notifications/message` go into
  the server's last lines, where its stderr's are (Q03): shown with its
  errors and by **Test**.
- List changes stay as Q04 has them: each run lists the tools again, so
  `list_changed` notifications and `subscriptions/listen` aren't needed.

## Q21. Resources in the composer

The agent doesn't read resources; the user attaches them.

- The composer's attach menu has **MCP resource**: a picker of the enabled
  servers' resources (`resources/list`, by server) and templates
  (`resources/templates/list`), whose arguments are filled in a small form,
  with suggestions from `completion/complete` where the server offers
  them.
- The resource is read (`resources/read`) where the chat runs (its host's
  daemon for a host's chat) when it's attached; text becomes an attachment
  of text with its URI, an image an image attachment, other binary content
  a note.
- A resource link in a tool's result stays as Q05 has it: its name and URI.

## Q22. Prompts in the composer

- The composer's `/` picker (connectorCommands.ts) lists the enabled
  servers' prompts as `/<server>:<prompt>`, with their descriptions,
  beside the connector's commands.
- Picking one asks for its arguments (required ones first, with
  `completion/complete` suggestions), then `prompts/get` fills the
  composer with its messages' text (an embedded resource as its text, an
  image as an attachment), for the user to read, change and send. The
  agent never calls prompts.

## Caveats the README states

The first plan's, and:

- A chat on a cloud host or in WSL runs the servers there: their commands
  must be installed there, and their secrets and tokens are kept there,
  where root can read them.
- A project's servers run only once the user allows them, and again after
  any change to them.

## Left to the builder

- How the start carries MCP secrets and project approvals to a daemon,
  within the hosts' existing key transfer.
- The idle timeout's exact mechanics on a host (Q11's 30 minutes is the
  intent).
- The cards' and pickers' exact look, in the chat's and composer's
  existing style.
