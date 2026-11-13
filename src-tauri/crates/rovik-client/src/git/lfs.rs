//! libgit2 does not execute external clean filters. LFS repositories use
//! Git for staging and status, and LFS plumbing before publishing refs.
//! Repository hooks remain independent: never overwrite a user's hook,
//! and never let `--no-verify` bypass the object upload.

use super::{
    hooks::{collect, Hooks},
    ErrText, GitFile,
};
use git2::{AttrCheckFlags, Repository};
use rovik_core::agent_run::command_output::{cap_output, clean_output};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};

const POINTER_HEADER: &[u8] = b"version https://git-lfs.github.com/spec/v1\n";

fn path_from_bytes(bytes: &[u8]) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Path::new(std::ffi::OsStr::from_bytes(bytes)).to_path_buf()
    }
    #[cfg(not(unix))]
    Path::new(&String::from_utf8_lossy(bytes).into_owned()).to_path_buf()
}

/// Require the standard filter even when `git lfs install` was never run.
/// Command-local configuration leaves global settings and hooks untouched.
pub(super) fn configure(cmd: &mut Command) {
    cmd.args([
        "-c",
        "filter.lfs.required=true",
        "-c",
        "filter.lfs.clean=git-lfs clean -- %f",
        "-c",
        "filter.lfs.smudge=git-lfs smudge -- %f",
        "-c",
        "filter.lfs.process=git-lfs filter-process",
        "-c",
        "lfs.allowincompletepush=false",
        "-c",
        "lfs.fetchexclude=",
    ])
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_ASKPASS", "")
    .env_remove("GIT_LFS_SKIP_PUSH");
}

fn run(repo: &Repository, args: &[&str], input: Option<&[u8]>) -> Result<(), String> {
    let (mut cmd, out) = Hooks::of(repo).command("git", input.is_some())?;
    configure(&mut cmd);
    let child = cmd.args(args).spawn().map_err(|e| {
        format!(
            "Couldn't start Git: {e}. Install Git and Git LFS \
                 on the machine running this repository."
        )
    })?;
    drop(cmd);
    let (status, output) = collect(child, out, input)?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Git LFS failed ({status}): {}\nInstall Git LFS on the machine running \
             this repository and check its credentials and object storage.",
            cap_output(clean_output(&output).trim(), 10, 60, 8000)
        ))
    }
}

/// Whether Git attributes select the LFS filter for this path.
pub(crate) fn is_lfs_path(
    repo: &Repository,
    path: &Path,
    flags: AttrCheckFlags,
) -> Result<bool, String> {
    Ok(repo.get_attr(path, "filter", flags).err_text()? == Some("lfs"))
}

fn is_pointer(repo: &Repository, oid: git2::Oid) -> Result<bool, String> {
    let (size, kind) = repo.odb().err_text()?.read_header(oid).err_text()?;
    // Avoid loading assets just to discover that they are not pointers.
    if kind != git2::ObjectType::Blob || size > 1024 {
        return Ok(false);
    }
    Ok(repo
        .find_blob(oid)
        .err_text()?
        .content()
        .starts_with(POINTER_HEADER))
}

/// Detect attributes (including nested, global and info/attributes) and
/// existing pointers, rather than assuming a filename extension means LFS.
pub(super) fn uses_lfs(repo: &Repository) -> Result<bool, String> {
    for entry in repo.index().err_text()?.iter() {
        if entry.mode & 0o170000 != 0o100000 {
            continue;
        }
        let path = path_from_bytes(&entry.path);
        if is_lfs_path(repo, &path, AttrCheckFlags::FILE_THEN_INDEX)?
            || (entry.file_size <= 1024 && is_pointer(repo, entry.id)?)
        {
            return Ok(true);
        }
    }
    for entry in repo
        .statuses(Some(&mut super::status_options()))
        .err_text()?
        .iter()
    {
        if entry.status().is_wt_new()
            && is_lfs_path(
                repo,
                &path_from_bytes(entry.path_bytes()),
                AttrCheckFlags::FILE_THEN_INDEX,
            )?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

// project_scope escapes glob characters for libgit2. Git's literal magic
// needs the original path, including actual backslashes in Unix filenames.
fn native_scope(scope: &str) -> String {
    let mut path = String::from(":(top,literal)");
    let mut chars = scope.chars();
    while let Some(c) = chars.next() {
        path.push(if c == '\\' {
            chars.next().unwrap_or(c)
        } else {
            c
        });
    }
    path
}

/// Stages the project's changes through Git's required LFS clean filter.
pub(crate) fn stage(repo: &Repository, scope: &str) -> Result<(), String> {
    let spec = native_scope(scope);
    run(repo, &["add", "--all", "--", &spec], None)
}

/// Stages a conflict resolution through the LFS clean filter.
pub(crate) fn stage_file(repo: &Repository, path: &Path) -> Result<(), String> {
    stage(repo, &super::changes::literal_pathspec(path))
}

/// A hook may replace staged entries itself. Validate again after it runs,
/// so libgit2-based hooks cannot reintroduce an unfiltered asset.
pub(crate) fn check_index(repo: &Repository, index: &git2::Index) -> Result<(), String> {
    for entry in index.iter() {
        if entry.mode & 0o170000 != 0o100000 {
            continue;
        }
        let path = path_from_bytes(&entry.path);
        if is_lfs_path(repo, &path, AttrCheckFlags::INDEX_ONLY)?
            && !is_pointer(repo, entry.id)?
            && repo.odb().err_text()?.read_header(entry.id).err_text()?.0 != 0
        {
            return Err(format!(
                "Not committed: {} is tracked by Git LFS but the index contains its full contents. Restage it with Git LFS and check whether a commit hook bypasses LFS filters.",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Look through outgoing history too: an LFS asset may have been removed
/// from HEAD, or its attributes may have changed since it was committed.
fn history_uses_lfs(
    repo: &Repository,
    local: git2::Oid,
    remote: git2::Oid,
) -> Result<bool, String> {
    let mut walk = repo.revwalk().err_text()?;
    walk.push(local).err_text()?;
    if !remote.is_zero() && repo.find_commit(remote).is_ok() {
        walk.hide(remote).err_text()?;
    }
    let mut visited = HashSet::new();
    for oid in walk {
        let tree = repo
            .find_commit(oid.err_text()?)
            .err_text()?
            .tree()
            .err_text()?;
        if tree_uses_lfs(repo, tree, &mut visited)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn tree_uses_lfs(
    repo: &Repository,
    tree: git2::Tree<'_>,
    visited: &mut HashSet<git2::Oid>,
) -> Result<bool, String> {
    if !visited.insert(tree.id()) {
        return Ok(false);
    }
    for entry in tree.iter() {
        match entry.kind() {
            Some(git2::ObjectType::Tree) => {
                if tree_uses_lfs(repo, repo.find_tree(entry.id()).err_text()?, visited)? {
                    return Ok(true);
                }
            }
            Some(git2::ObjectType::Blob) => {
                if !visited.insert(entry.id()) {
                    continue;
                }
                if is_pointer(repo, entry.id())? {
                    return Ok(true);
                }
                if entry.name_bytes() == b".gitattributes" {
                    let blob = repo.find_blob(entry.id()).err_text()?;
                    if blob
                        .content()
                        .windows(b"filter=lfs".len())
                        .any(|s| s == b"filter=lfs")
                    {
                        return Ok(true);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Validates outgoing pointers and uploads objects before any ref is sent.
pub(super) fn pre_push(
    repo: &Repository,
    remote_name: &str,
    updates: &[git2::PushUpdate<'_>],
    input: &[u8],
) -> Result<(), String> {
    let mut enabled = uses_lfs(repo)?;
    for update in updates {
        if !enabled
            && !update.dst().is_zero()
            && history_uses_lfs(repo, update.dst(), update.src())?
        {
            enabled = true;
            break;
        }
    }
    if !enabled {
        return Ok(());
    }
    for update in updates {
        if update.dst().is_zero() {
            continue;
        }
        let revisions = if !update.src().is_zero() && repo.find_commit(update.src()).is_ok() {
            vec![format!("{}..{}", update.src(), update.dst())]
        } else {
            // fsck of HEAD alone misses raw binaries in earlier commits.
            // One root plus root..tip covers every outgoing commit, including
            // merged histories; rev-list also honors shallow boundaries.
            let roots = output(
                repo,
                &["rev-list", "--max-parents=0", &update.dst().to_string()],
            )?;
            let roots = String::from_utf8(roots).map_err(|e| e.to_string())?;
            let root = roots
                .lines()
                .next()
                .ok_or("Git did not return a history root for the push")?;
            vec![root.to_string(), format!("{root}..{}", update.dst())]
        };
        for revision in revisions {
            run(repo, &["lfs", "fsck", "--pointers", &revision], None).map_err(|e| {
                format!(
                    "{e}\nAn outgoing commit may contain a raw LFS-tracked file. \
                         Repair the unpublished commits with Git LFS (for example, \
                         `git lfs migrate import` after reviewing its history changes), \
                         then retry. Rovik has not rewritten any commits."
                )
            })?;
        }
    }
    // The URL argument is optional and ignored by git-lfs; resolving the
    // remote itself honors pushurl, .lfsconfig, storage and credential helpers.
    run(repo, &["lfs", "pre-push", remote_name], Some(input))
}

fn output(repo: &Repository, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = rovik_core::process::command("git");
    configure(&mut cmd);
    cmd.current_dir(super::workdir(repo)?)
        .args(args)
        .stdin(Stdio::null());
    rovik_core::process::no_window(&mut cmd);
    rovik_core::process::own_session(&mut cmd);
    let out = cmd.output().map_err(|e| {
        format!("Couldn't start Git: {e}. Install Git and Git LFS on this machine.")
    })?;
    if !out.status.success() {
        return Err(format!(
            "Git LFS inspection failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(out.stdout)
}

/// Native status runs the clean filter and refreshes Git's stat cache, so a
/// hydrated large asset stays clean without hashing it on every UI poll.
pub(super) fn status_files(repo: &Repository, scope: &str) -> Result<Vec<GitFile>, String> {
    let spec = native_scope(scope);
    let bytes = output(
        repo,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            &spec,
        ],
    )?;
    let mut files = parse_status(&bytes)?;
    let mut counts = HashMap::<String, (usize, usize)>::new();
    for args in [
        vec![
            "diff",
            "--numstat",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--",
            &spec,
        ],
        vec![
            "diff",
            "--cached",
            "--numstat",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--",
            &spec,
        ],
    ] {
        for record in output(repo, &args)?
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
        {
            let mut fields = record.splitn(3, |b| *b == b'\t');
            let (Some(add), Some(del), Some(path)) = (fields.next(), fields.next(), fields.next())
            else {
                return Err("Git returned an invalid diff record".into());
            };
            let total = counts
                .entry(String::from_utf8_lossy(path).into_owned())
                .or_default();
            total.0 += String::from_utf8_lossy(add).parse::<usize>().unwrap_or(0);
            total.1 += String::from_utf8_lossy(del).parse::<usize>().unwrap_or(0);
        }
    }
    for file in &mut files {
        if is_lfs_path(repo, Path::new(&file.path), AttrCheckFlags::FILE_THEN_INDEX)? {
            continue;
        }
        let (add, del) = counts.get(&file.path).copied().unwrap_or_default();
        file.additions = add;
        file.deletions = del;
        if file.status == "?" {
            file.additions =
                super::text_lines(&super::workdir(repo)?.join(&file.path)).unwrap_or(0);
        }
    }
    Ok(files)
}

fn parse_status(bytes: &[u8]) -> Result<Vec<GitFile>, String> {
    let mut records = bytes.split(|b| *b == 0).filter(|s| !s.is_empty());
    let mut files = Vec::new();
    while let Some(record) = records.next() {
        if record.len() < 4 || record[2] != b' ' {
            return Err("Git returned an invalid status record".into());
        }
        let (x, y) = (record[0], record[1]);
        if [x, y].iter().any(|s| matches!(s, b'R' | b'C')) && records.next().is_none() {
            return Err("Git returned a rename without its source path".into());
        }
        let conflict = x == b'U' || y == b'U' || matches!((x, y), (b'A', b'A') | (b'D', b'D'));
        let status = if conflict {
            "U"
        } else if x == b'?' {
            "?"
        } else if x == b'A' {
            "A"
        } else if x == b'D' || y == b'D' {
            "D"
        } else if x == b'R' || y == b'R' {
            "R"
        } else {
            "M"
        };
        files.push(GitFile {
            path: String::from_utf8_lossy(&record[3..]).into_owned(),
            status: status.into(),
            staged: x != b' ' && x != b'?' && !conflict,
            unstaged: y != b' ' && y != b'?' && !conflict,
            additions: 0,
            deletions: 0,
        });
    }
    Ok(files)
}

#[cfg(test)]
mod tests;
