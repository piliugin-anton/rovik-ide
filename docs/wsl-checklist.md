# Checking WSL on a Windows machine

On Windows the agent works in WSL2 (`docs/cloud-hosts.md`, "WSL on Windows").
CI can't run WSL2: GitHub's Windows runners have no nested virtualization.
It compiles and unit-tests the Windows code; everything below needs a real
Windows 10 (build 18362 or newer) or 11 machine, with virtualization on.
Run it before a release that changes the WSL side: `src-tauri/crates/
rovik-client/src/wsl.rs`, `host_shell.rs`, `hosts_page.rs` (its WSL part),
`remote/moving.rs` (`go_home`), `overlay/confine.rs` (Windows), and the
frontend's `wsl*.ts` and `WslSettings.tsx`.

Note what you saw against each step; the expected result is in italics.

## 1. A computer without WSL

Start from a Windows without WSL (or `wsl --unregister` every distribution
and `wsl --uninstall`).

1. Start the app. _A notification: "The agent needs WSL", with Install
   WSL. No offer to install a virtual-files driver (WinFsp is gone), and
   Settings → This computer has no Virtual files item._
2. Open a project and send a message in a chat. _It doesn't start: "On
   Windows the agent's commands run sandboxed in this computer's Linux side
   (WSL)…", pointing at Settings → This computer → WSL._
3. A chat without a project (no folder open): send a message. _It runs;
   a command the agent tries is refused with the same text._
4. Click Install WSL. _Windows asks for permission (UAC). Declining: "the
   install was cancelled". Accepting: "Installing WSL…", then "WSL is
   installed. Restart the computer…" on a first install._
5. Restart, open Settings → This computer → WSL. _Ubuntu is listed; the
   page offers Set up._

## 2. Setting it up

1. Set up, leaving the user empty. _It installs bubblewrap and fuse3 as
   root inside the distribution, makes a `rovik` user (the distribution was
   installed without its first run, so its default user is root), deploys
   Rovik and connects: "Rovik 0.x.y runs in Ubuntu as rovik · Connected"._
2. Test. _Every step passes, the WSL distribution and Isolation extras
   (bubblewrap) included._
3. In the distribution (`wsl -d Ubuntu -u rovik`):
   `ls ~/.rovik/agentd.sock` exists; `pgrep -f "rovik-agentd serve"` shows
   one daemon.
4. `wsl --shutdown`, then send a message in a chat of a project. _The app
   starts the daemon again by itself; the message runs._

## 3. A chat runs in WSL

1. In a project with a git repository, send a message asking the agent to
   change a file and run the project's tests. _The chat's place reads
   "This computer (WSL)", without a host badge; it briefly showed "Moving
   to This computer (WSL)…" on the first message._
2. Ask the agent to run `cat ~/.ssh/id_rsa ~/.rovik/config.toml`. _Both
   are empty or missing to it: the sandbox hides them._
3. Ask it to run `touch /usr/x` and `ps aux`. _The first fails (read-only
   system); the second lists only the command's own processes._
4. When the run ends, the change lands in the project on Windows (or
   waits in the bar, with the project's conflicts, as a local chat's
   would). _The editor shows it once it lands, not while the agent
   writes._
5. A second chat of the same project runs at the same time. _Each in its
   own worktree; neither sees the other's changes before they land._

6. A new chat of a project set to CLMs (the context-mode menu): _no
   "CLMs need Virtual files" warning before its first message; after it,
   the context panel shows the agent's context file and its revisions, as
   the daemon in WSL mounts the virtual files with FUSE. If it falls back
   to Trimming instead, `wsl -d Ubuntu -u rovik -- ls /dev/fuse` and the
   daemon's log (`~/.rovik/logs`) say why._

## 4. Startup failures are told plainly

Each of these should show, on the WSL page and in a notification at the
app's start, what is wrong and what to do, not only the raw error.

1. Virtualization off: from an administrator prompt,
   `bcdedit /set hypervisorlaunchtype off`, restart, start the app. _"WSL2's
   virtual machine can't start: virtualization is turned off…", naming the
   BIOS/UEFI setting and the Virtual Machine Platform, with the original
   `HCS_E_HYPERV_NOT_INSTALLED` error after it._ A message sent in a chat
   says why it can't run. Undo: `bcdedit /set hypervisorlaunchtype auto`,
   restart.
2. WSL turned off: `dism /online /disable-feature
/featurename:VirtualMachinePlatform`, restart. _The same plain message
   (Virtual Machine Platform)._ Undo with `/enable-feature … /all`.
3. The distribution gone: `wsl --unregister Ubuntu` (with the app
   closed), start the app. _"The distribution isn't installed any more: set
   WSL up again…"._
4. A Windows too old for WSL2 (before build 18362), if one is at hand: _the
   page says "WSL2 needs Windows 10 version 1903 (build 18362) or newer" and
   offers no install._

## 5. Projects, moves and updates

1. Delete a project whose chats ran in WSL. _It goes; in the distribution
   `ls ~/.local/share/rovik/work` no longer has those chats' worktrees._
2. Open a copy of a project in another folder and answer "Looks like a
   moved project" with Use its chats here (a merge). _Its chats in WSL come
   back first (their changes land), then the merge goes; their next
   message takes them to WSL again._ While one of them runs: _refused,
   "stop it, or let it finish"._
3. Chat menu → Move out of WSL… _The chat comes back; its changes land in
   the project; the chat runs in WSL again on its next message._
4. Install the next version of the app over this one. _At its start Rovik
   in the distribution is updated without asking ("Rovik in Ubuntu is
   updated")._
5. Settings → This computer → WSL → Remove. _Rovik leaves the distribution;
   the distribution stays; chats of projects don't start until it's set up
   again._
