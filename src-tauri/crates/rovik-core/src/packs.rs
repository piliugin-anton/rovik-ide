//! Git packs between a client's shadow repository and a daemon's project
//! repository (6.4), built and taken in the same way on both sides. A pack
//! holds what the wanted commits reach and the other side's commits don't,
//! and it is taken in whole or not at all: a pack cut short leaves nothing.

use git2::{ObjectType, Odb, Oid, Repository};
use std::collections::HashSet;
use std::io::Write;

/// Builds the pack of the objects `wants` reach and `haves` don't, and
/// hands it to `out` a piece at a time; `out` returns false to stop. Haves
/// that aren't commits here are passed over: the other side may have more
/// than this one. Returns how many objects the pack holds.
pub fn build(
    repo: &Repository,
    wants: &[Oid],
    haves: &[Oid],
    out: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<usize, String> {
    let mut walk = repo.revwalk().map_err(git)?;
    for want in wants {
        walk.push(*want).map_err(git)?;
    }
    for have in haves {
        if repo.find_commit(*have).is_ok() {
            walk.hide(*have).map_err(git)?;
        }
    }
    let mut builder = repo.packbuilder().map_err(git)?;
    builder.insert_walk(&mut walk).map_err(git)?;
    let objects = builder.object_count();
    builder.foreach(|piece| out(piece)).map_err(git)?;
    Ok(objects)
}

/// Takes a pack into the repository's objects, from `pieces` in order.
/// Returns how many objects it held; a pack that is cut short or broken is
/// refused, and leaves nothing behind. No pieces at all is no pack.
pub fn receive<I>(repo: &Repository, pieces: I) -> Result<usize, String>
where
    I: IntoIterator<Item = Vec<u8>>,
{
    let mut pieces = pieces.into_iter().peekable();
    if pieces.peek().is_none() {
        return Ok(0);
    }
    let odb = repo.odb().map_err(git)?;
    let mut writer = odb.packwriter().map_err(git)?;
    let objects = std::rc::Rc::new(std::cell::Cell::new(0));
    let total = objects.clone();
    writer.progress(move |progress| {
        total.set(progress.total_objects());
        true
    });
    for piece in pieces {
        writer
            .write_all(&piece)
            .map_err(|_| "The pack is broken.".to_string())?;
    }
    writer
        .commit()
        .map_err(|e| format!("The pack is broken: {}", e.message()))?;
    Ok(objects.get())
}

/// Whether the repository has `commit`, its whole tree and its parents:
/// what a checkout of it and a walk past it need.
pub fn complete(repo: &Repository, commit: Oid) -> bool {
    let (Ok(commit), Ok(odb)) = (repo.find_commit(commit), repo.odb()) else {
        return false;
    };
    commit.parent_ids().all(|parent| odb.exists(parent))
        && tree_complete(repo, &odb, commit.tree_id(), &mut HashSet::new())
}

fn tree_complete(repo: &Repository, odb: &Odb, tree: Oid, seen: &mut HashSet<Oid>) -> bool {
    if !seen.insert(tree) {
        return true;
    }
    let Ok(tree) = repo.find_tree(tree) else {
        return false;
    };
    tree.iter().all(|entry| match entry.kind() {
        Some(ObjectType::Tree) => tree_complete(repo, odb, entry.id(), seen),
        Some(ObjectType::Blob) => odb.exists(entry.id()),
        // A gitlink names a commit of another repository.
        _ => true,
    })
}

fn git(e: git2::Error) -> String {
    e.message().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_dir;
    use git2::Signature;
    use std::path::Path;

    /// Commits `files` on top of the repository's `HEAD`.
    fn commit(repo: &Repository, files: &[(&str, &str)]) -> Oid {
        let root = repo.workdir().unwrap();
        let mut index = repo.index().unwrap();
        for (path, content) in files {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), content).unwrap();
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let me = Signature::now("Test", "test@example.com").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<_> = parent.iter().collect();
        repo.commit(Some("HEAD"), &me, &me, "work", &tree, &parents)
            .unwrap()
    }

    fn pack(repo: &Repository, wants: &[Oid], haves: &[Oid]) -> (usize, Vec<u8>) {
        let mut bytes = Vec::new();
        let objects = build(repo, wants, haves, &mut |piece| {
            bytes.extend_from_slice(piece);
            true
        })
        .unwrap();
        (objects, bytes)
    }

    fn pack_files(repo: &Repository) -> Vec<String> {
        let dir = repo.path().join("objects/pack");
        let names = std::fs::read_dir(dir).unwrap().flatten();
        names
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_pack_carries_what_the_other_side_lacks() {
        let dir = temp_dir();
        let here = Repository::init(dir.0.join("here")).unwrap();
        let there = Repository::init_bare(dir.0.join("there.git")).unwrap();
        commit(&here, &[("a.txt", "a\n"), ("src/b.rs", "b\n")]);
        let second = commit(&here, &[("a.txt", "a, again\n")]);

        let (objects, bytes) = pack(&here, &[second], &[]);
        assert_eq!(objects, 8, "two commits, three trees, three blobs");
        assert_eq!(receive(&there, [bytes]), Ok(8));
        assert!(complete(&there, second));

        let third = commit(&here, &[("src/b.rs", "b, changed\n")]);
        let (objects, bytes) = pack(&here, &[third], &[second]);
        assert_eq!(objects, 4, "a commit, the root and src trees, a blob");
        assert_eq!(receive(&there, [bytes]), Ok(4));
        assert!(complete(&there, third));

        let (objects, bytes) = pack(&here, &[third], &[third]);
        assert_eq!(objects, 0);
        assert_eq!(receive(&there, [bytes]), Ok(0));
        assert_eq!(receive(&there, Vec::<Vec<u8>>::new()), Ok(0));
    }

    #[test]
    fn a_cut_pack_is_refused_and_leaves_nothing() {
        let dir = temp_dir();
        let here = Repository::init(dir.0.join("here")).unwrap();
        let there = Repository::init_bare(dir.0.join("there.git")).unwrap();
        let first = commit(&here, &[("a.txt", "a\n")]);
        let (_, mut bytes) = pack(&here, &[first], &[]);
        bytes.truncate(bytes.len() - 10);

        assert!(receive(&there, [bytes]).is_err());
        assert_eq!(pack_files(&there), Vec::<String>::new());
        assert!(!complete(&there, first));
    }

    #[test]
    fn a_commit_without_its_parent_or_tree_is_incomplete() {
        let dir = temp_dir();
        let here = Repository::init(dir.0.join("here")).unwrap();
        let there = Repository::init_bare(dir.0.join("there.git")).unwrap();
        let first = commit(&here, &[("a.txt", "a\n")]);
        let second = commit(&here, &[("a.txt", "b\n")]);
        // What `second` adds to `first`, taken in where `first` isn't.
        let (_, bytes) = pack(&here, &[second], &[first]);
        receive(&there, [bytes]).unwrap();
        assert!(!complete(&there, second));

        // `first` alone, without its tree.
        let odb = here.odb().unwrap();
        let raw = odb.read(first).unwrap();
        let there_odb = there.odb().unwrap();
        there_odb.write(ObjectType::Commit, raw.data()).unwrap();
        assert!(!complete(&there, first), "its tree isn't here");
    }
}
