//! Subordinate ids on a cloud host (D21): the uids and gids each user's
//! Podman maps the users inside their containers to, from `/etc/subuid`
//! and `/etc/subgid`. A container's `postgres` (uid 70, say) is the user's
//! first subordinate uid plus 69 on the server: an id no one else owns, so
//! what runs in a container can't touch the user's own files, nor anyone
//! else's.
//!
//! `useradd` gives a new local account a range on most distributions, but
//! an account made another way (an older `useradd`, LDAP, a cloud image's
//! first user) may have none, and then its user's containers can't run.
//! Rovik adds a range of [`COUNT`] where one is missing: for each user with
//! a daemon here when Rovik is installed or upgraded (`admin containers`),
//! and for a user whose daemon `user start` starts. A range is never moved
//! or changed: files a container wrote are owned by it.

use super::{flock_file, Passwd, System};

/// The files, uids' and gids'.
const FILES: [&str; 2] = ["/etc/subuid", "/etc/subgid"];

/// The ids in a range Rovik adds: every id a container's image may use
/// (`nobody` is 65534).
pub(crate) const COUNT: u64 = 65536;

/// Where the first range starts, as shadow's `login.defs` has it
/// (`SUB_UID_MIN`): above the ids of every account.
const FIRST: u64 = 100_000;

/// One line of a subuid or subgid file: `owner:start:count`, the owner a
/// name or a uid.
#[derive(Debug, PartialEq, Eq)]
struct Range<'a> {
    owner: &'a str,
    start: u64,
    count: u64,
}

/// The ranges `text`, a subuid or subgid file, gives. A line that isn't
/// one is left out, as shadow leaves it.
fn ranges(text: &str) -> impl Iterator<Item = Range<'_>> {
    text.lines().filter_map(|line| {
        let mut fields = line.trim().split(':');
        let (owner, start, count) = (fields.next()?, fields.next()?, fields.next()?);
        if owner.is_empty() || fields.next().is_some() {
            return None;
        }
        Some(Range {
            owner,
            start: start.parse().ok()?,
            count: count.parse().ok()?,
        })
    })
}

/// The ids `text` gives the user named `name` with uid `uid`, all their
/// ranges together.
pub(crate) fn ids_of(text: &str, name: &str, uid: u32) -> u64 {
    let uid = uid.to_string();
    ranges(text)
        .filter(|range| range.owner == name || range.owner == uid)
        .map(|range| range.count)
        .sum()
}

/// The start of [`COUNT`] ids no range of `texts` gives anyone: after the
/// last range of all, and no lower than [`FIRST`]. The same in both files,
/// though nothing needs that, so they're easy to read side by side.
fn free_start(texts: &[&str]) -> u64 {
    texts
        .iter()
        .flat_map(|text| ranges(text))
        .map(|range| range.start.saturating_add(range.count))
        .fold(FIRST, u64::max)
}

/// `user`'s ranges, in each file that lacks one: whether any was added.
/// Under another root (a dry run) the files there are read and written.
/// One run at a time changes them, holding `/etc/rovik/.subids.lock`, so
/// two users starting their daemons at once never get the same range.
pub(super) fn ensure(sys: &System, user: &Passwd) -> Result<bool, String> {
    let _lock = flock_file(&sys.path("/etc/rovik/.subids.lock"))?;
    let read = |file: &str| {
        let path = sys.path(file);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    };
    let texts = [read(FILES[0])?, read(FILES[1])?];
    let lacking: Vec<usize> = (0..FILES.len())
        .filter(|&i| ids_of(&texts[i], &user.name, user.uid) == 0)
        .collect();
    if lacking.is_empty() {
        return Ok(false);
    }
    let start = free_start(&[&texts[0], &texts[1]]);
    for i in lacking {
        let mut text = texts[i].clone();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("{}:{start}:{COUNT}\n", user.name));
        sys.write(FILES[i], text.as_bytes(), 0o644)?;
    }
    println!(
        "{} has subordinate ids {start}-{} for their containers now.",
        user.name,
        start + COUNT - 1
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn user(name: &str, uid: u32) -> Passwd {
        Passwd {
            name: name.into(),
            uid,
            gid: uid,
            home: PathBuf::from(format!("/home/{name}")),
            shell: "/bin/sh".into(),
        }
    }

    #[test]
    fn a_users_ids_are_counted_by_their_name_or_their_uid() {
        let text =
            "ann:100000:65536\n1001:165536:1000\n# a comment\nbob:x:5\n\ncarl:300000:65536:extra\n";
        assert_eq!(ids_of(text, "ann", 1000), 65536);
        assert_eq!(ids_of(text, "dan", 1001), 1000, "by uid");
        assert_eq!(ids_of(text, "bob", 1002), 0, "a line that isn't one");
        assert_eq!(ids_of(text, "carl", 1003), 0, "four fields isn't a range");
    }

    #[test]
    fn a_new_range_starts_after_every_range_in_either_file() {
        assert_eq!(free_start(&["", ""]), 100_000);
        assert_eq!(
            free_start(&["ann:100000:65536\n", "ann:100000:65536\n"]),
            165_536
        );
        // The gids may run further than the uids: after both.
        assert_eq!(
            free_start(&["ann:100000:65536\n", "ann:100000:65536\nbob:231072:65536\n"]),
            296_608
        );
        // Ranges below the first are passed over, not fitted between.
        assert_eq!(free_start(&["old:1000:500\n", ""]), 100_000);
    }

    #[test]
    fn a_user_without_ids_gets_a_range_in_each_file_and_keeps_it() {
        let dir = rovik_core::testing::temp_dir();
        let sys = System::new(Some(dir.0.clone()));
        std::fs::create_dir_all(dir.0.join("etc/rovik")).unwrap();
        std::fs::write(dir.0.join("etc/subuid"), "ann:100000:65536").unwrap();
        std::fs::write(dir.0.join("etc/subgid"), "ann:100000:65536\n").unwrap();

        assert_eq!(ensure(&sys, &user("bob", 1001)), Ok(true));
        let uids = std::fs::read_to_string(dir.0.join("etc/subuid")).unwrap();
        let gids = std::fs::read_to_string(dir.0.join("etc/subgid")).unwrap();
        assert_eq!(uids, "ann:100000:65536\nbob:165536:65536\n");
        assert_eq!(gids, "ann:100000:65536\nbob:165536:65536\n");

        // Asked again, nothing changes: a range is never moved.
        assert_eq!(ensure(&sys, &user("bob", 1001)), Ok(false));
        assert_eq!(ensure(&sys, &user("ann", 1000)), Ok(false));
        assert_eq!(
            std::fs::read_to_string(dir.0.join("etc/subuid")).unwrap(),
            uids
        );
    }

    #[test]
    fn a_missing_file_is_made_and_only_the_file_lacking_a_range_changes() {
        let dir = rovik_core::testing::temp_dir();
        let sys = System::new(Some(dir.0.clone()));
        std::fs::create_dir_all(dir.0.join("etc")).unwrap();
        std::fs::write(dir.0.join("etc/subuid"), "cara:200000:65536\n").unwrap();

        assert_eq!(ensure(&sys, &user("cara", 1002)), Ok(true));
        assert_eq!(
            std::fs::read_to_string(dir.0.join("etc/subuid")).unwrap(),
            "cara:200000:65536\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.0.join("etc/subgid")).unwrap(),
            "cara:265536:65536\n"
        );
    }
}
