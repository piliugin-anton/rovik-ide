//! Which paths are a chat's work, and which are by-products.
//!
//! Build output (`target/`, `dist/`, …), dependencies (`node_modules/`,
//! `.venv/`, …) and `.git` change when a chat builds, installs or runs git,
//! but they're not changes to land in the project: landing skips them, and
//! they don't count toward a chat's size limit.
//!
//! What the project ignores (its `.gitignore` files, read with the `ignore`
//! crate: no git needed) is a by-product. A project without any falls back
//! to a short list of names that are build output or dependencies nearly
//! everywhere. Names that can be source in some projects (`build/`, `bin/`,
//! `vendor/`) count only when the project's ignore files say so.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// By-products whatever the project's ignore files say.
const ALWAYS: &[&str] = &[".git"];
/// By-products in a project that ignores nothing.
const FALLBACK: &[&str] = &[
    "node_modules", "target", ".venv", "__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".tox", ".next",
    ".nuxt", ".svelte-kit", ".turbo", ".gradle", ".dart_tool", ".stack-work", "dist-newstyle", ".zig-cache", "zig-cache",
];

pub struct Tiers {
    root: PathBuf,
    /// Each folder's own rules (its `.gitignore` and `.ignore`), rooted at
    /// that folder as git reads them; None: it has none.
    rules: Mutex<HashMap<String, Option<Gitignore>>>,
}

impl Tiers {
    pub fn new(root: &Path) -> Tiers {
        Tiers { root: root.to_path_buf(), rules: Mutex::new(HashMap::new()) }
    }

    /// Whether a project-relative path (with `/`) is build output,
    /// dependencies or `.git`.
    pub fn is_by_product(&self, rel: &str, is_dir: bool) -> bool {
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        if parts.iter().any(|p| ALWAYS.contains(p)) {
            return true;
        }
        // Ignored itself, or inside an ignored folder. Each path is checked
        // against the rules of the folders above it, the deepest first: the
        // first that says something decides (a deeper `!keep` un-ignores).
        let mut any_rules = false;
        for end in 1..=parts.len() {
            let path = self.root.join(parts[..end].join("/"));
            let as_dir = end < parts.len() || is_dir;
            for depth in (0..end).rev() {
                let Some(rules) = self.rules_of(&parts[..depth].join("/")) else { continue };
                any_rules = true;
                let m = rules.matched(&path, as_dir);
                if m.is_ignore() {
                    return true;
                }
                if m.is_whitelist() {
                    break;
                }
            }
        }
        // No ignore files on the way: the short list.
        !any_rules && parts.iter().any(|p| FALLBACK.contains(p))
    }

    /// A folder's own ignore rules, anchored at that folder.
    fn rules_of(&self, dir: &str) -> Option<Gitignore> {
        let mut cache = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(found) = cache.get(dir) {
            return found.clone();
        }
        let at = if dir.is_empty() { self.root.clone() } else { self.root.join(dir) };
        let mut builder = GitignoreBuilder::new(&at);
        let mut any = false;
        for name in [".gitignore", ".ignore"] {
            let file = at.join(name);
            if file.is_file() {
                any |= builder.add(file).is_none();
            }
        }
        let rules = if any { builder.build().ok() } else { None };
        cache.insert(dir.to_string(), rules.clone());
        rules
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project(files: &[(&str, &str)]) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("tiers-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (path, content) in files {
            let p = dir.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        dir
    }

    #[test]
    fn the_projects_ignore_files_decide() {
        let dir = project(&[(".gitignore", "/target\n*.log\n"), ("web/.gitignore", "dist/\n")]);
        let t = Tiers::new(&dir);
        assert!(t.is_by_product("target/debug/app", false));
        assert!(t.is_by_product("target", true));
        assert!(t.is_by_product("logs/run.log", false));
        assert!(t.is_by_product("web/dist/index.js", false));
        assert!(t.is_by_product(".git/index", false));
        assert!(!t.is_by_product("src/main.rs", false));
        assert!(!t.is_by_product("dist/keep.js", false), "only web/ ignores dist/");
        // Not ignored here, so source.
        assert!(!t.is_by_product("node_modules/x.js", false));
        assert!(!t.is_by_product("bin/tool.sh", false));
    }

    #[test]
    fn a_subfolders_rules_are_anchored_there_and_ignore_files_count() {
        let dir = project(&[("web/.gitignore", "/build\nout/gen\n"), ("api/.ignore", "cache/\n"), (".gitignore", "*.tmp\n!keep.tmp\n")]);
        let t = Tiers::new(&dir);
        assert!(t.is_by_product("web/build/app.js", false), "anchored at web/");
        assert!(!t.is_by_product("build/app.js", false), "not at the root");
        assert!(t.is_by_product("web/out/gen/x.rs", false));
        assert!(t.is_by_product("api/cache/x", false), ".ignore files count");
        assert!(t.is_by_product("a.tmp", false));
        assert!(!t.is_by_product("keep.tmp", false));
    }

    #[test]
    fn ignore_files_only_in_a_subfolder() {
        let dir = project(&[("web/.gitignore", "dist/\n")]);
        let t = Tiers::new(&dir);
        assert!(t.is_by_product("web/dist/a.js", false));
        // Elsewhere nothing says otherwise: the short list.
        assert!(t.is_by_product("node_modules/x.js", false));
    }

    #[test]
    fn without_ignore_files_a_short_list() {
        let dir = project(&[("src/a.rs", "")]);
        let t = Tiers::new(&dir);
        assert!(t.is_by_product("node_modules/x/index.js", false));
        assert!(t.is_by_product("target/debug/app", false));
        assert!(!t.is_by_product("build/script.sh", false));
        assert!(!t.is_by_product("src/a.rs", false));
    }
}
