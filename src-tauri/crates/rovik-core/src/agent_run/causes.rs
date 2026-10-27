//! The repair loop's grouping (docs/design/subagents-plan.md, R03): a
//! round's failures, as keys (command_output.rs `failure_keys`), grouped
//! into root causes and each cause given to whoever fixes it: the worker
//! whose write scope holds it, the lead (across scopes, the contract, or
//! files no worker owns), or the lead under the rule for changing tests
//! (a test the test writer wrote).
//!
//! One root cause usually breaks many places: a changed signature fails
//! at twenty call sites. So failures with the same check, code and
//! message are one cause wherever they point, and a triage's root causes
//! (C03), when one ran this round, come before any guess from locations.

use super::triage::Triage;
use super::worker::WriteScope;
use crate::folders::OutputRoots;
use serde::Serialize;

/// Who fixes a cause.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Owner {
    /// Entirely inside one worker's scope: that worker, resumed.
    Worker { label: String, step: usize },
    /// The lead: `reason` is contract, across scopes, unowned or unlocated.
    Lead { reason: LeadReason },
    /// A test the test writer wrote: the lead, under the rule for
    /// changing tests.
    TestRule { file: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LeadReason {
    /// In the group's contract, or the same failure in several workers'
    /// files (they share the contract).
    Contract,
    /// In several workers' files, with no contract between them.
    AcrossScopes,
    /// In files no worker owns: a seam.
    Unowned,
    /// The failure names no file (a test, a crash, a timeout).
    Unlocated,
}

impl Owner {
    pub fn is_worker(&self) -> bool {
        matches!(self, Owner::Worker { .. })
    }

    /// "worker w2 (step 3)", "the lead (contract)".
    pub fn describe(&self) -> String {
        match self {
            Owner::Worker { label, step } => format!("worker {label} (step {step})"),
            Owner::Lead { reason } => format!(
                "the lead ({})",
                match reason {
                    LeadReason::Contract => "the contract",
                    LeadReason::AcrossScopes => "across workers' files",
                    LeadReason::Unowned => "a seam: no worker owns it",
                    LeadReason::Unlocated => "no file named",
                }
            ),
            Owner::TestRule { file } => {
                format!("the lead, under the rule for changing tests ({file})")
            }
        }
    }
}

/// One root cause and the failures it explains.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cause {
    pub owner: Owner,
    /// The failure keys it explains.
    pub keys: Vec<String>,
    /// The files they point at (or triage named).
    pub files: Vec<String>,
    /// What it is: triage's diagnosis, or the commonest message.
    pub summary: String,
    /// high, medium or low, when triage rated it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
}

impl Cause {
    pub fn count(&self) -> usize {
        self.keys.len()
    }
}

/// Who owns what, for assigning causes.
pub struct Owners<'a> {
    /// The group's workers: label, step, write scope.
    pub workers: &'a [(String, usize, WriteScope)],
    /// Files the group's contract settles.
    pub contract: &'a [String],
    /// Files the test writer wrote.
    pub test_files: &'a [String],
    /// Whether the run created a file (it wasn't there before).
    pub created: &'a dyn Fn(&str) -> bool,
    /// How the output's absolute paths read as the chat's (its view, its
    /// folders), for `resolve`.
    pub roots: &'a OutputRoots,
    /// Project-relative files the run knows (changed, in a scope): a path
    /// relative to a subfolder is the one of them it ends.
    pub known: &'a [String],
}

/// One failure key, read back.
#[derive(Debug, Clone, PartialEq)]
struct Failure {
    key: String,
    check: String,
    file: Option<String>,
    /// "code|message" for an error, "test|name" for a test.
    what: String,
}

fn parse_key(key: &str) -> Failure {
    // A test's name is the rest of the key, `|` and all.
    if let [check, "test", name] = key.splitn(3, '|').collect::<Vec<_>>()[..] {
        return Failure {
            key: key.into(),
            check: check.to_string(),
            file: None,
            what: format!("test|{name}"),
        };
    }
    let parts: Vec<&str> = key.splitn(4, '|').collect();
    let check = parts[0].to_string();
    match parts.as_slice() {
        [_, file, code, msg] => Failure {
            key: key.into(),
            check,
            file: (!file.is_empty()).then(|| file.to_string()),
            what: format!("{code}|{msg}"),
        },
        _ => Failure {
            key: key.into(),
            check,
            file: None,
            what: parts[1..].join("|"),
        },
    }
}

fn norm(path: &str) -> String {
    let p = path.trim().replace('\\', "/");
    // `src/a.rs:12:5` → src/a.rs
    super::paths::without_line(p.trim_start_matches("./")).to_string()
}

/// A relative path in its one spelling (`paths::norm_rel`), else as it is.
fn tidy(path: &str) -> String {
    super::paths::norm_rel(path).unwrap_or_else(|| path.to_string())
}

/// A path from a check's output as a chat path: an absolute one through
/// `roots` (`/home/me/app/src/x.ts` is `src/x.ts`), and one relative to a
/// subfolder the check ran in (`cd src-tauri && cargo check` prints
/// `src/x.rs`) as the one known file it's the end of. Otherwise as it is.
pub fn resolve(path: &str, roots: &OutputRoots, known: &[String]) -> String {
    let p = norm(path);
    if let Some(in_chat) = roots.chat_path(&p) {
        return tidy(&in_chat);
    }
    let p = tidy(&p);
    if let Some(k) = known.iter().find(|k| super::paths::same(k, &p)) {
        return k.clone();
    }
    let tail = format!("/{p}");
    let ends: Vec<&String> = known.iter().filter(|k| k.ends_with(&tail)).collect();
    match ends.as_slice() {
        [one] => (*one).clone(),
        _ => p,
    }
}

/// The worker whose scope owns `path`: its exact files and folders first;
/// a file the run created, the one worker whose folder it's in (F06).
pub fn worker_of<'a>(
    path: &str,
    workers: &'a [(String, usize, WriteScope)],
    created: bool,
) -> Option<&'a (String, usize, WriteScope)> {
    let path = norm(path);
    let by = |exists: bool| -> Vec<&'a (String, usize, WriteScope)> {
        workers
            .iter()
            .filter(|(_, _, scope)| scope.allows(&path, exists))
            .collect()
    };
    match by(true).first() {
        Some(o) => Some(*o),
        None if created => {
            let beside = by(false);
            (beside.len() == 1).then(|| beside[0])
        }
        None => None,
    }
}

/// A file's stem ("posts" for tests/posts.test.ts), for matching test names.
fn stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split('.').next().unwrap_or(name).to_lowercase()
}

/// Who fixes failures pointing at `files` (empty: they name none).
fn owner_for(files: &[String], tests: &[String], o: &Owners) -> Owner {
    if let Some(f) = files.iter().find(|f| o.test_files.iter().any(|t| t == *f)) {
        return Owner::TestRule { file: f.clone() };
    }
    if files.is_empty() {
        // A test the test writer wrote, by its name.
        let test = tests.iter().find_map(|name| {
            let name = name.to_lowercase();
            o.test_files
                .iter()
                .find(|t| {
                    let s = stem(t);
                    s.len() > 2 && name.contains(&s)
                })
                .cloned()
        });
        return match test {
            Some(file) => Owner::TestRule { file },
            None => Owner::Lead {
                reason: LeadReason::Unlocated,
            },
        };
    }
    if files.iter().any(|f| o.contract.iter().any(|c| c == f)) {
        return Owner::Lead {
            reason: LeadReason::Contract,
        };
    }
    let mut labels: Vec<(&String, usize)> = Vec::new();
    let mut unowned = false;
    for f in files {
        match worker_of(f, o.workers, (o.created)(f)) {
            Some((label, step, _)) => {
                if !labels.iter().any(|(l, _)| *l == label) {
                    labels.push((label, *step));
                }
            }
            None => unowned = true,
        }
    }
    match (labels.as_slice(), unowned) {
        ([(label, step)], false) => Owner::Worker {
            label: (*label).clone(),
            step: *step,
        },
        ([], _) => Owner::Lead {
            reason: LeadReason::Unowned,
        },
        // Some of it is in no worker's files.
        (_, true) => Owner::Lead {
            reason: LeadReason::Unowned,
        },
        _ => Owner::Lead {
            reason: if o.contract.is_empty() {
                LeadReason::AcrossScopes
            } else {
                LeadReason::Contract
            },
        },
    }
}

/// A group of failures before it has an owner.
struct Cluster {
    failures: Vec<Failure>,
    files: Vec<String>,
    summary: String,
    confidence: Option<String>,
}

fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// "E0061 this function takes 2 arguments … (12 places)".
fn summary_of(failures: &[Failure]) -> String {
    let first = &failures[0];
    let what = match first.what.split_once('|') {
        Some(("test", name)) => format!("test {name} fails"),
        Some(("", msg)) => msg.to_string(),
        Some((code, msg)) => format!("{code} {msg}"),
        None => first.what.clone(),
    };
    let what = what.trim().to_string();
    if failures.len() > 1 {
        format!("{what} ({} failures)", failures.len())
    } else if what.is_empty() {
        format!("{} failed", first.check)
    } else {
        what
    }
}

/// R03: the round's failure keys as root causes, each with its owner,
/// most failures first. `triage`: this round's triage, when one ran.
pub fn group(keys: &[String], owners: &Owners, triage: Option<&Triage>) -> Vec<Cause> {
    let mut failures: Vec<Failure> = keys.iter().map(|k| parse_key(k)).collect();
    let resolved = |p: &str| resolve(p, owners.roots, owners.known);
    for f in &mut failures {
        f.file = f.file.as_deref().map(resolved);
    }
    let mut clusters: Vec<Cluster> = Vec::new();

    // Triage's causes first: each takes the failures in its files, and
    // its root cause takes the rest of the check it read.
    if let Some(t) = triage {
        let mut causes: Vec<(Vec<String>, String, String)> = vec![(
            t.files
                .iter()
                .flat_map(|f| super::triage::paths_in(f))
                .map(|f| resolved(&f))
                .collect(),
            t.root_cause.clone(),
            t.confidence.clone(),
        )];
        for c in &t.other_causes {
            causes.push((
                c.files.iter().map(|f| resolved(f)).collect(),
                c.cause.clone(),
                c.confidence.clone(),
            ));
        }
        let mut taken: Vec<Vec<Failure>> = vec![Vec::new(); causes.len()];
        failures.retain(|f| {
            let at = f.file.as_ref().and_then(|file| {
                causes
                    .iter()
                    .position(|(files, _, _)| files.iter().any(|c| c == file))
            });
            match at {
                Some(i) => {
                    taken[i].push(f.clone());
                    false
                }
                None => true,
            }
        });
        // The rest of the triaged check: its root cause.
        failures.retain(|f| {
            if f.check == t.check {
                taken[0].push(f.clone());
                false
            } else {
                true
            }
        });
        for ((files, summary, confidence), found) in causes.into_iter().zip(taken) {
            if found.is_empty() {
                continue;
            }
            let mut all = files;
            for f in &found {
                if let Some(file) = &f.file {
                    push_unique(&mut all, file.clone());
                }
            }
            clusters.push(Cluster {
                failures: found,
                files: all,
                summary,
                confidence: Some(confidence),
            });
        }
    }

    // The rest: the same check, code and message is one cause wherever it
    // points; a failure naming no file is one of its own.
    let mut by_what: Vec<(String, Vec<Failure>)> = Vec::new();
    for f in failures {
        let what = format!("{}|{}", f.check, f.what);
        match by_what.iter_mut().find(|(w, _)| *w == what) {
            Some((_, list)) => list.push(f),
            None => by_what.push((what, vec![f])),
        }
    }
    for (_, list) in by_what {
        let mut files = Vec::new();
        for f in &list {
            if let Some(file) = &f.file {
                push_unique(&mut files, file.clone());
            }
        }
        clusters.push(Cluster {
            summary: summary_of(&list),
            failures: list,
            files,
            confidence: None,
        });
    }

    // Each its owner; one cause per owner (one fixer each, D11's
    // "at most one fixer per root cause"), most failures first.
    let mut causes: Vec<Cause> = Vec::new();
    for c in clusters {
        let tests: Vec<String> = c
            .failures
            .iter()
            .filter_map(|f| f.what.strip_prefix("test|").map(str::to_string))
            .collect();
        let owner = owner_for(&c.files, &tests, owners);
        let keys: Vec<String> = c.failures.iter().map(|f| f.key.clone()).collect();
        match causes.iter_mut().find(|x| x.owner == owner) {
            Some(x) => {
                x.keys.extend(keys);
                for f in c.files {
                    push_unique(&mut x.files, f);
                }
                x.summary = format!("{}; {}", x.summary, c.summary);
                // Triage's rating holds only for what it rated.
                x.confidence = match (&x.confidence, &c.confidence) {
                    (Some(a), Some(b)) if a == b => Some(a.clone()),
                    _ => None,
                };
            }
            None => causes.push(Cause {
                owner,
                keys,
                files: c.files,
                summary: c.summary,
                confidence: c.confidence,
            }),
        }
    }
    causes.sort_by_key(|c| std::cmp::Reverse(c.count()));
    causes
}

/// V05: the files a round changed (`changed`, project-relative) that its
/// regressions (`broke`, keys) are behind, so a rollback can put back only
/// those: an error's own file when the round changed it, and for a failing
/// test the changed files whose name it carries (`posts` in
/// `posts > lists`, a stem of three letters or more). None when a key
/// names no file the round changed (a compiler's error in a file it didn't
/// touch, a test named after none, "test|failed"): what's behind it isn't
/// known, so everything goes back.
pub fn files_behind(
    broke: &[String],
    changed: &[String],
    roots: &OutputRoots,
) -> Option<Vec<String>> {
    let mut files: Vec<String> = Vec::new();
    for key in broke {
        let failure = parse_key(key);
        let found: Vec<String> = match (&failure.file, failure.what.split_once('|')) {
            (Some(file), _) => {
                let file = resolve(file, roots, changed);
                changed
                    .iter()
                    .filter(|c| super::paths::same(c, &file))
                    .cloned()
                    .collect()
            }
            (None, Some(("test", name))) => {
                let name = name.to_lowercase();
                changed
                    .iter()
                    .filter(|c| {
                        let s = stem(c);
                        s.len() > 2 && name.contains(&s)
                    })
                    .cloned()
                    .collect()
            }
            _ => Vec::new(),
        };
        if found.is_empty() {
            return None;
        }
        for f in found {
            push_unique(&mut files, f);
        }
    }
    files.sort();
    (!files.is_empty()).then_some(files)
}

/// R07: the causes that may be fixed at the same time: two or more, each
/// a different worker's and rated high by triage. Empty otherwise.
pub fn parallel_set(causes: &[Cause]) -> Vec<&Cause> {
    let set: Vec<&Cause> = causes
        .iter()
        .filter(|c| c.owner.is_worker() && c.confidence.as_deref() == Some("high"))
        .collect();
    // Every cause must qualify: one the lead owns waits for nothing.
    if set.len() < 2 || set.len() != causes.len() {
        return Vec::new();
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(files: &[&str]) -> WriteScope {
        let step = super::super::plan::PlanStep {
            number: 0,
            written: None,
            title: String::new(),
            files: files
                .iter()
                .map(|p| super::super::plan::PlanFile {
                    path: p.to_string(),
                    new: false,
                    read_only: false,
                })
                .collect(),
            body: String::new(),
            group: None,
        };
        WriteScope::for_step(&step)
    }

    fn workers() -> Vec<(String, usize, WriteScope)> {
        vec![
            (
                "w1".into(),
                2,
                scope(&["src/feed/list.ts", "src/feed/item.ts"]),
            ),
            (
                "w2".into(),
                3,
                scope(&["src/posts/edit.ts", "src/posts/view.ts"]),
            ),
        ]
    }

    fn not_created(_: &str) -> bool {
        false
    }

    #[test]
    fn twenty_errors_from_one_changed_signature_are_one_cause_for_the_lead() {
        // Each worker owns a folder; the contract declares `loadPost`.
        let w = vec![
            ("w1".to_string(), 2, scope(&["src/feed/"])),
            ("w2".to_string(), 3, scope(&["src/posts/"])),
        ];
        let contract = vec!["src/api.ts".to_string()];
        let owners = Owners {
            workers: &w,
            contract: &contract,
            test_files: &[],
            created: &not_created,
            roots: &OutputRoots::default(),
            known: &[],
        };
        // `loadPost` gained an argument: twenty call sites, in both folders.
        let keys: Vec<String> = (0..20)
            .map(|i| {
                let dir = if i % 2 == 0 { "feed" } else { "posts" };
                format!("typecheck|src/{dir}/use{i}.ts|TS2554|Expected 2 arguments, but got 1.")
            })
            .collect();
        let causes = group(&keys, &owners, None);
        assert_eq!(causes.len(), 1, "{causes:?}");
        assert_eq!(
            causes[0].owner,
            Owner::Lead {
                reason: LeadReason::Contract
            }
        );
        assert_eq!(causes[0].count(), 20);
        assert!(causes[0].summary.starts_with("TS2554 Expected 2 arguments"));
    }

    /// A workspace chat whose folders are named apart from their last
    /// parts (`/w/x/app` is `app`, `/w/y/app` `y-app`).
    fn two_apps() -> OutputRoots {
        let folder = |name: &str, path: &str| crate::folders::Folder {
            name: name.into(),
            path: path.into(),
        };
        crate::folders::Folders::named(vec![folder("app", "/w/x/app"), folder("y-app", "/w/y/app")])
            .unwrap()
            .output_roots(std::path::Path::new("/run/chats/c1"))
    }

    /// A failure printed by a folder's real path, or by its place in the
    /// chat's view, is the chat's file by its folder's name: a cause's and
    /// the files behind a regression (V05).
    #[test]
    fn a_folders_real_path_reads_as_its_name() {
        let roots = two_apps();
        let changed = vec!["app/src/b.ts".to_string(), "y-app/src/b.ts".to_string()];
        let r = |p: &str| resolve(p, &roots, &changed);
        assert_eq!(r("/w/y/app/src/b.ts:3:1"), "y-app/src/b.ts");
        assert_eq!(r("/run/chats/c1/app/src/b.ts"), "app/src/b.ts");
        let key = "typecheck|/w/y/app/src/b.ts|TS2322|m".to_string();
        assert_eq!(
            files_behind(&[key], &changed, &roots),
            Some(vec!["y-app/src/b.ts".to_string()])
        );
    }

    #[test]
    fn absolute_and_subfolder_paths_are_read_as_the_projects() {
        let roots = crate::folders::Folders::one("/home/u/proj".into())
            .output_roots(std::path::Path::new("/run/chats/abc"));
        let known = vec![
            "src-tauri/src/posts.rs".to_string(),
            "src/posts.ts".to_string(),
            "src-tauri/src/lib.rs".to_string(),
            "crates/a/src/lib.rs".to_string(),
        ];
        let r = |p: &str| resolve(p, &roots, &known);
        assert_eq!(r("/home/u/proj/src/posts.ts:4:2"), "src/posts.ts");
        assert_eq!(
            r("/run/chats/abc/src-tauri/src/posts.rs"),
            "src-tauri/src/posts.rs"
        );
        // `cd src-tauri && cargo check` prints paths from there.
        assert_eq!(r("src/posts.rs"), "src-tauri/src/posts.rs");
        // Known as it is: kept.
        assert_eq!(r("src/posts.ts"), "src/posts.ts");
        // Two files end that way: can't tell, kept.
        assert_eq!(r("src/lib.rs"), "src/lib.rs");

        let w = vec![("w2".to_string(), 3, scope(&["src-tauri/src/posts.rs"]))];
        let owners = Owners {
            workers: &w,
            contract: &[],
            test_files: &[],
            created: &not_created,
            roots: &roots,
            known: &known,
        };
        let keys = vec![
            "lint|/home/u/proj/src-tauri/src/posts.rs|no-unused|x".to_string(),
            "test|src/posts.rs|E0425|y".to_string(),
        ];
        let causes = group(&keys, &owners, None);
        assert_eq!(causes.len(), 1, "{causes:?}");
        assert!(causes[0].owner.is_worker(), "{causes:?}");
    }

    #[test]
    fn errors_only_in_one_workers_files_are_that_workers() {
        let w = workers();
        let owners = Owners {
            workers: &w,
            contract: &[],
            test_files: &[],
            created: &not_created,
            roots: &OutputRoots::default(),
            known: &[],
        };
        let keys = vec![
            "typecheck|src/posts/edit.ts|TS2322|Type 'string' is not assignable to type 'number'."
                .to_string(),
            "typecheck|src/posts/view.ts|TS2339|Property 'title' does not exist on type 'Post'."
                .to_string(),
            "lint|src/posts/view.ts|no-unused-vars|'x' is defined but never used.".to_string(),
        ];
        let causes = group(&keys, &owners, None);
        assert_eq!(causes.len(), 1, "{causes:?}");
        assert_eq!(
            causes[0].owner,
            Owner::Worker {
                label: "w2".into(),
                step: 3
            }
        );
        assert_eq!(causes[0].count(), 3);
    }

    #[test]
    fn causes_are_ordered_by_the_failures_they_explain() {
        let w = workers();
        let owners = Owners {
            workers: &w,
            contract: &[],
            test_files: &[],
            created: &not_created,
            roots: &OutputRoots::default(),
            known: &[],
        };
        let keys = vec![
            "typecheck|src/feed/list.ts|TS2322|a".to_string(),
            "typecheck|src/posts/edit.ts|TS2322|b".to_string(),
            "typecheck|src/posts/view.ts|TS2339|c".to_string(),
            "typecheck|src/main.ts|TS2304|Cannot find name 'x'.".to_string(),
        ];
        let causes = group(&keys, &owners, None);
        let owners: Vec<String> = causes.iter().map(|c| c.owner.describe()).collect();
        assert_eq!(
            owners,
            vec![
                "worker w2 (step 3)",
                "worker w1 (step 2)",
                "the lead (a seam: no worker owns it)"
            ]
        );
    }

    #[test]
    fn the_same_error_in_two_workers_files_without_a_contract_is_across_scopes() {
        let w = workers();
        let owners = Owners {
            workers: &w,
            contract: &[],
            test_files: &[],
            created: &not_created,
            roots: &OutputRoots::default(),
            known: &[],
        };
        let keys = vec![
            "typecheck|src/feed/list.ts|TS2304|Cannot find name 'Post'.".to_string(),
            "typecheck|src/posts/edit.ts|TS2304|Cannot find name 'Post'.".to_string(),
        ];
        let causes = group(&keys, &owners, None);
        assert_eq!(causes.len(), 1);
        assert_eq!(
            causes[0].owner,
            Owner::Lead {
                reason: LeadReason::AcrossScopes
            }
        );
    }

    #[test]
    fn a_failing_test_the_test_writer_wrote_goes_under_the_test_rule() {
        let w = workers();
        let tests = vec!["src/posts/posts.test.ts".to_string()];
        let owners = Owners {
            workers: &w,
            contract: &[],
            test_files: &tests,
            created: &not_created,
            roots: &OutputRoots::default(),
            known: &[],
        };
        let keys = vec![
            "test|test|posts > edits a post".to_string(),
            "test|test|feed > lists".to_string(),
        ];
        let causes = group(&keys, &owners, None);
        let found: Vec<&Owner> = causes.iter().map(|c| &c.owner).collect();
        assert!(found.contains(&&Owner::TestRule {
            file: "src/posts/posts.test.ts".into()
        }));
        assert!(found.contains(&&Owner::Lead {
            reason: LeadReason::Unlocated
        }));
    }

    #[test]
    fn a_test_name_with_a_bar_stays_a_test() {
        let f = parse_key("test|test|parses a | b");
        assert_eq!(f.file, None);
        assert_eq!(f.what, "test|parses a | b");
        assert_eq!(summary_of(&[f]), "test parses a | b fails");
        let e = parse_key("lint|src/a.ts|rule|x | y");
        assert_eq!(e.file.as_deref(), Some("src/a.ts"));
        assert_eq!(e.what, "rule|x | y");
    }

    #[test]
    fn a_new_file_belongs_to_the_one_worker_whose_folder_it_is_in() {
        let w = workers();
        let created = |p: &str| p == "src/posts/new.ts";
        let owners = Owners {
            workers: &w,
            contract: &[],
            test_files: &[],
            created: &created,
            roots: &OutputRoots::default(),
            known: &[],
        };
        let keys = vec!["typecheck|src/posts/new.ts|TS2322|x".to_string()];
        let causes = group(&keys, &owners, None);
        assert!(causes[0].owner.is_worker());
    }

    #[test]
    fn triage_causes_group_failures_by_their_files() {
        let w = workers();
        let owners = Owners {
            workers: &w,
            contract: &[],
            test_files: &[],
            created: &not_created,
            roots: &OutputRoots::default(),
            known: &[],
        };
        let triage = Triage {
            check: "typecheck".into(),
            root_cause: "`Post.title` was renamed in edit.ts".into(),
            files: vec!["`src/posts/edit.ts`: rename it back".into()],
            next_step: "rename".into(),
            confidence: "high".into(),
            confidence_note: String::new(),
            other_causes: vec![super::super::triage::OtherCause {
                files: vec!["src/feed/list.ts".into()],
                confidence: "high".into(),
                cause: "a missing import".into(),
            }],
        };
        let keys = vec![
            "typecheck|src/posts/edit.ts|TS2339|a".to_string(),
            "typecheck|src/posts/edit.ts|TS2339|b".to_string(),
            "typecheck|src/feed/list.ts|TS2304|c".to_string(),
        ];
        let causes = group(&keys, &owners, Some(&triage));
        assert_eq!(causes.len(), 2);
        assert_eq!(causes[0].summary, "`Post.title` was renamed in edit.ts");
        assert_eq!(causes[0].confidence.as_deref(), Some("high"));
        assert_eq!(parallel_set(&causes).len(), 2);
        // With a cause for the lead too, none of them go in parallel.
        let mut keys = keys;
        keys.push("lint|src/main.ts|x|y".into());
        let mut t = triage.clone();
        t.check = "test".into();
        let causes = group(&keys, &owners, Some(&t));
        assert!(parallel_set(&causes).is_empty());
    }

    /// V05: what a round's regressions are behind, by the failures' files
    /// and the tests' names; nothing when one names no changed file.
    #[test]
    fn a_regression_is_behind_the_files_it_names() {
        let changed: Vec<String> = [
            "src/users.ts",
            "src/posts.ts",
            "tests/posts.test.ts",
            "a.ts",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let roots =
            crate::folders::Folders::one("/p".into()).output_roots(std::path::Path::new("/p"));
        let behind = |keys: &[&str]| {
            let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
            files_behind(&keys, &changed, &roots)
        };
        // A test by its name: the changed files it carries.
        assert_eq!(
            behind(&["test|test|posts > lists posts"]),
            Some(vec![
                "src/posts.ts".to_string(),
                "tests/posts.test.ts".to_string()
            ])
        );
        // An error in a changed file, however it's printed.
        assert_eq!(
            behind(&["typecheck|/p/src/users.ts|TS2322|Type 'string' is not assignable"]),
            Some(vec!["src/users.ts".to_string()])
        );
        // Several keys: all of their files, once.
        assert_eq!(
            behind(&["test|test|users > saves", "lint|src/users.ts||unused"]),
            Some(vec!["src/users.ts".to_string()])
        );
        // One key named after no changed file: not known.
        assert_eq!(
            behind(&["test|test|users > saves", "test|test|tags > counts"]),
            None
        );
        // An error in a file the round didn't change, a short stem, a key
        // with nothing to go on: not known.
        assert_eq!(
            behind(&["typecheck|src/b.ts|TS2554|Expected 2 arguments"]),
            None
        );
        assert_eq!(behind(&["test|test|a"]), None);
        assert_eq!(behind(&["test|failed"]), None);
        assert_eq!(behind(&["test|and 6 more"]), None);
        assert_eq!(behind(&[]), None);
    }
}
