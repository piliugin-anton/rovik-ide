//! Open folders are client-owned; a second window must never redirect another
//! project's remote results or editor context.
use rovik_core::chat_db::projects::path_key;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(super) struct OpenProjects(HashMap<String, (String, PathBuf)>);

impl OpenProjects {
    pub(super) fn set(&mut self, client: &str, project: Option<(String, PathBuf)>) {
        if let Some(project) = project {
            self.0.insert(client.into(), project);
        } else {
            self.0.remove(client);
        }
    }
    pub(super) fn client(&self, client: &str) -> Option<(String, PathBuf)> {
        self.0.get(client).cloned()
    }
    pub(super) fn contains(&self, project: &str) -> bool {
        self.0.values().any(|(id, _)| id == project)
    }
    pub(super) fn ids(&self) -> HashSet<String> {
        self.0.values().map(|(id, _)| id.clone()).collect()
    }
    pub(super) fn at(&self, root: &Path) -> Option<String> {
        self.0
            .values()
            .find(|(_, path)| path_key(path) == path_key(root))
            .map(|(id, _)| id.clone())
    }
    pub(super) fn other(
        &self,
        client: &str,
        project: &str,
        root: Option<&Path>,
    ) -> Option<PathBuf> {
        self.0
            .iter()
            .find(|(owner, (id, path))| {
                *owner != client
                    && (id == project || root.is_some_and(|root| path_key(root) == path_key(path)))
            })
            .map(|(_, (_, path))| path.clone())
    }
    pub(super) fn root(&self, project: &str) -> Result<Option<PathBuf>, String> {
        let mut paths = self
            .0
            .values()
            .filter(|(id, _)| id == project)
            .map(|(_, path)| path);
        let Some(first) = paths.next() else {
            return Ok(None);
        };
        if paths.any(|path| path_key(path) != path_key(first)) {
            return Err("This project's chats are open at different folders; close the other copy before using its remote results.".into());
        }
        Ok(Some(first.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projects_keep_their_roots_and_ambiguous_copies_are_refused() {
        let mut open = OpenProjects::default();
        open.set("a", Some(("p".into(), "/one".into())));
        open.set("b", Some(("q".into(), "/two".into())));
        assert_eq!(open.root("p").unwrap(), Some("/one".into()));
        assert_eq!(open.other("b", "p", None), Some("/one".into()));
        open.set("b", Some(("p".into(), "/copy".into())));
        assert!(open.root("p").is_err());
        open.set("b", None);
        assert_eq!(open.root("p").unwrap(), Some("/one".into()));
    }
}
