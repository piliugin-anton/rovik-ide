//! What a connector's harness changes with its own tools (Agent mode,
//! connectors/mod.rs) is the run's change, as what Rovik's own tools change
//! is. After each turn, its steps that succeeded are recorded as the Rovik
//! calls that do the same (`Connector::step_calls`), so the checks run on
//! them (verify.rs). A file it changed is one of the run's changed files,
//! with what it was before, taken as the run let the harness's call change
//! it (`Run::answer_command_check`): a fix round that broke something puts
//! it back, and the tests in it are compared with what they were
//! (repair.rs, R02 and R05). A command that may have changed files counts
//! as Rovik's own would (`Run::note_command`).
//!
//! Only steps that succeeded count: a harness reports a call refused before
//! it ran as it reports one that failed, and neither changed its file. So a
//! command that changed files and then failed isn't counted, and a command
//! longer than its step keeps of it is judged by the part kept.

use super::repair::{read, rel_key};
use super::run::Run;
use super::{permissions, tools};
use crate::connectors::{HarnessCall, HarnessStep};
use std::collections::HashMap;
use std::path::Path;

/// A file as it was before one of the harness's calls changed it: its path
/// relative to the root, and its bytes (None: it didn't exist).
pub(super) type Before = (String, Option<Vec<u8>>);

/// The files the harness's calls were let change in a turn, each as it was
/// before the first of them.
#[derive(Default)]
pub(super) struct Befores(HashMap<String, Option<Vec<u8>>>);

impl Befores {
    /// What a file was before a call: kept unless an earlier call of the
    /// turn already gave it.
    pub(super) fn add(&mut self, (key, bytes): Before) {
        self.0.entry(key).or_insert(bytes);
    }
}

impl Run {
    /// A file one of the harness's calls is about to change (`path`, as the
    /// call names it), as it is now. None outside the project, or when it
    /// can't be read.
    pub(super) fn before_harness_write(&self, path: &str) -> Option<Before> {
        let root = self.root.as_deref()?;
        crate::agent_tools::resolve_within_root(root, path).ok()?;
        let key = rel_key(root, path);
        let bytes = read(Path::new(&tools::to_absolute_path(&key, root))).ok()?;
        Some((key, bytes))
    }

    /// After a turn of a connector's harness: what its steps that succeeded
    /// changed (`befores`: what the files were before), recorded as the
    /// run's own calls' changes are.
    pub(super) fn note_harness_steps(&mut self, steps: &[HarnessStep], mut befores: Befores) {
        let (Some(kind), Some(root)) = (
            crate::connectors::kind_of(&self.profile.base_url),
            self.root.clone(),
        ) else {
            return;
        };
        let connector = crate::connectors::connector(kind);
        for step in steps.iter().filter(|step| step.ok == Some(true)) {
            for call in connector.step_calls(step) {
                match call {
                    HarnessCall::Command(command) => self.note_command(&command),
                    HarnessCall::Tool { name, args } if permissions::modifies_files(&name) => {
                        if let Some(path) = permissions::trimmed_str(args.get("path")) {
                            self.note_harness_write(&root, path, &mut befores);
                        }
                    }
                    HarnessCall::Tool { .. } => {}
                }
            }
        }
    }

    /// A file a step of the harness's changed (`path`, as the step names
    /// it): one of the run's changed files, with what it was. One outside
    /// the project (a scratch file in the system's temp folder) isn't the
    /// project's change.
    fn note_harness_write(&mut self, root: &str, path: &str, befores: &mut Befores) {
        if crate::agent_tools::resolve_within_root(root, path).is_err() {
            return;
        }
        let key = rel_key(root, path);
        let abs = tools::to_absolute_path(&key, root);
        let before = befores.0.remove(&key);
        if let Some(bytes) = &before {
            self.note_original(&key, bytes.clone());
        }
        self.note_write(&key, Some(&abs));
        // The run has seen the file as the harness left it (gone: as it
        // was), as it has what its own tools write: a rollback undoes only
        // what the run changed last (repair.rs, `roll_back`).
        let abs = Path::new(&abs);
        match read(abs) {
            Ok(Some(bytes)) => self.seen.record(abs, &bytes),
            Ok(None) => {
                if let Some(Some(bytes)) = &before {
                    self.seen.record(abs, bytes);
                }
            }
            Err(_) => {}
        }
    }
}
