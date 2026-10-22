# Task: implement plan 2026-10-23-shared toolchains/caches

Implement `.rovik/plans/2026-10-23-write-a-plan-implementing-a-shared-management.md`. ALL 10 steps and the plan's test list are done.

Files — agentd: `limits.rs` (`[toolchains]` + `ToolchainSettings`), `installer/toolchains.rs` (new: Kind/parse_version/flags/ensure/list/remove/prune/setup/installed_here/not_set_up), `installer.rs` (consts TOOLCHAINS/CACHES/SHARE_GROUP, `System::run_with`, `both()`, install/upgrade/uninstall/user/admin arms, unit `SupplementaryGroups=rovik`), `cli.rs` (USAGE, `-c`/`-t` as words), `doctor.rs`, `serve.rs` (`shared_toolchains`/`can_write`/`root_installer`), Cargo.toml (flate2+tar). Core: `toolchains.rs` (new), `lib.rs`, `command_tool.rs`, `overlay/confine.rs`, `agent_run/prepare.rs`, `agent_run/tools.rs`. Docs: `docs/cloud-hosts.md`, `src/components/CloudHostsSettings.tsx`.

Verification: agentd 73 unit + 13 tests/install.rs green; core 1749 green; the last three added tests each green individually. Unrelated environment failures: `tests/deploy.rs` (needs real sudo), `browser_tools::real_chrome` (needs Chrome).

NOTE: the bash sandbox lost the Rust toolchain (`cargo`/`git` gone, `~/.rovik` tmpfs), so the final combined run and `cargo clippy --workspace --all-targets` could not be repeated here. Edits after the last green run were the three test files (each ran green) and removing an unused `use crate::home;` from serve.rs's test module (no `home::` left there).

Deviations: Node tarball `.tar.gz` (same SHASUMS256.txt check) not `.tar.xz`; `checked_name` allows `_`, refuses a leading `-`.

NEXT: none pending; if tooling returns, run the two suites and clippy.
