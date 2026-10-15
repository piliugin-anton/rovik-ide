# Rovik on a team server

This page is for the administrators of a Linux server that several
people reach over SSH, each with their own account, and on which they run
Rovik's agent. Each person's chats run in their own daemon, as that
person, and nothing of theirs is shared with the others.

## What gets installed

Rovik is installed once per server, by root:

| Path                                                      | What it is                                               |
| --------------------------------------------------------- | -------------------------------------------------------- |
| `/opt/rovik/bin/rovik-agentd-<version>`                   | The daemon, owned by root. Users can't replace it.       |
| `/opt/rovik/bin/rovik-agentd`                             | A link to the current version.                           |
| `/etc/rovik/agentd.toml`                                  | The administrators and the limits. Everyone can read it. |
| `/etc/systemd/system/rovik-agentd@.service`               | The unit. Each user's daemon is an instance of it.       |
| `/etc/systemd/system/rovik-agentd@.service.d/limits.conf` | The memory, task and CPU caps on each daemon.            |
| `/etc/systemd/system/rovik.slice`                         | The caps on all of Rovik together.                       |
| `/etc/sudoers.d/rovik`                                    | Lets users run the commands described below.             |
| The `rovik-admin` group                                   | The administrators.                                      |

Each user's daemon keeps its files in `~/.rovik` and works in
`~/.local/share/rovik`. Both folders are the user's own, with mode 0700,
and the daemon refuses to start if either is open to anyone else.

## Installing

The first person to deploy from the app installs Rovik and becomes its
administrator. That takes `sudo`. If sudo asks for a password, the app
asks for it once and doesn't keep it. Without sudo, the app shows the
command for someone with root to run:

```sh
sudo /path/to/rovik-agentd install --admin USER
```

The installer copies the binary, writes `agentd.toml` with USER as the
administrator and with limits sized to the machine, writes the units, adds
USER to `rovik-admin`, installs the sudoers rule once `visudo` accepts it,
and starts USER's daemon. To see what it would do without changing
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
```

The app runs `user start` when it deploys to a server that already has
Rovik. Each daemon starts at boot, so nobody needs to stay logged in.

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
sudo /opt/rovik/bin/rovik-agentd admin upgrade FILE
sudo /opt/rovik/bin/rovik-agentd admin uninstall
```

These change the limits, the version and the list of administrators, and
nothing else. Being Rovik's administrator gives no access to other users'
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

When a command goes over its daemon's memory cap, the kernel kills that
command, and the daemon and its other runs carry on (`OOMPolicy=continue`).
The daemon is the last process the kernel picks when memory runs out
(`OOMScoreAdjust=-500`). The commands it starts are back at the ordinary
score.

Each daemon has its own `/tmp` (`PrivateTmp=yes`), and the agent's
commands keep their temporary files in `~/.local/share/rovik/tmp`, so
nothing of one user's work lands in a shared `/tmp`.

## What can read what

- The agent's commands run as the user and can read whatever the user can.
- Worktrees and API keys sit in the user's 0700 folders. Other users can't
  read them, but root can. The app tells users: "Your API keys are stored in
  your account on this server. The agent's commands there and anyone with
  root on it can read them."
- Administrators can read the same things as before they became
  administrators, and nothing more.

## Ports are shared

A dev server, or anything else the agent's commands open on a port, can be
reached by every user of the server, and two people's agents can clash on
one port. Until Rovik drives Chrome over a pipe, the browser tools open
Chrome's DevTools on a `127.0.0.1` port that other users of the server
could reach. Leave the browser tools off on servers you share with people
you don't trust.

## Upgrades

`admin upgrade FILE` installs a new build beside the current one, moves
the link and restarts the daemons that were running. Releases are to be
signed, and the installed binary will check them with a key built into it.
Until signed releases are published, it takes only a build you allow by
setting `allow_unsigned = true` in `agentd.toml`.

## Removing Rovik

`admin uninstall` stops every daemon and removes everything listed above.
Each user's `~/.rovik` and `~/.local/share/rovik` stay, because their
chats are there. A user who no longer wants Rovik runs `user stop` and
deletes those two folders. The app does this when the user removes the
host, after confirming what will be deleted.
