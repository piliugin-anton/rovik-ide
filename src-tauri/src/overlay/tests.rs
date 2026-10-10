use super::fs::{OverlayChange, OverlayFs};
use crate::vfs::backend::Backend;
use crate::vfs::store::{Kind, RenameMode, SetAttr, VfsError, ROOT};
use std::fs;
use std::path::{Path, PathBuf};

struct Dirs {
    root: PathBuf,
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Dirs {
    fn project(&self) -> PathBuf {
        self.root.join("project")
    }
    fn store(&self) -> PathBuf {
        self.root.join("store")
    }
}

fn setup(files: &[(&str, &str)]) -> Dirs {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!("overlay-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&root);
    let d = Dirs { root };
    fs::create_dir_all(d.project()).unwrap();
    for (path, content) in files {
        let p = d.project().join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    d
}

fn overlay(d: &Dirs) -> OverlayFs {
    OverlayFs::open(&d.project(), &d.store(), 1 << 30).unwrap()
}

fn ino(o: &OverlayFs, path: &str) -> u64 {
    o.resolve(path).unwrap()
}

fn read_all(o: &OverlayFs, path: &str) -> String {
    String::from_utf8(o.read(ino(o, path), 0, 1 << 20).unwrap()).unwrap()
}

fn write_all(o: &OverlayFs, path: &str, content: &str) {
    let i = ino(o, path);
    o.setattr(i, &SetAttr { size: Some(0), ..Default::default() }).unwrap();
    o.write(i, 0, content.as_bytes()).unwrap();
}

fn paths(changes: &[OverlayChange]) -> Vec<String> {
    changes
        .iter()
        .map(|c| match c {
            OverlayChange::Delete { path, .. } => format!("-{path}"),
            OverlayChange::Write { path, .. } => format!("+{path}"),
        })
        .collect()
}

#[test]
fn untouched_files_read_through_and_writes_stay_in_the_overlay() {
    let d = setup(&[("src/a.txt", "one\n"), ("src/b.txt", "two\n")]);
    let o = overlay(&d);
    assert_eq!(read_all(&o, "src/a.txt"), "one\n");
    // The project underneath is live for what the chat hasn't changed.
    fs::write(d.project().join("src/b.txt"), "TWO\n").unwrap();
    assert_eq!(read_all(&o, "src/b.txt"), "TWO\n");

    write_all(&o, "src/a.txt", "changed\n");
    assert_eq!(read_all(&o, "src/a.txt"), "changed\n");
    assert_eq!(fs::read_to_string(d.project().join("src/a.txt")).unwrap(), "one\n");
    let changes = o.changes();
    assert_eq!(paths(&changes), vec!["+src/a.txt"]);
    let OverlayChange::Write { base_copy: Some(base), upper: Some(upper), .. } = &changes[0] else { panic!() };
    assert_eq!(fs::read_to_string(base).unwrap(), "one\n");
    assert_eq!(fs::read_to_string(upper).unwrap(), "changed\n");
}

#[test]
fn creates_deletes_and_renames_are_listed_for_landing() {
    let d = setup(&[("a.txt", "a"), ("dir/b.txt", "b"), ("dir/c.txt", "c"), ("keep.txt", "k")]);
    let o = overlay(&d);
    let f = o.create(ROOT, "new.txt", Kind::File, 0o644, 0, 0, None).unwrap();
    o.write(f.ino, 0, b"new").unwrap();
    o.unlink(ROOT, "a.txt").unwrap();
    o.rename(ino(&o, "dir"), "b.txt", ROOT, "b-moved.txt", RenameMode::Replace).unwrap();
    assert_eq!(o.lookup(ROOT, "a.txt").unwrap_err(), VfsError::NotFound);
    assert_eq!(read_all(&o, "b-moved.txt"), "b");
    assert_eq!(
        paths(&o.changes()),
        vec!["-a.txt", "+b-moved.txt", "-dir/b.txt", "+new.txt"]
    );
    // Moved back: no change left there.
    o.rename(ROOT, "b-moved.txt", ino(&o, "dir"), "b.txt", RenameMode::Replace).unwrap();
    assert_eq!(paths(&o.changes()), vec!["-a.txt", "+new.txt"]);
}

#[test]
fn a_moved_folder_keeps_its_files_and_a_replaced_one_is_deleted_first() {
    let d = setup(&[("old/x.txt", "x"), ("old/sub/y.txt", "y"), ("src/z.txt", "z")]);
    let o = overlay(&d);
    o.rename(ROOT, "old", ROOT, "new", RenameMode::Replace).unwrap();
    assert_eq!(read_all(&o, "new/sub/y.txt"), "y");
    // `rm -rf src && mkdir src`: the project's src goes, the new one comes.
    let src = ino(&o, "src");
    o.unlink(src, "z.txt").unwrap();
    o.rmdir(ROOT, "src").unwrap();
    o.create(ROOT, "src", Kind::Dir, 0o755, 0, 0, None).unwrap();
    assert_eq!(
        paths(&o.changes()),
        vec!["-old", "-src", "+new", "+new/sub", "+new/sub/y.txt", "+new/x.txt", "+src"]
    );
}

#[test]
fn files_appearing_in_the_project_show_up_unless_the_chat_removed_them() {
    let d = setup(&[("a.txt", "a")]);
    let o = overlay(&d);
    o.readdir_plus(ROOT).unwrap();
    o.unlink(ROOT, "a.txt").unwrap();
    fs::write(d.project().join("later.txt"), "l").unwrap();
    let names: Vec<String> = o.readdir_plus(ROOT).unwrap().into_iter().map(|(e, _)| e.name).collect();
    assert_eq!(names, vec!["later.txt"]);
    // Untouched entries that leave the project leave the view too.
    fs::remove_file(d.project().join("later.txt")).unwrap();
    assert!(o.readdir_plus(ROOT).unwrap().is_empty());
}

#[test]
fn the_limit_counts_changes_but_not_build_output() {
    let d = setup(&[(".gitignore", "/target\n"), ("src/a.txt", "")]);
    let o = OverlayFs::open(&d.project(), &d.store(), 1000).unwrap();
    let warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let w = warned.clone();
    o.on_warn(Box::new(move |_, _| w.store(true, std::sync::atomic::Ordering::Relaxed)));
    let a = ino(&o, "src/a.txt");
    o.write(a, 0, &[b'x'; 700]).unwrap();
    assert!(!warned.load(std::sync::atomic::Ordering::Relaxed));
    o.write(a, 700, &[b'x'; 150]).unwrap();
    assert!(warned.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(o.write(a, 850, &[b'x'; 200]).unwrap_err(), VfsError::NoSpace);
    // Build output doesn't count.
    let target = o.create(ROOT, "target", Kind::Dir, 0o755, 0, 0, None).unwrap();
    let big = o.create(target.ino, "app", Kind::File, 0o755, 0, 0, None).unwrap();
    o.write(big.ino, 0, &[0; 5000]).unwrap();
    assert_eq!(o.used().0, 850);
    let OverlayChange::Write { by_product, .. } = o.changes().into_iter().find(|c| c.path() == "target/app").unwrap() else {
        panic!()
    };
    assert!(by_product);
}

#[test]
fn an_overlay_reopens_as_it_was() {
    let d = setup(&[("a.txt", "a"), ("dir/b.txt", "b"), ("gone.txt", "g")]);
    {
        let o = overlay(&d);
        write_all(&o, "a.txt", "A");
        o.unlink(ROOT, "gone.txt").unwrap();
        o.rename(ROOT, "dir", ROOT, "dir2", RenameMode::Replace).unwrap();
        let n = o.create(ROOT, "n.txt", Kind::File, 0o600, 0, 0, None).unwrap();
        o.write(n.ino, 0, b"N").unwrap();
        o.setattr(ino(&o, "dir2/b.txt"), &SetAttr { perm: Some(0o755), ..Default::default() }).unwrap();
        o.flush();
    }
    let o = overlay(&d);
    assert_eq!(read_all(&o, "a.txt"), "A");
    assert_eq!(read_all(&o, "n.txt"), "N");
    assert_eq!(read_all(&o, "dir2/b.txt"), "b");
    assert_eq!(o.getattr(ino(&o, "dir2/b.txt")).unwrap().perm, 0o755);
    assert_eq!(o.lookup(ROOT, "gone.txt").unwrap_err(), VfsError::NotFound);
    assert_eq!(o.lookup(ROOT, "dir").unwrap_err(), VfsError::NotFound);
    assert_eq!(paths(&o.changes()), vec!["-dir", "-gone.txt", "+a.txt", "+dir2", "+dir2/b.txt", "+n.txt"]);
}

#[test]
fn renames_follow_posix_rules() {
    let d = setup(&[("f.txt", "f"), ("g.txt", "g"), ("d/x", "x"), ("e/.keep", "")]);
    let o = overlay(&d);
    assert_eq!(o.rename(ROOT, "f.txt", ROOT, "g.txt", RenameMode::NoReplace).unwrap_err(), VfsError::Exists);
    o.rename(ROOT, "f.txt", ROOT, "g.txt", RenameMode::Exchange).unwrap();
    assert_eq!(read_all(&o, "g.txt"), "f");
    assert_eq!(read_all(&o, "f.txt"), "g");
    assert_eq!(o.rename(ROOT, "d", ino(&o, "d"), "inside", RenameMode::Replace).unwrap_err(), VfsError::Invalid);
    assert_eq!(o.rename(ROOT, "f.txt", ROOT, "d", RenameMode::Replace).unwrap_err(), VfsError::IsDir);
    assert_eq!(o.rename(ROOT, "e", ROOT, "d", RenameMode::Replace).unwrap_err(), VfsError::NotEmpty);
    assert_eq!(o.rmdir(ROOT, "d").unwrap_err(), VfsError::NotEmpty);
    // A hard link shares the content; removing one name keeps the other.
    let f = ino(&o, "f.txt");
    o.link(f, ROOT, "f2.txt").unwrap();
    o.write(f, 0, b"G2").unwrap();
    assert_eq!(read_all(&o, "f2.txt"), "G2");
    o.unlink(ROOT, "f.txt").unwrap();
    assert_eq!(read_all(&o, "f2.txt"), "G2");
}

#[test]
fn a_file_unlinked_while_open_stays_readable_until_released() {
    let d = setup(&[]);
    let o = overlay(&d);
    let f = o.create(ROOT, "tmp", Kind::File, 0o644, 0, 0, None).unwrap();
    o.write(f.ino, 0, b"data").unwrap();
    o.open(f.ino, false).unwrap();
    o.unlink(ROOT, "tmp").unwrap();
    assert_eq!(o.read(f.ino, 0, 10).unwrap(), b"data");
    o.release(f.ino);
    assert_eq!(o.read(f.ino, 0, 10).unwrap_err(), VfsError::NotFound);
    assert!(o.changes().is_empty());
}

/// Through a real mount: ordinary programs and random operations, checked
/// against the same operations on a plain copy of the project.
#[cfg(target_os = "linux")]
mod mounted {
    use super::*;
    use std::sync::Arc;

    fn mount(d: &Dirs) -> Option<(crate::vfs::Mount, PathBuf, Arc<OverlayFs>)> {
        let o = Arc::new(overlay(d));
        let at = d.root.join("view");
        match crate::vfs::platform_mount(o.clone(), Some(&at)) {
            Ok((m, at)) => Some((m, at, o)),
            Err(e) => {
                eprintln!("no FUSE here, skipping: {e:?}");
                None
            }
        }
    }

    fn sh(dir: &Path, cmd: &str) {
        let ok = std::process::Command::new("sh").args(["-c", cmd]).current_dir(dir).status().unwrap().success();
        assert!(ok, "{cmd}");
    }

    #[test]
    #[ignore] // needs FUSE: cargo test -- --ignored
    fn common_tools_change_only_the_overlay() {
        let d = setup(&[("src/main.rs", "fn main() {}\n"), ("README.md", "hi\n"), ("old/x", "x\n")]);
        let Some((m, view, o)) = mount(&d) else { return };
        sh(&view, "sed -i 's/main/start/' src/main.rs");
        sh(&view, "echo more >> README.md && mkdir -p docs/a && echo doc > docs/a/b.md");
        sh(&view, "mv old renamed && rm README.md && chmod +x src/main.rs && ln -s src/main.rs link");
        sh(&view, "test \"$(cat src/main.rs)\" = 'fn start() {}' && test -x src/main.rs && test \"$(cat link)\" = 'fn start() {}'");
        // The project is untouched.
        assert_eq!(fs::read_to_string(d.project().join("src/main.rs")).unwrap(), "fn main() {}\n");
        assert!(d.project().join("README.md").exists());
        assert!(d.project().join("old/x").exists());
        let mut p = paths(&o.changes());
        p.sort();
        assert_eq!(p, vec!["+docs", "+docs/a", "+docs/a/b.md", "+link", "+renamed", "+renamed/x", "+src/main.rs", "-README.md", "-old"]);
        // `sed -i` saved through a temporary file: still the same file, with
        // what it was before as the base to merge against.
        let main = o.changes().into_iter().find(|c| c.path() == "src/main.rs").unwrap();
        let OverlayChange::Write { origin, base_copy: Some(base), .. } = main else { panic!("{main:?}") };
        assert_eq!(origin.as_deref(), Some("src/main.rs"));
        assert_eq!(fs::read_to_string(base).unwrap(), "fn main() {}\n");
        m.unmount().unwrap();
    }

    #[test]
    #[ignore] // needs FUSE
    fn random_operations_match_a_plain_copy() {
        let files: Vec<(String, String)> =
            (0..6).map(|i| (format!("d{}/f{i}.txt", i % 3), format!("content {i}\n"))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let d = setup(&refs);
        let copy = d.root.join("copy");
        sh(&d.root, "cp -r project copy");
        let Some((m, view, _o)) = mount(&d) else { return };
        let mut seed: u64 = 0x2545F4914F6CDD1D;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for step in 0..300 {
            let a = format!("d{}/f{}.txt", next(4), next(8));
            let b = format!("d{}/f{}.txt", next(4), next(8));
            let op = next(6);
            let apply = |root: &Path| -> bool {
                let (pa, pb) = (root.join(&a), root.join(&b));
                match op {
                    0 => fs::write(&pa, format!("step {step}\n")).is_ok(),
                    1 => fs::OpenOptions::new().append(true).open(&pa).and_then(|mut f| std::io::Write::write_all(&mut f, b"+")).is_ok(),
                    2 => fs::remove_file(&pa).is_ok(),
                    3 => fs::rename(&pa, &pb).is_ok(),
                    4 => fs::create_dir_all(pa.parent().unwrap()).is_ok(),
                    _ => fs::remove_dir(pa.parent().unwrap()).is_ok(),
                }
            };
            let (x, y) = (apply(&copy), apply(&view));
            assert_eq!(x, y, "step {step}: op {op} on {a} / {b}");
        }
        assert_eq!(snapshot(&copy), snapshot(&view));
        m.unmount().unwrap();
    }

    fn snapshot(root: &Path) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir).unwrap() {
                let e = e.unwrap();
                let rel = e.path().strip_prefix(root).unwrap().to_string_lossy().into_owned();
                if e.file_type().unwrap().is_dir() {
                    out.push((rel, None));
                    stack.push(e.path());
                } else {
                    out.push((rel, Some(fs::read_to_string(e.path()).unwrap())));
                }
            }
        }
        out.sort();
        out
    }
}
