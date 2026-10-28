# Allowed hosts: plan

The user can limit the hosts the agent's work reaches over the network: an
allowlist in **Settings → Permissions & approval → Network**. With it on,
every connection the agent's work makes from the computer it runs on (this
computer, WSL, or a cloud host) goes to a listed host, to this computer's
own services (localhost), or nowhere.

Two choices were made with the user before this plan:

- **Enforced, not advisory.** The list holds for the agent's commands too
  (`curl`, `npm install`, `git clone`, a test that opens a socket), not only
  for its web tools. A command can't step around it by ignoring a proxy
  variable: its process has no other way out.
- **Exact names, wildcards on request.** `github.com` matches only
  `github.com`; `*.github.com` matches its subdomains. Allowing `github.io`
  doesn't allow every user's site under it.

Decision ids here are `J01`…; comments in the code cite them with this
file's name.

## Phases at a glance

| Phase | What                                                                    | Proof                                                   |
| ----- | ----------------------------------------------------------------------- | ------------------------------------------------------- |
| 1     | The setting, host matching, web tools checked before they run (J01-J02) | Unit tests; settings component tests                    |
| 2     | The filter: Rovik's proxy that passes only listed hosts (J03)           | Tests against local servers, an upstream proxy          |
| 3     | The browser through the filter (J09)                                    | Real Chrome: a page's other hosts are kept out          |
| 4     | Linux and WSL: the agent's network sealed but for the filter (J04-J06)  | Commands in the sealed network reach a listed host only |
| 5     | macOS: Seatbelt keeps commands and connectors to localhost (J07)        | Profile text tests here; real refusals on macOS CI      |
| 6     | Connectors, services, Windows (J08, J10, J11)                           | Argument and policy tests                               |
| 7     | README and docs/cloud-hosts.md                                          |                                                         |

Each phase is a commit of its own, its tests passing.

---

## J01. The setting and how a host matches

`Permissions` (rovik-protocol, the frontend's `AgentPermissions`) gets two
fields beside the commands' pair, with serde defaults so stored settings
and older apps keep working:

- `allow_any_host: bool`, default `true`: every host may be reached, the
  list is ignored. Today's behaviour.
- `allowed_hosts: Vec<String>`, default empty: the hosts that may be
  reached when `allow_any_host` is off.

An entry is one of:

- a name: `docs.rs` matches `docs.rs` only;
- a name's subdomains: `*.github.com` matches `api.github.com` and
  `a.b.github.com`, not `github.com` (list both for both);
- an address: `10.0.0.5`, `[fd00::1]` (an IPv4-mapped IPv6 address is its
  IPv4 address).

Entries are normalised the way a URL's host is (lower case, punycode, no
trailing dot); a scheme, a sign-in, a port and a path typed with one are
dropped (`https://docs.rs/tokio` is `docs.rs`). `*`, an empty entry, a
wildcard anywhere but a leading `*.`, and characters no host has are
refused by the settings page. The core parses the same way and skips an
entry it can't read (the page wouldn't have saved it).

Ports and paths aren't part of the list: a listed host is reached on any
port, any path.

**This computer's own services** (loopback: `localhost`, `*.localhost`,
`127.0.0.0/8`, `::1`) are never on the list and always reachable directly:
the agent's own dev servers, a local database. They're never reached
through the filter (J03).

The list is the computer's: one list for all chats and projects, as the
allowed commands are. Autopilot keeps it (`permissions_for` copies the
user's other fields).

## J02. What it covers, and what not

Every network connection the agent's work makes from the computer it runs
on:

| Origin                                                                               | Held by                                           |
| ------------------------------------------------------------------------------------ | ------------------------------------------------- |
| The agent's commands, and Rovik's on its behalf (checks, installs, background tasks) | The sandbox's network (J04-J07), the filter (J03) |
| The agent's browser (`web_search`, `browse_url`, `browser_screenshot`)               | Chrome's proxy is the filter (J09)                |
| Claude Code and Codex: their programs and what they run                              | The same network as the commands (J10)            |
| A cloud host's project services (D21)                                                | An internal container network (J11)               |

Not covered, by design:

- Rovik's own requests: the model providers, Rovik's updates, the pinned
  browser and toolchains a cloud host installs, a service's image. They
  aren't the agent's choice, and the model never shapes them.
- The connectors' hosted web search (Claude Code's `WebSearch`, Codex's
  `web_search`), which their providers run on their own servers: there's no
  host here to check, so it's turned off while the list is on (J10).
- The user's own terminal, which is never confined.

The web tools are also checked by the permission layer before they run
(`permissions::check_tool_call`): `browse_url`, `browser_screenshot` with a
`url`, and a connector's `WebFetch` ask are refused when their URL's host
isn't listed, with a reason the model can act on ("docs.rs isn't in the
user's allowed hosts: github.com, *.npmjs.org. Use one of them, or ask the
user."). `web_search` goes to `html.duckduckgo.com`: it's refused, and left
out of the tools the model is offered, unless that host is listed.
`browse_url`'s description names the listed hosts, as `bash`'s names the
allowed commands. These checks give clear errors early; the filter is what
holds.

## J03. The filter

A small HTTP proxy in the core (`allowed_hosts/filter.rs`), started when
the list is turned on, in the process that runs the agent's work (the app,
a cloud host's daemon, the WSL daemon).

- **What it speaks**: `CONNECT host:port` (HTTPS, and any TCP a client
  tunnels), and plain HTTP in absolute form (`GET http://host/path`). That's
  what the standard variables (`HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`,
  lower case too) make nearly every tool send: curl and git (libcurl),
  npm, pip, cargo, Go, Python, Ruby, .NET, Deno, Bun, Node 24 with
  `NODE_USE_ENV_PROXY=1`, Claude Code, Codex.
- **What it lets through**: a host the list matches (J01). Anything else
  gets `403 Forbidden` with a plain-text reason naming the host and the
  setting; the denial is remembered (host, when, which client) for the
  notes below. Loopback destinations get a 403 too: through the filter
  they'd be the filter's own loopback, which on a cloud host is every
  user's. A listed name that resolves to a loopback, unspecified or
  link-local address (DNS rebinding, a cloud's metadata service) is
  refused when the filter connects directly.
- **How it goes on**: each client kind has the upstream it would have
  without the filter. The browser's: Settings → Network's browser row
  (`net::browser_hop`). Commands' and connectors': the system's proxy, as
  the environment and the system settings give it (`net::system_hop`),
  what their own environment would have made them use. Through an upstream
  HTTP proxy every request is a `CONNECT` to the host it was checked for,
  so nothing a client sends on the same connection reaches another host.
  A plain HTTP request goes upstream with `Connection: close`, one request
  per connection.
- **Where it listens**: on Linux, inside the agent's network (J04), by a
  socket made there (`Network::listen`), one listener per client kind; on
  macOS and Windows, on 127.0.0.1. It asks for no password: inside the
  network nothing else can reach it, and on 127.0.0.1 it only ever reaches
  listed hosts.
- **The model is told.** A command's result gets a note naming the hosts
  the filter refused while it ran ("The allowed hosts kept these out:
  registry.npmjs.org. If the task needs one, ask the user to add it in
  Settings → Permissions & approval."), and so does a browser tool's (a
  page's resources from other hosts). A command's tools say only "403" or
  "Forbidden", which the model would take for the server's answer. The
  commands' proxy variables carry the chat as the user name
  (`http://<chat>@127.0.0.1:port`), which most tools send: a note names only
  that chat's denials, or the ones no client named.

## J04. Linux: the agent's network, sealed

The agent's processes run in one network namespace of the user's (D20's
`Network`: a holder in a user and network namespace, joined by each
command before its own sandbox's steps, pasta attached from outside). It
has two modes:

- **Open**: pasta gives the namespace the host's addresses and routes and
  carries its connections out (`--config-net`), as D20 has it.
- **Sealed**: pasta attaches with no addresses, routes or port forwards
  (`-t none -T none -u none -U none`, no `--config-net`): it brings the
  loopback up and nothing else. A connection to any other address fails at
  once (`Network is unreachable`), and so does a name lookup (no resolver
  is reachable). The filter's listeners are the only way out.

pasta is needed only to bring the loopback up; it's kept attached in both
modes so the watcher that starts it again is the same. A sealed network is
ready when the namespace's loopback has its address
(`/proc/<holder>/net/fib_trie` lists `127.0.0.1`).

Commands keep their own sandbox inside (bubblewrap or Landlock), and get
the filter's variables (J03) and `NO_PROXY=localhost,127.0.0.1,::1`.

## J05. Linux, where each mode is used

- **A team server** has the network from the daemon's start (D20). The
  list turns on: pasta starts again sealed, and the daemon's filter
  listens inside. Off: pasta starts again open. The commands, the browser
  and the connectors are in it already. Their servers stay where they are.
- **A desktop, and WSL**, keep the host's network today (D20). The list
  turns on: a sealed network is made (the same `Network`, without the
  pages' shims, which need a daemon) and becomes the agent's network
  (`network::agents`): commands, the browser and the connectors start in
  it from then on. The list turns off: it's dropped, and they start in the
  host's network again. Either change stops the agent's background
  commands (a dev server it left running): they'd keep the network they
  started in, the old one reaching anywhere, or a dropped one nobody
  reaches.

Where the network can't be made (pasta isn't installed, user namespaces
are forbidden), the agent's commands are refused while the list is on,
with why and what to install; the browser still goes through the filter
on 127.0.0.1 (J09).

## J06. Linux desktop and WSL: localhost on both sides

On a desktop the namespace's loopback is its own, but the user expects
the agent's dev server at `localhost:5173` in the editor's browser tab and
their own browser, and the agent's tests expect the database the user
runs at `localhost:5432`. Rovik bridges the two loopbacks itself (pasta's
own forwarding listens on every address of the computer, which would put
the agent's servers on the LAN):

- a port a program in the network listens on is opened on the computer's
  127.0.0.1 and ::1 by Rovik, each connection carried in
  (`Network::connect`);
- a port the computer listens on (127.0.0.1, ::1 or a wildcard address) is
  opened in the network's loopback (`Network::listen_on`), each connection
  carried to the computer's;
- but never a port Rovik's own process holds (the bridge's own listeners,
  the filter, MCP servers), DNS's 53, or a proxy's port the environment,
  the system or Settings → Network names (a way around the list).

The bridge looks every second, opens what's new and closes what's gone. A
port taken on one side stays taken on the other, as on one loopback. The
agent's servers are then reachable only from this computer, not from the
LAN, even bound to 0.0.0.0. A cloud host has no bridge: its network is
private by design (D20).

## J07. macOS: Seatbelt

Commands already run under Seatbelt. While the list is on their profile
ends with:

```
(deny network-outbound (remote ip "*:*"))
(allow network-outbound (remote ip "localhost:*"))
(deny network-outbound (remote ip "localhost:<each proxy port>"))
```

Seatbelt takes the last rule that matches, so the order is the rule:
outward connections reach this computer alone (the filter, the agent's
servers, a local database), but not a local proxy the environment, the
system or Settings → Network names, nor any other host, one on this
computer's own network included. Seatbelt's `localhost` is every address
of this computer's, its LAN address too, not the loopback alone. (A single
deny that carved the loopback out with `require-not` denied the loopback
too, on macOS CI.)
The commands get the filter's variables (J03, on 127.0.0.1). Name lookups
stay
possible (macOS resolves `localhost` through mDNSResponder too, and
refusing it would break every test that opens `http://localhost:…`): a
command can still look a name up, which can carry a little data out in the
name itself. That's said in the README.

Connector programs aren't under Seatbelt today. While the list is on they
run under a profile of these network rules alone (`(allow default)` and
the rules above), so they and what they run reach the filter and the
loopback only.

## J08. Windows

The agent's commands run in WSL (Linux, J05's desktop case). What runs on
Windows itself is the browser of a chat without a project, which goes
through the app's filter (J09). A connector's program running on Windows
itself can't be confined: such a run is refused while the list is on, with
why.

## J09. The browser

While the list is on, Chrome starts with the filter as its proxy (the
browser client's listener, J03), over Settings → Network's browser row,
which becomes the filter's upstream. `--force-webrtc-ip-handling-policy=
disable_non_proxied_udp` keeps WebRTC's UDP from going around it. Chrome
goes to loopback directly, as it always does with a proxy. The tools start
Chrome again when the list turns on or off (as they do when the proxy
changes), and on Linux in the agent's network (J05), as on a cloud host.

Every request Chrome makes passes the filter: a redirect to another host,
a page's scripts, images, frames and WebSockets. A tool's result names the
hosts that were kept out ("Not loaded (not in the allowed hosts):
fonts.googleapis.com"), so the model knows a page may look incomplete.

## J10. Connectors

- **Their programs** (Claude Code, Codex) start where the commands do:
  in the agent's network on Linux (they already join `network::current`),
  under J07's network profile on macOS, refused on Windows itself (J08).
  They get the filter's variables. Their own service hosts must be listed
  for their runs to work (`api.anthropic.com`; `api.openai.com` or
  `chatgpt.com`); a run that fails says which hosts the filter refused, so
  the user sees what to add. Rovik doesn't keep a list of their hosts: it
  would go stale, and the filter's note is always exact.
- **Hosted web search is off** while the list is on: Claude Code is given
  `--disallowedTools WebSearch`, Codex `web_search = "disabled"`.
- **Claude Code's `WebFetch`** asks Rovik first under Rovik's permissions:
  J02's check refuses a host that isn't listed before it runs. In Claude
  Code's own modes it runs, and the filter holds it.

## J11. Project services on a cloud host

The services' containers (D21) are started by the daemon from the
project's compose file, which the agent can change. While the list is on,
the model Rovik gives Compose makes each of the project's networks
internal (`internal: true`), named apart from its open one so that one is
never reused: they reach each other, not the outside. A service on a
network that isn't the project's, or in a network mode but `none` and
another service's, isn't started. A project's containers are made again
(`--force-recreate`) when the list was turned on or off since the daemon
made them, as they'd keep the network they were made with. Their ports
reach the agent's network as before (D21's relays go into each
container's own namespace).

## J12. Changing the setting

- The hosts apply at once: the filter reads the current list for each
  connection.
- Turning the list on or off changes where the agent's processes run
  (J05), and restarts the browser (J09).
- A cloud host follows the last computer to configure it, as its network
  settings do (`set_host_network`): that computer's list is the host's.

## J13. Failing closed

- The list is on but the commands' network can't be enforced here: the
  agent's commands are refused, with why (`confine::refusal`).
- The filter can't start: the web tools and the commands are refused.
- The filter's upstream fails (a proxy down): `502 Bad Gateway` with the
  reason, as a proxy would.

## J14. What the list can't promise

Said in the README, plainly:

- **Loopback is open.** On a desktop, commands reach this computer's own
  services. A local service that fetches URLs for its callers could carry
  data out; the known proxies' ports and (on Linux) DNS's are closed, others
  aren't.
- **On macOS, name lookups work** (J07).
- **Programs that ignore the proxy variables** can't reach the network at
  all while the list is on (raw sockets; Java without its proxy
  properties): they fail, they don't escape.
- **By host, not by path**: a listed host is reached in full.

---

## Tests

- J01: entries normalise and match as described, wildcards, addresses,
  IPv4-mapped IPv6, IDN, refused entries; the frontend's normaliser in
  `permissions.test.ts`, the settings section in a component test.
- J02: `check_tool_call` refuses `browse_url`, `browser_screenshot`,
  `WebFetch` and `web_search` off the list and lets listed and loopback
  ones through; `tools_for` leaves `web_search` out; `browse_url`'s
  description names the hosts.
- J03: against servers on 127.0.0.1 (the filter's tests allow a test host
  that resolves there): CONNECT and plain HTTP to a listed host pass, an
  unlisted one gets 403 and a denial; loopback gets 403; a listed name
  resolving to loopback is refused; through an upstream HTTP proxy every
  request is a CONNECT to the checked host; a denial note for a chat
  names only its denials.
- J04/J05: (Linux, pasta installed, as CI has it) a sealed network's
  command reaches a listed host through the filter, not an unlisted one,
  not an address directly, and can't look a name up; a team server's
  network switches modes and back.
- J06: a server in the network is reached on the computer's 127.0.0.1, a
  server on the computer is reached in the network, a proxy's port and the
  bridge's own aren't bridged, a closed server's port goes.
- J07: the profile's text; on macOS, a confined command reaches a loopback
  server but not a non-loopback address.
- J09: Chrome's launch has the filter and the WebRTC flag; a real Chrome
  (where the browser tests run) doesn't load an unlisted host's resource.
- J10, J11: the connectors' arguments and the Compose model have what's
  described.
