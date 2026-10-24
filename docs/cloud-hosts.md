# Rovik on a team server

This page is for the administrators of a Linux server that several
people reach over SSH, each with their own account, and on which they run
Rovik's agent. Each person's chats run in their own daemon, as that
person, and nothing of theirs is shared with the others.

## What gets installed

Rovik is installed once per server, by root:

| Path                                                         | What it is                                                                 |
| ------------------------------------------------------------ | -------------------------------------------------------------------------- |
| `/opt/rovik/bin/rovik-agentd-<version>`                      | The daemon, owned by root. Users can't replace it.                         |
| `/opt/rovik/bin/rovik-agentd`                                | A link to the current version.                                             |
| `/etc/rovik/agentd.toml`                                     | The administrators and the limits. Everyone can read it.                   |
| `/etc/systemd/system/rovik-agentd@.service`                  | The unit. Each user's daemon is an instance of it.                         |
| `/etc/systemd/system/rovik-agentd@.service.d/limits.conf`    | The memory, task and CPU caps on each daemon.                              |
| `/etc/systemd/system/rovik.slice`                            | The caps on all of Rovik together.                                         |
| `/etc/sudoers.d/rovik`                                       | Lets users run the commands described below.                               |
| `/opt/rovik/browser/<version>/`                              | The browser tools' browser (below), owned by root.                         |
| `/opt/rovik/toolchains/rust/`, `/opt/rovik/toolchains/node/` | The Rust and Node versions every user's chats run (below), owned by root.  |
| The distro's `claude-code` package, on request               | Claude Code for the connector of that name (below), once for everyone.     |
| `/var/cache/rovik/npm`, `/var/cache/rovik/crates`            | The package caches the daemons share, owned by root and the `rovik` group. |
| The distro's `bubblewrap` package                            | Confines the chats' commands (see What can read what).                     |
| The distro's `passt` package                                 | Its `pasta` gives each user's commands a network of their own (below).     |
| The `rovik-admin` group                                      | The administrators.                                                        |
| The `rovik` group                                            | Every daemon, which writes the shared caches (below).                      |

Each user's daemon keeps its files in `~/.rovik` and works in
`~/.local/share/rovik`. Both folders are the user's own, with mode 0700,
and the daemon refuses to start if either is open to anyone else.

## Installing

The first person to deploy from the app installs Rovik and becomes its
administrator. That takes `sudo`. If sudo asks for a password, the app
asks for it once and doesn't keep it. Without sudo, the app shows the
command for someone with root to run. It boils down to this:

```sh
sudo /path/to/rovik-agentd install --admin USER
```

The build is uploaded to the user's home, so before root runs it, root
copies it into a folder of its own and checks the copy's SHA-256 against
the build's: another process of the user's can't swap the file between
the app's check and root running it.

The installer first installs the distro's `passt` package, which each
user's network needs ([Each user's own network](#each-users-own-network)),
and stops before changing anything if it can't. It then copies the binary,
writes `agentd.toml` with USER as the
administrator and with limits sized to the machine, writes the units, adds
USER to `rovik-admin`, installs the sudoers rule once `visudo` accepts it,
starts USER's daemon, installs the browser for the browser tools
([The browser](#the-browser)) and the distro's `bubblewrap` package, which
confines the chats' commands ([What can read what](#what-can-read-what)).
Neither stops the install when it can't be installed, and `--no-browser`
leaves the browser out, as the app does for a WSL distribution. To see
what it would do without changing
anything, add `--root /tmp/somewhere`. It then writes the files under that
folder and prints each command it would run.

The daemon never runs as root. A deploy over a root login stops and asks
for a regular account. Root can still install Rovik by naming a regular
user as its administrator.

Without systemd, as in a container, the installer installs the binary and
the settings, and the app starts each user's daemon when it connects. Runs
then continue after a reboot only once that user's app connects again, and
only the daemon's own limits (slots, worktrees, disk) apply.

## Each user's daemon

Later users don't need sudo rights of their own. The sudoers rule lets
everyone run these, and they act only on the daemon of the person running
them:

```sh
sudo /opt/rovik/bin/rovik-agentd user start     # enable and start it
sudo /opt/rovik/bin/rovik-agentd user stop      # stop and disable it
sudo /opt/rovik/bin/rovik-agentd user restart
sudo /opt/rovik/bin/rovik-agentd user status
sudo /opt/rovik/bin/rovik-agentd user toolchain rust 1.82
sudo /opt/rovik/bin/rovik-agentd user toolchain node 20
```

The app runs `user start` when it deploys to a server that already has
Rovik. Each daemon starts at boot, so nobody needs to stay logged in.
`user toolchain` installs a Rust or Node version for the whole server
([Shared toolchains and caches](#shared-toolchains-and-caches)); a chat
runs it by itself when its project needs one.

When a daemon stops, for an upgrade, a reboot or `user restart`, it lets
the commands its runs are in end and pauses each run there, for up to
80 seconds. A run waiting for its user's answer doesn't hold it up. Its
next start continues them. A command still running after that is cut,
and the user is asked before it runs again.

## The administrators

Administrators run the `admin` commands. The sudoers rule allows them to
the `rovik-admin` group, and the daemon also checks the `admins` list in
`agentd.toml`:

```sh
sudo /opt/rovik/bin/rovik-agentd admin limits                    # show them
sudo /opt/rovik/bin/rovik-agentd admin limits memory_max=6G max_slots=2
sudo /opt/rovik/bin/rovik-agentd admin limits cpu_quota=none     # remove one
sudo /opt/rovik/bin/rovik-agentd admin add USER
sudo /opt/rovik/bin/rovik-agentd admin remove USER
sudo /opt/rovik/bin/rovik-agentd admin browser                   # install it if it's missing
sudo /opt/rovik/bin/rovik-agentd admin bubblewrap                # install it if it's missing
sudo /opt/rovik/bin/rovik-agentd admin pasta                     # install it if it's missing
sudo /opt/rovik/bin/rovik-agentd admin claude-code               # Claude Code for everyone, or its update
sudo /opt/rovik/bin/rovik-agentd admin toolchain list            # the versions installed
sudo /opt/rovik/bin/rovik-agentd admin toolchain add rust 1.82   # for everyone
sudo /opt/rovik/bin/rovik-agentd admin toolchain add node 20
sudo /opt/rovik/bin/rovik-agentd admin toolchain remove node 20
sudo /opt/rovik/bin/rovik-agentd admin toolchain prune 90        # what nobody used for 90 days
sudo /opt/rovik/bin/rovik-agentd admin toolchains user-install off
sudo /opt/rovik/bin/rovik-agentd admin toolchains shared-caches off
sudo /opt/rovik/bin/rovik-agentd admin toolchains setup          # the group and folders again
sudo /opt/rovik/bin/rovik-agentd admin upgrade FILE
sudo /opt/rovik/bin/rovik-agentd admin uninstall
```

These change the limits, the version, the browser, the toolchains and the
list of administrators, and nothing else. Being Rovik's administrator gives no access to other users'
files or chats.

## Limits

`agentd.toml` holds the limits, one per line:

| Key                | Applies to   | What it limits                                                   |
| ------------------ | ------------ | ---------------------------------------------------------------- |
| `max_slots`        | each user    | Heavy commands (builds, test runs) running at once               |
| `max_worktrees`    | each user    | Chats' working copies kept on the server                         |
| `max_disk_gb`      | each user    | `~/.rovik` and `~/.local/share/rovik` together, in GB            |
| `memory_max`       | each user    | The daemon and everything it starts (`MemoryMax=`), such as `8G` |
| `tasks_max`        | each user    | Processes and threads (`TasksMax=`)                              |
| `cpu_quota`        | each user    | CPU time, `200%` being two CPUs (`CPUQuota=`)                    |
| `total_memory_max` | all of Rovik | Memory of every daemon together, on `rovik.slice`                |
| `total_cpu_quota`  | all of Rovik | CPU of every daemon together, on `rovik.slice`                   |

The installer gives each user half the machine's memory, all of Rovik 80%
of it, and 4096 tasks per user. CPU is shared equally: every daemon has
the same weight, and no quota unless you set one. The daemon itself
enforces the slots, the worktrees and the disk. A new chat is refused
when its user is at the cap, and the message names the administrators.

A user can ask for fewer slots in their own `~/.rovik/config.toml`
(`max_slots = 1`), never for more than `max_slots` here. When nobody has
set a number, each daemon takes one slot while other users' daemons run
on the server.

New limits apply to each daemon the next time it starts.

## Shared toolchains and caches

Each Rust and Node version is installed once per server, as root, in
`/opt/rovik/toolchains`, where every user can read and run it and change
nothing. `node_modules` and `target/` stay in each chat's own working
copy. A chat whose project names a version the server lacks has it
installed, through the same sudo rule as `user start`, so nobody has to
ask an administrator:

```sh
sudo /opt/rovik/bin/rovik-agentd user toolchain rust 1.82
sudo /opt/rovik/bin/rovik-agentd user toolchain node 20
```

What a project asks for comes from its `rust-toolchain.toml` or
`rust-toolchain` (the `channel`, with any `components` and `targets` it
names), and from its `.nvmrc`, `.node-version` or `engines.node`. Without
them, Rust is `stable` and Node the latest LTS. Rovik installs it before
the installs that prepare the project, and again when a command fails
because its program isn't found. The versions install while the chat
waits: a Rust version is about 1 GB on disk and takes a few minutes.

Root checks every word first. A version is `stable`, `beta`, `nightly`,
`nightly-YYYY-MM-DD`, `1.N` or `1.N.P` for Rust, and `lts`, a major
version or `X.Y.Z` for Node; anything else is refused, so a chat can't
pass text of its own to root. Rust comes from `static.rust-lang.org` and
Node from `nodejs.org`, checked against the SHA-256 the servers publish
(`rustup-init.sha256`, `SHASUMS256.txt`), and a Node version's folder only
takes its name once all of it is unpacked. One install runs at a time.

An administrator can turn it off, and name what a chat may install:

```toml
[toolchains]
user_install = false          # only `admin toolchain add` installs
shared_caches = false         # each daemon keeps its own package caches
allowed_rust = ["1.82", "nightly"]   # empty: any version Rovik takes
```

`admin toolchain add` works whether or not chats may install versions.
`admin toolchain list` says what's there and when each was last used, and
`admin toolchain prune DAYS` removes what no chat used in that time.

The package caches are shared too, in `/var/cache/rovik`, which the
`rovik` group writes (every daemon runs in it). That is only done where
the result is checked:

- **npm.** Only `npm ci` with a lockfile uses the shared cache, because
  npm checks every package it takes for you against the lockfile's
  `integrity`. An install whose project or user `.npmrc` holds a registry
  token keeps the user's own cache, so a private package never lands in
  the shared one.
- **cargo.** Cargo doesn't check what is already in its cache, so Rovik
  does: a crate from the shared store is copied into a user's cargo cache
  only when its SHA-256 is the checksum in that project's `Cargo.lock`,
  and after a `cargo fetch` the user's checked crates go into the store.
  A file another user left there that doesn't match is skipped, never
  used.

The daemons find the caches because their unit names the `rovik` group
(`SupplementaryGroups=rovik`). Without systemd, a daemon isn't in the
group: it keeps the user's own caches, and everything else above works
the same.

`install`, `admin bubblewrap` and each upgrade put the `rovik` group, the
folders and the unit in place, when they aren't there yet:
`admin toolchains setup` does it alone. `rovik-agentd doctor` says which
versions are installed and how the two settings stand.

When a command goes over its daemon's memory cap, the kernel kills that
command, and the daemon and its other runs carry on (`OOMPolicy=continue`).
The daemon is the last process the kernel picks when memory runs out
(`OOMScoreAdjust=-500`). The commands it starts are back at the ordinary
score.

Each daemon has its own `/tmp` (`PrivateTmp=yes`), and the agent's
commands keep their temporary files in `~/.local/share/rovik/tmp`, so
nothing of one user's work lands in a shared `/tmp`.

## Checking a server

`rovik-agentd doctor`, run by a user, says what their account and the
server lack for their daemon (pasta, and a network of their own that their
commands can't get without it, among them), then the versions, the free space where
Rovik's folders are, the install and its limits, the shared Rust and
Node versions (and whether chats may install one and the caches are
shared), the connectors installed for them
(`connectors installed: Claude Code`), the heavy commands their daemon
runs at once, and what its folders and logs take. The app's
connection test reads the same once Rovik is deployed. The daemon's log,
`~/.rovik/logs/agentd.log`, is rotated at 5 MB, with 3 older files kept.

## Cleaning up

Each daemon cleans up after its user once a day, and when they click
"Clean up now" on the host's page in Settings → Cloud hosts. What their
deleted chats left goes, and so do the working copies of chats moved
back to their computer, a week after the move ("Clean up now" doesn't
wait). So do attachments no message uses, an hour after they were
uploaded, and the agent's temporary files in `~/.local/share/rovik/tmp`
that nothing changed for a day, which go when the daemon starts too.
"Clean up now" takes those nothing changed for an hour: a command may
still use newer ones. A folder holding a socket stays. Where git is
installed, `git gc --auto` runs in each project's repository; "Clean up
now" runs a full `git gc`, which drops what no ref has reached for an
hour, such as what deleted chats committed. A chat still on the server
keeps everything: its messages, attachments and working copy. The
host's page shows what Rovik's folders take, and what "Clean up now"
freed.

## What can read what

- The agent's commands run as the user, in a bubblewrap sandbox: the
  system is read-only to them but for the worktree, `/tmp`, the
  toolchains' caches under the home folder and the shared caches of
  [Shared toolchains and caches](#shared-toolchains-and-caches); the
  credential stores
  (`~/.rovik` with the API keys, `~/.ssh`, `~/.aws`, `~/.gnupg`, the cloud
  CLIs' folders) are empty to them; they see no other process and hold no
  capability, so they can't undo any of that. The installer adds
  bubblewrap from the distro's package and, where AppArmor restricts
  unprivileged user namespaces (Ubuntu 23.10 and later), the AppArmor
  profile `rovik-bwrap`, which lets `/usr/bin/bwrap` make one. Without
  them the commands run under the kernel's Landlock (Linux 6.2 and later)
  with the same folders, but seeing the other processes; with neither,
  they're refused. `rovik-agentd doctor` and the app's connection test
  report which. `admin bubblewrap` adds both again. A command that names `~/.rovik` still needs the user's approval.
  The sandbox also closes the user's local sockets to them (the SSH and
  GPG agents', the session bus, everything in `/run/user/<uid>`, the
  system bus and the container daemons'), and empties single login files
  (`~/.netrc`, `~/.git-credentials`, `~/.npmrc`, cargo's token).
- Each of a user's computers sees its own projects on the daemon: the
  project's repository, packs, refs, settings and remote belong to the
  computer that carried it there, and another computer of the same
  account is refused them, as it is another computer's chats.
- The shared caches are one folder per server: the `rovik` group reads
  and writes them, so every daemon's user can read what another's chat
  downloaded. Nothing private goes in them: npm's only for `npm ci` with a
  lockfile and no registry token, and crates only when they match a
  project's `Cargo.lock`. The toolchains themselves are root's, readable
  by all and writable by none but root.
- Worktrees and API keys sit in the user's 0700 folders. Other users can't
  read them, but root can. The app tells users: "Your API keys are stored in
  your account on this server, so chats there run while this computer is
  away. The agent's commands there and anyone with root on it can read
  them." Each user can remove them there from Settings → Cloud hosts.
- Administrators can read the same things as before they became
  administrators, and nothing more, as long as `agentd.toml` says
  `allow_unsigned = false`, as it does unless someone changes it. With
  `allow_unsigned = true`, each administrator can install a build of their
  own, and that build is what sudo runs as root for every user's `user`
  and `admin` commands, and the upgrade runs it as root at once: each
  administrator has root on the server. Allow unsigned builds only where
  every administrator may have root.

## Each user's own network

Each user's commands run in a network of their own, as on their own
computer: every command the agent runs, and every run of a connector's
program (Claude Code), starts in it. A server there can listen on any port,
on 127.0.0.1 or 0.0.0.0, without clashing with another user's server on
the same port, and no one else's process can reach it: nothing comes into
the network from outside. The agent tests its servers there as it would on
the user's computer, with `curl localhost:3000` or a test suite. The ports
are the user's to share among their own servers, from all their chats: one
that a server holds is busy for the others, and the agent is told which of
the user's processes holds it (`port 3000 is held by "node server.js" (pid
4242)`). A connection refused at a local address is explained to it too:
nothing listens there, or the server was still starting.

Connections out work as they do on the server: pasta, from the distro's
`passt` package, carries them through ordinary sockets of the user's. DNS
works where the server's resolver is on its loopback (systemd-resolved)
too. A proxy that the daemon's environment names on the server's loopback
(`HTTPS_PROXY=http://127.0.0.1:3128`) is reached at the same address, and
the server's own services at the network's gateway address. The browser
tools open no port either: Rovik drives the agent's Chrome over a pipe
that only that user's daemon holds, not a DevTools port, so other users of
the server can't reach the browser, which runs as that user.

`install` and `admin upgrade` install `passt` before they change anything,
and stop where it can't be installed; `admin pasta` tries again. Where the
network can't be made all the same (user namespaces forbidden), a user's
commands run in the server's network, whose ports every user shares and
can clash on: `rovik-agentd doctor`, the app's connection test and the
daemon's log say why. `ROVIK_PRIVATE_NETWORK=0` in a daemon's environment
turns the network off, for troubleshooting. A WSL distribution, one
person's computer, keeps its own network.

### Pages

A command that opens a page of one of its servers, as `vite --open` does,
or `xdg-open http://localhost:5173`, or a tool that runs `$BROWSER`, opens it in
the browser on the user's computer that the chat belongs to: Rovik forwards
a port of that computer's loopback to the server's port in the user's
network, the same port where it's free there and another where it isn't,
and opens the page at it. The chat shows the forwarded ports under its
title, "Opened ports: 5173", each with the server's own port and a button
that copies the address on the computer that leads to it (`localhost:5174`
where 5173 was taken there). A port stays forwarded until nothing listens
on it in the network any more. Only pages of the user's own servers open
this way, http or https at localhost, 127.0.0.1 or ::1: any other address
is refused, for the user's safety, and the agent is told why. A chat's
commands open at most 5 pages a minute.

## WSL on Windows

On Windows the same daemon runs in a WSL2 distribution, as Rovik's Linux
side on that computer: the agent's commands run there in the sandbox
(bubblewrap), and the chats' overlays use FUSE. It isn't a cloud host: it's
set up from Settings → This computer → WSL, which installs Rovik in the
distribution as root inside it (`wsl.exe -u root`, no password) with
bubblewrap and fuse3, adds the computer's token and starts the daemon;
the app starts the daemon again whenever the distribution restarts, and
updates Rovik there with its own updates. The page has no limits, no
upgrade and no chat list: the chats there are the computer's own. The
app reaches the daemon through `wsl.exe -d <distro> --exec rovik-agentd
attach --stdio`, the byte bridge an SSH exec channel uses too.

Every chat of a project moves there by itself when it first runs, as
"Move to" a cloud host would (its project synced into a worktree there,
its results landing back in the project), and is shown as "This computer
(WSL)"; a move that fails is the start's failure, with the cause. A chat
without a project stays on this computer, where the agent's commands are
refused, as they are until WSL is set up. Deleting a project deletes its chats
in WSL too; merging one into another brings its chats in WSL back to this
computer first (and is refused, with why, while one is running or can't
come back), and their next message takes them to WSL again. When the distribution can't
start, the page and a notification say why in plain words for the known
cases: virtualization turned off in the BIOS/UEFI or Windows' Virtual
Machine Platform missing (`HCS_E_HYPERV_NOT_INSTALLED`, `0x80370102`),
WSL turned off (`0x8007019e`), its kernel out of date (`0x800701bc`), the
WSL service disabled (`0x80070422`); and a Windows too old for WSL2
(before Windows 10 version 1903, build 18362) is told so before anything
is tried. A computer without WSL is offered its install, at start and on
the page: `wsl --install -d <distro> --no-launch` run as an administrator
(Windows asks for permission), which may need a restart; where there's no
`wsl.exe` yet, the two Windows features are turned on with `dism` and the
restart brings it. The distribution isn't launched, so its default user
is root; setting it up then makes a `rovik` user to run as.

CI can't run WSL2 (no nested virtualization on its Windows runners): the
checks to make by hand on a Windows machine are in `docs/wsl-checklist.md`.

## The browser

The browser tools use Chrome for Testing, Google's build of Chromium for
automation, at one version that Rovik pins: the version eoka, the library
that drives it, is tested with. The installer puts it in
`/opt/rovik/browser/<version>`, and `admin browser` installs it when it's
missing. It downloads Google's archive for the server's CPU (x86_64 or
aarch64, about 200 MB) and refuses it unless its size and SHA-256 are the
ones Rovik pins. It then installs the libraries the archive lists for the
server's package manager (apt-get, dnf or zypper), and only then gives the
folder its version's name. Chrome for Testing needs the GNU C library, so
on Alpine the browser tools use Alpine's own `chromium` package instead.
A browser that can't be installed doesn't stop an install or an upgrade:
the browser tools say what's missing when they're used.

The browser tools use the pinned browser first, then Chromium
(`/usr/bin/chromium`, `/usr/bin/chromium-browser` or the snap), then Chrome.
They drive it over a pipe, never a DevTools port, and they close a Chrome
that listens on a TCP port anyway, which a flag in a Chrome flags file can
make it do.

## Claude Code

The Claude Code connector (Settings → Connectors) runs `claude` on the
computer a chat runs on, so a chat on a server needs Claude Code there.
An administrator installs it once for everyone, from Anthropic's own
package repository for the server's package manager (apt, dnf or apk):

```sh
sudo /opt/rovik/bin/rovik-agentd admin claude-code      # install, or update
```

It lands in the system's `bin`, where each user's daemon finds it on the
PATH, as `claude` installed by hand would be found too (the native
installer puts it in `~/.local/bin`, for that user alone). The package
doesn't update itself: run the command again for the newest version.
Where the server has another package manager, install Claude Code by hand
or name the program's path in the connector's settings. A user can also
install it for themselves alone: **Set up** in Settings → Connectors, with
the host chosen, runs Anthropic's installer as that user, which puts
`claude` in their `~/.local/bin` (it needs `curl` and `bash` on the
server). **Update to …**, offered there when a newer version is out, runs
`claude update` as that user: it updates their own installation, while
the shared package is updated by the administrator with `admin
claude-code`. However it was installed, the host's page in Settings →
Cloud hosts then says "Connectors installed: Claude Code", as
`rovik-agentd doctor` run as that user does: both look for the program
where that user's chats look when the connector's settings name none.

Each user signs in as their own. The token or key of the connector's
settings travels with the chat's model and reaches the program as
`CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`; the daemon keeps it as
it keeps providers' keys ([What can read what](#what-can-read-what)). A
connector set to the computer's login uses the login in that user's
`~/.claude` on the server, made by running `claude` there once, or by
**Set up**: it runs `claude auth login` as that user on the server, opens
the sign-in page in the browser of the user's own computer, and passes
the code that page shows back to the program. The login is that user's
alone. Claude
Code's own conversation for a chat (its session) is kept by the program
in that user's home; Rovik remembers which session each chat continues.

Claude Code runs the agent's commands itself, not through Rovik's
sandbox. What it may do is decided by Rovik's Permissions & approval
(the connector's default permission mode), or by Claude Code's own
permission mode and allowed tools when the connector's settings pick one.

## Upgrades

`admin upgrade FILE` installs a new build beside the current one, moves
the link and restarts the daemons that were running, which pause their
runs first and continue them on the new build. A release's build comes
with its signature in `FILE.sig`, which the installed binary checks with
the release key built into it, so a new build can't vouch for itself. The
app uploads both. A build of your own has no signature: it goes in only
once you allow that by setting `allow_unsigned = true` in `agentd.toml`,
which gives every administrator root on the server (see
[What can read what](#what-can-read-what)).
A build older than the installed one is refused unless the command says
`--allow-downgrade`. The installer reads FILE once, with your own access
to it, so it reads nothing you couldn't, and installs what it read: a file
in your home is yours to change meanwhile, so it never goes back to it. It
asks a copy in memory its version, as you rather than as root, and the
copy under `/opt/rovik/bin` is root's alone until it has passed. Each
build is named for the version it says it is. Before anything changes, the
upgrade installs `passt`, and stops where it can't
([Each user's own network](#each-users-own-network)). Before the daemons start again, the new build installs
the browser it pins and bubblewrap, whichever is missing, and the browser's
other versions go. It also puts the `rovik` group, the shared caches and
the unit that names the group in place, so the daemons that restart run
in it. An upgrade
from the app does the same for a server whose installed build is older
than that.

The app updates itself (README, Updates), and the server's Rovik stays as
it was. Once an administrator's app runs a newer version than the
server's, it offers them, once, to upgrade the server to that version:
for everyone on it, as "Upgrade for everyone" on the host's page in
Settings → Cloud hosts does. Other users' apps tell them which
administrators to ask.

## Removing Rovik

`admin uninstall` stops every daemon and removes everything listed above,
`/var/cache/rovik` with the shared caches among them, and the `rovik` and
`rovik-admin` groups. `/opt/rovik/toolchains` goes with `/opt/rovik`. The
libraries the browser needed stay installed.
Each user's `~/.rovik` and `~/.local/share/rovik` stay, because their
chats are there. A user who no longer wants Rovik runs `user stop` and
deletes those two folders. The app does this with "Remove Rovik from this
server" on the host's page in Settings → Cloud hosts, after confirming
what will be deleted, once no chats of theirs are left there.
