//! Each project's bare repository on the daemon (5.1), and the packs that
//! go in and out of it (4.4, 6.4): the refs it has, a pack pushed to move
//! one of them, and a pack of what a client lacks. A ref moves only once
//! its commit is complete here, and only from the commit the client last
//! saw there.

use git2::{ErrorCode, Oid, Reference, Repository};
use rovik_core::packs;
use rovik_protocol::packs::{Fetch, Refs, Update};
use rovik_protocol::{Fail, FailCode};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The projects' repositories, at `projects/<project id>.git`.
#[derive(Clone)]
pub struct Projects {
    dir: PathBuf,
}

impl Projects {
    pub fn new(dir: PathBuf) -> Projects {
        Projects { dir }
    }

    /// Each ref of the project, with its commit: none before its first
    /// push.
    pub fn refs(&self, project: &str) -> Result<Refs, Fail> {
        let Some(repo) = self.open(project)? else {
            return Ok(Refs::default());
        };
        let mut refs = BTreeMap::new();
        for reference in repo.references().map_err(internal)?.flatten() {
            if let (Ok(name), Some(id)) = (reference.name(), reference.target()) {
                refs.insert(name.to_string(), id.to_string());
            }
        }
        Ok(Refs { refs })
    }

    /// Takes in the pack `pieces` bring, then moves the update's ref to
    /// its commit, when the commit is complete here and the ref still
    /// points at `old`. Returns how many objects the pack held.
    pub fn receive<I>(&self, project: &str, update: &Update, pieces: I) -> Result<usize, Fail>
    where
        I: IntoIterator<Item = Vec<u8>>,
    {
        if !Reference::is_valid_name(&update.name) {
            return Err(bad_request(format!("{} isn't a ref's name.", update.name)));
        }
        let repo = self.repository(project)?;
        let objects = packs::receive(&repo, pieces).map_err(bad_request)?;
        let new = oid(&update.new)?;
        if !packs::complete(&repo, new) {
            return Err(bad_request(
                "The pack leaves the commit incomplete: the ref stays where it was.",
            ));
        }
        let moved = match &update.old {
            Some(old) => repo.reference_matching(&update.name, new, true, oid(old)?, "push"),
            None => repo.reference(&update.name, new, false, "push"),
        };
        moved.map_err(|e| match e.code() {
            ErrorCode::Modified | ErrorCode::Exists => Fail::new(
                FailCode::Busy,
                format!("{} moved since the refs were read.", update.name),
            ),
            _ => internal(e),
        })?;
        Ok(objects)
    }

    /// The project's repository and the commits of `fetch`, when it wants
    /// only what the project's refs point at. Haves it doesn't know are
    /// left to the pack to pass over.
    pub fn wanted(&self, project: &str, fetch: &Fetch) -> Result<Wanted, Fail> {
        let refs = self.refs(project)?;
        let tips: Vec<&String> = refs.refs.values().collect();
        if let Some(stray) = fetch.wants.iter().find(|want| !tips.contains(want)) {
            return Err(bad_request(format!("{stray} isn't a ref's commit here.")));
        }
        let wants = fetch
            .wants
            .iter()
            .map(|id| oid(id))
            .collect::<Result<_, _>>()?;
        let haves = fetch.haves.iter().filter_map(|id| Oid::from_str(id).ok());
        Ok(Wanted {
            repo: self.repository(project)?,
            wants,
            haves: haves.collect(),
        })
    }

    /// The project's repository, ready for its chats' worktrees (6.4):
    /// they check out what the snapshots hold, byte for byte (K40), and
    /// ignore what the client ignores (K32) and the project's ID file.
    pub fn prepare(&self, project: &str, excludes: &[String]) -> Result<Repository, Fail> {
        let repo = self.repository(project)?;
        let mut config = repo.config().map_err(internal)?;
        config.set_bool("core.autocrlf", false).map_err(internal)?;
        let mut rules =
            String::from("# What the client ignores and the server doesn't know of (K32).\n");
        for pattern in excludes {
            rules.push_str(pattern);
            rules.push('\n');
        }
        rules.push_str(
            "# The project's ID file, which each worktree has.\n**/.rovik/project.json\n",
        );
        let file = repo.path().join("info/exclude");
        std::fs::create_dir_all(repo.path().join("info"))
            .and_then(|()| std::fs::write(file, rules))
            .map_err(|e| Fail::new(FailCode::Internal, e.to_string()))?;
        Ok(repo)
    }

    /// The projects that have a repository here.
    pub fn ids(&self) -> Vec<String> {
        let entries = std::fs::read_dir(&self.dir).into_iter().flatten().flatten();
        let names = entries.map(|entry| entry.file_name().to_string_lossy().into_owned());
        names
            .filter_map(|name| name.strip_suffix(".git").map(String::from))
            .collect()
    }

    /// The project's repository, when it has one.
    pub fn open(&self, project: &str) -> Result<Option<Repository>, Fail> {
        Ok(Repository::open_bare(self.path(project)?).ok())
    }

    /// The project's repository, created the first time.
    fn repository(&self, project: &str) -> Result<Repository, Fail> {
        match self.open(project)? {
            Some(repo) => Ok(repo),
            None => Repository::init_bare(self.path(project)?).map_err(internal),
        }
    }

    fn path(&self, project: &str) -> Result<PathBuf, Fail> {
        let plain = |c: char| c.is_ascii_alphanumeric() || c == '-';
        if project.is_empty() || project.len() > 64 || !project.chars().all(plain) {
            return Err(bad_request(format!("{project} isn't a project's id.")));
        }
        Ok(self.dir.join(format!("{project}.git")))
    }
}

/// What a fetch's pack is built from.
pub struct Wanted {
    pub repo: Repository,
    pub wants: Vec<Oid>,
    pub haves: Vec<Oid>,
}

impl Wanted {
    /// Builds the pack, handing it to `out` a piece at a time.
    pub fn send(&self, out: &mut dyn FnMut(&[u8]) -> bool) -> Result<usize, Fail> {
        packs::build(&self.repo, &self.wants, &self.haves, out)
            .map_err(|e| Fail::new(FailCode::Internal, e))
    }
}

fn oid(id: &str) -> Result<Oid, Fail> {
    Oid::from_str(id).map_err(|_| bad_request(format!("{id} isn't a commit's id.")))
}

fn bad_request(message: impl Into<String>) -> Fail {
    Fail::new(FailCode::BadRequest, message)
}

fn internal(e: git2::Error) -> Fail {
    Fail::new(FailCode::Internal, e.message())
}
