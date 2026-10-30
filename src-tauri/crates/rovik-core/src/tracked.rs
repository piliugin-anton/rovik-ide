//! What git tracks in a project. A `.gitignore` rule applies only to files
//! git doesn't track: a file committed before the rule was added (or added
//! with `--force`) is still the user's, and git lists its changes. What
//! takes ignored paths for build output (a chat's by-products in
//! overlay/tiers.rs, the app's file watcher) asks here first.

use std::path::Path;

/// The repository a project is in, for what its index tracks.
pub struct Tracked {
    repo: git2::Repository,
    /// The project's folder in the repository's working folder, as the
    /// index names paths: `/`-separated, ending in `/`; "" when the project
    /// is the working folder.
    prefix: String,
}

impl Tracked {
    /// The repository `project` is in; None outside one (or in a bare one).
    pub fn of(project: &Path) -> Option<Tracked> {
        let repo = git2::Repository::discover(project).ok()?;
        // Both as the file system names them: one may be reached through a
        // link (macOS's /tmp), or spelled verbatim (Windows).
        let workdir = std::fs::canonicalize(repo.workdir()?).ok()?;
        let project = std::fs::canonicalize(project).ok()?;
        let prefix = project
            .strip_prefix(&workdir)
            .ok()?
            .components()
            .map(|part| format!("{}/", part.as_os_str().to_string_lossy()))
            .collect();
        Some(Tracked { repo, prefix })
    }

    /// Whether git tracks `rel`, a `/`-separated path in the project, or,
    /// when it's a folder, anything in it. The index is read as git last
    /// wrote it: again only when it changed on disk.
    pub fn covers(&self, rel: &str) -> bool {
        let Ok(mut index) = self.repo.index() else {
            return false;
        };
        // A failed read keeps what was read before.
        let _ = index.read(false);
        let path = format!("{}{}", self.prefix, rel.trim_matches('/'));
        let path = path.trim_end_matches('/');
        // The file itself is the first entry its name begins: a longer name
        // sorts after it.
        let file = index
            .find_prefix(path)
            .ok()
            .and_then(|at| index.get(at))
            .is_some_and(|entry| entry.path == path.as_bytes());
        file || index.find_prefix(format!("{path}/")).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A repository at `dir` with `files` written and staged: the index is
    /// what git tracks, committed or not.
    fn repo_tracking(dir: &Path, files: &[&str]) -> git2::Repository {
        let repo = git2::Repository::init(dir).unwrap();
        let mut index = repo.index().unwrap();
        for file in files {
            let abs = dir.join(file);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(&abs, "x\n").unwrap();
            index.add_path(Path::new(file)).unwrap();
        }
        index.write().unwrap();
        repo
    }

    #[test]
    fn a_tracked_file_and_the_folders_holding_it_are_covered() {
        let dir = crate::testing::temp_dir();
        repo_tracking(
            &dir.0,
            &[
                ".superpowers/brainstorm/s1/layout.html",
                ".superpowers-old/x",
            ],
        );
        fs::write(dir.0.join("notes.txt"), "not added\n").unwrap();
        let tracked = Tracked::of(&dir.0).unwrap();
        for path in [
            ".superpowers/brainstorm/s1/layout.html",
            ".superpowers/brainstorm",
            ".superpowers",
            ".superpowers/",
        ] {
            assert!(tracked.covers(path), "{path}");
        }
        for path in [
            ".superpowers/brainstorm/s1/other.html",
            ".superpowers/sdd",
            ".super",
            "notes.txt",
            "target",
        ] {
            assert!(!tracked.covers(path), "{path}");
        }
    }

    #[test]
    fn a_name_that_only_begins_a_tracked_one_is_not_covered() {
        let dir = crate::testing::temp_dir();
        repo_tracking(&dir.0, &[".superpowers-old/x", "build.rs"]);
        let tracked = Tracked::of(&dir.0).unwrap();
        assert!(!tracked.covers(".superpowers"));
        assert!(!tracked.covers("build"));
        assert!(tracked.covers("build.rs"));
    }

    #[test]
    fn what_git_adds_later_is_covered_without_opening_the_repository_again() {
        let dir = crate::testing::temp_dir();
        let repo = repo_tracking(&dir.0, &["src/main.rs"]);
        let tracked = Tracked::of(&dir.0).unwrap();
        assert!(!tracked.covers("dist/app.js"));
        // As `git add --force dist/app.js` in a terminal does.
        fs::create_dir_all(dir.0.join("dist")).unwrap();
        fs::write(dir.0.join("dist/app.js"), "x\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("dist/app.js")).unwrap();
        index.write().unwrap();
        assert!(tracked.covers("dist/app.js"));
        assert!(tracked.covers("dist"));
    }

    #[test]
    fn a_project_in_a_subfolder_of_the_repository_sees_its_own_paths() {
        let dir = crate::testing::temp_dir();
        repo_tracking(
            &dir.0,
            &["packages/app/.superpowers/x.md", "other/.superpowers/y.md"],
        );
        let tracked = Tracked::of(&dir.0.join("packages/app")).unwrap();
        assert!(tracked.covers(".superpowers"));
        assert!(tracked.covers(".superpowers/x.md"));
        assert!(!tracked.covers(".superpowers/y.md"));
        assert!(!tracked.covers("other"));
    }

    #[test]
    fn outside_a_repository_nothing_is_tracked() {
        let dir = crate::testing::temp_dir();
        assert!(Tracked::of(&dir.0).is_none());
    }
}
