//! Files the agent may not read or change in a project.
//!
//! What the agent reads goes to its model's provider and stays in the
//! chat's history: a secret it reads has left the computer. And a file of
//! credentials it can't see, it could only overwrite. So such files are
//! kept from it, and a call that would read or change one is refused with
//! the reason (agent_run/restricted.rs checks the calls):
//!
//! - By default, the files whose names say they hold credentials
//!   ([`looks_secret`]): `.env` files but their templates (`.env.example`),
//!   private keys, package registries' and clouds' logins, Terraform's
//!   variables and state. Reading them, whether they exist or not (git
//!   keeps a deleted one); changing them once they exist: a new one has
//!   nothing in it yet, so the agent may create one.
//! - The user's own choices, per file or folder, kept with the project's
//!   settings under [`SETTINGS_KEY`] (Settings → Restricted files, which
//!   lists what [`scan`] finds): reading and writing, each blocked or not,
//!   whatever the name. The entry for the file itself wins over one for a
//!   folder it's in.

use crate::agent_run::{paths, repair, tools};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Where the user's list is kept in a project's settings
/// (`project_profile::load_project_settings`).
pub const SETTINGS_KEY: &str = "restrictedFiles";

/// Where the user changes what's restricted, as the refusals name it.
pub const SETTINGS_PAGE: &str = "Settings → Restricted files";

/// Files looked at, at most, by one [`scan`]: a huge project's scan ends
/// with what it found by then.
const SCAN_MAX_ENTRIES: usize = 100_000;

/// Folders whose every file is a login: SSH's, AWS's, GnuPG's, kubectl's.
const SECRET_FOLDERS: &[&str] = &[".ssh", ".aws", ".gnupg", ".kube"];

/// Files of environment variables: the name itself, and with a suffix
/// (`.env.local`, `.env.production`, `.dev.vars.staging`). Wrangler's
/// `.dev.vars` is Cloudflare's `.env`.
const ENV_FILES: &[&str] = &[".env", ".dev.vars"];

/// Files named so hold logins or keys: direnv's variables, package
/// registries', git's, databases', web servers', Docker's, S3 tools',
/// Vault's, Terraform Cloud's, kubectl's, the act runner's, Azure
/// Functions' local settings, SSH's private keys.
const SECRET_NAMES: &[&str] = &[
    ".envrc",
    ".npmrc",
    ".pypirc",
    ".netrc",
    "_netrc",
    ".git-credentials",
    ".pgpass",
    ".my.cnf",
    ".htpasswd",
    ".dockercfg",
    ".s3cfg",
    ".boto",
    ".vault-token",
    ".terraformrc",
    "terraform.rc",
    "kubeconfig",
    ".secrets",
    "local.settings.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "id_ecdsa_sk",
    "id_ed25519_sk",
];

/// Extensions of private keys, certificate stores and keystores, and of
/// kubectl's configurations.
const SECRET_EXTENSIONS: &[&str] = &[
    "pem",
    "key",
    "pfx",
    "p12",
    "jks",
    "keystore",
    "ppk",
    "kubeconfig",
];

/// Terraform's variables (`prod.tfvars`, `x.tfvars.json`) and its state,
/// which holds every secret it manages (`terraform.tfstate.backup` too).
const TERRAFORM_PARTS: &[&str] = &[".tfvars", ".tfstate"];

/// Names of data files of secrets, before their extension (or alone):
/// `credentials`, `credentials.json`, `secrets.yaml`, `secret.txt`.
const SECRET_STEMS: &[&str] = &["credentials", "secrets", "secret"];
const DATA_EXTENSIONS: &[&str] = &[
    "json",
    "yaml",
    "yml",
    "toml",
    "ini",
    "env",
    "txt",
    "xml",
    "properties",
    "conf",
    "cfg",
];

/// Parts of the names of JSON keys from Google Cloud and Firebase: an OAuth
/// client's secret, a service account's key.
const SECRET_JSON_PARTS: &[&str] = &[
    "client_secret",
    "service-account",
    "service_account",
    "serviceaccount",
    "firebase-adminsdk",
];

/// Words that make a file a template, committed to show which values to
/// set: `.env.example`, `example.env`, `secrets.sample.yaml`, `.env.dist`.
const TEMPLATE_WORDS: &[&str] = &[
    "example", "sample", "template", "tpl", "tmpl", "dist", "defaults", "schema",
];

/// Whether `path`'s name says the file holds credentials: one of the files
/// above, or anything in a folder of logins (`.ssh/`). By name alone, in
/// any case: whether it exists is the caller's to know.
pub fn looks_secret(path: &str) -> bool {
    let path = path.replace('\\', "/");
    let mut parts: Vec<String> = path
        .split('/')
        .filter(|p| !p.is_empty())
        .map(str::to_lowercase)
        .collect();
    let Some(name) = parts.pop() else {
        return false;
    };
    parts
        .iter()
        .any(|folder| SECRET_FOLDERS.contains(&folder.as_str()))
        || secret_name(&name)
}

/// [`looks_secret`] for a file's name, in lower case.
fn secret_name(name: &str) -> bool {
    let words: Vec<&str> = name.split('.').collect();
    if words.iter().any(|w| TEMPLATE_WORDS.contains(w)) {
        return false;
    }
    let stem = words[0];
    let extension = if words.len() > 1 {
        words[words.len() - 1]
    } else {
        ""
    };
    ENV_FILES
        .iter()
        .any(|env| name == *env || name.starts_with(&format!("{env}.")))
        || name.ends_with(".env")
        || SECRET_NAMES.contains(&name)
        || SECRET_EXTENSIONS.contains(&extension)
        || TERRAFORM_PARTS.iter().any(|part| name.contains(part))
        || (SECRET_STEMS.contains(&stem)
            && (words.len() == 1 || DATA_EXTENSIONS.contains(&extension)))
        || (extension == "json" && SECRET_JSON_PARTS.iter().any(|part| stem.contains(part)))
}

/// What the agent would do with a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Read,
    Write,
}

fn blocked_by_default() -> bool {
    true
}

/// One entry of the user's list: a file or a folder in the project, and
/// what the agent may not do with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestrictedFile {
    /// Relative to the project, with `/`. A folder's entry is for the files
    /// in it too.
    pub path: String,
    /// The agent may not read it. Left out (a hand-written entry), it may not.
    #[serde(default = "blocked_by_default")]
    pub block_read: bool,
    /// The agent may not change it. Left out, it may not.
    #[serde(default = "blocked_by_default")]
    pub block_write: bool,
}

impl RestrictedFile {
    fn blocks(&self, op: Op) -> bool {
        match op {
            Op::Read => self.block_read,
            Op::Write => self.block_write,
        }
    }
}

/// A project's restricted files: the user's list, and the files that look
/// secret ([`looks_secret`]) where it says nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RestrictedFiles {
    listed: Vec<RestrictedFile>,
}

impl RestrictedFiles {
    /// From a project's settings, as the frontend stored them: its list,
    /// without the entries that don't parse or name a path outside the
    /// project.
    pub fn from_settings(settings: &serde_json::Value) -> RestrictedFiles {
        let listed = settings
            .get(SETTINGS_KEY)
            .and_then(serde_json::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| serde_json::from_value::<RestrictedFile>(e.clone()).ok())
                    .filter_map(|mut e| {
                        e.path = paths::norm_rel(&e.path).filter(|p| !p.is_empty())?;
                        Some(e)
                    })
                    .collect()
            })
            .unwrap_or_default();
        RestrictedFiles { listed }
    }

    /// The project's, as saved under `settings_root` (the project's folder,
    /// not a chat's view of it) in `config_dir`.
    pub fn load(config_dir: &Path, settings_root: &str) -> RestrictedFiles {
        let settings = crate::project_profile::load_project_settings(config_dir, settings_root);
        RestrictedFiles::from_settings(&settings)
    }

    /// The user's entry that decides for `key` (project-relative): the one
    /// for the file itself, else for the nearest folder it's in.
    fn entry_for(&self, key: &str) -> Option<&RestrictedFile> {
        self.listed
            .iter()
            .filter(|e| paths::under(&e.path, key))
            .max_by_key(|e| e.path.len())
    }

    /// Whether the file `key` (project-relative, or absolute outside the
    /// project), at `abs`, is kept from the agent for `op`. A folder whose
    /// name looks secret isn't (`.env/`, a virtual environment).
    fn blocks(&self, key: &str, abs: &Path, op: Op) -> bool {
        match self.entry_for(key) {
            Some(entry) => entry.blocks(op),
            None => {
                looks_secret(key)
                    && match op {
                        Op::Read => !abs.is_dir(),
                        Op::Write => abs.is_file(),
                    }
            }
        }
    }

    /// Whether `rel`, a file a walk of the project found, is kept from the
    /// agent for `op`.
    pub fn blocks_file(&self, rel: &str, op: Op) -> bool {
        match self.entry_for(rel) {
            Some(entry) => entry.blocks(op),
            None => looks_secret(rel),
        }
    }

    /// The name to refuse `op` on `path` by, when the agent may not: `path`
    /// as a tool call or a command names it (relative to `root`, or
    /// absolute), or where its links lead (a link to `.env` is `.env`).
    /// Project-relative inside the project. None: the agent may.
    pub fn blocked(&self, root: &str, path: &str, op: Op) -> Option<String> {
        let path = path.trim();
        if path.is_empty() {
            return None;
        }
        let key = repair::rel_key(root, path);
        let abs = std::path::PathBuf::from(tools::to_absolute_path(path, root));
        if self.blocks(&key, &abs, op) {
            return Some(key);
        }
        let real = match repair::real_key(root, path) {
            Some(real) => real,
            // Outside the project once its links are followed: its name there.
            None => abs.canonicalize().ok()?.to_string_lossy().into_owned(),
        };
        (real != key && self.blocks(&real, &abs, op)).then_some(real)
    }
}

/// The files a connector's harness may not read and may not change with
/// its own tools, which it's told as its own rules: relative to the
/// project, with `/`; a folder's ends with `/`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Denied {
    pub read: Vec<String>,
    pub write: Vec<String>,
}

impl RestrictedFiles {
    /// [`Denied`] for the project in `root`: the files found that look
    /// secret ([`scan`]), and the user's entries, as far as they keep the
    /// agent out. A folder's entry is for all in it, a file in it the user
    /// lets the agent read included: a harness's rules can't make an
    /// exception.
    pub fn denied(&self, root: &Path) -> Denied {
        let mut denied = Denied::default();
        let mut add = |path: String, read: bool, write: bool| {
            if read && !denied.read.contains(&path) {
                denied.read.push(path.clone());
            }
            if write && !denied.write.contains(&path) {
                denied.write.push(path);
            }
        };
        for path in scan(root) {
            let (read, write) = (
                self.blocks_file(&path, Op::Read),
                self.blocks_file(&path, Op::Write),
            );
            add(path, read, write);
        }
        for entry in &self.listed {
            let path = if root.join(&entry.path).is_dir() {
                format!("{}/", entry.path)
            } else {
                entry.path.clone()
            };
            add(path, entry.block_read, entry.block_write);
        }
        denied
    }
}

/// One line of the settings page's table.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    /// Relative to the project, with `/`.
    pub path: String,
    pub block_read: bool,
    pub block_write: bool,
    /// Its name looks secret, and it's in the project.
    pub detected: bool,
    /// The user's list has an entry for it: the user may remove it.
    pub listed: bool,
}

/// The settings page's table for the project in `root`: the files found
/// whose names look secret, and the user's entries, each with what applies
/// to it, by path.
pub fn rows(root: &Path, restricted: &RestrictedFiles) -> Vec<Row> {
    let listed = |path: &str| restricted.listed.iter().any(|e| paths::same(&e.path, path));
    let mut rows: Vec<Row> = scan(root)
        .into_iter()
        .map(|path| {
            let entry = restricted.entry_for(&path);
            Row {
                block_read: entry.is_none_or(|e| e.block_read),
                block_write: entry.is_none_or(|e| e.block_write),
                detected: true,
                listed: listed(&path),
                path,
            }
        })
        .collect();
    for entry in &restricted.listed {
        if !rows.iter().any(|r| paths::same(&r.path, &entry.path)) {
            rows.push(Row {
                path: entry.path.clone(),
                block_read: entry.block_read,
                block_write: entry.block_write,
                detected: false,
                listed: true,
            });
        }
    }
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    rows
}

/// The project's files whose names look secret, relative to `root` with
/// `/`, sorted. Hidden and ignored files too: `.env` files are usually
/// both. Not in the folders a walk for them leaves out ([`skipped_folder`]),
/// but in build output, which may hold a copy (`.next/standalone/.env`).
pub fn scan(root: &Path) -> Vec<String> {
    let walk = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .filter_entry(|e| {
            let dir = e.file_type().is_some_and(|t| t.is_dir());
            !(dir && skipped_folder(&e.file_name().to_string_lossy()))
        })
        .build();
    let mut found: Vec<String> = walk
        .take(SCAN_MAX_ENTRIES)
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| {
            let rel = e.path().strip_prefix(root).ok()?;
            Some(rel.to_string_lossy().replace('\\', "/"))
        })
        .filter(|rel| looks_secret(rel))
        .collect();
    found.sort();
    found
}

/// Folders of dependencies and caches, and version control's own: never
/// where the user keeps credentials, and often huge.
const SKIPPED_FOLDERS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "bower_components",
    "vendor",
    "target",
    "__pycache__",
    ".venv",
    "site-packages",
    "dist-packages",
    "Pods",
    ".gradle",
    ".tox",
    ".cache",
];

/// Whether a walk for restricted files leaves out a folder, by its name
/// ([`SKIPPED_FOLDERS`]).
pub(crate) fn skipped_folder(name: &str) -> bool {
    SKIPPED_FOLDERS.contains(&name)
}

impl RestrictedFiles {
    /// Whether a walk for restricted files leaves out `folder`, of the
    /// project in `root`: a [`skipped_folder`], unless the user's list
    /// names it or something in it.
    pub(crate) fn skips(&self, root: &Path, folder: &Path) -> bool {
        let name = folder.file_name().map(|n| n.to_string_lossy());
        if !name.is_some_and(|name| skipped_folder(&name)) {
            return false;
        }
        let Ok(rel) = folder.strip_prefix(root) else {
            return true;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        !self.listed.iter().any(|e| paths::under(&rel, &e.path))
    }
}

/// Why the agent may not do `op` with a restricted file, and what it
/// should do instead: the end of every refusal for one.
pub fn reason(op: Op) -> String {
    match op {
        Op::Read => format!(
            "the agent may not read it, since it may hold credentials (API keys, passwords, tokens) or other secrets the user keeps from the model. Don't try to read it another way, with a command or a search. If you need something from it, ask the user; they can allow reading it in {SETTINGS_PAGE}."
        ),
        Op::Write => format!(
            "the agent may not change it, since it may hold credentials or other secrets the user keeps there. Don't try to change it another way, with a command. Tell the user what to change in it instead; they can allow changing it in {SETTINGS_PAGE}."
        ),
    }
}

/// The refusal of a tool call that would do `op` with `path`, a
/// restricted file.
pub fn refusal(op: Op, path: &str) -> String {
    format!("Error: {path} is a restricted file: {}", reason(op))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_dir;
    use serde_json::json;
    use std::fs;

    fn project(files: &[&str]) -> crate::testing::TempDir {
        let dir = temp_dir();
        for file in files {
            let path = dir.0.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "X=1\n").unwrap();
        }
        dir
    }

    fn root(dir: &crate::testing::TempDir) -> String {
        dir.0.to_string_lossy().into_owned()
    }

    #[test]
    fn env_files_keys_and_logins_look_secret_by_their_names() {
        for path in [
            ".env",
            ".env.local",
            ".env.production",
            ".env.development.local",
            "apps/web/.env.test",
            "config/production.env",
            "docker.env",
            ".envrc",
            ".dev.vars",
            ".dev.vars.staging",
            ".npmrc",
            ".pypirc",
            ".netrc",
            ".git-credentials",
            "config/master.key",
            "certs/server.pem",
            "deploy/KEY.PEM",
            "signing/release.jks",
            "id_rsa",
            "keys/id_ed25519",
            "credentials",
            ".aws/credentials",
            ".aws/config",
            "home/.ssh/config",
            "config/credentials.json",
            "config/secrets.yml",
            "secrets.production.yaml",
            "client_secret_123.apps.googleusercontent.com.json",
            "my-app-firebase-adminsdk-x1.json",
            "serviceAccountKey.json",
            "terraform.tfvars",
            "envs/prod.tfvars.json",
            "terraform.tfstate",
            "terraform.tfstate.backup",
            "local.settings.json",
            "C:\\Users\\me\\.aws\\config",
        ] {
            assert!(looks_secret(path), "{path}");
        }
    }

    #[test]
    fn templates_code_and_public_keys_dont_look_secret() {
        for path in [
            ".env.example",
            ".env.local.example",
            ".env.sample",
            ".env.template",
            ".env.dist",
            "example.env",
            ".dev.vars.example",
            "config/secrets.example.yml",
            "credentials.sample.json",
            "terraform.tfvars.example",
            "id_rsa.pub",
            "certs/ca.crt",
            "src/environment.ts",
            "src/env.rs",
            "src/credentials.ts",
            "app/secrets.py",
            "docs/credentials.md",
            "config/credentials.yml.enc",
            "src/keyboard.ts",
            ".env/bin/activate",
            "package.json",
            "README.md",
            "",
        ] {
            assert!(!looks_secret(path), "{path}");
        }
    }

    #[test]
    fn a_file_that_looks_secret_is_kept_from_the_agent_both_ways() {
        let dir = project(&[".env", "config/master.key", "src/app.ts"]);
        let (root, none) = (root(&dir), RestrictedFiles::default());
        for op in [Op::Read, Op::Write] {
            assert_eq!(none.blocked(&root, ".env", op).as_deref(), Some(".env"));
            assert_eq!(
                none.blocked(&root, "./config//master.key", op).as_deref(),
                Some("config/master.key")
            );
            assert_eq!(none.blocked(&root, "src/app.ts", op), None);
        }
        let absolute = dir.0.join(".env").to_string_lossy().into_owned();
        assert_eq!(
            none.blocked(&root, &absolute, Op::Read).as_deref(),
            Some(".env")
        );
    }

    #[test]
    fn a_new_secret_file_may_be_created_but_not_read() {
        let dir = project(&[".env/bin/activate"]);
        let (root, none) = (root(&dir), RestrictedFiles::default());
        // Nothing in it yet to lose; git may still hold a deleted one.
        assert_eq!(none.blocked(&root, ".env.local", Op::Write), None);
        assert_eq!(
            none.blocked(&root, ".env.local", Op::Read).as_deref(),
            Some(".env.local")
        );
        // A virtual environment called `.env` is a folder, not a secret.
        assert_eq!(none.blocked(&root, ".env", Op::Read), None);
        assert_eq!(none.blocked(&root, ".env/bin/activate", Op::Read), None);
    }

    #[test]
    fn the_users_entries_win_over_names_and_the_nearest_one_decides() {
        let dir = project(&[
            ".env",
            ".env.local",
            "docs/notes.txt",
            "private/a.txt",
            "private/open.txt",
        ]);
        let restricted = RestrictedFiles::from_settings(&json!({
            "checks": [],
            "restrictedFiles": [
                // Allowed: the user unticked both.
                { "path": ".env", "blockRead": false, "blockWrite": false },
                // Read-only for the agent.
                { "path": "docs/notes.txt", "blockRead": false, "blockWrite": true },
                // A folder, and a file in it that's let through.
                { "path": "private/", "blockRead": true, "blockWrite": true },
                { "path": "private/open.txt", "blockRead": false, "blockWrite": false },
                // Not yet there: listed, so kept even before it exists.
                { "path": "config/prod.json", "blockRead": true, "blockWrite": true },
            ]
        }));
        let root = root(&dir);
        let blocked = |path: &str, op| restricted.blocked(&root, path, op);
        assert_eq!(blocked(".env", Op::Read), None);
        assert_eq!(blocked(".env", Op::Write), None);
        assert_eq!(
            blocked(".env.local", Op::Read).as_deref(),
            Some(".env.local")
        );
        assert_eq!(blocked("docs/notes.txt", Op::Read), None);
        assert_eq!(
            blocked("docs/notes.txt", Op::Write).as_deref(),
            Some("docs/notes.txt")
        );
        assert_eq!(
            blocked("private/a.txt", Op::Read).as_deref(),
            Some("private/a.txt")
        );
        assert_eq!(blocked("private/open.txt", Op::Write), None);
        assert_eq!(
            blocked("config/prod.json", Op::Write).as_deref(),
            Some("config/prod.json")
        );
        assert!(!restricted.blocks_file(".env", Op::Read));
        assert!(restricted.blocks_file(".env.local", Op::Read));
        assert!(restricted.blocks_file("private/a.txt", Op::Write));
    }

    #[test]
    fn entries_that_dont_parse_or_leave_the_project_are_left_out() {
        let restricted = RestrictedFiles::from_settings(&json!({
            "restrictedFiles": [
                { "path": "./config\\a.json" },
                { "path": "../outside.txt", "blockRead": true },
                { "path": "/etc/passwd", "blockRead": true },
                { "path": "", "blockRead": true },
                { "blockRead": true },
                "not an entry",
            ]
        }));
        assert_eq!(
            restricted.listed,
            vec![RestrictedFile {
                path: "config/a.json".into(),
                block_read: true,
                block_write: true,
            }],
            "a hand-written entry without its flags blocks both"
        );
        assert_eq!(
            RestrictedFiles::from_settings(&serde_json::Value::Null),
            RestrictedFiles::default()
        );
    }

    #[test]
    fn a_file_outside_the_project_is_judged_by_its_name() {
        let outside = project(&["other/.env", "other/readme.txt"]);
        let inside = project(&["src/a.ts"]);
        let none = RestrictedFiles::default();
        let env = outside.0.join("other/.env").to_string_lossy().into_owned();
        let readme = outside
            .0
            .join("other/readme.txt")
            .to_string_lossy()
            .into_owned();
        assert!(none.blocked(&root(&inside), &env, Op::Read).is_some());
        assert_eq!(none.blocked(&root(&inside), &readme, Op::Read), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_restricted_file_is_that_file() {
        let dir = project(&[".env"]);
        std::os::unix::fs::symlink(dir.0.join(".env"), dir.0.join("settings.txt")).unwrap();
        let none = RestrictedFiles::default();
        assert_eq!(
            none.blocked(&root(&dir), "settings.txt", Op::Read)
                .as_deref(),
            Some(".env")
        );
    }

    #[test]
    fn the_scan_finds_hidden_and_ignored_secrets_but_not_dependencies_or_templates() {
        let dir = project(&[
            ".env",
            ".env.example",
            "apps/api/.env.local",
            "config/master.key",
            "src/main.rs",
            "node_modules/pkg/.env",
            "target/debug/.env",
            "venv/lib/python3.12/site-packages/certifi/cacert.pem",
            ".git/config",
            // A build's copy is one more place the secret is.
            ".next/standalone/.env",
        ]);
        fs::write(dir.0.join(".gitignore"), ".env*\nconfig/\n").unwrap();
        assert_eq!(
            scan(&dir.0),
            [
                ".env",
                ".next/standalone/.env",
                "apps/api/.env.local",
                "config/master.key"
            ]
        );
    }

    #[test]
    fn rows_show_what_applies_to_each_file_found_and_listed() {
        let dir = project(&[".env", ".env.local", "config/app.json"]);
        let restricted = RestrictedFiles::from_settings(&json!({
            "restrictedFiles": [
                { "path": ".env.local", "blockRead": false, "blockWrite": true },
                { "path": "config/app.json", "blockRead": true, "blockWrite": false },
                { "path": "later.txt", "blockRead": true, "blockWrite": true },
            ]
        }));
        let row = |path: &str, read, write, detected, listed| Row {
            path: path.into(),
            block_read: read,
            block_write: write,
            detected,
            listed,
        };
        assert_eq!(
            rows(&dir.0, &restricted),
            vec![
                row(".env", true, true, true, false),
                row(".env.local", false, true, true, true),
                row("config/app.json", true, false, false, true),
                row("later.txt", true, true, false, true),
            ]
        );
    }

    #[test]
    fn a_harness_is_denied_what_the_agent_may_not_read_or_change() {
        let dir = project(&[".env", ".env.local", "config/master.key", "private/a.txt"]);
        let restricted = RestrictedFiles::from_settings(&json!({
            "restrictedFiles": [
                { "path": ".env.local", "blockRead": false, "blockWrite": true },
                { "path": "private", "blockRead": true, "blockWrite": false },
                { "path": "later.json", "blockRead": true, "blockWrite": true },
            ]
        }));
        assert_eq!(
            restricted.denied(&dir.0),
            Denied {
                read: vec![
                    ".env".into(),
                    "config/master.key".into(),
                    "private/".into(),
                    "later.json".into(),
                ],
                write: vec![
                    ".env".into(),
                    ".env.local".into(),
                    "config/master.key".into(),
                    "later.json".into(),
                ],
            }
        );
    }

    #[test]
    fn refusals_say_why_and_where_the_user_changes_it() {
        let read = refusal(Op::Read, ".env");
        assert!(read.starts_with("Error: .env is a restricted file: the agent may not read it,"));
        assert!(read.contains("ask the user") && read.contains(SETTINGS_PAGE));
        let write = refusal(Op::Write, ".env");
        assert!(write.starts_with("Error: .env is a restricted file: the agent may not change it,"));
        assert!(write.contains(SETTINGS_PAGE));
    }
}
