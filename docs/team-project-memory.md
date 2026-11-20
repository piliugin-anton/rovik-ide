# Team project memory

A project can share selected facts and procedures through a self-hosted HTTPS hub. Each desktop keeps a separate team replica. Local retrieval and editing continue offline. Personal cloud hosts can receive imported memories through their existing desktop connection.

## Connect a desktop

1. Open **Settings → Repository accounts**. Choose GitHub or GitLab. For team memory, enter the trusted hub URL as the sign-in service. Enter the displayed code on the provider’s verification page. The account applies across projects. The desktop key store holds the provider credentials and a separate session for that service.
2. In **Settings → Long-term memory**, select the provider, enter the same hub URL and the team project UUID supplied by the operator, then choose **Link project**. Linking starts with a download. Existing local notes require individual approval before publication.
3. In **Manage memories**, select **Review for sharing**. Review the projected title, text, evidence, tags and file references. Approval queues the revision and enables subsequent eligible desktop edits to that record. Personal-host edits need review. Private notes, episodes, restricted files and detected credentials are excluded. The manager shows the authenticated claim publisher and revision submitter for team imports.
4. **Sync now** runs a bounded exchange. Open projects also retry periodically and on reconnect. **Pause sharing** suspends exchanges. **Withdraw from team** queues a deletion while retaining the personal note. Other devices receive it on their next successful sync. Hub revision history and backups retain prior copies.
5. A conflicting personal edit remains available in the manager. Review it and either keep it as personal memory or explicitly approve a new publication. After a hub restore or device copy, **Reconcile team link** downloads a fresh replica, preserves divergent notes and resets the link’s progress.

Global repository accounts also support direct provider login without a hub. With the optional sign-in service field empty, the desktop uses its configured public OAuth client ID. Sign-in requests identity scopes. Backend features can request a provider token for a named scope through `rovik_client::forge_accounts::provider_token`. A future PR or MR operation requiring extra scopes must obtain consent for those permissions. Signing out removes the global credential for that provider and revokes the hosted session when the service is reachable.

## Configure the hub

Run the binary built by `cargo build --release -p rovik-team-memory` from `src-tauri/`. It listens on loopback, port 7878 by default. Terminate TLS at a reverse proxy on the same machine. Allow streaming for snapshot responses. Use a private data directory and a dedicated service account. Keep bearer tokens, the device-proof header and OAuth response bodies out of proxy and application logs.

| Setting                   | Purpose                                                                                                                 |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `ROVIK_TEAM_DATA`         | Writable directory for SQLite project stores and the session database                                                   |
| `ROVIK_TEAM_PORT`         | Optional loopback port                                                                                                  |
| `ROVIK_TEAM_PROJECTS`     | Project registry, defaulting to `projects.json` inside the data directory                                               |
| `ROVIK_GITHUB_CLIENT_ID`  | Public GitHub OAuth app client ID with device flow enabled                                                              |
| `ROVIK_GITLAB_CLIENT_ID`  | Public, non-confidential GitLab OAuth application client ID with device authorization and `read_user` scope             |
| `ROVIK_TEAM_GITHUB_TOKEN` | Operator credential permitting repository lookup and collaborator/permission checks for registered repositories         |
| `ROVIK_TEAM_GITLAB_TOKEN` | Operator credential permitting project membership lookup, including inherited membership, on registered GitLab projects |

Each registry entry selects a provider and its numeric repository or project ID. The Rovik UUID is a separate, stable team identifier. Both configured providers can serve projects from the same hub. Set `allow_personal_hosts` only for projects that permit imports into desktop stores that synchronize with personal hosts. The hub denies desktop access when this permission is absent.

```json
[
  {
    "project": "16a2bb26-b9ff-46a7-a6e1-1854ffb168e5",
    "provider": "github",
    "repository_id": 12345,
    "allow_personal_hosts": true
  },
  {
    "project": "633f20bc-87b5-42e7-80b9-194fc889638b",
    "provider": "gitlab",
    "repository_id": 67890,
    "allow_personal_hosts": true
  }
]
```

Provider sign-in targets github.com and gitlab.com. The public client IDs may also be configured when building or launching the desktop for direct account login. Membership checks run server-side with the operator credential. GitHub read/triage and GitLab reporter can pull. GitHub write/maintain and GitLab developer/maintainer can publish. GitHub admin and GitLab owner can clear the team project. A grant binds the project, immutable provider account ID and device replica. Successful membership results are cached for up to five minutes. The project registry and global session are checked on each authenticated request. Hosted sessions expire after seven days.

## Operate and recover

- **Back up.** Use SQLite’s online backup facility for each project database and `devices.db`, or stop the service before taking a consistent filesystem snapshot. Save the registry and deployment configuration separately. Set the backup interval and retention with the team. Restrict backup access to the same operators who can read project stores.
- **Restore.** Stop the service, retain a private copy of the current data, restore the selected project DB, and run `rovik-team-memory rotate-restored-project PROJECT_UUID` with the same data directory. Restart the service. Clients detect the changed identity and can reconcile. Restore the identity database separately. Revoke restored sessions before resuming access if the backup predates revocation.
- **Revoke.** Remove repository membership at the provider. To revoke a whole project immediately, remove its registry entry. To invalidate a hosted session immediately, remove its row from the hub’s `sessions` table. Device bindings are stored in the `replicas` table.
- **Clear.** An admin obtains the current hub identity and epoch by pulling. Send an authenticated exchange with `hub`, `after: 0`, `push: null` and `clear` containing the observed epoch and a fresh operation UUID. Retry with the same operation UUID. The server records the principal and rejects a stale epoch. A clear affects shared material. Each client preserves private and divergent personal records.
- **Purge.** After withdrawal or clear, an admin can POST `{"hub":"HUB_REPLICA","epoch":CURRENT_EPOCH}` to `/v1/projects/PROJECT_UUID/records/TEAM_RECORD_UUID/purge` with the same account and device headers as exchange. This removes stored revision and claim text and records a permanent receipt that prevents replay. Checkpoint and truncate the SQLite WAL during maintenance and expire backups under the agreed retention period.
- **Observe.** The hub emits JSON exchange events on standard error with project UUID, requested and returned cursors, outstanding-page flag, push count and rejection count. Aggregate those fields per project to identify backlogs and failed publications.
- **Retention.** A withdraw or clear propagates to replicas. Revision and claim history, backups and offline devices have separate retention lifetimes. Coordinate deletion from those stores with the team’s operator. Before reusing a project identifier, account for every previously linked device.
- **Pilot.** Use two test desktops, one personal host, and a non-production project. Approve a harmless note, edit offline, withdraw another note, revoke one member, and exercise backup/restore before inviting developers. Measure actual propagation latency, rate-limit responses and snapshot duration at the team’s expected concurrency.
